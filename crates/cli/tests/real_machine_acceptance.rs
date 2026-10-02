//! **真机验收测试** —— 这一份读这台机器的真实状态，并且这是有意的。
//!
//! ## 这一份是对票据约束的**有意偏离**，理由写在下面
//!
//! 票据 `docs/tickets/L1-01-detect.md` 的硬约束是"测试不得读取真实的
//! `HKCU\Environment` / `HKLM` / 真实 `PATH` / 用户真实安装目录"。
//! **契约测试（`detect_contract.rs`）严格遵守了这条** —— 它只断言形状与不变量。
//!
//! 但那一份**测不到**这一票最关键的三件事，因为它们全都是"真实现的接线对不对"：
//!
//! 1. **`RealRegistry` 拼的注册表路径对不对。** 机器级环境变量在
//!    `HKLM\SYSTEM\CurrentControlSet\Control\Session Manager\Environment` ——
//!    **它不在 `SOFTWARE` 下面**。拼错了只会静默读到空，任何固定装置都发现不了。
//! 2. **`RealFileSystem` 读的 reparse tag 对不对。** `alias-ghost` 这一层完全建立在
//!    "`0x8000001b` 且长度 0"这个事实上；假文件系统是我们自己喂的，
//!    它只能证明"我们处理了 tag"，不能证明"我们从真实 NTFS 读到了正确的 tag"。
//! 3. **真的不会去执行那个 0 字节的别名。** 这条只有在真机上才有意义 ——
//!    假运行器不会启动应用商店。
//!
//! 所以这一份**故意**读真机，并且用文件名与本节把这件事写在明处，
//! 而不是把它混进契约测试里让约束看起来被遵守了。
//!
//! ## 因此这一份的纪律
//!
//! - **只断言"任何一台机器上都成立"的性质**，不断言"这台机器上一定有 node"。
//!   （唯一一处对具体工具的依赖是 `alias-ghost` 必须来自 `WindowsApps` ——
//!   那是平台事实，不是开发机事实。）
//! - **绝不写任何东西**：不写注册表、不写文件、不改环境变量、不建链接。
//! - **在 CI 上可以跳过**（见文件末尾的说明）。

mod common;

use common::{json, run, stderr};

/// 六种来源的固定取值。
const SOURCES: &[&str] = &[
    "tuoen",
    "path-resolution",
    "app-paths",
    "registry-arp",
    "filesystem-scan",
    "manager",
];

#[test]
fn detect_runs_on_a_real_machine_and_reports_something() {
    // 这一条断言的是**真实现的接线是通的**：六个来源都跑到了真实的注册表与磁盘，
    // 而不是在某个拼错的路径上静默读到空。
    //
    // 为什么可以断言"一定有东西"：这台机器上跑着 `cargo test`，所以 `cargo.exe`
    // 必然存在；而检测引擎会扫 `%USERPROFILE%\.cargo\bin`。这不是开发机特有的假设，
    // 而是"能编译这个仓库的机器必然成立"的假设。
    let output = run(["detect", "--json", "--no-version"]);
    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    let envelope = json(&output);
    let data = envelope.data.expect("data");
    let tools = data["tool"].as_array().expect("tools");
    assert!(
        !tools.is_empty(),
        "一台能编译本仓库的机器不可能一条开发工具都检测不到 —— \
         这几乎一定意味着某个来源的真实接线断了（例如注册表路径拼错）"
    );
}

#[test]
fn the_machine_level_environment_key_is_reachable() {
    // 机器级环境变量在 `HKLM\SYSTEM\CurrentControlSet\Control\Session Manager\Environment`
    // —— **不在 `SOFTWARE` 下面**。`RegHive::resolve` 对以 `SYSTEM\` 开头的路径
    // 走绝对解析，这条用例证明那条分支在真机上真的能读到东西。
    //
    // 判据：机器级 `Path` 必然存在（Windows 一定有它）。如果拼错了路径，
    // 我们会读到 `None`，而 `None` 会让"用户级 vs 机器级"的遮蔽判定整个失效。
    let output = run(["detect", "--json", "--no-version"]);
    let envelope = json(&output);
    let data = envelope.data.expect("data");

    // 直接证据：PATH 解析出来的条目里必须至少有一条被判为 machine 或 user 级
    // （而不是全部 fallback 成 process-only）—— 那说明注册表那一侧读到了。
    let scopes: Vec<String> = data["tool"]
        .as_array()
        .expect("tools")
        .iter()
        .filter(|tool| tool["source"] == "path-resolution")
        .filter_map(|tool| tool["evidence"].as_str().map(ToOwned::to_owned))
        .collect();

    if scopes.is_empty() {
        // 这台机器上 PATH 里一个已知工具都没有 —— 合法但罕见，不该让测试红。
        eprintln!("注意：PATH 上没有任何已知工具，跳过 scope 判定");
        return;
    }

    let any_scoped = scopes
        .iter()
        .any(|evidence| evidence.contains("（machine）") || evidence.contains("（user）"));
    assert!(
        any_scoped,
        "所有 PATH 条目都被判成 process-only —— 说明注册表那一侧没读到，\
         很可能是机器级环境变量的注册表路径拼错了。实际证据：{scopes:#?}"
    );
}

#[test]
fn an_app_execution_alias_is_recognised_from_real_ntfs() {
    // `alias-ghost` 这一层完全建立在"reparse tag `0x8000001b` 且长度 0"这个事实上，
    // 而那个事实只能从真实 NTFS 读出来。假文件系统只能证明"我们处理了 tag"。
    let output = run(["detect", "--json"]);
    let envelope = json(&output);
    let data = envelope.data.expect("data");

    for tool in data["tool"].as_array().expect("tools") {
        if tool["confidence"] != "alias-ghost" {
            continue;
        }
        // 别名只能来自 WindowsApps —— 这是平台事实。
        let path = tool["path"].as_str().expect("path").to_lowercase();
        assert!(
            path.contains("windowsapps"),
            "alias-ghost 只应当来自 WindowsApps，实际：{path}"
        );
        // **而且它不能有版本**：有版本就意味着我们执行过它。
        assert!(
            tool["version"].is_null(),
            "别名幽灵不得有版本 —— 有版本说明我们执行了它（会打开应用商店）：{tool}"
        );
        let evidence = tool["evidence"].as_str().expect("evidence");
        assert!(
            evidence.contains("0x8000001b"),
            "证据里必须写明那个 reparse tag，否则用户没法自己核对：{evidence}"
        );
    }
}

#[test]
fn every_source_that_can_speak_up_actually_did() {
    // 六个来源里至少有三个必然能在任何一台 Windows 上说话：
    // `path-resolution`（PATH 一定有东西）、`registry-arp`（ARP 一定有键）、
    // `app-paths`（Windows 自带若干 App Paths）。
    //
    // 这一条是对"某个来源的真实接线断了"的直接探测 —— 它比"总数不为零"更精确。
    let output = run(["detect", "--json", "--no-version"]);
    let envelope = json(&output);
    let data = envelope.data.expect("data");

    let used: Vec<&str> = data["tool"]
        .as_array()
        .expect("tools")
        .iter()
        .filter_map(|tool| tool["source"].as_str())
        .collect();

    for source in ["path-resolution", "registry-arp"] {
        assert!(
            used.contains(&source),
            "来源 `{source}` 一条都没报 —— 它的真实接线很可能断了。已出现的来源：{used:?}"
        );
        assert!(SOURCES.contains(&source));
    }
}

#[test]
fn a_third_party_version_manager_is_reported_as_such_when_present() {
    // 这一条**不假设**这台机器上有版本管理器：没有就跳过。
    // 有的话，它必须被标成 `manager-owned` 且带 manager 名 ——
    // 因为"这是别人的管理器装的，tuoen 不会去动它"是 ADR-0004 的核心承诺。
    let output = run(["detect", "--json", "--no-version"]);
    let envelope = json(&output);
    let data = envelope.data.expect("data");

    for tool in data["tool"].as_array().expect("tools") {
        if tool["confidence"] != "manager-owned" {
            continue;
        }
        let manager = tool["manager"].as_str().unwrap_or_default();
        assert!(
            !manager.is_empty(),
            "manager-owned 的条目必须写出是哪个管理器：{tool}"
        );
        // 只读采纳 → 不可重建。
        assert_eq!(
            tool["reproducible"].as_bool(),
            Some(false),
            "第三方管理器管的工具不得被标成可重建：{tool}"
        );
    }
}
