//! `tuoen path show` / `add` / `remove` 的**进程边界**契约测试。
//!
//! ## 这一票为什么必须走进程边界
//!
//! `PATH` 的读、分析、计划、落盘在 `tuoen-platform` 里有单元测试（条目解析、
//! 预算悬崖、写回类型规则、计划与落盘的一致性），但**只有跑真实二进制才能回答
//! 票据真正问的问题**："用户敲 `tuoen path show` 之后看到什么、
//! 敲 `tuoen path add <目录> --dry-run` 之后机器有没有被碰过"。
//! 参数解析、`--json` 形状、退出码、中文输出，全都只有在这一层才看得见。
//!
//! ## 四条硬性约束（本文件里每一条都有对应的用例）
//!
//! 1. **绝不写真实的 `HKCU\Environment`。** 本文件**只跑** `path show` 与
//!    `path add|remove --dry-run` —— 它们一个字节都不写。真正的证据是
//!    [`a_dry_run_writes_nothing_and_the_report_proves_it`]：`show --json` 的
//!    输出里带着注册表原文（`scopes[].raw`）与类型，所以"前后逐字节相同"
//!    **就是**"注册表没被动过"的证据，而不是一句声称。
//! 2. **不给出厂二进制留后门开关。** 没有"用假注册表"的环境变量：一个被误设的
//!    变量会把垃圾写进用户真实的 `PATH`。测试要隔离的是**存储位置**
//!    （`LOCALAPPDATA`，shim 目录在它下面），不是注册表。
//! 3. **不启动任何进程**（除了被测的 `tuoen` 自己）。这一族没有转发器、没有探针。
//! 4. **`--json` 必须逐字节稳定**，且成功载荷里不出现中文 —— 两条都有用例。
//!
//! ## 为什么每一处都用 [`IsolatedHome`]
//!
//! `path` 一族会去读 shim 目录（判断我们发布的命令有没有被遮蔽、以及保护它不被
//! `remove` 摘掉）。指到临时目录之后，这份报告的内容**不依赖开发者的机器上
//! 有没有装过 shim** —— 于是断言可以是确定的，而不是"在本机恰好成立"。

mod common;

use std::path::{Path, PathBuf};
use std::process::Output;

use common::{IsolatedHome, json, stdout};
use serde_json::{Value, json as value};

/// 四个预算档位的稳定 slug。**改它们要递增 `schemaVersion`。**
const BUDGET_LEVELS: [&str; 4] = ["ok", "warning", "critical", "exceeded"];

// ─────────────────────────────────────────────────────────────────────────────
// 测试基础设施
// ─────────────────────────────────────────────────────────────────────────────

/// 跑一次 `tuoen path …`，环境指向隔离的家目录。
///
/// `IsolatedHome::run` 已经隔离了 `LOCALAPPDATA` / `APPDATA`，而 shim 目录
/// 就在 `%LOCALAPPDATA%\tuoen\shims` —— 所以这一族的报告不会被开发者的
/// 真实 shim 目录影响。
fn run_path(home: &IsolatedHome, args: &[&str]) -> Output {
    home.run(args)
}

/// 一次调用的两路输出，用于断言失败时的可读信息。
///
/// `--json` 的错误走 **stdout**（那是信封的规矩），所以只打印 stderr 会得到
/// 一句没有信息量的断言失败。
fn describe(output: &Output) -> String {
    format!(
        "stdout：{} stderr：{}",
        stdout(output),
        String::from_utf8_lossy(&output.stderr)
    )
}

/// `path show --json` 的载荷。
fn show_data(home: &IsolatedHome) -> Value {
    let output = run_path(home, &["path", "show", "--json"]);
    assert_eq!(output.status.code(), Some(0), "{}", describe(&output));
    let envelope = json(&output);
    assert!(envelope.ok, "show 必须是成功信封：{:?}", envelope.error);
    assert_eq!(envelope.command, "path.show");
    envelope.data.expect("成功必须有 data")
}

/// 在隔离的家目录下造一个真实存在的目录。
///
/// 用一个**真的存在**的目录是刻意的：不存在的话 `plan_add` 照样会计划加它
/// （`PATH` 条目允许指向还没建出来的目录），但"加一个不存在的目录"会让
/// `show` 的失效条目计数变化，而那与本文件要测的东西无关。
fn a_real_dir(home: &IsolatedHome, name: &str) -> PathBuf {
    let dir = home.local_app_data().join("path-contract").join(name);
    std::fs::create_dir_all(&dir).expect("造一个真实目录");
    dir
}

/// shim 目录：`<隔离的 LOCALAPPDATA>\tuoen\shims`。
///
/// **在测试里自己拼是刻意的**：测试要知道"东西应该落在哪"才能构造出
/// "拿 shim 目录去 remove"这个用例，而向生产代码问路径等于让测试跟着实现走。
fn shims_dir(home: &IsolatedHome) -> PathBuf {
    home.local_app_data().join("tuoen").join("shims")
}

fn contains_cjk(text: &str) -> bool {
    text.chars()
        .any(|c| (0x4E00..=0x9FFF).contains(&(u32::from(c))))
}

/// 真实的 `%LOCALAPPDATA%\tuoen` 下现在有哪些名字。
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
    // **`shims` 要往里看一层。** 只列顶层的话，最严重的那一类事故看不见：
    // "在真实的 shim 目录里生成了一个 shim" —— 因为 `shims` 这个名字本来就在那儿，
    // 空目录和 4 条 shim 的顶层列表长得一模一样。
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

/// 用户级 `PATH` 里的第一条非空条目（没有就 `None`）。
fn first_user_entry(data: &Value) -> Option<String> {
    data["scopes"]
        .as_array()?
        .iter()
        .find(|scope| scope["scope"] == value!("user"))?
        .get("entries")?
        .as_array()?
        .iter()
        .find(|entry| entry["empty"] == value!(false))?
        .get("value")?
        .as_str()
        .map(ToOwned::to_owned)
}

// ─────────────────────────────────────────────────────────────────────────────
// 命令面
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn help_lists_all_three_subcommands() {
    // **票据的验收条件之一**：三个子命令必须都在帮助里看得见。
    // 人类帮助是中文，但子命令名与参数名是英文且不本地化（决策 35）。
    let home = IsolatedHome::new("path-help");
    let output = run_path(&home, &["path", "--help"]);
    assert_eq!(output.status.code(), Some(0), "{}", describe(&output));
    let text = stdout(&output);
    for name in ["show", "add", "remove"] {
        assert!(text.contains(name), "帮助里必须有 `{name}`：{text}");
    }
    // `--dry-run` 只属于会写的那两条，而帮助里要说得出它。
    assert!(text.contains("--dry-run"), "{text}");
    assert!(contains_cjk(&text), "帮助要是中文：{text}");

    // 顶层帮助里也要有 `path`。
    let top = stdout(&run_path(&home, &["--help"]));
    assert!(top.contains("path"), "{top}");
}

#[test]
fn a_bare_path_is_exactly_path_show() {
    // **这一族最重要的一条默认值**：`tuoen path` 不许做任何别的事。
    // 两条命令行的输出必须逐字节相同，否则"敲了就改了 PATH"迟早会发生。
    let home = IsolatedHome::new("path-bare");
    let bare = run_path(&home, &["path"]);
    let explicit = run_path(&home, &["path", "show"]);
    assert_eq!(bare.status.code(), Some(0), "{}", describe(&bare));
    assert_eq!(explicit.status.code(), Some(0), "{}", describe(&explicit));
    assert_eq!(
        stdout(&bare),
        stdout(&explicit),
        "`tuoen path` 与 `tuoen path show` 必须是同一件事"
    );
}

#[test]
fn show_json_parses_and_carries_every_promised_top_level_key() {
    let home = IsolatedHome::new("path-show-keys");
    let data = show_data(&home);

    for key in [
        "name",
        "budget",
        "scopes",
        "effective",
        "machineEntries",
        "userEntries",
        "processOnly",
        "processOnlyCount",
        "duplicates",
        "duplicateCount",
        "duplicateExtraSegments",
        "dangling",
        "danglingCount",
        "usernameDependencies",
        "usernameDependencyCount",
        "usernameDependenciesAtMachineScope",
        "shadowedShims",
        "shadowedShimCount",
        "shimDir",
    ] {
        assert!(!data[key].is_null(), "缺少顶层键 `{key}`：{data}");
    }
    assert_eq!(data["name"], value!("Path"), "变量名是注册表里的那个写法");

    // 预算：四个 slug 之一，而且数要对得上。
    let budget = &data["budget"];
    let level = budget["level"].as_str().expect("budget.level 是字符串");
    assert!(
        BUDGET_LEVELS.contains(&level),
        "`{level}` 不是四个稳定档位之一：{budget}"
    );
    assert_eq!(
        budget["cliff"].as_u64(),
        Some(8191),
        "悬崖是 cmd.exe 的那个数字（不是 setx 的 1024）：{budget}"
    );
    let effective = budget["effectiveChars"].as_u64().expect("effectiveChars");
    let remaining = budget["remaining"].as_u64().expect("remaining");
    assert_eq!(
        remaining,
        8191_u64.saturating_sub(effective),
        "剩余字符数必须与生效长度对得上：{budget}"
    );

    // 两个作用域按生效顺序：[0] 机器级、[1] 用户级。
    let scopes = data["scopes"].as_array().expect("scopes 是数组");
    assert_eq!(scopes.len(), 2, "两个注册表作用域都要有：{data}");
    assert_eq!(scopes[0]["scope"], value!("machine"));
    assert_eq!(scopes[1]["scope"], value!("user"));
    for scope in scopes {
        assert!(
            scope["entries"].is_array(),
            "每个作用域都要有逐段明细：{scope}"
        );
        assert!(
            scope["emptyPositions"].is_array(),
            "空段的位置要报出来（本机机器级里真的有 `;;`）：{scope}"
        );
    }

    // 生效顺序逐条带 scope，而它是"谁赢名字冲突"的唯一答案。
    for entry in data["effective"].as_array().expect("effective 是数组") {
        let scope = entry["scope"].as_str().expect("scope");
        assert!(
            ["machine", "user", "process-only"].contains(&scope),
            "`{scope}` 不是三个作用域 slug 之一：{entry}"
        );
    }

    // 计数与明细必须对得上 —— 否则 GUI 画的概览与列表会互相矛盾。
    for (count_key, list_key) in [
        ("duplicateCount", "duplicates"),
        ("danglingCount", "dangling"),
        ("usernameDependencyCount", "usernameDependencies"),
        ("shadowedShimCount", "shadowedShims"),
        ("processOnlyCount", "processOnly"),
    ] {
        assert_eq!(
            data[count_key].as_u64(),
            Some(data[list_key].as_array().expect("数组").len() as u64),
            "`{count_key}` 与 `{list_key}` 的长度对不上：{data}"
        );
    }
}

#[test]
fn show_json_is_byte_stable_in_the_same_machine_state() {
    // 两次运行之间机器状态没变，所以输出必须**逐字节相同**。
    // 任何 `HashMap` / `HashSet` 迭代混进序列化都会让这一条红。
    let home = IsolatedHome::new("path-stable");
    for args in [
        vec!["path", "show", "--json"],
        vec!["path", "--json"],
        vec![
            "path",
            "add",
            "definitely-not-on-path-xyz",
            "--dry-run",
            "--json",
        ],
    ] {
        assert_eq!(
            stdout(&run_path(&home, &args)),
            stdout(&run_path(&home, &args)),
            "{args:?} 的 --json 输出必须逐字节稳定"
        );
    }
}

#[test]
fn show_is_read_only_and_says_so_in_chinese() {
    let home = IsolatedHome::new("path-show-human");
    let output = run_path(&home, &["path", "show"]);
    assert_eq!(output.status.code(), Some(0), "{}", describe(&output));
    let text = stdout(&output);
    assert!(contains_cjk(&text), "人类输出要是中文：{text}");
    assert!(!text.contains("panicked"), "不该 panic：{text}");
    // 三件必须出现在报告里的事。
    assert!(text.contains("8191"), "要报出悬崖：{text}");
    assert!(text.contains("机器级"), "要报作用域：{text}");
    assert!(
        text.contains("不自动清理") || text.contains("只报告"),
        "{text}"
    );
}

#[test]
fn success_payloads_contain_no_localised_text() {
    // 中文优先针对的是**人类输出**。JSON 里出现中文就意味着界面语言把脚本绑死了。
    let home = IsolatedHome::new("path-json-no-cjk");
    let dir = a_real_dir(&home, "toolbox");
    for args in [
        vec!["path", "show", "--json"],
        vec![
            "path",
            "add",
            dir.to_str().expect("UTF-8 路径"),
            "--dry-run",
            "--json",
        ],
        vec![
            "path",
            "remove",
            dir.to_str().expect("UTF-8 路径"),
            "--dry-run",
            "--json",
        ],
    ] {
        let output = run_path(&home, &args);
        let text = stdout(&output);
        assert_eq!(output.status.code(), Some(0), "{args:?}：{text}");
        assert!(
            !contains_cjk(&text),
            "{args:?} 的成功载荷里不该有 CJK：{text}"
        );
        // 逐**值**走一遍，而不是只看原文：`\uXXXX` 转义过的中文同样是本地化文本，
        // 而它不会以字符的形式出现在原文里。走值才能一次钉死两种形态。
        let payload: Value = serde_json::from_str(&text).expect("载荷必须是合法 JSON");
        assert_no_cjk_strings(&payload, &mut String::from("$"));
    }
}

/// 递归走一遍 JSON，任何**字符串值**里出现 CJK 就算失败。
///
/// `path` 是给失败信息用的（指出是哪一层出的问题），不是 JSON Pointer。
fn assert_no_cjk_strings(value: &Value, path: &mut String) {
    match value {
        Value::String(text) => assert!(!contains_cjk(text), "`{path}` 的值里有中文：{text}"),
        Value::Array(items) => {
            for (index, item) in items.iter().enumerate() {
                let mark = path.len();
                path.push_str(&format!("[{index}]"));
                assert_no_cjk_strings(item, path);
                path.truncate(mark);
            }
        }
        Value::Object(fields) => {
            for (key, field) in fields {
                let mark = path.len();
                path.push_str(&format!(".{key}"));
                assert_no_cjk_strings(field, path);
                path.truncate(mark);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// add / remove：计划与拒绝
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn add_dry_run_writes_nothing_and_the_report_proves_it() {
    // **本文件最重要的一条断言。**
    //
    // `show --json` 的载荷里带着注册表原文（`scopes[].raw`）与类型，所以
    // "前后逐字节相同"**就是**"注册表没被动过"的证据 —— 而"我没碰它"只是一句话。
    let home = IsolatedHome::new("path-dry-run");
    let dir = a_real_dir(&home, "toolbox");
    let before = stdout(&run_path(&home, &["path", "show", "--json"]));

    let output = run_path(
        &home,
        &[
            "path",
            "add",
            dir.to_str().expect("UTF-8 路径"),
            "--dry-run",
            "--json",
        ],
    );
    assert_eq!(output.status.code(), Some(0), "{}", describe(&output));
    let envelope = json(&output);
    assert!(envelope.ok, "演练成功就是成功信封：{:?}", envelope.error);
    let data = envelope.data.expect("成功必须有 data");
    assert_eq!(data["dryRun"], value!(true));
    assert_eq!(data["action"], value!("add"));
    assert_eq!(data["scope"], value!("user"), "只写用户级");
    assert_eq!(
        data["applied"],
        value!(null),
        "演练没有落盘结果 —— 那正是它什么都没做的证据"
    );
    let changes = data["changes"].as_array().expect("changes 是数组");
    assert!(
        changes.iter().any(|change| change["kind"] == value!("add")),
        "演练也要说清会加什么：{data}"
    );
    assert!(
        data["budgetAfter"]["level"]
            .as_str()
            .is_some_and(|level| BUDGET_LEVELS.contains(&level)),
        "计划里也要报写回之后的档位：{data}"
    );

    // **证据**：报告逐字节不变 ⇒ 注册表原文与类型都没变 ⇒ 一个字节都没写。
    let after = stdout(&run_path(&home, &["path", "show", "--json"]));
    assert_eq!(
        before, after,
        "演练动了真实状态 —— 这是本仓库最容易造成真实伤害的地方"
    );
}

#[test]
fn add_dry_run_of_something_already_present_is_a_successful_noop() {
    let home = IsolatedHome::new("path-already-present");
    let Some(existing) = first_user_entry(&show_data(&home)) else {
        // 一台用户级 `PATH` 是空的机器：这条用例没有可用的输入，但**不许假装测过**。
        eprintln!("跳过：这台机器的用户级 PATH 里没有非空条目");
        return;
    };

    let output = run_path(&home, &["path", "add", &existing, "--dry-run", "--json"]);
    assert_eq!(
        output.status.code(),
        Some(0),
        "「已经在里面了」是成功的空操作：{}",
        describe(&output)
    );
    let data = json(&output).data.expect("data");
    assert!(
        data["changes"]
            .as_array()
            .expect("changes")
            .iter()
            .any(|change| change["kind"] == value!("noop")
                && change["reason"] == value!("already-present")),
        "必须报成 `noop` + `already-present`：{data}"
    );

    // 人话里要说清"什么都没有改"。
    let human = stdout(&run_path(&home, &["path", "add", &existing, "--dry-run"]));
    assert!(
        human.contains("什么都不做") || human.contains("什么都没有改"),
        "{human}"
    );
}

#[test]
fn add_dry_run_of_a_semicolon_argument_fails_with_a_stable_code() {
    // `;` 是 `PATH` 的条目分隔符，**引号保护不了它**（Windows 先按 `;` 切、
    // 再决定要不要剥引号）。所以带 `;` 的目录名会被当场拒绝 —— 而且解释要说清
    // "加引号也不行"，因为那是用户最可能不服气的一处。
    let home = IsolatedHome::new("path-semicolon");
    let before = stdout(&run_path(&home, &["path", "show", "--json"]));

    for dir in [r"C:\a;b", r"C:\Program Files\a;b"] {
        let output = run_path(&home, &["path", "add", dir, "--dry-run", "--json"]);
        assert_eq!(
            output.status.code(),
            Some(1),
            "{dir}：{}",
            describe(&output)
        );
        let envelope = json(&output);
        assert!(!envelope.ok, "拒绝不该报成成功");
        let error = envelope.error.expect("失败必须有 error");
        assert_eq!(error.code, "unsupported", "错误码是稳定契约");
        assert!(
            error
                .code
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'),
            "错误码必须是小写 kebab ASCII：{}",
            error.code
        );
        assert!(
            error.message.contains(dir),
            "要点名是哪个目录：{}",
            error.message
        );
        assert!(
            error.message.contains(';'),
            "要说清 `;` 是分隔符：{}",
            error.message
        );
        assert!(
            error.message.contains("引号"),
            "要正面回答「加引号行不行」：{}",
            error.message
        );
    }

    // 拒绝路径上一个字节都不许写。
    let after = stdout(&run_path(&home, &["path", "show", "--json"]));
    assert_eq!(before, after, "被拒的输入不该碰真实状态");
}

#[test]
fn remove_dry_run_of_the_shim_directory_is_refused() {
    // **拒绝与空操作是两种结果**：拿 shim 目录去 `remove` 是"我不给你做这件事"，
    // 而不是"那里本来就没有它"。所以退出码非 0，而且解释要说清代价。
    let home = IsolatedHome::new("path-protect-shims");
    let dir = shims_dir(&home);
    let dir_text = dir.to_str().expect("UTF-8 路径").to_owned();
    let before = stdout(&run_path(&home, &["path", "show", "--json"]));

    let output = run_path(&home, &["path", "remove", &dir_text, "--dry-run", "--json"]);
    assert_eq!(
        output.status.code(),
        Some(1),
        "拒绝必须是退出码 1：{}",
        describe(&output)
    );
    let envelope = json(&output);
    assert!(!envelope.ok, "有东西没做成就不该报成功");
    let error = envelope.error.expect("失败必须有 error");
    assert_eq!(error.code, "protected-shim-dir", "错误码是稳定契约");
    assert!(
        error.message.contains("shim"),
        "要说清被保护的是什么：{}",
        error.message
    );
    assert!(
        error.message.contains("不许"),
        "要说清这是拒绝而不是空操作：{}",
        error.message
    );

    // 拒绝也带着计划 —— 拒绝的理由就写在里面。
    let data = envelope.data.expect("拒绝也要有 data：理由在计划里");
    assert!(
        data["changes"]
            .as_array()
            .expect("changes")
            .iter()
            .any(|change| change["kind"] == value!("noop")
                && change["reason"] == value!("protected-shim-dir")),
        "计划里必须点出原因：{data}"
    );
    assert_eq!(
        data["willWrite"],
        value!(false),
        "拒绝路径上什么都不写：{data}"
    );

    let after = stdout(&run_path(&home, &["path", "show", "--json"]));
    assert_eq!(before, after, "拒绝路径上一个字节都不该写");
}

#[test]
fn remove_dry_run_of_something_absent_is_a_successful_noop() {
    // **"你要删的东西不在"不是失败。** 它与"不给你删 shim 目录"是两种结果，
    // 而脚本必须分得开这两件事（一个退出码 0、一个退出码 1）。
    let home = IsolatedHome::new("path-remove-absent");
    let missing = home
        .local_app_data()
        .join("path-contract")
        .join("never-on-path-xyz");
    let missing_text = missing.to_str().expect("UTF-8 路径").to_owned();
    let before = stdout(&run_path(&home, &["path", "show", "--json"]));

    let output = run_path(
        &home,
        &["path", "remove", &missing_text, "--dry-run", "--json"],
    );
    assert_eq!(
        output.status.code(),
        Some(0),
        "本来就不在是成功的空操作：{}",
        describe(&output)
    );
    let data = json(&output).data.expect("data");
    assert_eq!(data["willWrite"], value!(false), "{data}");
    assert!(
        data["changes"]
            .as_array()
            .expect("changes")
            .iter()
            .any(|change| change["kind"] == value!("noop") && change["reason"] == value!("absent")),
        "必须报成 `noop` + `absent`：{data}"
    );

    // 人话要说清"什么都没有改"，而不是让人以为删掉了什么。
    let human = stdout(&run_path(
        &home,
        &["path", "remove", &missing_text, "--dry-run"],
    ));
    assert!(contains_cjk(&human), "{human}");
    assert!(
        human.contains("什么都不做") || human.contains("什么都没有改"),
        "{human}"
    );

    let after = stdout(&run_path(&home, &["path", "show", "--json"]));
    assert_eq!(before, after, "空操作不该碰真实状态");
}

#[test]
fn remove_dry_run_of_a_semicolon_argument_fails_the_same_way_as_add() {
    // 两条写命令的参数校验来自**同一个** `reject_separator_in_argument`，
    // 所以错误码必须一致 —— 不一致会让消费者写两套分支。
    let home = IsolatedHome::new("path-semicolon-remove");
    let output = run_path(&home, &["path", "remove", r"C:\a;b", "--dry-run", "--json"]);
    assert_eq!(output.status.code(), Some(1), "{}", describe(&output));
    let error = json(&output).error.expect("失败必须有 error");
    assert_eq!(error.code, "unsupported");
}

#[test]
fn a_fresh_machine_gets_chinese_output_and_never_panics() {
    // "换一台新机器"时第一个问题就是"这里什么都没有，工具会崩吗"。
    let home = IsolatedHome::new("path-fresh");
    for args in [
        vec!["path"],
        vec!["path", "show"],
        vec!["path", "add", r"C:\definitely\not\here", "--dry-run"],
        vec!["path", "remove", r"C:\definitely\not\here", "--dry-run"],
    ] {
        let output = run_path(&home, &args);
        let text = format!(
            "{}{}",
            stdout(&output),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(!text.is_empty(), "{args:?} 不该没有任何输出");
        assert!(!text.contains("panicked"), "{args:?} 不该 panic：{text}");
        assert!(contains_cjk(&text), "{args:?} 的人类输出应当是中文：{text}");
    }
}

#[test]
fn usage_errors_exit_two_and_writing_commands_need_their_argument() {
    // 退出码约定：0 成功，1 运行期错误，2 用法错误（由 clap 直接退出）。
    let home = IsolatedHome::new("path-usage");
    for args in [
        vec!["path", "add"],
        vec!["path", "remove"],
        vec!["path", "show", "--dry-run"],
    ] {
        let output = run_path(&home, &args);
        assert_eq!(output.status.code(), Some(2), "{args:?} 应当是用法的错");
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// 真实机器：这一票**绝不**碰它
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn the_real_tuoen_home_was_not_touched_by_this_files_tests() {
    // 这一票比其他票更需要这条断言：`path add` / `remove` 改的是**整机所有命令
    // 能不能找到**的那一条变量。
    //
    // 为什么不是"断言真实的 `%LOCALAPPDATA%\tuoen` 不存在"：那是一句**会被误报**
    // 的断言（L0 的真机验收真的在那里装过 node）。所以这里断言的是更准确的东西：
    // 跑完一整套**只读 + 演练**的生命周期，那个目录的顶层内容一个字节都没变，
    // 而且 `shims` 这个（`path show` 唯一会读的）目录从来没有被创建。
    let before = real_home_listing();

    let home = IsolatedHome::new("path-real-home");
    let dir = a_real_dir(&home, "toolbox");
    let dir_text = dir.to_str().expect("UTF-8 路径");
    let shims = shims_dir(&home);
    let shims_text = shims.to_str().expect("UTF-8 路径");
    for args in [
        vec!["path", "show", "--json"],
        vec!["path", "add", dir_text, "--dry-run", "--json"],
        vec!["path", "remove", dir_text, "--dry-run", "--json"],
        vec!["path", "remove", shims_text, "--dry-run", "--json"],
    ] {
        let output = run_path(&home, &args);
        // 最后一条是**拒绝**（退出码 1），其余是成功。
        assert!(
            output.status.code().is_some(),
            "{args:?} 不该被信号杀掉：{}",
            describe(&output)
        );
    }

    let after = real_home_listing();
    assert_eq!(
        before, after,
        "测试碰到了真实的 `%LOCALAPPDATA%\\tuoen` —— 这是本仓库最容易造成真实伤害的地方"
    );
    // **不断言"真实的 shim 目录不存在"。** 那是机器状态，不是被测代码的性质：
    // 任何真的用 `tuoen shim add` 装过东西的人都有这个目录（L0 的真机验收就有），
    // 于是那条断言会在最不该红的时候红。上面那个 before/after 才是真要钉的东西 ——
    // 而且 `real_home_listing` 已经往 `shims` 里看了一层，所以"在真实的 shim
    // 目录里写了一个 shim"照样会红。
}

#[test]
fn the_isolated_home_never_reaches_the_real_one() {
    // 隔离本身也要被证明：如果 `LOCALAPPDATA` 没被指到临时目录，
    // 上面那条断言就会变成一句"恰好成立"的话。这里正面钉住隔离有效。
    let home = IsolatedHome::new("path-isolation");
    let data = show_data(&home);
    let shim_dir = data["shimDir"]
        .as_str()
        .expect("隔离之后 shimDir 一定算得出来");
    assert!(
        Path::new(shim_dir).starts_with(home.local_app_data()),
        "shim 目录 `{shim_dir}` 不在隔离的家目录里 —— 隔离失效了"
    );
    assert!(
        !Path::new(shim_dir).exists(),
        "`path show` **什么都不改** —— 连 shim 目录都不该被创建"
    );
}
