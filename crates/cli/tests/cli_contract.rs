//! `tuoen list` 的**进程边界**契约测试。
//!
//! 这一票（ticket #2）定型全仓库的测试形状：跑真实二进制，断言 stdout / stderr / 退出码。
//! 后续票新增子命令时应当复制这个结构，而不是另发明一套。

mod common;

use common::{IsolatedHome, json, run, stderr, stdout};

/// **票据 #6 起，`list` 读的是真实存储**，所以这一族测试必须隔离
/// `%LOCALAPPDATA%` —— 否则"在这台开发机上跑过一次真机验收"之后，
/// `list --json` 就不再是空的，而这些断言会开始莫名其妙地失败。
/// 反过来也一样：不隔离的话测试会去读（并让人误以为依赖）开发者真实的存储。
fn empty_store() -> IsolatedHome {
    IsolatedHome::new("list-empty")
}

#[test]
fn list_json_output_is_parseable_and_has_the_contract_shape() {
    let home = empty_store();
    let output = home.run(["list", "--json"]);

    assert!(
        output.status.success(),
        "list --json 应当成功，stderr：{}",
        stderr(&output)
    );

    let envelope = json(&output);
    assert_eq!(envelope.schema_version, 2, "schema 版本是破坏性变更的哨兵");
    assert_eq!(envelope.command, "list");
    assert!(envelope.ok, "成功时 ok 必须为 true");
    assert!(
        envelope.error.is_none(),
        "成功时不应有 error 键，实际：{:?}",
        envelope.error
    );

    let data = envelope.data.expect("成功时必须有 data");
    let tools = data
        .get("tools")
        .and_then(serde_json::Value::as_array)
        .expect("data.tools 必须是数组");
    assert!(tools.is_empty(), "空存储上 list 应当是空的");
}

#[test]
fn list_json_is_byte_stable() {
    // 输出稳定性是决策 35：`--json` 必须稳定且不本地化。
    // 逐字节比较能抓到"某次重构顺手加了字段"这类漂移。
    let home = empty_store();
    let first = stdout(&home.run(["list", "--json"]));
    let second = stdout(&home.run(["list", "--json"]));
    assert_eq!(first, second, "--json 输出必须逐字节稳定");

    let expected = r#"{"schemaVersion":2,"command":"list","ok":true,"data":{"tools":[]}}"#;
    assert_eq!(first.trim(), expected);
}

#[test]
fn list_json_contains_no_localised_text() {
    // 中文优先针对的是**人类输出**。JSON 里出现中文意味着界面语言把脚本绑死了。
    let text = stdout(&empty_store().run(["list", "--json"]));
    let has_cjk = text.chars().any(|c| {
        let cp = u32::from(c);
        (0x4E00..=0x9FFF).contains(&cp) || (0x3000..=0x303F).contains(&cp)
    });
    assert!(!has_cjk, "--json 输出里不得出现 CJK：{text}");
}

#[test]
fn list_human_output_is_chinese_by_default() {
    let output = empty_store().run(["list"]);
    assert!(output.status.success(), "stderr：{}", stderr(&output));

    let text = stdout(&output);
    assert!(
        text.contains("还没有管理任何工具"),
        "默认输出应当是中文，实际：{text}"
    );
    // 空列表必须给出下一步，否则用户会怀疑工具坏了。
    assert!(
        text.contains("tuoen install"),
        "空列表应当提示 install：{text}"
    );
    assert!(
        text.contains("tuoen detect"),
        "空列表应当说清 list 与 detect 的分工：{text}"
    );
}

#[test]
fn human_output_is_not_json() {
    // 反向断言：不加 --json 时不得输出 JSON。
    let text = stdout(&empty_store().run(["list"]));
    assert!(
        serde_json::from_str::<serde_json::Value>(text.trim()).is_err(),
        "人类输出不应是 JSON：{text}"
    );
}

#[test]
fn exit_code_is_zero_on_success() {
    let home = empty_store();
    assert_eq!(home.run(["list"]).status.code(), Some(0));
    assert_eq!(home.run(["list", "--json"]).status.code(), Some(0));
}

#[test]
fn unknown_subcommand_exits_with_usage_error() {
    let output = run(["definitely-not-a-command"]);
    assert_eq!(
        output.status.code(),
        Some(2),
        "用法错误必须是退出码 2（脚本依赖它）"
    );
}

#[test]
fn missing_subcommand_exits_with_usage_error() {
    let output = run(Vec::<String>::new());
    assert_eq!(output.status.code(), Some(2));
}

#[test]
fn help_is_chinese_and_names_the_project() {
    let text = stdout(&run(["--help"]));
    assert!(
        text.contains("拓境"),
        "帮助文本应当是中文且含项目名：{text}"
    );
    assert!(text.contains("tuoen"), "帮助文本应当含二进制名：{text}");
}

#[test]
fn version_flag_prints_a_version() {
    let output = run(["--version"]);
    assert!(output.status.success());
    let text = stdout(&output);
    assert!(text.contains("0.1.0"), "版本输出：{text}");
}
