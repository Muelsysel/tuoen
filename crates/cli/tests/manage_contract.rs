//! `tuoen install` / `use` / `uninstall` / `list` 的**进程边界**契约测试。
//!
//! ## 这一票为什么必须走进程边界
//!
//! 票据 #6 把三条链路接了起来：目录 → 下载 → 安全解压 → 存储 → junction。
//! 每一段都有自己的单元测试（`tuoen-{manifest,download,archive,store}`），
//! 但**只有跑真实二进制才能回答票据真正问的问题**：
//! "用户敲 `tuoen install node@24.19.0` 会发生什么"。参数解析、门禁顺序、
//! 退出码、`--json` 形状、以及**磁盘上到底留下了什么**，全都只有在这一层才看得见。
//!
//! ## 硬性约束：**绝不碰真实的 `%LOCALAPPDATA%`**
//!
//! 每个用例都用 [`IsolatedHome`]，它把 `LOCALAPPDATA` / `APPDATA` 指向临时目录。
//! 这不是洁癖：`tuoen install` 会真的下载 35 MB 并写进存储，
//! 不隔离就意味着 `cargo test` 在往开发者真实的机器上装东西。
//!
//! ## 这一族**不做网络**
//!
//! 所有用例要么走 `--dry-run`（只读目录），要么在隔离存储里**直接摆好**版本目录
//! （用 `tuoen_store` 的 API，它就是被测存储的写方）。真正的端到端下载在
//! `scripts/acceptance-L0-06.ps1` 里做 —— 那是**真机验收**，不是回归测试。
//! 把网络放进 `cargo test` 会让测试在离线时变红，而"离线"不是 bug。

mod common;

use common::{IsolatedHome, json, stderr, stdout};
use tuoen_store::Store;

/// 在一个隔离的存储里摆好一个版本目录（`versions/<version>/`）。
///
/// 只摆目录、不写记录：**目录是事实来源**（决策 48），
/// 所以这样造出来的版本在 `list` 里就应该看得见。
fn put_version(home: &IsolatedHome, tool: &str, version: &str) {
    let store = Store::new(home.store_root());
    std::fs::create_dir_all(store.version_dir(tool, version)).expect("造版本目录");
    std::fs::write(
        store.version_dir(tool, version).join("marker.txt"),
        "installed by the test",
    )
    .expect("写 marker");
}

// ─────────────────────────────────────────────────────────────────────────────
// install：拒绝的路径（不联网）
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn installing_an_unknown_tool_names_the_known_ones() {
    let home = IsolatedHome::new("install-unknown");
    let output = home.run(["install", "definitely-not-a-tool", "--json"]);
    assert_eq!(output.status.code(), Some(1), "拒绝必须是退出码 1");

    let envelope = json(&output);
    assert!(!envelope.ok);
    let error = envelope.error.expect("失败必须有 error");
    assert_eq!(error.code, "unknown-tool", "错误码是稳定契约");
    // 报错必须给出**下一步**：列出我们认识哪些工具。
    assert!(
        error.message.contains("node"),
        "应当列出已知工具：{}",
        error.message
    );
    assert!(
        error.message.contains("temurin"),
        "应当列出已知工具：{}",
        error.message
    );
}

#[test]
fn installing_with_a_trailing_at_is_not_the_same_as_omitting_the_version() {
    // `node@` 是"你写了 @ 但忘了版本"，`node` 是"给我最新的"。
    // 混成一种会让一次手误静默装上一个最新版。
    let home = IsolatedHome::new("install-empty-version");
    let output = home.run(["install", "node@", "--json"]);
    assert_eq!(output.status.code(), Some(1));

    let error = json(&output).error.expect("失败必须有 error");
    assert_eq!(error.code, "empty-version");
    assert!(
        error.message.contains("node"),
        "要说清是哪个工具：{}",
        error.message
    );
}

#[test]
fn installing_with_a_leading_at_is_its_own_error() {
    let home = IsolatedHome::new("install-empty-tool");
    let output = home.run(["install", "@24.19.0", "--json"]);
    assert_eq!(output.status.code(), Some(1));
    let error = json(&output).error.expect("失败必须有 error");
    assert_eq!(error.code, "empty-tool");
}

#[test]
fn a_prohibited_tool_is_refused_before_anything_else_happens() {
    // 门禁必须在**解析 recipe 之前**跑。Oracle JDK 是种子里故意留的反例：
    // 它没有任何 recipe，所以顺序错了就会报成"没有适用的 recipe" ——
    // 那是把"我们不许可你装"说成了"我们不知道去哪装"。
    let home = IsolatedHome::new("install-prohibited");
    let output = home.run(["install", "oracle-jdk@8", "--json"]);
    assert_eq!(output.status.code(), Some(1), "许可证拒绝是失败");

    let error = json(&output).error.expect("失败必须有 error");
    assert_eq!(
        error.code, "licence-prohibited",
        "必须是许可证拒绝，不是'没有 recipe'：{}",
        error.message
    );
    // 拒绝必须给出**理由**，否则用户只会觉得工具坏了。
    assert!(
        error.message.contains("Oracle") || error.message.contains("再分发"),
        "要给出具体原因：{}",
        error.message
    );
    // 而且**绝不能**顺手给出下载地址（拒绝却给地址是自相矛盾的）。
    assert!(
        !error.message.contains("http"),
        "拒绝时不该出现任何 URL：{}",
        error.message
    );
}

#[test]
fn a_prohibited_tool_is_refused_even_when_installing_by_bare_name() {
    // 不带版本号时会去挑"最新的可安装版本"。没有可安装版本时
    // 必须报**许可证**那条，而不是"没有可安装的版本" ——
    // 后者听起来像是我们去上游找了一圈没找到。
    let home = IsolatedHome::new("install-prohibited-bare");
    let output = home.run(["install", "oracle-jdk", "--json"]);
    assert_eq!(output.status.code(), Some(1));
    let error = json(&output).error.expect("失败必须有 error");
    assert_eq!(error.code, "licence-prohibited", "{}", error.message);
}

#[test]
fn an_unresolvable_version_says_which_platform_and_what_exists() {
    let home = IsolatedHome::new("install-unresolved");
    let output = home.run(["install", "node@0.0.1-nonexistent", "--json"]);
    assert_eq!(output.status.code(), Some(1));

    let error = json(&output).error.expect("失败必须有 error");
    assert_eq!(error.code, "unresolved");
    assert!(
        error.message.contains("windows-x64"),
        "要说清是在哪个平台上找的：{}",
        error.message
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// install --dry-run：什么都写不了
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn a_dry_run_does_not_touch_the_disk() {
    // **这是 `--dry-run` 唯一的验收判据**：磁盘上什么都不该多出来。
    // 断言"目录不存在"而不是"目录是空的"—— 前者更强。
    let home = IsolatedHome::new("install-dry-run");
    let output = home.run(["install", "node@24.19.0", "--dry-run", "--json"]);
    assert!(
        output.status.success(),
        "dry-run 应当成功，stderr：{}",
        stderr(&output)
    );

    assert!(
        !home.store_root().exists(),
        "`--dry-run` 之后存储根都不该存在，实际存在：{}",
        home.store_root().display()
    );

    let envelope = json(&output);
    assert_eq!(envelope.command, "install");
    let data = envelope.data.expect("成功必须有 data");
    assert_eq!(data["dryRun"], serde_json::json!(true));
    assert_eq!(
        data["result"],
        serde_json::Value::Null,
        "dry-run 什么都没做，result 必须是 null"
    );
}

#[test]
fn a_dry_run_still_answers_where_it_would_go_and_what_it_would_fetch() {
    // dry-run 最有用的东西不是"我不会动你的机器"（那是承诺），
    // 而是**具体的数字**：从哪个 URL、什么哈希、装到哪个路径。
    let home = IsolatedHome::new("install-dry-run-plan");
    let data = json(&home.run(["install", "node@24.19.0", "--dry-run", "--json"]))
        .data
        .expect("成功必须有 data");
    let plan = &data["plan"];

    assert_eq!(plan["tool"], serde_json::json!("node"));
    assert_eq!(plan["version"], serde_json::json!("24.19.0"));
    assert_eq!(plan["redistribution"], serde_json::json!("allowed"));
    assert_eq!(
        plan["sha256"],
        serde_json::json!("57f71ab3652e797d84acddc79c81cc9ff1c6ddb2a1974cdb83f00fee9bff4c73"),
        "哈希必须来自内置目录（那是信任锚），且要如实显示"
    );
    assert!(
        plan["url"]
            .as_str()
            .expect("url 是字符串")
            .starts_with("https://nodejs.org/dist/v24.19.0/"),
        "URL 里的 {{version}} 必须被渲染：{}",
        plan["url"]
    );
    // 路径要落在**隔离的**存储里 —— 顺带证明 `--dry-run` 没有偷看真实位置。
    let installed_to = plan["installedTo"].as_str().expect("installedTo 是字符串");
    assert!(
        installed_to.starts_with(&home.store_root().display().to_string()),
        "装到的路径必须在这个隔离存储里：{installed_to}"
    );
    assert_eq!(
        plan["layout"]["stripComponents"],
        serde_json::json!(1),
        "Node 的归档套着一层 node-v24.19.0-win-x64/"
    );
    assert_eq!(plan["layout"]["bin"]["node"], serde_json::json!("node.exe"));
    assert_eq!(
        plan["nextCommand"],
        serde_json::json!("tuoen use node 24.19.0"),
        "装完不等于生效，所以必须把下一步命令原样给出（决策 49）"
    );
}

#[test]
fn install_use_swallows_the_next_command_because_there_is_nothing_left_to_type() {
    let home = IsolatedHome::new("install-dry-run-use");
    let data = json(&home.run(["install", "node@24.19.0", "--dry-run", "--use", "--json"]))
        .data
        .expect("成功必须有 data");
    assert_eq!(data["plan"]["activateAfter"], serde_json::json!(true));
    assert_eq!(
        data["plan"]["nextCommand"],
        serde_json::Value::Null,
        "已经会激活了就不该再让用户敲一次 use"
    );
}

#[test]
fn a_dry_run_without_a_version_picks_the_newest_installable_one() {
    // 不带版本号时取的是**自然排序**下最新的可安装版本。
    // 种子目录里 node 有 24.21.0 / 24.20.0 / 24.19.0，所以答案必须是 24.21.0。
    let home = IsolatedHome::new("install-newest");
    let data = json(&home.run(["install", "node", "--dry-run", "--json"]))
        .data
        .expect("成功必须有 data");
    assert_eq!(
        data["plan"]["version"],
        serde_json::json!("24.21.0"),
        "不带版本号必须给最新可安装版本"
    );
}

#[test]
fn the_at_split_is_visible_from_the_outside_too() {
    // 别名的解析要归一化到目录里的 id —— 否则存储里会出现 `nodejs/` 与 `node/`
    // 两个目录，而它们是同一个工具。
    let home = IsolatedHome::new("install-alias");
    let data = json(&home.run(["install", "nodejs@24.19.0", "--dry-run", "--json"]))
        .data
        .expect("成功必须有 data");
    assert_eq!(
        data["plan"]["tool"],
        serde_json::json!("node"),
        "别名必须归一化成 id"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// use / uninstall：在隔离存储里摆好版本，然后真的翻转
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn use_refuses_a_version_that_was_never_installed_and_says_what_to_do() {
    let home = IsolatedHome::new("use-not-installed");
    let output = home.run(["use", "node", "24.19.0", "--json"]);
    assert_eq!(output.status.code(), Some(1));

    let error = json(&output).error.expect("失败必须有 error");
    assert_eq!(error.code, "not-installed");
    assert!(
        error.message.contains("tuoen install"),
        "没装过时要给出装它的命令：{}",
        error.message
    );
}

#[test]
fn use_lists_the_versions_that_do_exist_when_the_asked_one_does_not() {
    let home = IsolatedHome::new("use-wrong-version");
    put_version(&home, "node", "24.19.0");

    let output = home.run(["use", "node", "24.21.0", "--json"]);
    assert_eq!(output.status.code(), Some(1));
    let error = json(&output).error.expect("失败必须有 error");
    assert_eq!(error.code, "not-installed");
    assert!(
        error.message.contains("24.19.0"),
        "要列出真正装过的版本：{}",
        error.message
    );
}

#[test]
fn the_first_activation_creates_the_link_and_the_second_repoints_it_in_place() {
    // **这是票据 #6 最核心的一条断言。**
    //
    // `created` → `replaced` 的转变就是"原子翻转"的证据：第二次不是
    // "删掉再建"（那会有空窗），而是用 `FSCTL_SET_REPARSE_POINT`
    // 就地替换重解析数据 —— 一次 IOCTL（决策 46）。
    // 单元测试在 `tuoen-platform` 里已经钉过一次，这里钉的是**用户能观察到的形态**。
    let home = IsolatedHome::new("use-flip");
    put_version(&home, "node", "24.19.0");
    put_version(&home, "node", "24.21.0");

    let first = json(&home.run(["use", "node", "24.19.0", "--json"]));
    assert!(first.ok, "第一次激活应当成功：{:?}", first.error);
    let data = first.data.expect("成功必须有 data");
    assert_eq!(data["repoint"]["outcome"], serde_json::json!("created"));
    assert_eq!(
        data["previous"],
        serde_json::Value::Null,
        "此前没有生效版本"
    );

    let second = json(&home.run(["use", "node", "24.21.0", "--json"]));
    assert!(second.ok, "重指应当成功：{:?}", second.error);
    let data = second.data.expect("成功必须有 data");
    assert_eq!(
        data["repoint"]["outcome"],
        serde_json::json!("replaced"),
        "第二次必须是替换而不是重建 —— 这是原子性的证据"
    );
    assert_eq!(
        data["previous"],
        serde_json::json!("24.19.0"),
        "要说清从哪个版本切过来"
    );
    assert_eq!(data["repoint"]["degradedReason"], serde_json::Value::Null);
}

#[test]
fn the_flip_is_visible_on_disk_as_a_junction_that_resolves() {
    // `--json` 说的和磁盘上真实存在的东西必须一致。
    let home = IsolatedHome::new("use-junction");
    put_version(&home, "node", "24.19.0");
    home.run(["use", "node", "24.19.0", "--json"]);

    let store = Store::new(home.store_root());
    let current = store.current_link("node");
    assert!(current.exists(), "{} 应当存在", current.display());

    // 通过链接读到的东西必须来自目标版本目录 —— 这是"链接真的通了"的判据，
    // 而不是"存在一个同名目录"。
    let marker = std::fs::read_to_string(current.join("marker.txt")).expect("通过链接读 marker");
    assert_eq!(marker, "installed by the test");

    // 而且它真的是一个重解析点，不是被复制出来的普通目录。
    // `junction_target` 返回 `Result<Option<PathBuf>, _>`：外层是"能不能读"，
    // 内层是"它到底是不是一个链接"。两层都要拆 —— 把 `None` 当成"读失败"
    // 会让一条普通目录冒充链接而不被发现。
    let target = tuoen_platform::junction_target(&current)
        .expect("读链接目标不该失败")
        .expect("`current` 必须是一个链接（junction），而不是被复制出来的普通目录");
    assert!(
        target.ends_with("24.19.0"),
        "链接应当指向 24.19.0，实际：{}",
        target.display()
    );
}

#[test]
fn list_marks_the_active_version_and_shows_the_others() {
    let home = IsolatedHome::new("list-versions");
    put_version(&home, "node", "24.19.0");
    put_version(&home, "node", "24.21.0");
    home.run(["use", "node", "24.19.0", "--json"]);

    let data = json(&home.run(["list", "--json"]))
        .data
        .expect("成功必须有 data");
    let tools = data["tools"].as_array().expect("tools 是数组");
    assert_eq!(tools.len(), 1, "一个工具一行（决策 37）");
    assert_eq!(tools[0]["name"], serde_json::json!("node"));
    assert_eq!(tools[0]["version"], serde_json::json!("24.19.0"));
    assert_eq!(tools[0]["source"], serde_json::json!("tuoen"));
    assert_eq!(
        tools[0]["installedVersions"],
        serde_json::json!(["24.21.0", "24.19.0"]),
        "全部版本必须降序列出（自然比较，不是字符串比较）"
    );

    // 人类输出里生效的那个带 `*` —— 一眼看出切过去的是哪个。
    let text = stdout(&home.run(["list"]));
    assert!(text.contains("24.19.0*"), "生效版本要带星号：{text}");
    assert!(text.contains("24.21.0"), "其它版本也要列出：{text}");
}

#[test]
fn uninstalling_the_active_version_is_refused_without_force_and_changes_nothing() {
    let home = IsolatedHome::new("uninstall-active-refused");
    put_version(&home, "node", "24.19.0");
    home.run(["use", "node", "24.19.0", "--json"]);
    let store = Store::new(home.store_root());

    let output = home.run(["uninstall", "node", "24.19.0", "--json"]);
    assert_eq!(output.status.code(), Some(1), "默认必须拒绝");
    let error = json(&output).error.expect("失败必须有 error");
    assert_eq!(error.code, "active-version");
    // 拒绝必须给出两条出路。
    assert!(
        error.message.contains("tuoen use"),
        "要说清先切到别的版本：{}",
        error.message
    );
    assert!(
        error.message.contains("--force"),
        "要给出强制删的开关：{}",
        error.message
    );

    // **拒绝必须是完全没有副作用的**：载荷还在，链接还在，还能通过链接读到东西。
    assert!(
        store.version_dir("node", "24.19.0").exists(),
        "载荷必须还在"
    );
    assert!(store.current_link("node").exists(), "链接必须还在");
    assert_eq!(
        std::fs::read_to_string(store.current_link("node").join("marker.txt")).expect("读 marker"),
        "installed by the test"
    );
}

#[test]
fn force_uninstalling_the_active_version_takes_the_link_down_first() {
    let home = IsolatedHome::new("uninstall-active-forced");
    put_version(&home, "node", "24.19.0");
    put_version(&home, "node", "24.21.0");
    home.run(["use", "node", "24.19.0", "--json"]);
    let store = Store::new(home.store_root());

    let output = home.run(["uninstall", "node", "24.19.0", "--force", "--json"]);
    assert!(
        output.status.success(),
        "带 --force 应当成功，stderr：{}",
        stderr(&output)
    );
    let data = json(&output).data.expect("成功必须有 data");
    assert_eq!(data["tool"], serde_json::json!("node"));
    assert_eq!(data["version"], serde_json::json!("24.19.0"));
    assert_eq!(data["wasActive"], serde_json::json!(true));

    // 载荷没了、链接没了，而**另一个版本一动没动**。
    assert!(
        !store.version_dir("node", "24.19.0").exists(),
        "载荷应当没了"
    );
    assert!(
        !store.current_link("node").exists(),
        "`current` 必须已经摘掉 —— 指向一个不存在的目录会让之后每个 shim 报假错误"
    );
    assert!(
        store.version_dir("node", "24.21.0").exists(),
        "别的版本不该受影响"
    );

    // 列表里剩下的那个**没有生效版本** —— 而不是悄悄把 24.21.0 报成生效的。
    let data = json(&home.run(["list", "--json"]))
        .data
        .expect("成功必须有 data");
    assert_eq!(data["tools"][0]["version"], serde_json::Value::Null);
    assert_eq!(
        data["tools"][0]["installedVersions"],
        serde_json::json!(["24.21.0"])
    );
}

#[test]
fn uninstalling_something_that_was_never_installed_is_its_own_error() {
    let home = IsolatedHome::new("uninstall-not-installed");
    let output = home.run(["uninstall", "node", "24.19.0", "--json"]);
    assert_eq!(output.status.code(), Some(1));
    let error = json(&output).error.expect("失败必须有 error");
    assert_eq!(error.code, "not-installed");
}

#[test]
fn a_fresh_machine_can_list_use_and_uninstall_without_panicking() {
    // 换一台全新机器时的第一个问题就是"这里什么都没有，工具会崩吗"。
    // 三条命令都必须给出**能用中文读懂**的拒绝，而不是 panic 或空输出。
    let home = IsolatedHome::new("fresh-machine");
    for args in [
        vec!["list"],
        vec!["use", "node", "24.19.0"],
        vec!["uninstall", "node", "24.19.0"],
    ] {
        let output = home.run(&args);
        let text = format!("{}{}", stdout(&output), stderr(&output));
        assert!(!text.is_empty(), "{args:?} 不该没有任何输出");
        assert!(!text.contains("panicked"), "{args:?} 不该 panic：{text}");
        assert!(
            text.chars()
                .any(|c| (0x4E00..=0x9FFF).contains(&(u32::from(c)))),
            "{args:?} 的人类输出应当是中文：{text}"
        );
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// `--json` 的两条契约：成功载荷不本地化，错误**码**不本地化
// ─────────────────────────────────────────────────────────────────────────────

fn has_cjk(text: &str) -> bool {
    text.chars()
        .any(|c| (0x4E00..=0x9FFF).contains(&(u32::from(c))))
}

#[test]
fn success_payloads_contain_no_localised_text() {
    // 中文优先针对的是**人类输出**。JSON 里出现中文意味着界面语言把脚本绑死了（决策 35）。
    let home = IsolatedHome::new("json-no-cjk-ok");
    put_version(&home, "node", "24.19.0");

    for args in [
        vec!["list", "--json"],
        vec!["install", "node@24.19.0", "--dry-run", "--json"],
        vec!["install", "node@24.19.0", "--dry-run", "--use", "--json"],
        vec!["use", "node", "24.19.0", "--json"],
        vec!["uninstall", "node", "24.19.0", "--force", "--json"],
    ] {
        let text = stdout(&home.run(&args));
        assert!(!has_cjk(&text), "{args:?} 的成功载荷里不该有 CJK：{text}");
    }
}

#[test]
fn error_codes_are_ascii_while_error_messages_may_be_chinese() {
    // **这是一条刻意的分工，不是遗漏。** 错误码是机器读的（脚本、未来的 GUI），
    // 所以它永远是小写 ASCII；中文消息是给人读的，所以它可以随时改。
    // 把两者混起来的后果是：改一句中文提示就破坏了脚本。
    let home = IsolatedHome::new("json-codes");
    let cases: Vec<(Vec<&str>, &str)> = vec![
        (vec!["install", "nope", "--json"], "unknown-tool"),
        (vec!["install", "node@", "--json"], "empty-version"),
        (
            vec!["install", "oracle-jdk@8", "--json"],
            "licence-prohibited",
        ),
        (vec!["use", "node", "24.19.0", "--json"], "not-installed"),
        (
            vec!["uninstall", "node", "24.19.0", "--json"],
            "not-installed",
        ),
    ];

    for (args, expected) in cases {
        let envelope = json(&home.run(&args));
        assert!(!envelope.ok, "{args:?} 应当是失败信封");
        let error = envelope.error.expect("失败必须有 error");
        assert_eq!(error.code, expected, "{args:?}");
        assert!(
            error
                .code
                .chars()
                .all(|c| c.is_ascii_lowercase() || c == '-' || c.is_ascii_digit()),
            "错误码必须是小写 kebab ASCII：{}",
            error.code
        );
        assert!(!error.message.is_empty(), "{args:?} 的错误消息不该为空");
    }
}

#[test]
fn the_manage_family_is_byte_stable_too() {
    // 稳定性不只对 `list` 成立。
    //
    // **`use` 要跑第三次才开始比**：第一次是 `created`、之后是 `replaced`，
    // 前两次的输出本来就**应该**不同（那正是"创建"与"就地重指"的区别）。
    // 把它们拿来比稳定性是比错了东西 —— 这也算一条真实的教训：
    // 一条"逐字节稳定"的断言必须用**同一个状态下的两次调用**。
    let home = IsolatedHome::new("json-stable");
    let dry_run = vec!["install", "node@24.19.0", "--dry-run", "--json"];
    assert_eq!(
        stdout(&home.run(&dry_run)),
        stdout(&home.run(&dry_run)),
        "{dry_run:?} 的 --json 输出必须逐字节稳定"
    );

    put_version(&home, "node", "24.19.0");
    let use_args = vec!["use", "node", "24.19.0", "--json"];
    let first = stdout(&home.run(&use_args));
    assert!(first.contains(r#""outcome":"created""#), "{first}");
    let second = stdout(&home.run(&use_args));
    let third = stdout(&home.run(&use_args));
    assert!(second.contains(r#""outcome":"replaced""#), "{second}");
    assert_eq!(second, third, "同一个状态下的两次 use 必须逐字节一致");
    assert_ne!(
        first, second,
        "第一次是 created、之后是 replaced —— 这个差别本身是被测的性质（决策 46）"
    );
}
