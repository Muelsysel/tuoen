//! `tuoen capture` 的**进程边界契约测试**。
//!
//! ## 为什么必须在这一层
//!
//! 采集器本身在 `tuoen_core` 里用假机器测到字节级（`CaptureFixture`），
//! 但**只有跑真实二进制才能回答票据真正问的问题**：敲 `tuoen capture --out <目录>`
//! 之后，磁盘上到底多了哪几个文件、`--only` 有没有真的少捕获一类、
//! `--json` 的形状对不对、两次跑出来是不是一模一样。参数解析、退出码、
//! 中文人类输出与 `skipped.toml` 那一段，全都只有在这一层才看得见。
//!
//! ## 四条硬性约束（每一条都有对应的用例）
//!
//! 1. **绝不写真实的注册表 / `PATH` / 安装目录。** `capture` 读机器（只读，允许），
//!    而它写出去的每一个字节都落在 `--out <临时目录>` 里 —— 本文件里没有一条用例
//!    让产出落在仓库里或真实家目录里。用例
//!    [`a_run_with_out_in_a_temp_dir_does_not_touch_the_working_directory`]
//!    是这一条的**证据**，而不是一句声称。
//! 2. **不依赖本机状态。** 本机 `PATH` 恰好是坏的（重复、失效条目、硬编码用户名），
//!    所以任何"本机 `PATH` 长什么样"的断言都是错的。这里只断言**形状与不变量**：
//!    文件存在、能解析、结构自洽、两次一样。同理，跳过清单里"恰好有几条"不许断言
//!    —— 那是开发机的状态，不是被测代码的性质（`AGENTS.md` 规矩五）。
//! 3. **不启动任何进程**（除了被测的 `tuoen` 自己）。全部用例都带 `--no-version`，
//!    于是版本探测那一步不会去 spawn 任何工具 —— 与 `detect_contract.rs` 同一条纪律。
//! 4. **`--json` 的成功载荷里不出现中文**（决策 35），且逐字节稳定。两条都有用例，
//!    而"中文只进人类输出"这条决策的落点是 `skipped[].reason`：它**不进 JSON**。
//!
//! ## 为什么每一处都用 [`IsolatedHome`]
//!
//! `capture` 的 `tuoen_roots` 里有存储根与 shim 目录，两者都从 `%LOCALAPPDATA%` 推出来；
//! 指到临时目录之后，这份快照里"哪些 `PATH` 条目是我们自己的"就不依赖开发机装过什么。

mod common;

use std::process::Output;

use common::{IsolatedHome, TempDir, json, list_files, stderr, stdout};
use serde_json::{Value, json as value};

/// 默认跑一次会产出的六个文件（`list_files` 是排序过的）。
const ALL_SIX: [&str; 6] = [
    "env.toml",
    "path.toml",
    "schema.toml",
    "skipped.toml",
    "tools.toml",
    "wsl.toml",
];

/// 形似凭据的串。**只断言形状**：断言"某个变量的值不在里面"会让用例依赖开发机。
const CREDENTIAL_SHAPES: [&str; 3] = ["glpat-", "ghp_", "AKIA"];

// ─────────────────────────────────────────────────────────────────────────────
// 测试基础设施
// ─────────────────────────────────────────────────────────────────────────────

fn run_capture(home: &IsolatedHome, args: &[&str]) -> Output {
    home.run(args)
}

/// 一次调用的两路输出，用于断言失败时的可读信息。
fn describe(output: &Output) -> String {
    format!(
        "stdout：{} stderr：{}",
        stdout(output),
        String::from_utf8_lossy(&output.stderr)
    )
}

/// `--out` 的值。**在测试里自己拼路径是刻意的**：产出落在哪里是这一票的契约，
/// 而向生产代码问路径等于让测试跟着实现走。
fn out_arg(dir: &TempDir) -> String {
    dir.path()
        .to_str()
        .expect("临时目录路径必须是 UTF-8")
        .to_owned()
}

fn read_file(dir: &TempDir, name: &str) -> String {
    std::fs::read_to_string(dir.path().join(name))
        .unwrap_or_else(|error| panic!("读 `{name}` 失败：{error}"))
}

/// 去掉时间戳，用来比较两次捕获。
///
/// **契约测试自己写一份，不从 `tuoen_core` 导入 `without_timestamp`。**
/// 导入生产代码的那一份会让两件事一起漂移：生产代码把时间戳键改了名，
/// 测试就跟着"通过"了，而两份输出其实已经不再可比 —— 测试要能发现
/// "生产代码改了形状而测试跟着改"这种共谋。
///
/// # 两种形状必须分开处理（这一条是真机验收抓出来的）
///
/// TOML 里时间戳**自己占一行**（`captured_at = "…"`），删掉那一行即可。
/// 而 `--json` 的成功载荷是**一整行**，删行等于删掉整份载荷 —— 第一版就是这么写的，
/// 于是"两次逐字节相同"这条断言实际上只在**两次调用落在同一秒**时才成立：
/// 断言没红不是因为输出稳定，而是因为时钟恰好没走。真机验收里它红了一次
/// （`19:18:03Z` vs `19:18:04Z`），红的原因却是这条用例自己的实现。
/// 现在 JSON 走**只替换值、保留键与引号**的分支：`"capturedAt":"<stripped>"`
/// 的形状本身也是契约，键名改了这条断言仍然要红。
fn strip_timestamp(text: &str) -> String {
    const KEY: &str = "\"capturedAt\"";
    let mut out = String::with_capacity(text.len());
    for line in text.lines() {
        if line.trim_start().starts_with("captured_at") {
            continue;
        }
        if let Some(at) = line.find(KEY) {
            // `"capturedAt"` 之后应当是 `:` 空白 `"值"`。三段里任何一段找不到引号，
            // 就原样留下这一行 —— 那时比较会红，而"红"正是我们要的结果：
            // 载荷的形状变了。
            let rest = &line[at + KEY.len()..];
            let after_colon = rest.find(':').map_or("", |colon| &rest[colon + 1..]);
            let value_and_tail = after_colon
                .find('"')
                .map_or("", |open| &after_colon[open + 1..]);
            match value_and_tail.find('"') {
                Some(close) => {
                    out.push_str(&line[..at]);
                    out.push_str("\"capturedAt\":\"<stripped>\"");
                    out.push_str(&value_and_tail[close + 1..]);
                }
                None => out.push_str(line),
            }
        } else {
            out.push_str(line);
        }
        out.push('\n');
    }
    out
}

/// 钉住上面那个助手自己：**单行 JSON 不许被删成空**。
///
/// 这条用例的价值在于它**在旧实现下必须红** —— 旧实现按行删，单行载荷会被整份删掉，
/// 于是"两次相同"退化成"两份空串相同"。一个把两边都删光的断言永远通过。
#[test]
fn strip_timestamp_scrubs_the_json_value_without_dropping_the_payload() {
    let first = r#"{"capturedAt":"2026-10-02T19:18:03Z","outDir":"C:\\x","files":["a"]}"#;
    let second = r#"{"capturedAt":"2026-10-02T19:18:04Z","outDir":"C:\\x","files":["a"]}"#;
    let a = strip_timestamp(first);
    let b = strip_timestamp(second);
    assert_eq!(a, b, "两次捕获只差时间戳，去掉时间戳后必须逐字节相同");
    assert!(
        a.contains(r#""capturedAt":"<stripped>""#),
        "键与引号要留下（形状也是契约）：{a}"
    );
    assert!(
        a.contains(r#""outDir":"C:\\x""#),
        "载荷的其余部分不许被删：{a}"
    );
    assert!(
        a.contains(r#""files":["a"]"#),
        "载荷的其余部分不许被删：{a}"
    );

    // TOML：时间戳自己占一行 → 整行删掉，且不留空行。
    let toml = "schema_version = 1\ncaptured_at = \"2026-10-02T19:18:03Z\"\nsections = []\n";
    assert_eq!(
        strip_timestamp(toml),
        "schema_version = 1\nsections = []\n",
        "TOML 的时间戳行要整行消失"
    );

    // 键改名了就必须留下时间戳（= 比较要红）：这是"生产代码改了形状"能被发现的地方。
    let renamed = r#"{"capturedAtMs":"2026-10-02T19:18:03Z"}"#;
    assert!(
        strip_timestamp(renamed).contains("2026-10-02T19:18:03Z"),
        "键改名后不许再被当成时间戳擦掉：{}",
        strip_timestamp(renamed)
    );
}

/// 从 `schema.toml` 里取出 `sections` 数组。
///
/// 手写而不是引一个 TOML 解析器：这一份测试要能发现**生产代码改了文件形状**，
/// 而一个跟着变的解析器发现不了。它只认这一件事，所以只有这几行。
fn sections_of(schema_toml: &str) -> Vec<String> {
    let start = schema_toml
        .find("sections")
        .unwrap_or_else(|| panic!("schema.toml 里必须有 sections：\n{schema_toml}"));
    let rest = &schema_toml[start..];
    let end = rest
        .find(']')
        .unwrap_or_else(|| panic!("sections 必须是一个数组：\n{schema_toml}"));
    rest[..end]
        .split('"')
        .skip(1)
        .step_by(2)
        .map(ToOwned::to_owned)
        .collect()
}

/// 粗略取出所有 `key = "值"` 的值（自己写的小解析器，理由同 [`sections_of`]）。
fn string_values(text: &str, key: &str) -> Vec<String> {
    let prefix = format!("{key} = \"");
    text.lines()
        .filter_map(|line| line.trim_start().strip_prefix(prefix.as_str()))
        .filter_map(|rest| rest.split('"').next())
        .map(ToOwned::to_owned)
        .collect()
}

/// 粗略取出 `key = 数字` 的值。
fn number_value(text: &str, key: &str) -> Option<u64> {
    let prefix = format!("{key} = ");
    text.lines()
        .find_map(|line| line.trim_start().strip_prefix(prefix.as_str()))
        .and_then(|rest| rest.trim().parse().ok())
}

fn contains_cjk(text: &str) -> bool {
    text.chars()
        .any(|c| (0x4E00..=0x9FFF).contains(&(u32::from(c))))
}

/// `tools.toml` 里的 `[[tool]]` 块，逐块取 `source` 与 `version`。
///
/// 自己写而不是引 TOML 解析器：理由同 [`sections_of`] —— 一个跟着变的解析器
/// 发现不了形状漂移。
fn tool_rows(tools_toml: &str) -> Vec<(String, Option<String>)> {
    tools_toml
        .split("[[tool]]")
        .skip(1)
        .map(|block| {
            let field = |key: &str| -> Option<String> {
                let prefix = format!("{key} = \"");
                block
                    .lines()
                    .find_map(|line| line.trim_start().strip_prefix(prefix.as_str()))
                    .and_then(|rest| rest.split('"').next())
                    .map(ToOwned::to_owned)
            };
            (
                field("source").expect("每条工具记录都必须带 source"),
                field("version"),
            )
        })
        .collect()
}

/// 版本**只可能来自探测**的那些来源。
///
/// 这是实测出来的分界，不是猜的（本机 `--no-version` 的输出里只有两处还有版本）：
/// `filesystem-scan` 的版本写在**目录名**里（`apache-maven-3.9.5`）、
/// `manager` 的版本写在**版本管理器的布局**里（nvm4w 的 `nodejs`）、
/// `tuoen` 的版本写在**我们自己的安装记录**里 —— 这三样都是**结构事实**，
/// 不探测也拿得到，所以 `--no-version` 不会（也不该）把它们抹掉。
///
/// 而 `path-resolution` / `app-paths` / `registry-arp` 上的版本只能靠
/// **运行那个工具**问出来（或它压根问不出来），所以它们必须是空的。
const PROBE_ONLY_SOURCES: [&str; 3] = ["path-resolution", "app-paths", "registry-arp"];

/// 稳定 slug：小写 kebab ASCII。`--json` 里结论性的取值只能是这个形状。
fn is_slug(text: &str) -> bool {
    !text.is_empty()
        && text
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// 递归走一遍 JSON，**我们写的**字符串值里出现 CJK 就算失败。
///
/// 两个例外是**机器给的**，不是我们写的文本：`outDir`（用户给的目录）与
/// `skipped[].name`（环境变量名）。一台用户名是中文的机器上它们天然含 CJK，
/// 而那与"输出被本地化了吗"无关 —— 断言不许因为开发机的状态变红
/// （`path_contract.rs` 的同类断言直接扫**真实 PATH**，比这里更依赖机器；
/// 这一条只钉我们自己的文本，而且递归走**值**，所以 `\uXXXX` 转义过的中文
/// 同样逃不掉）。
fn assert_no_cjk_strings(value: &Value, path: &str, ours: bool) {
    match value {
        Value::String(text) => {
            if ours {
                assert!(!contains_cjk(text), "`{path}` 的值里有中文：{text}");
            }
        }
        Value::Array(items) => {
            for (index, item) in items.iter().enumerate() {
                assert_no_cjk_strings(item, &format!("{path}[{index}]"), ours);
            }
        }
        Value::Object(fields) => {
            for (key, field) in fields {
                let ours = ours && key != "outDir" && key != "name";
                assert_no_cjk_strings(field, &format!("{path}.{key}"), ours);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// 1. 默认跑一次：恰好六个文件
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn a_default_run_writes_exactly_the_six_promised_files() {
    let home = IsolatedHome::new("capture-six");
    let out = TempDir::new("capture-six-out");
    let output = run_capture(
        &home,
        &["capture", "--out", out_arg(&out).as_str(), "--no-version"],
    );
    assert_eq!(output.status.code(), Some(0), "{}", describe(&output));

    // **多一个文件也是 bug**：消费者（将来的 `restore` / `doctor`）会按文件名读。
    assert_eq!(
        list_files(out.path()),
        ALL_SIX.to_vec(),
        "产出的文件集合必须恰好是这六个"
    );

    let text = stdout(&output);
    assert!(contains_cjk(&text), "人类输出要是中文：{text}");
    assert!(text.contains(&out_arg(&out)), "要报出写到哪个目录：{text}");
    for name in ALL_SIX {
        assert!(
            text.contains(name),
            "人类输出里必须**逐个列出**写出去的文件，缺 {name}：{text}"
        );
    }
    // 只读这件事必须自己说出来 —— 这一族最容易被误解成"改了点什么"。
    assert!(text.contains("只读"), "{text}");

    // 关键数字：从**磁盘上那份** path.toml 里取，再要求人类输出里有它 ——
    // 两边对不上就说明报告与文件说了两个真相。
    let path_toml = read_file(&out, "path.toml");
    let effective =
        number_value(&path_toml, "effective_chars").expect("path.toml 要有 effective_chars");
    assert!(
        text.contains(&effective.to_string()),
        "人类输出要报出生效字符数 {effective}：{text}"
    );
    assert!(text.contains("8191"), "人类输出要报出悬崖 8191：{text}");

    // 跳过那一段**任何情况下都不许省**（静默跳过是 bug）。断言的是"说了"，
    // 不是"说了零条"—— 开发机上有没有凭据是不该被断言的机器状态。
    let skipped_toml = read_file(&out, "skipped.toml");
    assert!(text.contains("skipped.toml"), "{text}");
    if skipped_toml.contains("[[skipped]]") {
        for name in string_values(&skipped_toml, "name") {
            assert!(
                text.contains(&name),
                "跳过项 `{name}` 必须在人类输出里被点名：{text}"
            );
        }
        for kind in string_values(&skipped_toml, "kind") {
            assert!(text.contains(&kind), "跳过原因 `{kind}` 也要在：{text}");
        }
    } else {
        assert!(
            text.contains("没有跳过任何东西"),
            "一条都没跳过时也要说一句：{text}"
        );
    }

    // 结尾必须有"下一步"。
    assert!(text.contains("下一步"), "{text}");
}

// ─────────────────────────────────────────────────────────────────────────────
// 2 / 3. `--only`：格式层面的选择性捕获
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn only_path_writes_only_the_path_file_and_says_so_in_the_schema() {
    let home = IsolatedHome::new("capture-only-path");
    let out = TempDir::new("capture-only-path-out");
    let output = run_capture(
        &home,
        &[
            "capture",
            "--only",
            "path",
            "--out",
            out_arg(&out).as_str(),
            "--no-version",
        ],
    );
    assert_eq!(output.status.code(), Some(0), "{}", describe(&output));
    assert_eq!(
        list_files(out.path()),
        vec!["path.toml".to_owned(), "schema.toml".to_owned()],
        "`--only path` 只该产出这两个文件"
    );

    // **这是"没捕获"与"没有"的区别**：没捕获 `tools` 时，`sections` 里连
    // `tools` 这个词都不出现 —— 于是 `restore` 读得出"没看"，而不是"没有"。
    let schema = read_file(&out, "schema.toml");
    assert_eq!(sections_of(&schema), ["path"], "{schema}");
    assert!(!schema.contains("tools"), "{schema}");

    // 没扫环境变量 → 就没有跳过清单。它不做的事不该有产物。
    assert!(
        !out.path().join("skipped.toml").exists(),
        "没扫过环境变量，不该有 skipped.toml"
    );
    let text = stdout(&output);
    assert!(
        text.contains("没有跳过清单"),
        "「跳过了什么」无从谈起时也要说清，而不是沉默：{text}"
    );
    assert!(
        text.contains("这次没捕获 `tools`"),
        "没捕获的 section 也要逐条说明：{text}"
    );
}

#[test]
fn only_is_repeatable_and_the_schema_sections_are_sorted() {
    let home = IsolatedHome::new("capture-only-two");
    let out = TempDir::new("capture-only-two-out");
    let output = run_capture(
        &home,
        &[
            "capture",
            "--only",
            "path",
            "--only",
            "env",
            "--out",
            out_arg(&out).as_str(),
            "--no-version",
        ],
    );
    assert_eq!(output.status.code(), Some(0), "{}", describe(&output));
    assert_eq!(
        list_files(out.path()),
        vec![
            "env.toml".to_owned(),
            "path.toml".to_owned(),
            "schema.toml".to_owned(),
            "skipped.toml".to_owned()
        ],
        "扫过环境变量 → 跳过清单也在（哪怕它是空的）"
    );
    let schema = read_file(&out, "schema.toml");
    assert_eq!(
        sections_of(&schema),
        ["env", "path"],
        "顺序必须是排序后的（顺序稳定才能比逐字节）：{schema}"
    );

    // 同一个 section 给两次不许变成两份 —— 去重与排序都是引擎的既定语义。
    let twice = TempDir::new("capture-only-dup");
    let output = run_capture(
        &home,
        &[
            "capture",
            "--only",
            "env",
            "--only",
            "env",
            "--only",
            "path",
            "--out",
            out_arg(&twice).as_str(),
            "--no-version",
        ],
    );
    assert_eq!(output.status.code(), Some(0), "{}", describe(&output));
    assert_eq!(
        sections_of(&read_file(&twice, "schema.toml")),
        ["env", "path"]
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// 4. 幂等：两次跑出来除时间戳外逐字节相同
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn two_runs_in_a_row_differ_only_in_the_timestamp() {
    let home = IsolatedHome::new("capture-idempotent");
    let out = TempDir::new("capture-idempotent-out");
    let out_path = out_arg(&out);
    let args = ["capture", "--out", out_path.as_str(), "--no-version"];

    let first = run_capture(&home, &args);
    assert_eq!(first.status.code(), Some(0), "{}", describe(&first));
    let before: Vec<(&str, String)> = ALL_SIX
        .iter()
        .map(|name| (*name, read_file(&out, name)))
        .collect();

    let second = run_capture(&home, &args);
    assert_eq!(second.status.code(), Some(0), "{}", describe(&second));

    for (name, first_text) in &before {
        let second_text = read_file(&out, name);
        assert_eq!(
            strip_timestamp(first_text),
            strip_timestamp(&second_text),
            "`{name}` 在两次运行之间变了 —— 幂等性坏了（任何 `HashMap` 迭代混进序列化都会这样）"
        );
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// 5. `--json`
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn json_is_stable_and_agrees_with_the_files_on_disk() {
    let home = IsolatedHome::new("capture-json");
    let out = TempDir::new("capture-json-out");
    let out_path = out_arg(&out);
    let args = [
        "capture",
        "--out",
        out_path.as_str(),
        "--no-version",
        "--json",
    ];

    let output = run_capture(&home, &args);
    assert_eq!(output.status.code(), Some(0), "{}", describe(&output));
    let envelope = json(&output);
    assert_eq!(envelope.schema_version, 1);
    assert_eq!(envelope.command, "capture");
    assert!(envelope.ok, "{:?}", envelope.error);
    assert!(envelope.error.is_none());
    let data = envelope.data.expect("成功必须有 data");

    // 顶层键一个都不能少 —— 一台"什么都没检测到"的机器也要给出完整形状。
    for key in [
        "outDir",
        "capturedAt",
        "schemaVersion",
        "sections",
        "files",
        "tools",
        "path",
        "env",
        "wsl",
        "skipped",
    ] {
        assert!(data.get(key).is_some(), "缺少顶层键 `{key}`：{data}");
    }
    assert_eq!(data["outDir"], value!(out_arg(&out)));
    assert_eq!(data["schemaVersion"].as_u64(), Some(1));

    // **载荷里说的文件集合必须与磁盘上一模一样。**
    // `files` 的顺序是**写入顺序**（= `file_names()`），而 `list_files` 是排序后的，
    // 所以比的是**集合**。写入顺序本身也是契约（它决定目录项的 mtime），
    // 那一条由"两次跑逐字节相同"守着。
    let mut files: Vec<String> = data["files"]
        .as_array()
        .expect("files 是数组")
        .iter()
        .map(|item| item.as_str().expect("文件名是字符串").to_owned())
        .collect();
    files.sort();
    assert_eq!(files, list_files(out.path()), "载荷里的文件清单与磁盘不符");

    // 计数不许是编的：拿磁盘上那份 TOML 逐个数一遍，与 JSON 里的对。
    let path_toml = read_file(&out, "path.toml");
    assert_eq!(
        data["path"]["entries"].as_u64(),
        Some(path_toml.matches("[[entry]]").count() as u64),
        "path.entries 与 path.toml 不符：{data}"
    );
    let duplicates = path_toml
        .lines()
        .filter(|line| line.trim_start().starts_with("dup_index = "))
        .filter(|line| !line.trim_end().ends_with("= 0"))
        .count();
    assert_eq!(
        data["path"]["duplicates"].as_u64(),
        Some(duplicates as u64),
        "path.duplicates 与 path.toml 不符：{data}"
    );
    let tools_toml = read_file(&out, "tools.toml");
    assert_eq!(
        data["tools"]["entries"].as_u64(),
        Some(tools_toml.matches("[[tool]]").count() as u64),
        "tools.entries 与 tools.toml 不符：{data}"
    );
    let env_toml = read_file(&out, "env.toml");
    assert_eq!(
        data["env"]["total"].as_u64(),
        Some(env_toml.matches("[[var]]").count() as u64),
        "env.total 与 env.toml 不符：{data}"
    );
    let wsl_toml = read_file(&out, "wsl.toml");
    assert_eq!(
        data["wsl"]["distributions"].as_u64(),
        Some(wsl_toml.matches("[[distribution]]").count() as u64),
        "wsl.distributions 与 wsl.toml 不符：{data}"
    );
    let skipped_toml = read_file(&out, "skipped.toml");
    assert_eq!(
        data["skipped"]
            .as_array()
            .expect("扫过环境变量 → skipped 必须是一个数组（空数组也是信息）")
            .len(),
        skipped_toml.matches("[[skipped]]").count(),
        "skipped 与 skipped.toml 不符：{data}"
    );

    // 结论性的取值必须是稳定 slug，**不许被本地化**。
    for section in data["sections"].as_array().expect("数组") {
        assert!(is_slug(section.as_str().expect("slug")), "{section}");
    }
    assert!(is_slug(data["path"]["level"].as_str().expect("level")));
    for row in data["path"]["byScope"].as_array().expect("数组") {
        assert!(is_slug(row["scope"].as_str().expect("scope")), "{row}");
    }
    for row in data["tools"]["byConfidence"].as_array().expect("数组") {
        assert!(
            is_slug(row["confidence"].as_str().expect("confidence")),
            "{row}"
        );
    }
    for row in data["skipped"].as_array().expect("数组") {
        assert!(is_slug(row["section"].as_str().expect("section")), "{row}");
        assert!(is_slug(row["kind"].as_str().expect("kind")), "{row}");
        // **`reason` 是中文，所以它不进成功载荷。** 判据是上面那个 `kind` slug；
        // 人话只在人类输出里。脚本要分支就分 `kind`，而它不会随界面语言变。
        assert!(
            row.get("reason").is_none(),
            "`reason` 是中文句子，不许出现在成功载荷里：{row}"
        );
    }
    // 递归走一遍**我们写的**字符串（见 `assert_no_cjk_strings` 的两个例外）。
    assert_no_cjk_strings(&data, "$", true);

    // 两次 `--json` 除时间戳外逐字节相同。
    let first = strip_timestamp(&stdout(&output));
    let again = run_capture(&home, &args);
    assert_eq!(again.status.code(), Some(0), "{}", describe(&again));
    assert_eq!(
        first,
        strip_timestamp(&stdout(&again)),
        "两次 `--json` 除时间戳外必须逐字节相同"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// 6. 密钥：只断言形状
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn no_written_file_contains_anything_that_looks_like_a_credential() {
    let home = IsolatedHome::new("capture-secrets");
    let out = TempDir::new("capture-secrets-out");
    let output = run_capture(
        &home,
        &["capture", "--out", out_arg(&out).as_str(), "--no-version"],
    );
    assert_eq!(output.status.code(), Some(0), "{}", describe(&output));

    // 把**每一个**产出文件的内容拼起来看一遍。
    //
    // **只断言形状，不断言"某个变量的某个值不在里面"**：后者会让这条用例依赖
    // 开发机上有什么环境变量。而"绝不捕获密钥材料、只捕获意图"是这一票的硬规矩，
    // 这条用例就是它在进程边界上的证据 —— 本机环境变量里实测零密钥，
    // 所以它同时是一张"真机上也不会漏"的回归网。
    for name in ALL_SIX {
        let text = read_file(&out, name);
        for shape in CREDENTIAL_SHAPES {
            assert!(
                !text.contains(shape),
                "`{name}` 里出现了形似凭据的串 `{shape}`：\n{text}"
            );
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// 7. 只写 `--out`：当前工作目录不被污染
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn a_run_with_out_in_a_temp_dir_does_not_touch_the_working_directory() {
    let home = IsolatedHome::new("capture-cwd");
    let out = TempDir::new("capture-cwd-out");

    // 被测二进制继承的是**测试进程的当前目录**，而 `--out` 的默认值 `tuoen.d`
    // 是相对它解析的 —— 所以"当前目录没被污染"这一条，只有在
    // **跑之前列一遍、跑之后再列一遍**时才是证据，而不是一句声称。
    let cwd = std::env::current_dir().expect("当前目录");
    let before = list_files(&cwd);

    let output = run_capture(
        &home,
        &["capture", "--out", out_arg(&out).as_str(), "--no-version"],
    );
    assert_eq!(output.status.code(), Some(0), "{}", describe(&output));

    let after = list_files(&cwd);
    assert_eq!(
        before, after,
        "当前工作目录被污染了 —— 产出必须只落在 `--out` 里"
    );
    assert!(
        !cwd.join("tuoen.d").exists(),
        "不许在仓库里造出默认的 `tuoen.d/`"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// 8. 命令面：帮助与用法错误
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn the_command_is_visible_in_help_and_an_unknown_section_is_a_usage_error() {
    let home = IsolatedHome::new("capture-help");

    let top = run_capture(&home, &["--help"]);
    assert_eq!(top.status.code(), Some(0), "{}", describe(&top));
    assert!(
        stdout(&top).contains("capture"),
        "顶层帮助里必须有 `capture`：{}",
        stdout(&top)
    );

    let help = run_capture(&home, &["capture", "--help"]);
    assert_eq!(help.status.code(), Some(0), "{}", describe(&help));
    let text = stdout(&help);
    assert!(contains_cjk(&text), "长帮助要是中文：{text}");
    for flag in ["--only", "--out", "--no-version", "--json"] {
        assert!(text.contains(flag), "帮助里缺少 `{flag}`：{text}");
    }
    // 长帮助必须说清那三件事：只读 / 跳过清单（因为它要进仓库）/ `--only` 的语义。
    assert!(text.contains("只读"), "要说清这条命令只读：{text}");
    assert!(text.contains("skipped.toml"), "要提到跳过清单：{text}");
    assert!(
        text.contains("格式"),
        "要说清 `--only` 是格式层面的选择性捕获：{text}"
    );

    // 一个拼错的 section 必须在**解析阶段**被拒（退出码 2）——
    // 静默忽略它会让"我以为捕获了 tools"变成一句假话。
    let scratch = TempDir::new("capture-bogus");
    let bogus = run_capture(
        &home,
        &[
            "capture",
            "--only",
            "bogus",
            "--out",
            out_arg(&scratch).as_str(),
        ],
    );
    assert_eq!(
        bogus.status.code(),
        Some(2),
        "未知 section 必须是用法错误：{}",
        describe(&bogus)
    );
    assert!(stderr(&bogus).contains("bogus"), "{}", stderr(&bogus));
    assert!(
        list_files(scratch.path()).is_empty(),
        "用法错误不该写出任何东西"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// 9. `--no-version`
// ─────────────────────────────────────────────────────────────────────────────

/// `--no-version` 关掉的是**探测**，不是"版本"。
///
/// 票据里写的是"`tools.toml` 里所有条目的 `version` 都缺席或为空"。
/// **那条描述与实测不符**，而这条用例断言的是实测出来的那条不变量：
/// 只能靠探测得到的版本必须缺席，而**结构事实**（目录名里的版本、
/// 版本管理器的布局、我们自己的安装记录）照旧在 —— 它们本来就不需要探测。
/// 证据（本机 `--no-version` 的真实输出）：`maven`（`filesystem-scan`，
/// 目录名 `apache-maven-3.9.5`）、`node`（`manager` 的 nvm4w 布局、`tuoen`
/// 的安装记录）三条仍然有版本，其余全部为空。
#[test]
fn no_version_removes_every_probed_version_and_keeps_the_structural_ones() {
    let home = IsolatedHome::new("capture-no-version");
    let out = TempDir::new("capture-no-version-out");
    let output = run_capture(
        &home,
        &["capture", "--out", out_arg(&out).as_str(), "--no-version"],
    );
    assert_eq!(output.status.code(), Some(0), "{}", describe(&output));

    let tools = read_file(&out, "tools.toml");
    let rows = tool_rows(&tools);
    if rows.is_empty() {
        // **如实跳过**：这台机器上一条工具都没检测到，于是这条断言没有对象。
        // 这不是"测试通过"，是"没得测"。
        eprintln!("（这台机器上一条工具都没检测到 —— 跳过 `--no-version` 的版本断言）");
        return;
    }

    for (source, version) in &rows {
        if PROBE_ONLY_SOURCES.contains(&source.as_str()) {
            assert!(
                version.is_none(),
                "`--no-version` 却给出了一个只能靠**探测**得到的版本：\
                 source={source} version={version:?}\n{tools}"
            );
        }
    }
    // 沉默地不报版本会让人以为"这些工具都没有版本"，而不是"我们没问"。
    assert!(
        stdout(&output).contains("没有探测版本"),
        "{}",
        stdout(&output)
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// 10. 失败路径：落盘失败要如实说，且不许对 `files` 说假话
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn a_write_failure_is_reported_as_partial_without_lying_about_files() {
    let home = IsolatedHome::new("capture-io-error");
    let scratch = TempDir::new("capture-io-error-out");
    // 用**一个普通文件**占住父目录的位置：建目录必然失败。
    // 这是一个真实的 IO 失败，不需要任何"用假注册表"之类的后门开关。
    let blocker = scratch.write("blocker", "这不是一个目录");
    let bad_out = blocker.join("nested");
    let bad_out = bad_out.to_str().expect("UTF-8 路径");

    let output = run_capture(
        &home,
        &["capture", "--only", "wsl", "--out", bad_out, "--json"],
    );
    assert_eq!(output.status.code(), Some(1), "{}", describe(&output));

    let envelope = json(&output);
    assert_eq!(envelope.command, "capture");
    assert!(!envelope.ok, "有东西没做成 → `ok` 必须是 false");
    let error = envelope.error.expect("失败必须有 error");
    assert_eq!(
        error.code, "capture-io",
        "错误码是稳定的机器可读串：{error:?}"
    );
    let data = envelope.data.expect("失败也要带出已经确定的事实");
    assert_eq!(data["outDir"].as_str(), Some(bad_out));
    assert_eq!(data["sections"], value!(["wsl"]));
    // 落盘是**逐文件**写的：第三个失败时前两个已经在磁盘上了。
    // 报一个空数组就是在说假话，所以这个键必须**缺席**（缺席 = 不知道）。
    assert!(
        data.get("files").is_none(),
        "失败时给不出准确的文件清单 —— 这个键必须缺席，而不是空数组：{data}"
    );
}
