//! 恶意归档的验收测试：**每一条规则一个用例**，全部跑真实 `tar.exe`。
//!
//! ## 为什么这些必须是"真的解压一次"
//!
//! 单元测试（`src/name.rs`）证明的是**判据**；这里证明的是**判据真的
//! 挡在了解压之前**，以及**失败之后磁盘上什么都没留下**。
//!
//! 两者的区别很实在：一个只在纯函数上正确的校验，完全可能因为调用顺序
//! 写错（比如先解压再校验）而在真机上形同虚设。所以每个用例都断言三件事：
//!
//! 1. **错误分类正确**（`kind()` 与 `name_violation()`）——
//!    票据要求"给出具体原因"，不是笼统的"解压失败"；
//! 2. **目标版本目录不存在** —— 失败不能留下半个版本；
//! 3. **临时目录被清理** —— `versions/` 下不该有 `.staging-` 残留。
//!
//! ## 这些测试不碰机器的任何东西
//!
//! 归档、解压目录、版本目录全在测试自己造的临时目录里
//! （`tuoen_platform::test_support::TempDir`）。唯一被调用的外部程序是
//! 系统自带的 `tar.exe`，而它的工作目录由我们指定。

mod common;

use std::path::Path;

use common::{TarEntry, Workspace, ZipEntry};
use tuoen_archive::{
    ArchiveError, ExtractLimits, InstallOutcome, InstallRequest, NameViolation, TarCli,
    install_archive,
};
use tuoen_platform::{RealFileSystem, SystemProcessRunner};

/// 装一个归档，返回结果。
fn install(
    workspace: &Workspace,
    archive: &Path,
    version: &str,
) -> Result<InstallOutcome, ArchiveError> {
    let cli = TarCli::probe().expect("本机应当有 tar.exe");
    install_archive(
        &SystemProcessRunner,
        &RealFileSystem,
        &cli,
        &InstallRequest::new(archive, workspace.versions(), version),
    )
}

/// 装一个归档并期望它失败，同时断言"什么都没留下"。
///
/// 这三条断言是**每个恶意归档用例都要过的**，所以收在一个函数里 ——
/// 分散写的话，很容易漏掉其中一条而让测试看起来是绿的。
fn expect_rejected(workspace: &Workspace, archive: &Path, version: &str) -> ArchiveError {
    let error = install(workspace, archive, version)
        .err()
        .unwrap_or_else(|| panic!("`{archive:?}` 应当被拒绝"));

    assert!(
        error.is_archive_at_fault(),
        "这应当被判成'归档本身有问题'（而不是环境问题）：{error}"
    );
    assert!(
        !workspace.versions().join(version).exists(),
        "**失败不能留下版本目录**：{}",
        workspace.versions().join(version).display()
    );
    assert_no_staging_left(workspace);
    error
}

/// `versions/` 下不该有任何 `.staging-` 残留。
fn assert_no_staging_left(workspace: &Workspace) {
    let versions = workspace.versions();
    if !versions.is_dir() {
        return;
    }
    let leftovers: Vec<String> = std::fs::read_dir(&versions)
        .expect("读 versions")
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with(".staging-"))
        .collect();
    assert!(
        leftovers.is_empty(),
        "**失败必须完全不留痕迹**，但有临时目录残留：{leftovers:?}"
    );
}

// ================= Zip Slip 的每一种变体 =================

#[test]
fn a_parent_traversal_zip_is_rejected() {
    let workspace = Workspace::new("zip-slip-parent");
    let archive = workspace.zip(
        "evil.zip",
        &[
            ZipEntry::file("ok.txt", b"fine"),
            ZipEntry::file("../escaped.txt", b"pwned"),
        ],
    );
    let error = expect_rejected(&workspace, &archive, "1.0.0");
    assert_eq!(error.kind(), "unsafe-entry");
    assert_eq!(error.name_violation(), Some(NameViolation::ParentTraversal));
    assert!(error.to_string().contains("../escaped.txt"), "{error}");
}

#[test]
fn a_backslash_parent_traversal_zip_is_rejected() {
    // **反斜杠形态必须单独测**：只查 `/` 的实现会放过它，
    // 而实测 `..\escaped-backslash.txt` 是真实存在的攻击形态。
    let workspace = Workspace::new("zip-slip-backslash");
    let archive = workspace.zip("evil.zip", &[ZipEntry::file(r"..\escaped.txt", b"pwned")]);
    let error = expect_rejected(&workspace, &archive, "1.0.0");
    assert_eq!(error.name_violation(), Some(NameViolation::ParentTraversal));
}

#[test]
fn an_absolute_path_zip_is_rejected() {
    let workspace = Workspace::new("zip-slip-absolute");
    let archive = workspace.zip("evil.zip", &[ZipEntry::file("/escaped-abs.txt", b"pwned")]);
    let error = expect_rejected(&workspace, &archive, "1.0.0");
    assert_eq!(error.name_violation(), Some(NameViolation::AbsolutePath));
}

#[test]
fn a_unc_path_zip_is_rejected() {
    let workspace = Workspace::new("zip-slip-unc");
    let archive = workspace.zip(
        "evil.zip",
        &[ZipEntry::file("//server/share/escaped-unc.txt", b"pwned")],
    );
    let error = expect_rejected(&workspace, &archive, "1.0.0");
    assert_eq!(error.name_violation(), Some(NameViolation::UncPath));
}

#[test]
fn a_drive_letter_zip_is_rejected() {
    let workspace = Workspace::new("zip-slip-drive");
    let archive = workspace.zip(
        "evil.zip",
        &[ZipEntry::file("C:/escaped-drive.txt", b"pwned")],
    );
    let error = expect_rejected(&workspace, &archive, "1.0.0");
    assert_eq!(error.name_violation(), Some(NameViolation::DriveLetter));
}

#[test]
fn a_reserved_device_name_zip_is_rejected() {
    // 实测：bsdtar **真的会创建** `CON`，而创建出来的文件普通路径
    // 看不见（`Test-Path` 报 False），于是删都删不干净。
    // 所以这一条不是"理论上不该有"，是"实测必须挡"。
    let workspace = Workspace::new("zip-device-name");
    let archive = workspace.zip(
        "evil.zip",
        &[
            ZipEntry::file("ok.txt", b"fine"),
            ZipEntry::file("sub/NUL.txt", b"residue"),
        ],
    );
    let error = expect_rejected(&workspace, &archive, "1.0.0");
    assert_eq!(
        error.name_violation(),
        Some(NameViolation::ReservedDeviceName)
    );
}

#[test]
fn a_trailing_dot_zip_is_rejected() {
    // 实测：bsdtar 真的创建了 `trailing.`，而 Win32 会吃掉结尾的点 ——
    // 于是文件在资源管理器与多数工具里打不开、删不掉。
    let workspace = Workspace::new("zip-trailing-dot");
    let archive = workspace.zip("evil.zip", &[ZipEntry::file("trailing.", b"stubborn")]);
    let error = expect_rejected(&workspace, &archive, "1.0.0");
    assert_eq!(
        error.name_violation(),
        Some(NameViolation::TrailingDotOrSpace)
    );
}

#[test]
fn a_trailing_space_zip_is_rejected() {
    let workspace = Workspace::new("zip-trailing-space");
    let archive = workspace.zip("evil.zip", &[ZipEntry::file("trailing ", b"stubborn")]);
    let error = expect_rejected(&workspace, &archive, "1.0.0");
    assert_eq!(
        error.name_violation(),
        Some(NameViolation::TrailingDotOrSpace)
    );
}

#[test]
fn an_alternate_data_stream_zip_is_rejected() {
    // 实测：bsdtar 把 `stream.txt:evil` **静默改名**成 `stream.txt_evil`
    // —— 没写进 ADS，但名字变了而没有任何提示。名字变了就是没挡住。
    let workspace = Workspace::new("zip-ads");
    let archive = workspace.zip("evil.zip", &[ZipEntry::file("stream.txt:evil", b"ads")]);
    let error = expect_rejected(&workspace, &archive, "1.0.0");
    assert_eq!(
        error.name_violation(),
        Some(NameViolation::AlternateDataStream)
    );
}

#[test]
fn a_control_character_zip_is_rejected() {
    // **这条特别重要**：实测 bsdtar 在列表里把换行转义成 `\n`，
    // 所以一个只看列表的实现会把 `evil\n../x` 读成 `evil/n../x` ——
    // 看起来人畜无害。我们的校验在**原样名字**上做，所以能看出来。
    let workspace = Workspace::new("zip-control-char");
    let archive = workspace.zip(
        "evil.zip",
        &[ZipEntry::file("evil\n../escaped.txt", b"pwned")],
    );
    let error = expect_rejected(&workspace, &archive, "1.0.0");
    assert_eq!(
        error.name_violation(),
        Some(NameViolation::ControlCharacter)
    );
}

#[test]
fn a_case_collision_zip_is_rejected_before_extraction() {
    // 实测：NTFS 大小写不敏感，于是 `Readme.txt` + `README.TXT`
    // **只剩一个文件，内容是后一个条目的** —— 归档可以借此偷偷替换掉
    // 自己的合法文件。而覆盖发生之后磁盘上已经看不出曾经有两个。
    let workspace = Workspace::new("zip-case-collision");
    let archive = workspace.zip(
        "evil.zip",
        &[
            ZipEntry::file("Readme.txt", b"the real one"),
            ZipEntry::file("README.TXT", b"the replacement"),
        ],
    );
    let error = expect_rejected(&workspace, &archive, "1.0.0");
    assert_eq!(error.kind(), "case-collision");
    match error {
        ArchiveError::CaseCollision { first, second } => {
            assert_eq!(first, "Readme.txt");
            assert_eq!(second, "README.TXT");
        }
        other => panic!("期望 CaseCollision，得到 {other}"),
    }
}

#[test]
fn a_zip_with_a_symlink_entry_is_rejected() {
    // 票据点名要"校验 symlink 条目目标"。我们的答案是**整个拒绝**：
    // 实测 bsdtar 在 Windows 上会把它落成一个**普通文件**（内容是指向
    // 目标的文本）—— 安全，但结果与归档声明不符且无提示。
    // 一个"声称是链接、实际是文本文件"的条目不该被当成正常安装。
    let workspace = Workspace::new("zip-symlink");
    let archive = workspace.zip(
        "evil.zip",
        &[
            ZipEntry::file("ok.txt", b"fine"),
            ZipEntry::symlink("link-out", "../../outside-target"),
        ],
    );
    let error = expect_rejected(&workspace, &archive, "1.0.0");
    assert_eq!(error.kind(), "non-regular-entry");
}

#[test]
fn a_tar_with_a_symlink_is_rejected() {
    let workspace = Workspace::new("tar-symlink");
    let archive = workspace.tar(
        "evil.tar",
        &[
            TarEntry::file("ok.txt", b"fine"),
            TarEntry::symlink("escape-link", "../outside-target"),
        ],
    );
    let error = expect_rejected(&workspace, &archive, "1.0.0");
    assert_eq!(error.kind(), "non-regular-entry");
}

#[test]
fn a_tar_with_a_hard_link_is_rejected() {
    let workspace = Workspace::new("tar-hardlink");
    let archive = workspace.tar(
        "evil.tar",
        &[TarEntry::hard_link("hl", "../../outside-target/secret.txt")],
    );
    let error = expect_rejected(&workspace, &archive, "1.0.0");
    assert_eq!(error.kind(), "non-regular-entry");
}

#[test]
fn a_tar_with_a_fifo_is_rejected() {
    let workspace = Workspace::new("tar-fifo");
    let archive = workspace.tar("evil.tar", &[TarEntry::fifo("myfifo")]);
    let error = expect_rejected(&workspace, &archive, "1.0.0");
    assert_eq!(error.kind(), "non-regular-entry");
}

#[test]
fn an_unknown_extension_is_reported_as_unknown_not_as_unsupported() {
    let workspace = Workspace::new("unknown-ext");
    let archive = workspace.archive_path("thing.rar");
    std::fs::write(&archive, b"not really a rar").expect("写文件");
    let error = tuoen_archive::guess_format(&archive).expect_err("rar 不在表里");
    assert_eq!(error.kind(), "unknown-format");
    // 顺带确认：`install_archive` 对不认识的格式不会崩，只是解压器会失败。
    let result = install(&workspace, &archive, "1.0.0");
    assert!(result.is_err(), "一个不是归档的文件不该装成功");
    assert_no_staging_left(&workspace);
}

// ================= 正常路径：必须真的能装上 =================

#[test]
fn a_clean_zip_installs_and_leaves_no_staging_behind() {
    let workspace = Workspace::new("clean-zip");
    let archive = workspace.zip(
        "node-v24.19.0-win-x64.zip",
        &[
            ZipEntry::dir("node-v24.19.0-win-x64"),
            ZipEntry::file("node-v24.19.0-win-x64/node.exe", b"binary"),
            ZipEntry::file("node-v24.19.0-win-x64/README.md", b"docs"),
            ZipEntry::dir("node-v24.19.0-win-x64/node_modules"),
            ZipEntry::file("node-v24.19.0-win-x64/node_modules/npm/package.json", b"{}"),
        ],
    );

    let cli = TarCli::probe().expect("tar.exe");
    let outcome = install_archive(
        &SystemProcessRunner,
        &RealFileSystem,
        &cli,
        &InstallRequest::new(&archive, workspace.versions(), "24.19.0").stripping(1),
    )
    .expect("一个干净的归档应当装上");

    // `strip_components = 1` 把顶层目录剥掉了。
    assert_eq!(
        std::fs::read(outcome.installed_to.join("node.exe")).unwrap(),
        b"binary"
    );
    assert!(
        outcome
            .installed_to
            .join("node_modules/npm/package.json")
            .is_file()
    );
    assert!(!outcome.installed_to.join("node-v24.19.0-win-x64").exists());
    assert!(outcome.audit.entries >= 4, "{:?}", outcome.audit);
    assert_no_staging_left(&workspace);
}

#[test]
fn a_tar_with_directory_entries_strips_cleanly() {
    // **目录条目必须能被剥掉。** 实测 `tar.exe -tf` 把顶层目录列成
    // `node-v24.19.0-win-x64/`（带结尾斜杠）—— 剥 1 层之后它本来就该
    // 消失。把它当成错误会让每一个真实归档都装不上
    // （这正是 `a_clean_zip_installs_and_leaves_no_staging_behind`
    // 当初抓出来的 bug）。
    let workspace = Workspace::new("tar-dir-entries");
    let archive = workspace.tar(
        "node.tar",
        &[
            TarEntry::dir("node-v24.19.0-win-x64"),
            TarEntry::dir("node-v24.19.0-win-x64/node_modules"),
            TarEntry::file("node-v24.19.0-win-x64/node.exe", b"binary"),
        ],
    );
    let cli = TarCli::probe().expect("tar.exe");
    let outcome = install_archive(
        &SystemProcessRunner,
        &RealFileSystem,
        &cli,
        &InstallRequest::new(&archive, workspace.versions(), "24.19.0").stripping(1),
    )
    .expect("带目录条目的 tar 必须能装");

    assert_eq!(
        std::fs::read(outcome.installed_to.join("node.exe")).unwrap(),
        b"binary"
    );
    assert!(outcome.installed_to.join("node_modules").is_dir());
    assert_no_staging_left(&workspace);
}

#[test]
fn a_path_with_spaces_and_a_version_number_installs() {
    // **这不是假想的路径。** 本机 PATH 上就有
    // `C:\Dev\IDE\IDEA26\IntelliJ IDEA 2026.1.1\bin` —— 同时含空格和版本号，
    // 而且是取证报告里"最糟的那一条"（48 条 PATH 里 14 条含空格）。
    let workspace = Workspace::new("spaces-and-version");
    let archive = workspace.zip(
        "idea.zip",
        &[
            ZipEntry::file("IntelliJ IDEA 2026.1.1/bin/idea64.exe", b"binary"),
            ZipEntry::file("IntelliJ IDEA 2026.1.1/lib/app.jar", b"jar"),
        ],
    );
    let cli = TarCli::probe().expect("tar.exe");
    let outcome = install_archive(
        &SystemProcessRunner,
        &RealFileSystem,
        &cli,
        &InstallRequest::new(&archive, workspace.versions(), "2026.1.1"),
    )
    .expect("含空格与版本号的路径必须能装");

    assert!(
        outcome
            .installed_to
            .join("IntelliJ IDEA 2026.1.1/bin/idea64.exe")
            .is_file()
    );
}

#[test]
fn a_long_path_installs_thanks_to_the_extended_prefix() {
    // 票据要求"长路径用 `\\?\` 前缀处理"。这条造一个**总长超过 260** 的
    // 归档路径，验证它真的能解开 —— 而 260 正是没有前缀时的硬限制。
    let workspace = Workspace::new("long-path");
    // 每一层 40 个字符 × 8 层 = 320+ 字符。
    let mut nested = String::new();
    for i in 0..8 {
        if !nested.is_empty() {
            nested.push('/');
        }
        nested.push_str(&format!("{}-{}", "d".repeat(36), i));
    }
    let deep_file = format!("{nested}/payload.bin");

    let archive = workspace.zip("deep.zip", &[ZipEntry::file(&deep_file, b"deep")]);
    let cli = TarCli::probe().expect("tar.exe");
    let outcome = install_archive(
        &SystemProcessRunner,
        &RealFileSystem,
        &cli,
        &InstallRequest::new(&archive, workspace.versions(), "1.0.0"),
    )
    .expect("深层路径应当能解开");

    let installed = outcome
        .installed_to
        .join(deep_file.replace('/', std::path::MAIN_SEPARATOR_STR));
    let total = installed.to_string_lossy().len();
    assert!(
        total > 260,
        "这条测试要造的是一条**超过 MAX_PATH** 的路径，实际只有 {total} 字符"
    );
    assert!(
        tuoen_archive::long_path(&installed).is_some(),
        "这么长的路径必须能加 \\\\?\\ 前缀（否则普通路径访问不到）"
    );
    assert!(installed.is_file(), "深层文件应当真的落盘了");
}

#[test]
fn the_depth_limit_rejects_an_archive_that_would_exhaust_the_path() {
    let workspace = Workspace::new("too-deep");
    let mut nested = String::new();
    for i in 0..20 {
        if !nested.is_empty() {
            nested.push('/');
        }
        nested.push_str(&format!("d{i}"));
    }
    let archive = workspace.zip("deep.zip", &[ZipEntry::file(&format!("{nested}/x"), b"x")]);
    let cli = TarCli::probe().expect("tar.exe");
    let request = InstallRequest::new(&archive, workspace.versions(), "1.0.0");
    let request = InstallRequest {
        limits: ExtractLimits {
            max_depth: 5,
            ..request.limits
        },
        ..request
    };
    let error = install_archive(&SystemProcessRunner, &RealFileSystem, &cli, &request)
        .expect_err("超过层数上限就该被拒");
    assert_eq!(error.name_violation(), Some(NameViolation::TooDeep));
    assert_no_staging_left(&workspace);
}

// ================= 原子性 =================

#[test]
fn a_failure_after_extraction_still_leaves_nothing_behind() {
    // **这条测的是最难保证的那一半原子性。**
    //
    // 构造：归档的**名字全部合法**（所以过得了第一层），但总量超过上限
    // —— 于是失败发生在**解压之后、落位之前**。这正是"解压到一半发现
    // 不对"的场景，也是唯一能验证"审计不通过时最终名字从未出现过"的场景。
    let workspace = Workspace::new("atomic-late-failure");
    let archive = workspace.zip(
        "big.zip",
        &[
            ZipEntry::file("a.bin", &vec![0u8; 2000]),
            ZipEntry::file("b.bin", &vec![0u8; 2000]),
        ],
    );

    let cli = TarCli::probe().expect("tar.exe");
    let request = InstallRequest {
        limits: ExtractLimits {
            max_total_bytes: 1000,
            ..ExtractLimits::default()
        },
        ..InstallRequest::new(&archive, workspace.versions(), "1.0.0")
    };
    let error = install_archive(&SystemProcessRunner, &RealFileSystem, &cli, &request)
        .expect_err("总量超限应当在审计阶段失败");
    assert_eq!(error.name_violation(), Some(NameViolation::TooLarge));

    assert!(
        !workspace.versions().join("1.0.0").exists(),
        "**审计不通过时，最终名字必须从来没有出现过**"
    );
    assert_no_staging_left(&workspace);
}

#[test]
fn installing_the_same_version_twice_is_refused_not_silently_overwritten() {
    let workspace = Workspace::new("no-overwrite");
    let archive = workspace.zip("v1.zip", &[ZipEntry::file("node.exe", b"first")]);
    let cli = TarCli::probe().expect("tar.exe");
    let request = InstallRequest::new(&archive, workspace.versions(), "1.0.0");

    install_archive(&SystemProcessRunner, &RealFileSystem, &cli, &request).expect("第一次装上");
    let error = install_archive(&SystemProcessRunner, &RealFileSystem, &cli, &request)
        .expect_err("第二次必须被拒");
    assert_eq!(error.kind(), "destination-exists");
    assert!(
        !error.is_archive_at_fault(),
        "这是调用方的问题，不是归档的问题"
    );

    // 而且**原来那份还在**，没有被覆盖成半成品。
    assert_eq!(
        std::fs::read(workspace.versions().join("1.0.0/node.exe")).unwrap(),
        b"first"
    );
    assert_no_staging_left(&workspace);
}

#[test]
fn two_versions_coexist_side_by_side() {
    // "多版本共存"是 L0 的招牌能力之一 —— 而它的基础就是
    // `versions/<ver>/` 这个布局。这条确认落位不会互相踩。
    let workspace = Workspace::new("two-versions");
    let cli = TarCli::probe().expect("tar.exe");
    for (name, version, content) in [
        ("a.zip", "20.18.3", b"old".as_slice()),
        ("b.zip", "24.19.0", b"new".as_slice()),
    ] {
        let archive = workspace.zip(name, &[ZipEntry::file("node.exe", content)]);
        install_archive(
            &SystemProcessRunner,
            &RealFileSystem,
            &cli,
            &InstallRequest::new(&archive, workspace.versions(), version),
        )
        .unwrap_or_else(|e| panic!("装 {version} 失败：{e}"));
    }
    assert_eq!(
        std::fs::read(workspace.versions().join("20.18.3/node.exe")).unwrap(),
        b"old"
    );
    assert_eq!(
        std::fs::read(workspace.versions().join("24.19.0/node.exe")).unwrap(),
        b"new"
    );
    assert_no_staging_left(&workspace);
}
