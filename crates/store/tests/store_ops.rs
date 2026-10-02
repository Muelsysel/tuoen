//! store 在**真实文件系统**上的行为。
//!
//! # 为什么必须在真磁盘上跑
//!
//! 本层的三条核心设计都是**关于磁盘状态**的断言：
//!
//! * "目录是事实来源" —— 要知道"记录坏了但目录还在"是什么表现；
//! * "`current` 是 junction，翻转是原子的" —— 要知道第二次重指是
//!   [`Repoint::Replaced`] 而不是"删掉再建"；
//! * "绝不穿透重解析点" —— 要知道拒绝之后链接**目标**里的东西一个不少。
//!
//! 用固定装置模拟这三件事，等于用假设验证假设。
//!
//! # 绝不碰真的 `%LOCALAPPDATA%\tuoen`
//!
//! 每个测试都用 `Store::new(<临时目录>)`。默认位置只被
//! `the_default_location_is_not_any_temp_path` **读**一次，从不写入。

use std::path::{Path, PathBuf};

use tuoen_manifest::Layout;
use tuoen_platform::Repoint;
use tuoen_platform::test_support::{TempDir, make_junction};
use tuoen_store::{
    InstallRecord, RECORD_SCHEMA_VERSION, Store, UninstallOptions, activate, active_version,
    adopt_payload, compare_versions, deactivate, find_version, installed_tools, installed_versions,
    store_is_empty, uninstall, write_record,
};

// ---------------------------------------------------------------------------
// 固定装置
// ---------------------------------------------------------------------------

/// 在临时目录里开一个 store（**不是**临时目录根：载荷要放在 store **外面**，
/// 这样"搬进去"这条断言才有意义）。
fn store_in(temp: &TempDir) -> Store {
    Store::new(temp.mkdir("store"))
}

/// 一份字段齐全的记录。
fn record_for(tool: &str, version: &str) -> InstallRecord {
    InstallRecord {
        schema_version: RECORD_SCHEMA_VERSION,
        tool: tool.to_owned(),
        version: version.to_owned(),
        display_name: "Node.js".to_owned(),
        source_id: "cn-npmmirror".to_owned(),
        url: format!("https://example.invalid/{tool}/{version}"),
        sha256: "0".repeat(64),
        archive: format!("{tool}-{version}.zip"),
        installed_at: "2025-10-02T13:45:01Z".to_owned(),
        payload_files: 0,
        payload_bytes: 0,
        layout: Layout::default(),
    }
}

/// 造一个载荷目录（自动建父目录）并搬进 store，返回落位后的版本目录。
fn install(
    temp: &TempDir,
    store: &Store,
    tool: &str,
    version: &str,
    files: &[(&str, &[u8])],
) -> PathBuf {
    let dir = temp.mkdir(&format!("payload-{tool}-{version}"));
    for (name, bytes) in files {
        let path = dir.join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("建载荷里的父目录");
        }
        std::fs::write(&path, bytes).expect("写载荷文件");
    }
    let mut record = record_for(tool, version);
    adopt_payload(store, tool, version, &dir, &mut record).expect("把载荷搬进 store");
    store.version_dir(tool, version)
}

/// 读一份记录文件。
fn read_back(path: &Path) -> InstallRecord {
    let text = std::fs::read_to_string(path).expect("读记录文件");
    InstallRecord::from_json(&text).expect("记录要能读回")
}

// ---------------------------------------------------------------------------
// 采纳载荷
// ---------------------------------------------------------------------------

#[test]
fn adopt_moves_the_payload_into_the_store_and_measures_it_after_the_move() {
    let temp = TempDir::new("store-adopt");
    let store = store_in(&temp);
    let payload = temp.mkdir("payload");
    std::fs::write(payload.join("a.txt"), b"12345").expect("写"); // 5 字节
    std::fs::write(payload.join("c.txt"), b"hello world").expect("写"); // 11 字节
    std::fs::create_dir_all(payload.join("bin")).expect("建 bin"); // 子目录
    std::fs::write(payload.join("bin").join("node.exe"), b"0123456789").expect("写"); // 10 字节

    let mut record = record_for("node", "24.19.0");
    // 调用方填的统计**不算数**：它不知道落盘后真有几个文件。
    record.payload_files = 999;
    record.payload_bytes = 999;

    let outcome =
        adopt_payload(&store, "node", "24.19.0", &payload, &mut record).expect("搬进 store");

    // **根目录自己不算一个条目**，子目录里的文件要算进来：2 + 1。
    assert_eq!(outcome.files, 3, "3 个文件（其中 1 个在子目录里）");
    assert_eq!(outcome.bytes, 5 + 11 + 10, "字节数是三份内容之和");
    assert_eq!(record.payload_files, 3, "记录里的数字必须被落盘结果覆盖");
    assert_eq!(record.payload_bytes, 26);

    // `rename` 是**移动**，不是复制：原路径不该还在。
    assert!(!payload.exists(), "载荷必须被搬走");
    assert_eq!(outcome.path, store.version_dir("node", "24.19.0"));
    assert!(outcome.path.join("bin").join("node.exe").is_file());

    // 记录落在版本目录**旁边**，而且读得回来。
    let path = store.record_path("node", "24.19.0");
    assert!(path.is_file(), "{path:?}");
    assert_eq!(read_back(&path), record);
    assert_eq!(record.tool, "node", "记录里的 tool 与落盘位置一致");
    assert_eq!(record.version, "24.19.0");

    // 记录不是事实来源：即使把它删掉，这个版本仍然在。
    std::fs::remove_file(&path).expect("删记录");
    let listed = installed_versions(&store, "node");
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].record, None, "记录缺失是 `None`，不是错误");
    assert_eq!(listed[0].files, 3, "统计不依赖记录文件");
}

#[test]
fn adopting_over_an_existing_version_refuses_and_moves_nothing() {
    let temp = TempDir::new("store-adopt-conflict");
    let store = store_in(&temp);
    let first = temp.mkdir("payload-1");
    std::fs::write(first.join("marker.txt"), b"already here").expect("写");
    let second = temp.mkdir("payload-2");
    std::fs::write(second.join("marker.txt"), b"the new one").expect("写");

    let mut record = record_for("node", "24.19.0");
    adopt_payload(&store, "node", "24.19.0", &first, &mut record).expect("第一次");

    let error =
        adopt_payload(&store, "node", "24.19.0", &second, &mut record).expect_err("第二次必须被拒");
    assert_eq!(error.kind(), "already-installed");
    assert!(error.is_user_error());
    assert!(
        error.to_string().contains("不覆盖"),
        "消息要说清'不覆盖'：{error}"
    );

    // **拒绝之后两边的文件都还在**：已经装好的那份可能是能用的，
    // 而交来的载荷是调用方的（由它自己清理）。
    assert_eq!(
        std::fs::read_to_string(store.version_dir("node", "24.19.0").join("marker.txt"))
            .expect("读已经装好的那份"),
        "already here",
        "已经装好的那份不能被覆盖"
    );
    assert_eq!(
        std::fs::read_to_string(second.join("marker.txt")).expect("读交来的载荷"),
        "the new one",
        "交来的载荷不能被删"
    );
}

#[test]
fn a_payload_must_be_a_real_directory() {
    let temp = TempDir::new("store-adopt-payload");
    let store = store_in(&temp);

    // 不存在。
    let mut record = record_for("node", "24.19.0");
    let missing = temp.join("nope");
    let error = adopt_payload(&store, "node", "24.19.0", &missing, &mut record)
        .expect_err("载荷不存在要报错");
    assert_eq!(error.kind(), "payload-missing");
    assert!(error.to_string().contains("nope"), "{error}");

    // 是一个**文件**，不是目录。
    let file = temp.write("payload-file", b"i am a file");
    let error =
        adopt_payload(&store, "node", "24.19.0", &file, &mut record).expect_err("文件不能当载荷");
    assert_eq!(error.kind(), "refused");
    assert!(file.is_file(), "被拒之后那个文件不能被动");
}

#[test]
fn a_failed_record_write_does_not_roll_back_the_payload() {
    // 记录是**附注**：写不下去时载荷已经就位，而且它仍然是一个版本（`record: None`）。
    // 为了附注把一份已经装好的工具链删掉，是拿事实去凑附注。
    let temp = TempDir::new("store-record-failure");
    let store = store_in(&temp);
    let payload = temp.mkdir("payload");
    std::fs::write(payload.join("marker.txt"), b"payload").expect("写");

    // 让记录写不下去：在记录的位置放一个**目录** —— `fs::write` 对目录必然失败，
    // 而版本目录本身的位置（`versions/24.19.0`）不受影响。
    let blocker = temp.mkdir("blocker");
    std::fs::create_dir_all(store.record_path("node", "24.19.0")).expect("在记录位置造一个目录");

    let mut record = record_for("node", "24.19.0");
    let error = adopt_payload(&store, "node", "24.19.0", &payload, &mut record)
        .expect_err("记录写不下去要报错");
    assert_eq!(error.kind(), "io");

    // **不回滚**：载荷还在，而且 `installed_versions` 仍然看得见这个版本。
    assert!(
        store
            .version_dir("node", "24.19.0")
            .join("marker.txt")
            .is_file(),
        "载荷不回滚"
    );
    assert!(!payload.exists(), "载荷已经被搬走了");
    let listed = installed_versions(&store, "node");
    let version = listed
        .iter()
        .find(|version| version.version == "24.19.0")
        .expect("载荷在就应当被列出来");
    assert_eq!(version.record, None, "记录坏了 → `None`");
    assert_eq!(version.files, 1);

    std::fs::remove_dir_all(&blocker).expect("清理");
}

#[test]
fn the_real_install_path_stages_inside_the_store_and_survives_the_move() {
    // 这条复刻 `tuoen install` 的**真实**路径：归档先解压到
    // `<store>/.incoming/<version>`（`tuoen-archive` 的临时区），然后由
    // `adopt_payload` 搬到 `<store>/<tool>/versions/<version>`。两步都在 store 里，
    // 所以 `rename` 同卷；而 `.incoming` 在任何时刻都**不能**被当成一个工具。
    let temp = TempDir::new("store-incoming");
    let store = store_in(&temp);
    let incoming = store.root().join(".incoming").join("24.19.0");
    std::fs::create_dir_all(incoming.join("bin")).expect("建 .incoming/<version>");
    std::fs::write(incoming.join("bin").join("node.exe"), b"exe").expect("写");
    std::fs::write(incoming.join("README.md"), b"readme").expect("写");

    assert!(store_is_empty(&store), "`.incoming` 不算工具");
    assert_eq!(installed_tools(&store), Vec::<String>::new());

    let mut record = record_for("node", "24.19.0");
    let outcome = adopt_payload(&store, "node", "24.19.0", &incoming, &mut record)
        .expect("把 .incoming 里的那一份搬进 store");
    assert_eq!(outcome.files, 2);
    assert_eq!(outcome.bytes, 3 + 6);
    assert!(!incoming.exists(), "staging 目录必须被搬走（不是复制）");

    assert_eq!(
        installed_tools(&store),
        ["node"],
        "`.incoming` 不能出现在工具列表里"
    );
    assert!(!store_is_empty(&store));
    assert_eq!(
        activate(&store, "node", "24.19.0").expect("激活"),
        Repoint::Created
    );
    assert_eq!(active_version(&store, "node"), Some("24.19.0".to_owned()));
    // `.incoming` 这个壳还在（`install_archive` 的下一个版本还要用它），但它不是工具。
    assert!(store.root().join(".incoming").is_dir());
    assert_eq!(installed_tools(&store), ["node"]);
}

// ---------------------------------------------------------------------------
// 列举
// ---------------------------------------------------------------------------

#[test]
fn installed_versions_are_sorted_naturally_descending() {
    let temp = TempDir::new("store-order");
    let store = store_in(&temp);
    let versions = store.versions_dir("node");
    std::fs::create_dir_all(&versions).expect("建 versions");
    for version in ["24.19.0", "24.21.0", "24.9.0"] {
        std::fs::create_dir(versions.join(version)).expect("建版本目录");
    }

    let listed: Vec<String> = installed_versions(&store, "node")
        .into_iter()
        .map(|version| version.version)
        .collect();
    assert_eq!(
        listed,
        ["24.21.0", "24.19.0", "24.9.0"],
        "版本号**降序**，而且 `24.9.0` 必须排在 `24.21.0` 下面"
    );

    // 这条断言是给未来的自己看的：如果哪天有人把实现换成字符串排序，
    // 上面那条会红 —— 但它红了以后，得看得出"这不是打字错误"。
    let mut as_text = listed.clone();
    as_text.sort();
    as_text.reverse();
    assert_ne!(
        as_text, listed,
        "如果字符串排序给出同一个顺序，这条测试就没在测自然比较"
    );

    // 手工建出来的版本目录没有记录（`record: None`）—— 目录才是事实来源。
    for version in installed_versions(&store, "node") {
        assert_eq!(version.record, None, "{} 没有记录", version.version);
        assert_eq!(version.files, 0);
        assert_eq!(version.bytes, 0);
        assert!(!version.active);
    }
}

#[test]
fn the_active_flag_and_active_version_follow_current() {
    let temp = TempDir::new("store-active");
    let store = store_in(&temp);
    for version in ["24.19.0", "24.21.0"] {
        install(
            &temp,
            &store,
            "node",
            version,
            &[("marker.txt", version.as_bytes())],
        );
    }

    assert_eq!(active_version(&store, "node"), None, "还没激活");
    for version in installed_versions(&store, "node") {
        assert!(!version.active, "{} 不该是激活的", version.version);
    }

    activate(&store, "node", "24.19.0").expect("激活 24.19.0");
    assert_eq!(active_version(&store, "node"), Some("24.19.0".to_owned()));
    let listed = installed_versions(&store, "node");
    assert_eq!(listed.len(), 2);
    for version in &listed {
        assert_eq!(
            version.active,
            version.version == "24.19.0",
            "{} 的 active 标志不对",
            version.version
        );
    }

    // 翻转之后标志跟着走（而不是第一次算完就缓存住了）。
    activate(&store, "node", "24.21.0").expect("激活 24.21.0");
    assert_eq!(active_version(&store, "node"), Some("24.21.0".to_owned()));
    let listed = installed_versions(&store, "node");
    assert_eq!(
        listed.iter().filter(|version| version.active).count(),
        1,
        "同一时刻只可能有一个激活的版本"
    );
    assert_eq!(listed[0].version, "24.21.0", "降序的第一个就是激活的那个");
}

#[test]
fn stray_files_and_staging_directories_are_not_versions() {
    // `tuoen-archive` 的 `install_archive` 把临时解压目录建在
    // `<dest_root>/.staging-<label>-<n>`，而票 #6 的 `dest_root` 正是
    // `<store>/<tool>/versions`。所以 `versions/` 里**正常情况下就会出现**
    // `.staging-*` —— 它绝不能被当成一个版本号，也不能污染别的版本的统计。
    let temp = TempDir::new("store-stray");
    let store = store_in(&temp);
    install(&temp, &store, "node", "24.19.0", &[("marker.txt", b"x")]);

    let versions = store.versions_dir("node");
    // 误放的文件。
    std::fs::write(versions.join("foo.json"), b"{}").expect("放一个多余的文件");
    // 正在解压的那一份。
    let staging = versions.join(".staging-node-1");
    std::fs::create_dir_all(&staging).expect("建 .staging-node-1");
    std::fs::write(staging.join("half-extracted.bin"), b"1234567890").expect("写半个制品");

    let listed = installed_versions(&store, "node");
    assert_eq!(listed.len(), 1, "只应当有 1 个版本：{listed:?}");
    assert_eq!(listed[0].version, "24.19.0");
    assert_eq!(listed[0].files, 1, "staging 里的文件不能算进版本的统计");
    assert_eq!(listed[0].bytes, 1);

    // 直接点名问也不行 —— 它们不是版本。
    assert!(
        find_version(&store, "node", "foo.json").is_none(),
        "文件不是版本"
    );
    assert!(
        find_version(&store, "node", ".staging-node-1").is_none(),
        "内部产物位不是版本"
    );
    // 而工具列表不受影响。
    assert_eq!(installed_tools(&store), ["node"]);
}

#[test]
fn a_version_directory_with_an_unsafe_name_is_skipped_not_panicked() {
    // **实测**（本机，Windows）：`CreateDirectoryW` 拒绝结尾带点的名字，
    // 但加 `\\?\` 前缀之后**真的能造出来**，而且普通路径的枚举**看得见**它
    // （`[IO.Directory]::GetDirectories(<普通路径>)` 数到 1）。
    // 所以这条用的是真磁盘上的真目录，不是固定装置。
    //
    // 对照组同样实测过：**控制字符连 `\\?\` 都造不出来** ——
    // `[IO.Directory]::CreateDirectory('\\?\…\ver\x01')` 报
    // "文件名、目录名或卷标语法不正确"，所以这里选了结尾带点那个形态。
    let temp = TempDir::new("store-unsafe-name");
    let store = store_in(&temp);
    install(&temp, &store, "node", "24.19.0", &[("marker.txt", b"x")]);

    // 路径是**手工拼**的，不走 `Store::version_dir` —— 那条路径上的
    // `debug_assert` 正是为了在有人拿不安全的名字去推路径时立刻炸（这里我们
    // 是**故意**要造出那个名字，所以必须绕开那个断言；它已经在第一次运行时
    // 抓到了我这一行）。
    let unsafe_dir = store.versions_dir("node").join("1.2.3.");
    let verbatim = PathBuf::from(format!(r"\\?\{}", unsafe_dir.display()));
    std::fs::create_dir(&verbatim)
        .unwrap_or_else(|error| panic!("本机应当能用 `\\\\?\\` 前缀造出结尾带点的目录：{error}"));
    std::fs::write(verbatim.join("marker.txt"), b"trailing dot").expect("往里面写一个文件");
    assert!(verbatim.is_dir(), "那个目录真的在（verbatim 形式看得见它）");
    assert!(
        !unsafe_dir.exists(),
        "而普通路径**看不见**它 —— 这正是这条规则存在的理由"
    );

    // 跳过，而不是 panic；也不影响那个正常的版本。
    let listed = installed_versions(&store, "node");
    assert_eq!(listed.len(), 1, "结尾带点的目录必须被跳过：{listed:?}");
    assert_eq!(listed[0].version, "24.19.0");
    assert!(find_version(&store, "node", "1.2.3.").is_none());
    assert_eq!(installed_tools(&store), ["node"]);

    // 清理只能用 verbatim 形式（普通路径删不掉它 —— `TempDir` 的 Drop 也删不掉）。
    std::fs::remove_dir_all(&verbatim).expect("用 verbatim 形式清理");
    assert!(!verbatim.exists(), "清理干净，别在 %TEMP% 里留残骸");
}

// ---------------------------------------------------------------------------
// 激活与摘链接
// ---------------------------------------------------------------------------

#[test]
fn activating_is_created_then_replaced_and_reports_the_active_version() {
    let temp = TempDir::new("store-activate");
    let store = store_in(&temp);
    install(
        &temp,
        &store,
        "node",
        "24.19.0",
        &[("marker.txt", b"nineteen")],
    );
    install(
        &temp,
        &store,
        "node",
        "24.21.0",
        &[("marker.txt", b"twenty-one")],
    );

    assert_eq!(
        activate(&store, "node", "24.19.0").expect("第一次激活"),
        Repoint::Created,
        "原本没有 `current`，所以是就地创建"
    );
    assert_eq!(active_version(&store, "node"), Some("24.19.0".to_owned()));
    assert_eq!(
        std::fs::read_to_string(store.current_link("node").join("marker.txt"))
            .expect("透过 junction 读"),
        "nineteen"
    );

    // **原子翻转的证据**：同标签的重解析点被**就地替换**
    // （一次 `FSCTL_SET_REPARSE_POINT`），不是"删掉再建"（那样有窗口）。
    assert_eq!(
        activate(&store, "node", "24.21.0").expect("第二次激活"),
        Repoint::Replaced,
        "如果这里是 `Degraded` 或报错，`tuoen use` 就不原子了"
    );
    assert_eq!(active_version(&store, "node"), Some("24.21.0".to_owned()));
    assert_eq!(
        std::fs::read_to_string(store.current_link("node").join("marker.txt"))
            .expect("重指后透过 junction 读"),
        "twenty-one"
    );
    // 两个版本目录都还在 —— 翻转只动链接。
    assert!(store.version_dir("node", "24.19.0").is_dir());
    assert!(store.version_dir("node", "24.21.0").is_dir());
}

#[test]
fn activating_something_that_is_not_an_installed_version_is_refused() {
    let temp = TempDir::new("store-activate-missing");
    let store = store_in(&temp);
    std::fs::create_dir_all(store.versions_dir("node")).expect("建 versions");

    let error = activate(&store, "node", "24.19.0").expect_err("没装就不能激活");
    assert_eq!(error.kind(), "not-installed");
    assert!(error.is_user_error());

    // 文件不是版本。
    std::fs::write(store.versions_dir("node").join("1.0.0"), b"not a dir").expect("放一个文件");
    assert_eq!(
        activate(&store, "node", "1.0.0")
            .expect_err("文件不是版本")
            .kind(),
        "not-installed"
    );

    // 内部产物位不是版本（`.staging-*` 可能正被另一个进程在写）。
    std::fs::create_dir_all(store.version_dir("node", ".staging-node-1")).expect("建 staging");
    assert_eq!(
        activate(&store, "node", ".staging-node-1")
            .expect_err("内部产物位不是版本")
            .kind(),
        "not-installed"
    );

    assert!(
        !store.current_link("node").exists(),
        "一次也不该建出 current"
    );
}

#[test]
fn deactivating_only_removes_the_link_and_never_the_target() {
    let temp = TempDir::new("store-deactivate");
    let store = store_in(&temp);
    let version_dir = install(
        &temp,
        &store,
        "node",
        "24.19.0",
        &[("marker.txt", b"one"), ("bin/node.exe", b"two")],
    );
    activate(&store, "node", "24.19.0").expect("激活");

    let marker_count = |dir: &Path| std::fs::read_dir(dir).expect("列目录").count();
    let before = marker_count(&version_dir);

    assert!(
        deactivate(&store, "node").expect("摘链接"),
        "有 current → true"
    );
    assert!(!store.current_link("node").exists(), "链接本身应当没了");
    assert!(version_dir.is_dir(), "版本目录必须还在");
    assert_eq!(
        marker_count(&version_dir),
        before,
        "**删链接绝不能递归进目标**"
    );
    assert!(version_dir.join("bin").join("node.exe").is_file());

    // 没有 `current` 时返回 false（而不是报错）。
    assert!(
        !deactivate(&store, "node").expect("再摘一次"),
        "没有 current → false"
    );
    assert_eq!(active_version(&store, "node"), None);
}

#[test]
fn deactivating_refuses_to_delete_a_real_directory_called_current() {
    // 最危险的一种误操作：`current` 是用户自己建的真实目录。
    // 把它当链接删掉就等于递归删掉里面的东西。
    let temp = TempDir::new("store-current-real-dir");
    let store = store_in(&temp);
    let real = temp.mkdir("store/node/current");
    std::fs::write(real.join("我的数据.txt"), b"precious").expect("写用户数据");

    let error = deactivate(&store, "node").expect_err("真目录必须被拒绝");
    assert_eq!(error.kind(), "refused");
    assert!(error.to_string().contains("真实目录"), "{error}");
    assert!(
        real.join("我的数据.txt").is_file(),
        "**用户的数据必须还在**"
    );
}

// ---------------------------------------------------------------------------
// 卸载
// ---------------------------------------------------------------------------

#[test]
fn uninstalling_the_active_version_needs_force_and_touches_nothing() {
    let temp = TempDir::new("store-uninstall-active");
    let store = store_in(&temp);
    let version_dir = install(&temp, &store, "node", "24.19.0", &[("marker.txt", b"x")]);
    install(&temp, &store, "node", "24.21.0", &[("marker.txt", b"y")]);
    activate(&store, "node", "24.19.0").expect("激活");

    let error = uninstall(&store, "node", "24.19.0", UninstallOptions::default())
        .expect_err("删激活版本必须被拒");
    assert_eq!(error.kind(), "active-version");
    assert!(error.is_user_error());
    let text = error.to_string();
    assert!(text.contains("先 `tuoen use` 到别的版本"), "{text}");
    assert!(text.contains("或加 `--force`"), "{text}");

    // **载荷一个字节都不能少**，`current` 也不能动。
    assert!(
        version_dir.join("marker.txt").is_file(),
        "被拒时载荷必须完好"
    );
    assert_eq!(active_version(&store, "node"), Some("24.19.0".to_owned()));
    assert!(store.record_path("node", "24.19.0").is_file());
}

#[test]
fn force_uninstalling_the_active_version_flips_first_then_removes_everything() {
    let temp = TempDir::new("store-uninstall-force");
    let store = store_in(&temp);
    install(
        &temp,
        &store,
        "node",
        "24.19.0",
        &[("marker.txt", b"12345"), ("bin/node.exe", b"0123456789")],
    );
    install(&temp, &store, "node", "24.21.0", &[("marker.txt", b"y")]);
    activate(&store, "node", "24.19.0").expect("激活");

    let outcome = uninstall(
        &store,
        "node",
        "24.19.0",
        UninstallOptions { force_active: true },
    )
    .expect("强制卸载");
    assert!(outcome.was_active, "它本来是激活的那个");
    assert_eq!(outcome.freed_files, 2);
    assert_eq!(outcome.freed_bytes, 5 + 10);

    // 三样东西都不该还在：链接、载荷、记录。
    assert!(!store.current_link("node").exists(), "current 必须先被摘掉");
    assert!(!store.version_dir("node", "24.19.0").exists());
    assert!(!store.record_path("node", "24.19.0").exists());
    // 而且 `active_version` 不能指向一个已经没了的目录。
    assert_eq!(active_version(&store, "node"), None);
    assert!(find_version(&store, "node", "24.19.0").is_none());

    // 另一个版本完好，而且现在可以被激活。
    assert!(
        store
            .version_dir("node", "24.21.0")
            .join("marker.txt")
            .is_file()
    );
    activate(&store, "node", "24.21.0").expect("另一个版本仍然可用");
}

#[test]
fn uninstalling_a_version_that_is_not_installed_is_refused() {
    let temp = TempDir::new("store-uninstall-missing");
    let store = store_in(&temp);

    let error = uninstall(&store, "node", "24.19.0", UninstallOptions::default())
        .expect_err("没装就不能卸载");
    assert_eq!(error.kind(), "not-installed");
    assert!(error.is_user_error());

    // 文件不是版本。
    std::fs::create_dir_all(store.versions_dir("node")).expect("建 versions");
    std::fs::write(store.versions_dir("node").join("1.0.0"), b"not a dir").expect("放一个文件");
    assert_eq!(
        uninstall(&store, "node", "1.0.0", UninstallOptions::default())
            .expect_err("文件不是版本")
            .kind(),
        "not-installed"
    );
    assert!(
        store.versions_dir("node").join("1.0.0").is_file(),
        "被拒时什么都不该被删"
    );
}

#[test]
fn a_junction_where_the_version_should_be_is_refused_by_uninstall_and_by_adopt() {
    // 这条守的是本层最不能出的事故：`remove_dir_all` 顺着链接走，
    // 删掉的是链接**目标**里的东西 —— 那可能是任何地方的任何文件。
    let temp = TempDir::new("store-junction");
    let store = store_in(&temp);
    let elsewhere = temp.mkdir("elsewhere");
    std::fs::write(elsewhere.join("precious.txt"), b"do not delete").expect("写");

    let dir = store.version_dir("node", "1.0.0");
    std::fs::create_dir_all(dir.parent().expect("versions 目录")).expect("建 versions");
    assert!(
        make_junction(&dir, &elsewhere),
        "本机未提权也能建 junction（ADR-0001）"
    );

    let error = uninstall(&store, "node", "1.0.0", UninstallOptions::default())
        .expect_err("版本位置是链接必须拒绝");
    assert_eq!(error.kind(), "refused");
    assert!(error.to_string().contains("重解析点"), "{error}");
    assert!(
        elsewhere.join("precious.txt").is_file(),
        "**链接目标里的东西必须完好**"
    );
    assert!(dir.is_dir(), "链接本身也不该被动");

    // 同一条规则适用于 adopt 的**目标位置**（往链接上搬载荷比"已经装过"严重）。
    let mut record = record_for("node", "1.0.0");
    let payload = temp.mkdir("payload");
    std::fs::write(payload.join("marker.txt"), b"x").expect("写");
    let error = adopt_payload(&store, "node", "1.0.0", &payload, &mut record)
        .expect_err("目标位置是链接必须拒绝");
    assert_eq!(error.kind(), "refused");
    assert!(elsewhere.join("precious.txt").is_file(), "目标里的东西还在");
    assert!(payload.join("marker.txt").is_file(), "载荷也不能被动");

    // 而**载荷**是一个链接时同样拒绝（载荷必须是真实目录）。
    let linked_payload = temp.join("linked-payload");
    assert!(make_junction(&linked_payload, &elsewhere));
    let mut record = record_for("node", "2.0.0");
    let error = adopt_payload(&store, "node", "2.0.0", &linked_payload, &mut record)
        .expect_err("载荷是链接必须拒绝");
    assert_eq!(error.kind(), "refused");

    tuoen_platform::remove_junction(&dir).expect("清理链接");
}

// ---------------------------------------------------------------------------
// 工具的列举与"空"
// ---------------------------------------------------------------------------

#[test]
fn installed_tools_and_store_is_empty_use_the_same_judgement() {
    let temp = TempDir::new("store-tools");
    let store = store_in(&temp);
    let nothing = Vec::<String>::new();

    // ① 全新的 store（连根目录都还没建）：空。
    assert!(store_is_empty(&store), "一个工具都没有");
    assert_eq!(installed_tools(&store), nothing);
    assert_eq!(installed_tools(&store).is_empty(), store_is_empty(&store));

    // ② 只有 `.incoming`（`tuoen install` 的临时解压区）与一个**空的** `versions/`：还是空。
    std::fs::create_dir_all(store.root().join(".incoming")).expect("建 .incoming");
    std::fs::create_dir_all(store.versions_dir("node")).expect("建空 versions");
    std::fs::write(store.root().join("stray.txt"), "root 下的散文件").expect("写散文件");
    assert!(
        store_is_empty(&store),
        "工具目录存在但 versions/ 空 → 没有工具；`.incoming` 也不算工具"
    );
    assert_eq!(installed_tools(&store), nothing);
    assert_eq!(installed_tools(&store).is_empty(), store_is_empty(&store));

    // ③ 装两个工具（各一个版本）：**升序**，而且两边不再说空。
    install(
        &temp,
        &store,
        "temurin",
        "21.0.12.1+1",
        &[("marker.txt", b"jdk")],
    );
    install(&temp, &store, "node", "24.19.0", &[("marker.txt", b"node")]);
    assert_eq!(installed_tools(&store), ["node", "temurin"], "升序");
    assert!(!store_is_empty(&store));
    assert_eq!(installed_tools(&store).is_empty(), store_is_empty(&store));

    // ④ 把最后一个版本也删掉：回到空（`.incoming` 与空目录不算数）。
    uninstall(&store, "node", "24.19.0", UninstallOptions::default()).expect("卸 node");
    uninstall(
        &store,
        "temurin",
        "21.0.12.1+1",
        UninstallOptions::default(),
    )
    .expect("卸 temurin");
    assert_eq!(installed_tools(&store), nothing);
    assert!(store_is_empty(&store));
}

// ---------------------------------------------------------------------------
// 名字安全：第一道闸
// ---------------------------------------------------------------------------

#[test]
fn unsafe_names_are_refused_before_anything_touches_the_disk() {
    let temp = TempDir::new("store-unsafe");
    let store = store_in(&temp);
    let payload = temp.mkdir("payload");
    std::fs::write(payload.join("marker.txt"), b"x").expect("写");

    let mut record = record_for("node", "1.0.0");
    // 带 `Result` 的函数：一律在**第一行**返回 `UnsafeName`。
    for (tool, version) in [
        ("..", "1.0.0"),
        ("a/b", "1.0.0"),
        ("node", "../x"),
        (r"node", r"a\b"),
        ("node", "CON"),
        ("node", "1.0."),
        ("node", ""),
    ] {
        let error = adopt_payload(&store, tool, version, &payload, &mut record)
            .expect_err("不安全的名字必须被拒");
        assert_eq!(error.kind(), "unsafe-name", "{tool:?} / {version:?}");
        assert!(error.is_user_error());
        assert!(
            error.to_string().contains(tool) || error.to_string().contains(version),
            "消息里要有那个不安全的名字：{error}"
        );
    }
    assert_eq!(
        activate(&store, "..", "1.0.0").expect_err("").kind(),
        "unsafe-name"
    );
    assert_eq!(
        uninstall(&store, "node", "CON", UninstallOptions::default())
            .expect_err("")
            .kind(),
        "unsafe-name"
    );
    assert_eq!(
        deactivate(&store, "a\\b").expect_err("").kind(),
        "unsafe-name"
    );

    // **内部产物位不能被创建成一个版本**（否则会出现一个列表里永远看不见的版本）。
    let error = adopt_payload(&store, "node", ".staging-node-1", &payload, &mut record)
        .expect_err("内部产物位不能当版本名");
    assert_eq!(error.kind(), "unsafe-name");
    assert!(error.to_string().contains("内部产物位"), "{error}");

    // 不带 `Result` 的函数：返回"没有"，**而且一个目录都不该被碰过**。
    assert!(installed_versions(&store, "..").is_empty());
    assert!(installed_versions(&store, "a/b").is_empty());
    assert!(find_version(&store, "node", "..").is_none());
    assert!(find_version(&store, "..", "1.0.0").is_none());
    assert_eq!(active_version(&store, "../.."), None);
    assert!(installed_tools(&store).is_empty());
    assert_eq!(installed_tools(&store), Vec::<String>::new());

    // 从头到尾，载荷与 store 根都没被动过。
    assert!(payload.join("marker.txt").is_file(), "载荷不能被动");
    assert_eq!(
        std::fs::read_dir(store.root())
            .expect("store 根还在")
            .count(),
        0,
        "不该有任何路径被拼出来过"
    );
}

#[test]
fn the_exported_comparator_is_the_one_the_listing_uses() {
    // `tuoen install`（不带版本号时取最新）与 `installed_versions` 必须**同一套顺序**，
    // 否则"列表里最上面那个"与"装到的是哪个"会不一致。
    let temp = TempDir::new("store-comparator");
    let store = store_in(&temp);
    for version in ["21.0.11+10", "21.0.12.1+1", "24.9.0", "24.19.0", "24.21.0"] {
        std::fs::create_dir_all(store.version_dir("temurin", version)).expect("建版本目录");
    }
    let listed: Vec<String> = installed_versions(&store, "temurin")
        .into_iter()
        .map(|version| version.version)
        .collect();

    let mut expected = listed.clone();
    expected.sort_by(|a, b| compare_versions(b, a));
    assert_eq!(
        listed, expected,
        "列表顺序必须就是 `compare_versions` 的顺序"
    );
    assert_eq!(
        listed.first().map(String::as_str),
        Some("24.21.0"),
        "最新在最上"
    );
    assert_eq!(listed.last().map(String::as_str), Some("21.0.11+10"));
}

// ---------------------------------------------------------------------------
// 记录
// ---------------------------------------------------------------------------

#[test]
fn write_record_writes_beside_the_version_and_refuses_a_foreign_schema() {
    let temp = TempDir::new("store-record");
    let store = store_in(&temp);
    std::fs::create_dir_all(store.version_dir("node", "1.0.0")).expect("建版本目录");

    let record = record_for("node", "1.0.0");
    write_record(&store, &record).expect("写记录");
    let path = store.record_path("node", "1.0.0");
    assert!(path.is_file(), "{path:?}");
    assert_eq!(read_back(&path), record);

    // 记录是**附注**：它让这个版本多出"从哪来的"，但不改变"它是不是一个版本"。
    let listed = installed_versions(&store, "node");
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].record, Some(record.clone()));

    // 拒绝写一份自己读不懂的记录 —— 而且**不踩坏**已经好的那一份。
    let mut foreign = record;
    foreign.schema_version = RECORD_SCHEMA_VERSION + 1;
    let error = write_record(&store, &foreign).expect_err("schema 不认识就不能写");
    assert_eq!(error.kind(), "record-broken");
    assert_eq!(
        read_back(&path),
        listed[0].record.clone().expect("原有记录"),
        "原有记录必须完好"
    );

    // 名字不安全时同样在开头被拒。
    let mut bad = record_for("..", "1.0.0");
    bad.schema_version = RECORD_SCHEMA_VERSION;
    assert_eq!(
        write_record(&store, &bad).expect_err("工具名不安全").kind(),
        "unsafe-name"
    );
}

// ---------------------------------------------------------------------------
// 默认位置
// ---------------------------------------------------------------------------

#[test]
fn the_default_location_is_not_any_temp_path() {
    // **绝不**在那个路径上创建任何东西：测试跑在开发者的真机上，
    // 往 `%LOCALAPPDATA%\tuoen\store` 写一个字节都是污染。
    let temp = TempDir::new("store-default");
    let store = store_in(&temp);
    let default = Store::at_default_location();

    let text = default.root().to_string_lossy().to_lowercase();
    assert!(text.contains("tuoen"), "默认位置里应当有 tuoen：{text}");
    assert_ne!(default.root(), temp.path(), "默认位置不能是测试临时目录");
    assert_ne!(default.root(), store.root());
    assert!(text.ends_with("store"), "{text}");
}
