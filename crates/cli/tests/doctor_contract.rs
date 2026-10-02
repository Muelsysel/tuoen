//! `tuoen doctor` 的**进程边界契约测试**。
//!
//! ## 为什么必须在这一层
//!
//! 事实与判据在 `tuoen_core` 里用假机器测到逐条（每个正例都有反例），但
//! **只有跑真实二进制才能回答票据真正问的问题**：敲 `tuoen doctor` 之后看到什么、
//! `--json` 的形状对不对、两次跑出来是不是一模一样、退出码是不是 0。
//! 参数解析、退出码、中文人类输出，全都只有在这一层才看得见。
//!
//! ## 为什么这里**一条数量断言都没有**
//!
//! 测试跑在**真实的这台机器**上，而本机恰好是坏的（`PATH` 里有重复、有失效条目、
//! 有硬编码用户名）。"恰好 3 条警告"这种断言会在别人的机器上红，而且它红的时候
//! 什么都没说明 —— 因为被测代码的性质是**形状与不变量**，不是这台机器坏在哪
//! （`AGENTS.md` 规矩五）。所以这里断言的是：ID 一定在契约表里、severity 一定是
//! 三个 slug 之一、计数与明细一定对得上、两次一定逐字节相同。
//!
//! ## 四条硬性约束（本文件里每一条都有对应的用例）
//!
//! 1. **`doctor` 绝不写机器。** 证据是
//!    [`doctor_touches_neither_the_registry_nor_the_real_home`]：跑之前列一遍
//!    `%LOCALAPPDATA%\tuoen`（**含 `shims` 里一层**）、再取一次两个作用域的
//!    `Path` 原文与类型，跑完之后逐项相同。**不是**断言"某个目录不存在" ——
//!    那是机器状态，会被误报（L0 的真机验收踩过）。
//! 2. **`--json` 的成功载荷里不出现中文**，而且**连 `message` 这个键都没有**
//!    （它天生是一句中文）。中文只进人类输出。
//! 3. **不依赖本机状态**：ID 的白名单从 `tuoen_core::doctor::ids::ALL` 里 import，
//!    不在这里抄一份 —— 抄一份的后果是检查项改名之后测试跟着"通过"。
//! 4. **每一处都用 [`IsolatedHome`]**：存储根与 shim 目录都从 `%LOCALAPPDATA%`
//!    推出来，指到临时目录之后，"我们的 shim 目录在哪、里面有什么"就不依赖
//!    开发机上装过什么（`FactsSummary::shims_on_disk` 因此可以断言为 0）。

mod common;

use std::path::PathBuf;
use std::process::Output;

use common::{IsolatedHome, json, stderr, stdout};
use serde_json::Value;
use tuoen_core::doctor::ids;

/// 三个严重度 slug。**改它们要递增信封的 `schemaVersion`。**
const SEVERITIES: [&str; 3] = ["error", "warn", "info"];

// ─────────────────────────────────────────────────────────────────────────────
// 测试基础设施
// ─────────────────────────────────────────────────────────────────────────────

fn run_doctor(home: &IsolatedHome, args: &[&str]) -> Output {
    home.run(args)
}

/// 一次调用的两路输出，用于断言失败时的可读信息。
fn describe(output: &Output) -> String {
    format!("stdout：{} stderr：{}", stdout(output), stderr(output))
}

fn contains_cjk(text: &str) -> bool {
    text.chars()
        .any(|c| (0x4E00..=0x9FFF).contains(&(u32::from(c))))
}

/// 跑一次 `doctor --json` 并取出 `data`。
///
/// 不开 `--no-probe`：默认的那份**才是用户会敲的那条命令行**，而"默认能跑通"
/// 必须有用例守着（前缀探测要启动 `npm` 这类工具，它是默认路径上唯一会 spawn 的一步）。
fn doctor_data(home: &IsolatedHome, extra: &[&str]) -> Value {
    let mut args = vec!["doctor", "--json"];
    args.extend_from_slice(extra);
    let output = run_doctor(home, &args);
    assert_eq!(
        output.status.code(),
        Some(0),
        "{args:?}：{}",
        describe(&output)
    );
    let envelope = json(&output);
    assert_eq!(envelope.command, "doctor");
    assert!(envelope.ok, "体检跑完了就是成功信封：{:?}", envelope.error);
    assert!(
        envelope.error.is_none(),
        "成功信封里不该有 error 键：{:?}",
        envelope.error
    );
    envelope.data.expect("成功必须有 data")
}

/// `data.findings`，并断言它真的是一个数组。
fn findings(data: &Value) -> &[Value] {
    data["findings"]
        .as_array()
        .map(Vec::as_slice)
        .expect("findings 是数组")
}

/// 真实的 `%LOCALAPPDATA%\tuoen` 下现在有哪些名字。
///
/// **`shims` 要往里看一层。** 只列顶层的话，最严重的那一类事故看不见：
/// "在真实的 shim 目录里生成了一个 shim" —— 因为 `shims` 这个名字本来就在那儿，
/// 空目录和 4 条 shim 的顶层列表长得一模一样（`path_contract.rs` 的同一份教训）。
fn real_home_listing() -> Vec<String> {
    let Some(local) = std::env::var_os("LOCALAPPDATA") else {
        return Vec::new();
    };
    let home = PathBuf::from(local).join("tuoen");
    let Ok(entries) = std::fs::read_dir(&home) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    let mut inner: Vec<String> = std::fs::read_dir(home.join("shims"))
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    inner.sort();
    names.push(format!("shims/{{{}}}", inner.join(", ")));
    names
}

// ─────────────────────────────────────────────────────────────────────────────
// 1. 形状：ID 与严重度都在契约里
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn doctor_json_exits_zero_and_every_finding_is_a_known_id_with_a_known_severity() {
    let home = IsolatedHome::new("doctor-json");
    let data = doctor_data(&home, &[]);

    // 顶层键一个都不能少 —— 一台"什么都没发现"的机器也要给出完整形状。
    for key in ["counts", "summary", "findings"] {
        assert!(data.get(key).is_some(), "缺少顶层键 `{key}`：{data}");
    }
    for key in ["error", "warn", "info"] {
        assert!(
            data["counts"].get(key).is_some(),
            "`counts` 缺少 `{key}`：{data}"
        );
    }
    for key in [
        "pathEntries",
        "envVars",
        "toolRows",
        "wslDistributions",
        "shimsOnDisk",
        "resolvedCommands",
    ] {
        assert!(
            data["summary"].get(key).is_some(),
            "`summary` 缺少 `{key}`：{data}"
        );
    }

    // **ID 的白名单从 `tuoen_core::doctor::ids::ALL` 里来，不在这里抄一份。**
    // 抄一份的后果是检查项改名之后测试跟着"通过"，而契约早就断了。
    for finding in findings(&data) {
        let id = finding["id"].as_str().expect("id 是字符串");
        assert!(
            ids::ALL.contains(&id),
            "`{id}` 不在契约表里（23 个 ID 见 `tuoen_core::doctor::ids`）：{finding}"
        );
        let severity = finding["severity"].as_str().expect("severity 是字符串");
        assert!(
            SEVERITIES.contains(&severity),
            "`{severity}` 不是三个稳定 slug 之一：{finding}"
        );
        assert!(
            finding["evidence"].is_array(),
            "每条发现都要带 evidence（具体是哪几条）：{finding}"
        );
        assert!(
            finding["source"].is_string(),
            "每条发现都要说清结论从哪来：{finding}"
        );
        // `confidence` 只有真的有值时才出现（`skip_serializing_if`）——
        // 一条恒为 `null` 的键会让消费者以为"问了但没答出来"。
        if let Some(confidence) = finding.get("confidence") {
            assert!(
                !confidence.is_null(),
                "`confidence` 要么有值、要么连键都不出现：{finding}"
            );
        }
    }

    // 隔离之后我们自己的 shim 目录**不存在**，所以"盘上有几个 shim"必然是 0。
    // 这一条同时证明了 `LOCALAPPDATA` 真的被指到了临时目录（见下面第 8 条）。
    assert_eq!(
        data["summary"]["shimsOnDisk"].as_u64(),
        Some(0),
        "隔离的家目录里不该有我们的 shim：{data}"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// 2. 成功载荷里没有中文，也没有 `message` 这个键
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn the_success_payload_has_no_cjk_and_no_message_key() {
    let home = IsolatedHome::new("doctor-json-no-cjk");
    let output = run_doctor(&home, &["doctor", "--json", "--no-probe"]);
    assert_eq!(output.status.code(), Some(0), "{}", describe(&output));

    // **原文扫一遍**：`serde_json` 会把非 ASCII 转义成 `\uXXXX`，所以中文若漏进来，
    // 这里看得到的是转义之外的任何形态；而"整个 stdout 没有 CJK"这条断言同时保证了
    // 人类输出（第一行那句中文标题）绝不会混进 `--json` 的输出里。
    let text = stdout(&output);
    assert!(
        !contains_cjk(&text),
        "`--json` 的成功载荷里不该有 CJK：{text}"
    );

    // 更强也更**不依赖机器**的一条：`message` 这个键根本不该存在。
    // 中文的 `message` 天生属于人类输出，机器读的是 `id` 那个 slug。
    let data = doctor_data(&home, &["--no-probe"]);
    for finding in findings(&data) {
        assert!(
            finding.get("message").is_none(),
            "`message` 是中文句子，不许出现在成功载荷里：{finding}"
        );
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// 3. 计数与明细逐条数出来的一致
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn the_counts_agree_with_the_findings_one_by_one() {
    let home = IsolatedHome::new("doctor-counts");
    let data = doctor_data(&home, &["--no-probe"]);

    let mut counted = [0_u64; 3];
    for finding in findings(&data) {
        let severity = finding["severity"].as_str().expect("severity");
        let slot = SEVERITIES
            .iter()
            .position(|known| *known == severity)
            .unwrap_or_else(|| panic!("未知 severity：{severity}"));
        counted[slot] += 1;
    }
    for (index, key) in SEVERITIES.iter().enumerate() {
        assert_eq!(
            data["counts"][key].as_u64(),
            Some(counted[index]),
            "`counts.{key}` 与逐条数出来的数不一致 —— 报告里的两个数字说了两个真相：{data}"
        );
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// 4. 幂等：两次 `--json` 逐字节相同
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn two_json_runs_are_byte_identical() {
    // 报告里**没有时间戳**（`doctor` 不写文件，也不说"什么时候看的"），所以
    // 机器状态没变时两次输出必须逐字节相同。任何 `HashMap` 迭代、任何"按发现顺序
    // 而不是按（严重度，ID）排序"都会让这一条红 —— 而顺序不稳定本身就是 bug：
    // diff 失去意义，脚本也没法比两次体检的差别。
    //
    // 这一条**不开** `--no-probe`：默认路径才是用户会敲的那条，而探测的输出
    // （`npm config get prefix` 的答案）也必须稳定。
    let home = IsolatedHome::new("doctor-stable");
    let first = stdout(&run_doctor(&home, &["doctor", "--json"]));
    let second = stdout(&run_doctor(&home, &["doctor", "--json"]));
    assert_eq!(
        first, second,
        "两次 `doctor --json` 必须逐字节相同（顺序是（严重度，ID）确定的）"
    );
    assert!(!first.is_empty(), "不许什么都不输出");
}

// ─────────────────────────────────────────────────────────────────────────────
// 5. 人类输出：中文、逐条点名、末尾两行
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn human_output_is_chinese_names_every_id_and_ends_with_summary_and_scale() {
    let home = IsolatedHome::new("doctor-human");
    // 两次调用用**同一套开关**，否则"人类输出里有没有这个 ID"比的是两份不同的体检。
    let data = doctor_data(&home, &["--no-probe"]);
    let output = run_doctor(&home, &["doctor", "--no-probe"]);
    assert_eq!(output.status.code(), Some(0), "{}", describe(&output));
    let text = stdout(&output);

    assert!(contains_cjk(&text), "人类输出要是中文：{text}");
    // 只读这件事必须自己说出来 —— 这一族最容易被误解成"顺手修了点什么"。
    assert!(text.contains("只读"), "{text}");
    assert!(text.contains("没有写"), "{text}");
    // 关掉探测也要说清楚关掉了什么，否则"没有那一类发现"会被读成"没有那一类问题"。
    assert!(text.contains("--no-probe"), "{text}");

    // **每一条在 `--json` 里报出来的发现，都必须在人类输出里被点名。**
    // 反过来不成立（人类输出比 JSON 多一点语气），所以断言只朝一个方向做。
    let reported: Vec<String> = findings(&data)
        .iter()
        .map(|finding| finding["id"].as_str().expect("id").to_owned())
        .collect();
    if reported.is_empty() {
        // 真的一台健康机器：这条断言没有对象。**如实跳过，不假装测过。**
        eprintln!("（这台机器上 doctor 一条发现都没报 —— 跳过「逐条点名」的断言）");
        assert!(
            text.contains("没有发现问题"),
            "一条发现都没有时必须明说：{text}"
        );
    } else {
        for id in &reported {
            assert!(
                text.contains(id.as_str()),
                "发现 `{id}` 没在人类输出里：{text}"
            );
        }
    }

    // 末尾两行：汇总 + 规模。**顺序是契约**：先"有几个要动手的"，再"这一次看了多少"。
    let lines: Vec<&str> = text.lines().collect();
    let scale = lines.last().copied().unwrap_or_default();
    assert!(
        scale.starts_with("规模："),
        "最后一行必须是规模摘要：{text}"
    );
    for key in ["PATH 条目", "环境变量", "工具", "WSL 发行版", "shim"] {
        assert!(scale.contains(key), "规模摘要里缺少 `{key}`：{scale}");
    }
    let counts = lines
        .get(lines.len().saturating_sub(2))
        .copied()
        .unwrap_or_default();
    for key in ["错误", "警告", "提示"] {
        assert!(counts.contains(key), "汇总行里缺少 `{key}`：{counts}");
    }
    // 汇总之上的那一行不是它 —— 否则倒数第二行会被 `--no-probe` 那句注解顶掉。
    assert!(
        counts.contains(" · "),
        "汇总行的形状是「错误 N · 警告 N · 提示 N」：{counts}"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// 6. 命令面：没有 `--fix`，有 `--no-probe`
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn help_has_no_fix_flag_and_has_no_probe() {
    let home = IsolatedHome::new("doctor-help");
    let help = run_doctor(&home, &["doctor", "--help"]);
    assert_eq!(help.status.code(), Some(0), "{}", describe(&help));
    let text = stdout(&help);
    assert!(contains_cjk(&text), "长帮助要是中文：{text}");
    assert!(text.contains("--json"), "{text}");
    assert!(text.contains("--no-probe"), "{text}");
    // **这一票的判据之一**：绝不提供 `--fix`。它出现在帮助里就意味着有人开始
    // 让体检顺手改机器 —— 那会让"报告是症状"变成"报告是它自己动过的痕迹"。
    assert!(
        !text.contains("--fix"),
        "`doctor` 绝不修改任何东西，帮助里不许出现 `--fix`：{text}"
    );
    // 退出码的取舍必须在帮助里说清（"发现了 error 也是 0"最容易被误读）。
    assert!(text.contains("退出码"), "{text}");
    assert!(
        text.contains("0 = 体检跑完了") || text.contains("体检跑完了"),
        "{text}"
    );

    // 顶层帮助里也要有 `doctor`。
    let top = stdout(&run_doctor(&home, &["--help"]));
    assert!(top.contains("doctor"), "{top}");

    // 用法错误（退出码 2）：`--fix` / `--strict` / `--fail-on` 都得被 clap 当场拒掉，
    // 而不是被静默忽略 —— 静默忽略会让"我以为它修好了"变成一句假话。
    for flag in ["--fix", "--strict", "--fail-on"] {
        let output = run_doctor(&home, &["doctor", flag]);
        assert_eq!(
            output.status.code(),
            Some(2),
            "`{flag}` 必须是用法错误：{}",
            describe(&output)
        );
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// 7. 只读：跑前跑后，真实的注册表与家目录逐项相同
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn doctor_touches_neither_the_registry_nor_the_real_home() {
    // **本文件最重要的一条断言。**
    //
    // `path show --json` 的载荷里带着两个作用域 `Path` 的**注册表原文**与**类型**，
    // 所以"前后逐字节相同"**就是**"注册表没被动过"的证据 —— 而"我没碰它"
    // 只是一句话（`AGENTS.md`：声称没碰 ≠ 证明没碰）。
    // `doctor` 比别的命令更需要这条：它是唯一一条会**顺手启动第三方工具**问一句话的
    // 只读命令，而那种动作最容易被误写成"顺便改一下就好"。
    let home = IsolatedHome::new("doctor-read-only");
    let before_home = real_home_listing();
    let before_path = stdout(&run_doctor(&home, &["path", "show", "--json"]));

    // 证据本身也要能被复核：那份载荷里真的带着原文与类型。
    let scope_data: Value =
        serde_json::from_str(before_path.trim()).expect("path show --json 必须是合法 JSON");
    let scopes = scope_data["data"]["scopes"]
        .as_array()
        .expect("scopes 是数组");
    assert_eq!(scopes.len(), 2, "两个作用域都要在：{scope_data}");
    for scope in scopes {
        assert!(scope.get("raw").is_some(), "每个作用域都要有原文：{scope}");
        assert!(
            scope.get("regType").is_some(),
            "每个作用域都要有类型：{scope}"
        );
    }

    for args in [
        vec!["doctor"],
        vec!["doctor", "--json"],
        vec!["doctor", "--no-probe"],
        vec!["doctor", "--json", "--no-probe"],
    ] {
        let output = run_doctor(&home, &args);
        assert_eq!(
            output.status.code(),
            Some(0),
            "{args:?}：{}",
            describe(&output)
        );
    }

    let after_path = stdout(&run_doctor(&home, &["path", "show", "--json"]));
    assert_eq!(
        before_path, after_path,
        "`doctor` 动了真实的 `HKCU\\Environment` —— 这是本仓库最容易造成真实伤害的地方"
    );
    let after_home = real_home_listing();
    assert_eq!(
        before_home, after_home,
        "`doctor` 动了真实的 `%LOCALAPPDATA%\\tuoen`（含 shims 里一层）"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// 8. 隔离：`doctor` 读的是我们指给它的那个家目录
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn the_isolated_home_is_the_one_doctor_reads() {
    // 如果 `LOCALAPPDATA` 没被指到临时目录，上面那条"前后相同"就会变成一句
    // "恰好成立"的话。这里正面钉住隔离有效：隔离的家目录里**没有**我们的 shim，
    // 所以规模摘要里的 `shimsOnDisk` 必须是 0（第 1 条断言了同一件事的另一面）。
    let home = IsolatedHome::new("doctor-isolation");
    let data = doctor_data(&home, &["--no-probe"]);
    assert_eq!(data["summary"]["shimsOnDisk"].as_u64(), Some(0), "{data}");
    // 分子是 0、分母也该是 0：我们的 shim 一条都没有，所以"被遮蔽"这件事无从谈起。
    assert!(
        !home.local_app_data().join("tuoen").join("shims").exists(),
        "`doctor` **什么都不改** —— 连 shim 目录都不该被创建"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// 9. 失败路径：跑不起来时是一个带事实的部分信封
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn an_unwired_store_root_is_a_partial_envelope_with_a_stable_code() {
    // **装配失败**：清掉 `%LOCALAPPDATA%` 与 `%USERPROFILE%`，存储根就退成了相对路径
    // `.tuoen\store`（store 层的既定退路）。而相对的根本分不出"这条 `PATH` 条目是不是
    // 我们自己的"，于是两类发现会静默地变成"没有" —— 报失败比报一份看起来正常的
    // 错报告诚实。这一条不依赖开发机的状态：两个变量是**这次调用自己去掉的**。
    let output = common::tuoen()
        .args(["doctor", "--json"])
        .env_remove("LOCALAPPDATA")
        .env_remove("USERPROFILE")
        .output()
        .expect("运行 tuoen 应当成功");
    assert_eq!(
        output.status.code(),
        Some(1),
        "跑不起来是运行期错误：{}",
        describe(&output)
    );

    let envelope = json(&output);
    assert_eq!(envelope.command, "doctor");
    assert!(!envelope.ok, "有东西没做成就不该报成功");
    let error = envelope.error.expect("失败必须有 error");
    assert_eq!(error.code, "unwired-roots", "错误码是稳定契约：{error:?}");
    assert!(
        error
            .code
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'),
        "错误码必须是小写 kebab ASCII：{}",
        error.code
    );
    assert!(
        error.message.contains("LOCALAPPDATA"),
        "要说清差的是哪个变量：{}",
        error.message
    );

    // 失败也要带出**已经确定的事实**：算出来的那两个根。
    let data = envelope
        .data
        .expect("失败也要有 data：差的环境变量就在里面");
    assert!(!data["storeRoot"].as_str().expect("storeRoot").is_empty());
    assert!(!data["shimDir"].as_str().expect("shimDir").is_empty());
    // **没有跑过的体检不许报计数** —— 报三个 0 就是在说"我看过了，什么都没发现"。
    for key in ["counts", "summary", "findings"] {
        assert!(
            data.get(key).is_none(),
            "体检没跑，`{key}` 必须缺席（缺席 = 不知道）：{data}"
        );
    }

    // 人类输出那条路：错误走 stderr，退出码同样是 1。
    let human = common::tuoen()
        .args(["doctor"])
        .env_remove("LOCALAPPDATA")
        .env_remove("USERPROFILE")
        .output()
        .expect("运行 tuoen 应当成功");
    assert_eq!(human.status.code(), Some(1), "{}", describe(&human));
    assert!(
        contains_cjk(&stderr(&human)),
        "失败也要用中文说清为什么：{}",
        stderr(&human)
    );
}
