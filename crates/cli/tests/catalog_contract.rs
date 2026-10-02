//! `tuoen catalog` 的**进程边界**契约测试（ticket #3）。
//!
//! 这一票要能回答三个问题：认识哪些工具、每个工具的许可证是什么、某个版本能不能装。
//! 三个问题各有一族断言，另外还钉住**门禁拒绝与解析失败是两个不同的结果**。

mod common;

use common::{json, run, stderr, stdout};

#[test]
fn catalog_list_human_output_shows_the_licence_verdict_for_every_tool() {
    let output = run(["catalog", "list"]);
    assert!(output.status.success(), "stderr：{}", stderr(&output));

    let text = stdout(&output);
    for id in ["node", "temurin", "oracle-jdk", "msvc"] {
        assert!(text.contains(id), "缺少工具 {id}：{text}");
    }
    assert!(text.contains("allowed"), "{text}");
    assert!(text.contains("prohibited"), "{text}");
    // 不可再分发的条目必须在人类输出里被点名，而不是埋在 JSON 里。
    assert!(text.contains("不可再分发"), "{text}");
}

#[test]
fn catalog_list_json_is_stable_and_not_localised_in_its_enum_values() {
    let first = stdout(&run(["catalog", "list", "--json"]));
    let second = stdout(&run(["catalog", "list", "--json"]));
    assert_eq!(first, second, "catalog list --json 必须逐字节稳定");

    let envelope = json(&run(["catalog", "list", "--json"]));
    assert_eq!(envelope.command, "catalog.list");
    assert!(envelope.ok);

    let tools = envelope
        .data
        .as_ref()
        .and_then(|d| d.get("tool"))
        .and_then(serde_json::Value::as_array)
        .expect("data.tool");
    assert!(tools.len() >= 4);

    // 枚举取值必须是小写 kebab，不得本地化。
    let verdicts: Vec<&str> = tools
        .iter()
        .filter_map(|t| {
            t.pointer("/licence/redistribution")
                .and_then(|v| v.as_str())
        })
        .collect();
    assert!(verdicts.contains(&"allowed"), "{verdicts:?}");
    assert!(verdicts.contains(&"prohibited"), "{verdicts:?}");
    for v in verdicts {
        assert!(
            v.chars().all(|c| c.is_ascii_lowercase() || c == '-'),
            "取值 `{v}` 不是小写 kebab"
        );
    }
}

#[test]
fn catalog_check_allows_a_permitted_version_and_gives_url_and_hash() {
    let output = run(["catalog", "check", "node", "24"]);
    assert!(output.status.success(), "stderr：{}", stderr(&output));

    let text = stdout(&output);
    assert!(text.contains("可以安装"), "{text}");
    assert!(text.contains("nodejs.org"), "必须给出下载地址：{text}");
    // 哈希必须完整出现 —— 它是供应链保证，不能截断显示。
    assert!(
        text.contains("158f7685b44de51f6c0df1d153526cbcd3e1bc739a8dfc607721cef75de9e541"),
        "{text}"
    );
}

#[test]
fn catalog_check_rejects_a_prohibited_version_with_a_specific_reason() {
    // **ticket #3 的核心验收。**
    let output = run(["catalog", "check", "oracle-jdk", "8"]);
    // 门禁拒绝是**成功的回答**（我们确实回答了"能不能装"），所以退出码是 0。
    assert!(
        output.status.success(),
        "门禁拒绝应当是正常回答，stderr：{}",
        stderr(&output)
    );

    let text = stdout(&output);
    assert!(text.contains("不能安装"), "{text}");
    assert!(text.contains("原因："), "{text}");
    // 原因必须具体：带许可证名与可操作建议。
    assert!(text.contains("Oracle Binary Code License"), "{text}");
    assert!(text.contains("自行获取"), "{text}");
    // 不得是通用错误。
    assert!(!text.contains("许可问题"), "{text}");
    // 拒绝安装却顺手给出下载地址是自相矛盾的。
    assert!(!text.contains("下载地址"), "{text}");
    assert!(
        !text.contains("oracle.com/java/technologies/javase/javase8-archive"),
        "{text}"
    );
}

#[test]
fn catalog_check_rejects_msvc_as_prohibited_not_as_missing_recipe() {
    // MSVC 是**带外管理员前置条件**，不是包 —— 它没有 recipe 是正确的。
    // 但如果先解析 recipe 再判许可，就会报"没有适用的 recipe"，
    // 那是把"我们不许可你装"说成了"我们不知道去哪装"，是完全错误的回答。
    let output = run(["catalog", "check", "msvc", "1"]);
    assert!(output.status.success(), "stderr：{}", stderr(&output));

    let text = stdout(&output);
    assert!(text.contains("不能安装"), "{text}");
    assert!(text.contains("不可再分发"), "{text}");
    assert!(
        !text.contains("没有适用"),
        "不得把许可证拒绝报成缺 recipe：{text}"
    );
}

#[test]
fn catalog_check_json_reports_installable_false_with_the_reason() {
    let envelope = json(&run(["catalog", "check", "oracle-jdk", "8", "--json"]));
    assert_eq!(envelope.command, "catalog.check");
    assert!(envelope.ok);

    let data = envelope.data.expect("data");
    assert_eq!(
        data.get("installable"),
        Some(&serde_json::Value::Bool(false))
    );
    assert_eq!(
        data.pointer("/licence/redistribution")
            .and_then(|v| v.as_str()),
        Some("prohibited")
    );
    let reason = data.get("reason").and_then(|v| v.as_str()).expect("reason");
    assert!(reason.contains("不可再分发"), "{reason}");
    // 材料绝不出现：拒绝信息里不该有凭据或密钥。
    assert!(!reason.contains("glpat-"), "{reason}");
}

#[test]
fn catalog_check_reports_unknown_tool_with_the_known_list() {
    let output = run(["catalog", "check", "nonexistent", "1"]);
    assert_eq!(output.status.code(), Some(1), "工具不存在是运行期错误");

    let text = stderr(&output);
    assert!(text.contains("nonexistent"), "{text}");
    // 列出已知工具，用户就不用去猜。
    assert!(text.contains("node"), "{text}");
    assert!(text.contains("temurin"), "{text}");
}

#[test]
fn catalog_check_reports_unknown_version_with_the_known_versions() {
    let output = run(["catalog", "check", "node", "99"]);
    assert_eq!(output.status.code(), Some(1));

    let text = stderr(&output);
    assert!(text.contains("99"), "{text}");
    assert!(text.contains("24.21.0"), "应当列出已知版本：{text}");
}

#[test]
fn catalog_check_accepts_an_alias() {
    // `jdk` 是 temurin 的别名。
    let by_id = stdout(&run([
        "catalog",
        "check",
        "temurin",
        "21.0.11+10",
        "--json",
    ]));
    let by_alias = stdout(&run(["catalog", "check", "jdk", "21.0.11+10", "--json"]));
    assert_eq!(by_id, by_alias, "别名必须解析到同一个工具");
}

#[test]
fn catalog_check_temurin_url_encodes_the_plus_sign() {
    // 上游的实际形状：release_name 是 `21.0.12.1+1`，
    // 下载路径里是 `%2B`，文件名里是 `_`。这两处不一致是真实存在的。
    let text = stdout(&run(["catalog", "check", "temurin", "21.0.12.1+1"]));
    assert!(text.contains("jdk-21.0.12.1%2B1"), "{text}");
    assert!(
        text.contains("OpenJDK21U-jdk_x64_windows_hotspot_21.0.12.1_1.zip"),
        "{text}"
    );
    assert!(!text.contains('{'), "URL 里不该还有占位符：{text}");
}

#[test]
fn catalog_show_lists_every_version_with_its_verdict() {
    let text = stdout(&run(["catalog", "show", "temurin"]));
    assert!(text.contains("Eclipse Temurin JDK"), "{text}");
    assert!(text.contains("别名"), "{text}");
    assert!(text.contains("21.0.12.1+1"), "{text}");
    assert!(text.contains("allowed"), "{text}");
}

#[test]
fn catalog_show_on_a_prohibited_tool_says_why_and_offers_no_versions() {
    let text = stdout(&run(["catalog", "show", "oracle-jdk"]));
    assert!(text.contains("prohibited"), "{text}");
    assert!(text.contains("不可再分发"), "{text}");
    assert!(text.contains("没有可安装的版本"), "{text}");
    assert!(text.contains("自行获取"), "{text}");
}

#[test]
fn catalog_json_outputs_contain_no_cjk_in_enum_values() {
    // 中文优先针对人类输出；JSON 的**取值**必须是 ASCII。
    // （描述与 notes 是自由文本，允许中文。）
    for args in [
        vec!["catalog", "list", "--json"],
        vec!["catalog", "show", "node", "--json"],
        vec!["catalog", "check", "node", "24", "--json"],
    ] {
        let envelope = json(&run(args.clone()));
        let text = stdout(&run(args.clone()));
        // 用解析后的结构检查取值，而不是全文字符串（全文字符串里合法地含中文）。
        let data = envelope.data.expect("data");
        let mut stack = vec![data];
        while let Some(value) = stack.pop() {
            match value {
                serde_json::Value::Object(map) => {
                    for (k, v) in map {
                        // 键必须是 ASCII。
                        assert!(k.is_ascii(), "{args:?} 的键 `{k}` 不是 ASCII");
                        stack.push(v);
                    }
                }
                serde_json::Value::Array(items) => stack.extend(items),
                _ => {}
            }
        }
        assert!(!text.is_empty());
    }
}
