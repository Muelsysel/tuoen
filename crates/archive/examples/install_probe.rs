//! 票据 #5 的真机验收：在作者本机上真的装一个工具链，并让它跑起来。
//!
//! 跑法：`cargo run -p tuoen-archive --example install_probe`
//!
//! ## 为什么这一票的验收必须"真的装一个东西"
//!
//! 单元测试与集成测试证明的是**判据**与**编排**。它们证明不了的是：
//! 系统 `tar.exe` 在我们**真实会遇到的制品**上到底行不行 ——
//! 而这一票的验收标准里有"`.7z` 缺口有明确记录，且不影响其他格式"，
//! 那就必须拿真的 7z 走一遍**我们自己的代码路径**（而不是手工敲 `tar -xf`）。
//!
//! 所以这个例子做四件事：
//!
//! 1. **真的下载**（走 `tuoen-download`，票据 #4 的成果）node 的 `.zip` 与 `.7z`；
//! 2. **真的解压安装**（走 `install_archive`，票据 #5 的成果）；
//! 3. **真的执行**解出来的 `node.exe --version` —— 这是唯一能证明
//!    "解压出来的东西是完整可用的"的判据（大小对不代表能用）；
//! 4. **真的拒绝**每一个恶意归档变体，并**验证磁盘上没有留下任何东西**。
//!
//! ## 它碰机器的什么
//!
//! 只碰两个地方，都是**用户自己的缓存与临时目录**：
//! `%APPDATA%\tuoen\cache`（票据 #4 定的）与 `%TEMP%\tuoen-install-probe`。
//! **不写注册表、不写 PATH、不碰任何已安装的工具。**

use std::path::{Path, PathBuf};

use tuoen_archive::{ExtractLimits, InstallRequest, TarCli, install_archive};
use tuoen_download::{Artifact, Cache, FetchOptions, HttpTransport, Transport, fetch};
use tuoen_platform::{ProcessRunner, RealFileSystem, SystemProcessRunner};

const NODE_VERSION: &str = "24.19.0";
const NODE_UPSTREAM: &str = "https://nodejs.org/dist";

fn main() {
    let mut failures = 0;
    println!("=== tuoen 归档安装层 · 真机验收（票据 #5）===");
    println!(
        "机器：{}",
        std::env::var("COMPUTERNAME").unwrap_or_else(|_| "?".into())
    );
    println!();

    let runner = SystemProcessRunner;
    let fs = RealFileSystem;

    // ---- 0. 解压器 ----
    println!("=== 0. 解压器 ===");
    let tar = match TarCli::probe() {
        Ok(tar) => {
            println!("  程序：{}", tar.program().display());
            let version = runner.run(
                tar.program(),
                &["--version"],
                std::time::Duration::from_secs(30),
            );
            println!("  版本：{}", version.stdout.trim());
            tar
        }
        Err(error) => {
            println!("  ✗ {error}");
            std::process::exit(1);
        }
    };
    println!();

    let workspace = probe_workspace();
    println!("工作目录：{}", workspace.display());
    println!();

    // ---- 1. 取上游的哈希（期望值不当常量写死） ----
    println!("=== 1. 取上游公布的哈希 ===");
    let transport = HttpTransport::probe();
    let shasums_url = format!("{NODE_UPSTREAM}/v{NODE_VERSION}/SHASUMS256.txt");
    // **这一份不走 `fetch`**：`SHASUMS256.txt` 自己的哈希没有地方公布
    // （它的可信度由 TLS 提供）。走传输层是诚实的做法 ——
    // 假装我们知道一个不知道的哈希才是错的。
    let shasums = transport
        .fetch(&shasums_url, None, std::time::Duration::from_secs(60))
        .map_err(|error| error.to_string());
    let shasums = match shasums {
        Ok(outcome) => {
            println!("  取回 {} 字节：{shasums_url}", outcome.bytes.len());
            String::from_utf8_lossy(&outcome.bytes).into_owned()
        }
        Err(error) => {
            println!("  ✗ 取不到上游哈希：{error}");
            println!("  （这一步需要网络。离线时后面的步骤都会失败。）");
            std::process::exit(1);
        }
    };

    // ---- 2. 真的装一个 .zip ----
    let zip_name = format!("node-v{NODE_VERSION}-win-x64.zip");
    println!("=== 2. 真的装一个 .zip：{zip_name} ===");
    let zip_installed = match install_one(
        &runner, &fs, &tar, &transport, &workspace, &shasums, &zip_name, "zip",
    ) {
        Ok(path) => {
            failures += report_install(&runner, &path, &zip_name);
            Some(path)
        }
        Err(error) => {
            println!("  ✗ {error}");
            failures += 1;
            None
        }
    };
    println!();

    // ---- 3. 真的装一个 .7z（"缺口"那一说的正面证据） ----
    let sevenz_name = format!("node-v{NODE_VERSION}-win-x64.7z");
    println!("=== 3. 真的装一个 .7z：{sevenz_name} ===");
    println!("  这一条是**票据里那个假设的正面检验**：原设计写的是");
    println!("  \"`.7z` 是唯一需要内置库的缺口\"。");
    let sevenz_installed = match install_one(
        &runner,
        &fs,
        &tar,
        &transport,
        &workspace,
        &shasums,
        &sevenz_name,
        "7z",
    ) {
        Ok(path) => {
            failures += report_install(&runner, &path, &sevenz_name);
            Some(path)
        }
        Err(error) => {
            println!("  ✗ {error}");
            failures += 1;
            None
        }
    };
    println!();

    // ---- 4. 两个格式解出来的东西必须一样 ----
    println!("=== 4. 两个格式解出来的东西必须一致 ===");
    match (&zip_installed, &sevenz_installed) {
        (Some(zip), Some(sevenz)) => {
            let zip_files = count_files(zip);
            let sevenz_files = count_files(sevenz);
            println!("  .zip  解开 {zip_files} 个文件");
            println!("  .7z   解开 {sevenz_files} 个文件");
            if zip_files == sevenz_files && zip_files > 1000 {
                println!("  ✓ 两个格式给出同样的文件数（{zip_files}）");
            } else {
                println!("  ✗ 文件数不一致（{zip_files} vs {sevenz_files}）—— 有一个格式没解全");
                failures += 1;
            }
        }
        _ => println!("  （前面有格式没装上，跳过对比）"),
    }
    println!();

    // ---- 5. 恶意归档：每一种变体都必须被拒，且不留痕 ----
    println!("=== 5. 恶意归档：每一种变体都必须被拒，且磁盘上不留任何东西 ===");
    failures += probe_evil_archives(&runner, &fs, &tar, &workspace);
    println!();

    // ---- 6. 长路径与"含空格 + 版本号"的路径 ----
    println!("=== 6. 长路径与难路径 ===");
    failures += probe_hard_paths(&runner, &fs, &tar, &workspace);
    println!();

    // ---- 7. 原子性：失败之后什么都没有 ----
    println!("=== 7. 原子性：审计不通过时最终名字从未出现过 ===");
    failures += probe_atomicity(&runner, &fs, &tar, &workspace);
    println!();

    println!("=== 结论 ===");
    if failures == 0 {
        println!("  全部通过。");
    } else {
        println!("  {failures} 项失败。");
        std::process::exit(1);
    }
}

/// 验收用的工作目录。**每次重跑都清空**，免得上一轮的残留掩盖问题。
fn probe_workspace() -> PathBuf {
    let root = std::env::temp_dir().join("tuoen-install-probe");
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("建验收目录");
    root
}

/// 下载 + 校验 + 安装一个制品，返回装到哪了。
#[allow(clippy::too_many_arguments)]
fn install_one(
    runner: &SystemProcessRunner,
    fs: &RealFileSystem,
    tar: &TarCli,
    transport: &HttpTransport,
    workspace: &Path,
    shasums: &str,
    file_name: &str,
    label: &str,
) -> Result<PathBuf, String> {
    // 期望哈希**从上游刚取回的 SHASUMS256.txt 里读**，不是代码里的常量。
    let expected = shasums
        .lines()
        .find_map(|line| {
            let mut parts = line.split_whitespace();
            let hash = parts.next()?;
            let name = parts.next()?;
            (name == file_name).then(|| hash.to_owned())
        })
        .ok_or_else(|| format!("上游的 SHASUMS256.txt 里没有 {file_name}"))?;
    println!("  上游哈希：{expected}");

    let url = format!("{NODE_UPSTREAM}/v{NODE_VERSION}/{file_name}");
    let artifact = Artifact::new(file_name, "node", &url, "sha256", expected.clone());
    let options = FetchOptions {
        cache: Cache::at_default_location(),
        ..FetchOptions::default()
    };

    let started = std::time::Instant::now();
    let outcome =
        fetch(transport, &artifact, &options).map_err(|error| format!("下载失败：{error}"))?;
    println!(
        "  取到：{}（{} 字节，来自 {}，缓存命中：{}，{:.1}s）",
        outcome.path.display(),
        outcome.bytes,
        outcome.source_id,
        outcome.from_cache,
        started.elapsed().as_secs_f64()
    );

    // 归档要装到"临时目录"里 —— 验收**不碰**用户的真实安装位置。
    let versions = workspace.join("versions");
    let version = format!("{NODE_VERSION}-{label}");
    let request = InstallRequest::new(&outcome.path, &versions, &version).stripping(1);

    let started = std::time::Instant::now();
    let installed =
        install_archive(runner, fs, tar, &request).map_err(|error| format!("安装失败：{error}"))?;
    println!(
        "  装上：{}（{} 个条目，{:.1} MB，审计 {:.1}s）",
        installed.installed_to.display(),
        installed.audit.entries,
        installed.audit.total_bytes as f64 / 1_048_576.0,
        started.elapsed().as_secs_f64()
    );
    println!(
        "  临时目录 {} 已清理：{}",
        installed.staging.display(),
        !installed.staging.exists()
    );

    Ok(installed.installed_to)
}

/// 报告一次安装的结果，**并真的把解出来的程序跑起来**。
///
/// 这一步是整个验收里最重要的一条：**大小对不代表能用**。
/// 一个漏了文件的解压会给出正确的哈希与合理的大小，而程序跑不起来。
fn report_install(runner: &SystemProcessRunner, installed: &Path, archive: &str) -> usize {
    let node = installed.join("node.exe");
    if !node.is_file() {
        println!("  ✗ 没有解出 node.exe");
        return 1;
    }
    let size = std::fs::metadata(&node).map(|m| m.len()).unwrap_or(0);
    println!("  node.exe：{size} 字节");

    // **真的执行它。**
    let outcome = runner.run(&node, &["--version"], std::time::Duration::from_secs(60));
    let version = outcome.combined().trim().to_owned();
    if version.contains(NODE_VERSION) {
        println!("  ✓ `node.exe --version` → {version}（来自 {archive}）");
        0
    } else {
        println!("  ✗ `node.exe --version` 给出了 {version:?}，期望含 {NODE_VERSION}");
        println!("     stderr：{}", outcome.stderr.trim());
        1
    }
}

/// 数一个目录树里的文件数（不跟随 reparse point）。
fn count_files(root: &Path) -> usize {
    let mut count = 0;
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_dir() {
                stack.push(entry.path());
            } else {
                count += 1;
            }
        }
    }
    count
}

/// 每一个恶意变体：造归档 → 试着装 → 断言被拒 → 断言磁盘干净。
fn probe_evil_archives(
    runner: &SystemProcessRunner,
    fs: &RealFileSystem,
    tar: &TarCli,
    workspace: &Path,
) -> usize {
    // 这些归档用**测试里那套构造器**造不出来（那是 `cfg(test)` 的），
    // 所以这里用系统 `tar.exe` 自己打包 —— 它对**造**归档没有安全限制，
    // 只有**解**的时候才拦（而"解的时候会拦"正是我们要看的东西）。
    let source = workspace.join("evil-source");
    std::fs::create_dir_all(&source).expect("建源目录");

    let cases: &[(&str, &str, &str)] = &[
        ("parent-traversal", "..", "条目名里有 `..`"),
        (
            "absolute-path",
            "/escaped-abs.txt",
            "条目名是绝对路径（`-tf` **保留**前导 `/`，剥掉发生在解压时）",
        ),
        ("unc-path", "//server/share/unc.txt", "UNC 网络路径"),
        ("reserved-device-name", "NUL.txt", "保留设备名"),
        ("trailing-dot", "trailing.", "结尾的点"),
        (
            "alternate-data-stream",
            "stream.txt:evil",
            "`:`（ADS 写原语）",
        ),
    ];

    let mut failures = 0;
    for (label, name, why) in cases {
        // `tar -cf` 允许我们把一个**恶意名字**写进归档（它只管打包）。
        // 用一个临时文件冒充那个名字的内容。
        let payload = source.join("payload.bin");
        std::fs::write(&payload, b"pwned").expect("写 payload");
        let archive = workspace.join(format!("evil-{label}.zip"));
        let made = make_evil_zip(tar, runner, &payload, name, &archive);

        if !made {
            println!("  （跳过 {label}：系统 tar 不肯把 `{name}` 打进归档 —— 这本身是个结论）");
            continue;
        }

        let versions = workspace.join("evil-versions");
        let request = InstallRequest::new(&archive, &versions, (*label).to_owned());
        match install_archive(runner, fs, tar, &request) {
            Ok(_) => {
                println!("  ✗ `{name}` 竟然装上了 —— 这是**逃逸**，必须修");
                failures += 1;
            }
            Err(error) => {
                let kind = error.kind();
                let violation = error.name_violation().map(|v| v.as_str()).unwrap_or("-");
                let clean = !versions.join(label).exists() && no_staging(&versions);
                println!(
                    "  ✓ `{name}` 被拒：{kind} / {violation}（{why}）{}",
                    if clean {
                        "，磁盘干净"
                    } else {
                        "，**但有残留**"
                    }
                );
                if !clean {
                    failures += 1;
                }
            }
        }
    }
    failures
}

/// 用一个恶意名字造一个 zip。
///
/// 用 PowerShell 的 `Compress-Archive`？不行 —— 它会拒绝这些名字。
/// 所以直接写 ZIP 的字节（与测试里那套构造器同一个思路）。
fn make_evil_zip(
    _tar: &TarCli,
    _runner: &SystemProcessRunner,
    payload: &Path,
    evil_name: &str,
    archive: &Path,
) -> bool {
    let data = std::fs::read(payload).unwrap_or_default();
    let name = evil_name.as_bytes();
    let mut out: Vec<u8> = Vec::new();
    let mut central: Vec<u8> = Vec::new();
    let crc = crc32(&data);
    let size = data.len() as u32;

    out.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
    out.extend_from_slice(&20u16.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&0x21u16.to_le_bytes());
    out.extend_from_slice(&crc.to_le_bytes());
    out.extend_from_slice(&size.to_le_bytes());
    out.extend_from_slice(&size.to_le_bytes());
    out.extend_from_slice(&(name.len() as u16).to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(name);
    out.extend_from_slice(&data);

    central.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
    central.extend_from_slice(&0x031eu16.to_le_bytes());
    central.extend_from_slice(&20u16.to_le_bytes());
    central.extend_from_slice(&0u16.to_le_bytes());
    central.extend_from_slice(&0u16.to_le_bytes());
    central.extend_from_slice(&0u16.to_le_bytes());
    central.extend_from_slice(&0x21u16.to_le_bytes());
    central.extend_from_slice(&crc.to_le_bytes());
    central.extend_from_slice(&size.to_le_bytes());
    central.extend_from_slice(&size.to_le_bytes());
    central.extend_from_slice(&(name.len() as u16).to_le_bytes());
    central.extend_from_slice(&0u16.to_le_bytes());
    central.extend_from_slice(&0u16.to_le_bytes());
    central.extend_from_slice(&0u16.to_le_bytes());
    central.extend_from_slice(&0u16.to_le_bytes());
    central.extend_from_slice(&0o100_644u32.wrapping_shl(16).to_le_bytes());
    central.extend_from_slice(&0u32.to_le_bytes());
    central.extend_from_slice(name);

    let offset = out.len() as u32;
    let central_size = central.len() as u32;
    out.extend_from_slice(&central);
    out.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&central_size.to_le_bytes());
    out.extend_from_slice(&offset.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());

    std::fs::write(archive, out).is_ok()
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xffff_ffffu32;
    for byte in data {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xedb8_8320 & mask);
        }
    }
    !crc
}

fn no_staging(versions: &Path) -> bool {
    if !versions.is_dir() {
        return true;
    }
    std::fs::read_dir(versions)
        .map(|entries| {
            !entries
                .flatten()
                .any(|e| e.file_name().to_string_lossy().starts_with(".staging-"))
        })
        .unwrap_or(true)
}

/// 长路径、含空格与版本号的路径。
fn probe_hard_paths(
    runner: &SystemProcessRunner,
    fs: &RealFileSystem,
    tar: &TarCli,
    workspace: &Path,
) -> usize {
    let mut failures = 0;

    // ① 含空格 + 版本号（本机 PATH 上真实存在这种形状）。
    let spaces = workspace.join("hard-spaces.zip");
    write_zip(
        &spaces,
        &[(
            "IntelliJ IDEA 2026.1.1/bin/idea64.exe",
            b"binary".as_slice(),
        )],
    );
    let versions = workspace.join("hard-versions");
    let request = InstallRequest::new(&spaces, &versions, "spaces");
    match install_archive(runner, fs, tar, &request) {
        Ok(outcome) => {
            let target = outcome
                .installed_to
                .join("IntelliJ IDEA 2026.1.1/bin/idea64.exe");
            if target.is_file() {
                println!("  ✓ 含空格与版本号的路径装上了：{}", target.display());
            } else {
                println!("  ✗ 装上了但文件不在预期位置");
                failures += 1;
            }
        }
        Err(error) => {
            println!("  ✗ 含空格的路径被拒了：{error}");
            failures += 1;
        }
    }

    // ② 长路径：总长超过 MAX_PATH(260)。
    let mut nested = String::new();
    for i in 0..8 {
        if !nested.is_empty() {
            nested.push('/');
        }
        nested.push_str(&format!("{}-{i}", "d".repeat(36)));
    }
    let deep_name = format!("{nested}/payload.bin");
    let deep = workspace.join("hard-deep.zip");
    write_zip(&deep, &[(&deep_name, b"deep".as_slice())]);
    let request = InstallRequest::new(&deep, &versions, "deep");
    match install_archive(runner, fs, tar, &request) {
        Ok(outcome) => {
            let target = outcome
                .installed_to
                .join(deep_name.replace('/', std::path::MAIN_SEPARATOR_STR));
            let length = target.to_string_lossy().len();
            if target.is_file() && length > 260 {
                println!(
                    "  ✓ 长路径装上了（{length} 字符 > 260）：能读回 {} 字节",
                    std::fs::metadata(&target).map(|m| m.len()).unwrap_or(0)
                );
            } else {
                println!("  ✗ 长路径没装成（长度 {length}）");
                failures += 1;
            }
        }
        Err(error) => {
            println!("  ✗ 长路径被拒了：{error}");
            failures += 1;
        }
    }
    failures
}

/// 原子性：失败发生在解压**之后**时，最终名字必须从未出现过。
fn probe_atomicity(
    runner: &SystemProcessRunner,
    fs: &RealFileSystem,
    tar: &TarCli,
    workspace: &Path,
) -> usize {
    let archive = workspace.join("atomic.zip");
    static FILLER: [u8; 3000] = [0u8; 3000];
    write_zip(&archive, &[("a.bin", &FILLER), ("b.bin", &FILLER)]);
    let versions = workspace.join("atomic-versions");
    let request = InstallRequest {
        limits: ExtractLimits {
            max_total_bytes: 1000,
            ..ExtractLimits::default()
        },
        ..InstallRequest::new(&archive, &versions, "atomic".to_owned())
    };

    match install_archive(runner, fs, tar, &request) {
        Ok(_) => {
            println!("  ✗ 超限的归档竟然装上了");
            1
        }
        Err(error) => {
            let target_exists = versions.join("atomic").exists();
            let clean = no_staging(&versions);
            println!(
                "  ✓ 被拒：{} / {}",
                error.kind(),
                error.name_violation().map(|v| v.as_str()).unwrap_or("-")
            );
            println!(
                "    最终目录存在？{target_exists}　临时目录残留？{}",
                !clean
            );
            if target_exists || !clean {
                println!("  ✗ 失败留下了痕迹");
                1
            } else {
                println!("  ✓ 磁盘上什么都没有 —— \"要么完整成功，要么完全不留痕\"成立");
                0
            }
        }
    }
}

/// 最小的 ZIP 写手（stored）。验收例子不能依赖测试代码，所以这里自带一份。
fn write_zip(path: &Path, entries: &[(&str, &[u8])]) {
    let mut out: Vec<u8> = Vec::new();
    let mut central: Vec<u8> = Vec::new();
    for (name, data) in entries {
        let offset = out.len() as u32;
        let crc = crc32(data);
        let size = data.len() as u32;
        let name_bytes = name.as_bytes();

        out.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
        out.extend_from_slice(&20u16.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&0x21u16.to_le_bytes());
        out.extend_from_slice(&crc.to_le_bytes());
        out.extend_from_slice(&size.to_le_bytes());
        out.extend_from_slice(&size.to_le_bytes());
        out.extend_from_slice(&(name_bytes.len() as u16).to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(name_bytes);
        out.extend_from_slice(data);

        central.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
        central.extend_from_slice(&0x031eu16.to_le_bytes());
        central.extend_from_slice(&20u16.to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes());
        central.extend_from_slice(&0x21u16.to_le_bytes());
        central.extend_from_slice(&crc.to_le_bytes());
        central.extend_from_slice(&size.to_le_bytes());
        central.extend_from_slice(&size.to_le_bytes());
        central.extend_from_slice(&(name_bytes.len() as u16).to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes());
        central.extend_from_slice(&0o100_644u32.wrapping_shl(16).to_le_bytes());
        central.extend_from_slice(&offset.to_le_bytes());
        central.extend_from_slice(name_bytes);
    }
    let offset = out.len() as u32;
    let central_size = central.len() as u32;
    out.extend_from_slice(&central);
    out.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    out.extend_from_slice(&central_size.to_le_bytes());
    out.extend_from_slice(&offset.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    std::fs::write(path, out).expect("写 zip");
}
