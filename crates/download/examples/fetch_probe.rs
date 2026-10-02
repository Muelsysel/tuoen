//! 真机验收：下载层与镜像加速（票据 #4）。
//!
//! 单元测试用的是注入的 `FakeTransport`，它证明的是**语义**；这个例子
//! 跑的是**真实的 `curl.exe` 与真实的镜像**，它证明的是**这些语义在
//! 这个网络上真的成立**。两者缺一不可 —— 环回测试（`tests/loopback.rs`）
//! 覆盖了"数据不出本机"的那一半，这里覆盖"出了本机也算数"的那一半。
//!
//! 跑法：
//!
//! ```powershell
//! cargo run -p tuoen-download --example fetch_probe
//! ```
//!
//! 输出是给人看的（也贴进 `docs/acceptance/L0-03-download.md`），
//! 不是给脚本解析的 —— **这里不是公开契约，`--json` 才是**。
//!
//! ## 它刻意包含三类"应当失败"的步骤
//!
//! 一个只演示成功路径的验收报告回答不了"出问题了会怎样"。所以：
//!
//! * 一个**指向不存在主机**的用户镜像排在候选第一位 → 演示真实网络上的换源回退；
//! * 一个 **404** 的 URL → 演示失败分类是 `source-unavailable` 而不是"未知错误"；
//! * 一个**哈希写错**的制品 → 演示 `ChecksumMismatch` 会同时报出期望值与实际值，
//!   并且**不会**去试下一个源（被污染的字节不该靠换个镜像来"修好"）。

use std::collections::BTreeMap;
use std::time::Duration;

use tuoen_download::source::probe_sources;
use tuoen_download::{
    Artifact, Cache, DownloadError, FetchOptions, HttpTransport, MirrorConfig, MirrorTemplate,
    Transport, candidates_for, fetch, rank_by_probe, use_local_file,
};

/// 验收用的制品：本机装的 Node 就是 24.19.0，所以这不是随便挑的版本。
const NODE_VERSION: &str = "24.19.0";
const NODE_FILE: &str = "node-v24.19.0-win-x64.zip";
const NODE_URL: &str = "https://nodejs.org/dist/v24.19.0/node-v24.19.0-win-x64.zip";

/// 刻意指向一个**不存在的主机**，用来在真实网络上演示换源回退。
///
/// 用 `.invalid` 是 RFC 2606 保留的顶级域，保证它永远不会被解析成真东西
/// —— 我们想要的是"DNS 直接失败"，而不是"碰巧连上了谁"。
const DEAD_MIRROR: &str = "https://mirror.invalid/{url}";

fn main() {
    let mut exit = 0;
    if let Err(problem) = run(&mut exit) {
        eprintln!("\n验收没能跑完：{problem}");
        std::process::exit(1);
    }
    std::process::exit(exit);
}

fn run(exit: &mut i32) -> Result<(), Box<dyn std::error::Error>> {
    section("0. 传输层");
    let transport = HttpTransport::probe();
    match transport.program() {
        Some(program) => println!("curl: {}", program.display()),
        None => {
            println!("这台机器上没有 curl.exe，验收无法进行。");
            return Err("找不到 curl.exe".into());
        }
    }
    report(
        "用系统 curl 而不是打包 TLS 栈",
        "Schannel 走 Windows 证书存储 → 企业 TLS 拦截的根证书自动被信任（决策 39）",
    );

    section("1. 取上游索引（SHASUMS256.txt）");
    // 索引本身没有"发布的哈希"可验（它就是哈希的来源），所以直接用
    // 传输层读它。**这一步是本次验收的关键**：下面那个期望值不是我们
    // 抄进代码的常量，而是**刚从上游读回来的**。
    let index_url = format!("https://nodejs.org/dist/v{NODE_VERSION}/SHASUMS256.txt");
    let index = transport.fetch(&index_url, None, Duration::from_secs(60))?;
    let index_text = String::from_utf8_lossy(&index.bytes).into_owned();
    let published: BTreeMap<String, String> = index_text
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                return None;
            }
            let (hash, name) = line.split_once(char::is_whitespace)?;
            Some((name.trim().to_owned(), hash.trim().to_ascii_lowercase()))
        })
        .collect();
    let expected = published
        .get(NODE_FILE)
        .ok_or_else(|| format!("上游的 SHASUMS256.txt 里没有 {NODE_FILE}"))?
        .clone();
    println!("索引：{index_url}");
    println!(
        "  取回 {} 字节，解析出 {} 条制品",
        index.bytes.len(),
        published.len()
    );
    println!("  {NODE_FILE}");
    println!("  sha256 = {expected}   ← 来自上游，不是代码里的常量");

    section("2. 候选源（含一个故意坏掉的用户镜像）");
    let mirrors = MirrorConfig {
        // **用户模板优先。** 它指向一个不存在的主机，所以下载的第一步
        // 必定失败 —— 这正是我们要演示的路径。
        mirrors: vec![MirrorTemplate {
            id: "dead-on-purpose".to_owned(),
            match_prefix: "https://nodejs.org/dist/".to_owned(),
            template: DEAD_MIRROR.to_owned(),
            tool: String::new(),
            forward_credentials: false,
        }],
        use_builtin_mirrors: true,
        include_official: true,
    };
    let candidates = candidates_for("node", NODE_URL, &mirrors);
    for (index, source) in candidates.iter().enumerate() {
        println!(
            "  {}. {:<14} {:<11} {}",
            index + 1,
            source.id,
            source.region.as_str(),
            source.url
        );
    }
    println!("  顺序 = 用户模板 → 内置国内镜像 → 官方兜底");
    println!("  第 1 条指向 {DEAD_MIRROR} —— 它一定会失败，用来验证回退");

    section("3. 探测每个源的可用性（只取 1 个字节）");
    let probes = probe_sources(&transport, &candidates, Duration::from_secs(15));
    for probe in rank_by_probe(&probes) {
        println!(
            "  {:<14} {:<10} {:>6}ms  {}",
            probe.source.id,
            if probe.reachable {
                format!("可达 {}", probe.status)
            } else if probe.status == 0 {
                "不可达".to_owned()
            } else {
                format!("HTTP {}", probe.status)
            },
            probe.millis,
            probe.detail
        );
    }
    report(
        "探测发的是 range GET（`-r 0-0`）而不是 HEAD",
        "实测有镜像对 HEAD 回 405 而 GET 正常 —— 用 HEAD 会把能用的源淘汰掉",
    );
    report(
        "探测打的是 probe_url 而不是制品前缀",
        "阿里源的制品前缀目录 404（不可列举），但制品本身完全正常",
    );

    section("3b. Temurin（Adoptium）的源 —— 票据点名要的第二个工具");
    // 这里**不下载**（JDK 约 190 MB），只探测。目的是把"国内镜像不是
    // 普遍真理"这条结论从断言变成证据：同一台机器、同一个网络，
    // Node 有三个源可用，Adoptium 只有一个。
    let temurin_upstream =
        "https://api.adoptium.net/v3/binary/latest/21/ga/windows/x64/jdk/hotspot/normal/eclipse";
    let temurin_candidates = candidates_for("temurin", temurin_upstream, &MirrorConfig::default());
    for (index, source) in temurin_candidates.iter().enumerate() {
        println!(
            "  {}. {:<14} {:<11} {}",
            index + 1,
            source.id,
            source.region.as_str(),
            source.url
        );
    }
    println!("  **注意官方排在最前**，与 Node 表相反 —— 见 `TEMURIN_SOURCES` 的文档");
    let temurin_probes = probe_sources(&transport, &temurin_candidates, Duration::from_secs(15));
    let mut temurin_usable = 0;
    for probe in rank_by_probe(&temurin_probes) {
        if probe.reachable {
            temurin_usable += 1;
        }
        println!(
            "  {:<14} {:<10} {:>6}ms  {}",
            probe.source.id,
            if probe.reachable {
                format!("可达 {}", probe.status)
            } else if probe.status == 0 {
                "不可达".to_owned()
            } else {
                format!("HTTP {}", probe.status)
            },
            probe.millis,
            probe.detail
        );
    }
    println!("  {temurin_usable} / {} 个源可用", temurin_probes.len());
    if temurin_usable == 0 {
        println!("  ✗ **一个 Temurin 源都到不了** —— 这台机器上装不了 JDK");
        *exit = 1;
    }

    section("4. 真的下一次（30 MB，带哈希校验，第 1 个源必定失败）");
    let cache = Cache::at_default_location();
    println!("缓存目录：{}", cache.root().display());
    let artifact = Artifact::new("node", "node", NODE_URL, "sha256", &expected);
    let options = FetchOptions {
        mirrors: mirrors.clone(),
        timeout: Duration::from_secs(180),
        cache: cache.clone(),
        use_cache: true,
        store_in_cache: true,
        destination: None,
        attempts_per_source: 1,
    };
    let report_file = fetch(&transport, &artifact, &options)?;
    println!("  取到：{}", report_file.path.display());
    println!(
        "  大小：{} 字节（{:.1} MB）",
        report_file.bytes,
        report_file.bytes as f64 / 1_048_576.0
    );
    println!(
        "  来自：{}  {}",
        report_file.source_id, report_file.source_url
    );
    println!("  缓存命中：{}", report_file.from_cache);
    println!(
        "  在成功之前失败了 {} 个源：",
        report_file.failures_before_success()
    );
    for attempt in &report_file.attempts {
        let verdict = if attempt.ok { "OK  " } else { "失败" };
        let kind = attempt
            .kind
            .map_or_else(String::new, |kind| format!(" [{}]", kind.as_str()));
        println!(
            "    {verdict} {:<14} {:>6}ms{kind}  {}",
            attempt.source_id,
            attempt.millis,
            attempt.error.as_deref().unwrap_or("")
        );
    }
    if report_file.failures_before_success() == 0 {
        println!("  **注意**：没有失败就成功了 —— 那么这次验收没有覆盖回退路径。");
        *exit = 1;
    }

    // 独立核对：我们自己算出来的摘要与上游公布的必须一致。
    let actual = tuoen_download::verify_file(&report_file.path)?;
    println!("  我们自己算的 sha256 = {actual}");
    if actual == expected {
        println!("  ✓ 与上游公布的一致");
    } else {
        println!("  ✗ **不一致** 期望 {expected}");
        *exit = 1;
    }

    section("5. 再下一次（应当一个字节都不传）");
    let again = fetch(&transport, &artifact, &options)?;
    println!("  缓存命中：{}", again.from_cache);
    println!("  尝试次数：{}（应当是 0）", again.attempts.len());
    if again.from_cache && again.attempts.is_empty() {
        println!("  ✓ 第二次没有产生任何网络请求");
    } else {
        println!("  ✗ **第二次还是走了网络**");
        *exit = 1;
    }
    let stats = cache.stats();
    println!(
        "  缓存现状：{} 个制品 / {} 字节",
        stats.entries, stats.bytes
    );

    section("6. 哈希不符：报出期望值与实际值，并且不换源");
    // 故意把算法和哈希都写成错的。**用索引那个小文件**，不是 30 MB 的包
    // —— 反正会在校验那一步停下，没必要为此再下一次大包。
    let wrong = Artifact::new(
        "node-index",
        "node",
        &index_url,
        "sha256",
        "0000000000000000000000000000000000000000000000000000000000000000",
    );
    let wrong_options = FetchOptions {
        mirrors: MirrorConfig::default(),
        timeout: Duration::from_secs(60),
        cache: Cache::new(cache.root().join("acceptance-wrong-hash")),
        use_cache: true,
        store_in_cache: true,
        destination: None,
        attempts_per_source: 1,
    };
    match fetch(&transport, &wrong, &wrong_options) {
        Err(DownloadError::ChecksumMismatch {
            expected,
            actual,
            bytes,
            ..
        }) => {
            println!("  ✓ 报的是 ChecksumMismatch");
            println!("    期望 sha256 = {expected}");
            println!("    实际 sha256 = {actual}");
            println!("    字节数      = {bytes}");
            // 独立复核这个"实际值"：它应当是**同一份字节**的摘要。
            // 直接拿第 1 步取回来的那份重算 —— 下载层算的与实际值若
            // 出自同一个错掉的实现，两边会一起错；用另一个入口
            // （`sha256_hex`）重算才排得掉这种情况。
            let recomputed = tuoen_download::sha256_hex(&index.bytes);
            println!("    独立重算    = {recomputed}");
            if recomputed == actual {
                println!("    ✓ 两个入口算出的摘要一致");
            } else {
                println!("    ✗ **两个入口算出的摘要不一致**");
                *exit = 1;
            }
            // 报的是 ChecksumMismatch 而**不是** AllSourcesFailed，
            // 这本身就是"没有去试下一个源"的证据：`fetch` 只有在
            // 第一次失败就返回时才会把单个错误直接抛出来。
            println!("    ……而且没有换源：报的是单条错误，不是 AllSourcesFailed");
        }
        Err(other) => {
            println!("  ✗ 期望 ChecksumMismatch，实际是 {other}");
            *exit = 1;
        }
        Ok(_) => {
            println!("  ✗ **哈希写错了居然成功了**");
            *exit = 1;
        }
    }

    section("7. 404 的分类是 source-unavailable（会触发换源）");
    let missing = Artifact::new(
        "does-not-exist",
        "node",
        format!("https://nodejs.org/dist/v{NODE_VERSION}/this-file-does-not-exist.zip"),
        "sha256",
        &expected,
    );
    let missing_options = FetchOptions {
        mirrors: MirrorConfig {
            mirrors: Vec::new(),
            use_builtin_mirrors: false,
            include_official: true,
        },
        timeout: Duration::from_secs(30),
        cache: Cache::new(cache.root().join("acceptance-404")),
        use_cache: false,
        store_in_cache: false,
        destination: None,
        attempts_per_source: 1,
    };
    match fetch(&transport, &missing, &missing_options) {
        Err(error) => {
            let kind = error.kind();
            println!("  {error}");
            println!("  分类：{}（应当触发换源）", kind.as_str());
            println!("  建议：{}", kind.advice());
            if error.should_try_next_source() {
                println!("  ✓ 这个失败会换源");
            } else {
                println!("  ✗ **这个失败不会换源**，但它是源的问题");
                *exit = 1;
            }
        }
        Ok(_) => {
            println!("  ✗ 一个不存在的文件居然下载成功了");
            *exit = 1;
        }
    }

    section("8. 本地归档文件当输入（离线 bundle 的接口）");
    // 拿刚下载好的那个包当"U 盘里的归档"。
    let local = use_local_file(&report_file.path, &artifact)?;
    println!("  输入：{}", local.path.display());
    println!("  sha256 = {}", local.sha256);
    println!("  来源标记 = {}（不是网络）", local.source_id);
    // 反面对照：同一个文件配一个错的哈希必须被拒。
    let bad_local = Artifact::new("node", "node", NODE_URL, "sha256", "f".repeat(64));
    match use_local_file(&report_file.path, &bad_local) {
        Err(error) => println!("  哈希不符的本地文件被拒：{error}"),
        Ok(_) => {
            println!("  ✗ **哈希不符的本地文件被放过了**");
            *exit = 1;
        }
    }

    section("9. 不支持的算法在动手之前就被拒");
    let md5 = Artifact::new("node", "node", NODE_URL, "md5", &expected);
    match fetch(&transport, &md5, &options) {
        Err(error) => println!("  ✓ {error}"),
        Ok(_) => {
            println!("  ✗ **md5 被接受了**");
            *exit = 1;
        }
    }

    section("结论");
    if *exit == 0 {
        println!("全部检查通过。");
    } else {
        println!("**有检查没过**（退出码 {exit}）—— 上面标 ✗ 的那些就是。");
    }
    Ok(())
}

fn section(title: &str) {
    println!("\n=== {title} ===");
}

fn report(claim: &str, why: &str) {
    println!("  · {claim}");
    println!("    因为：{why}");
}
