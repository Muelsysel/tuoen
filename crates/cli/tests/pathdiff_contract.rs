//! `tuoen path diff` / `tuoen path apply` 的**进程边界**契约测试（票据 #15）。
//!
//! # 为什么这一层的"本机侧"只能读真实注册表（决策 146）
//!
//! `path diff` 的本机侧是**现场采集**出来的（决策 126），而 `capture` 必须在进程边界
//! 之外读真实注册表 —— 没有注入点。要让测试**读不到**它只有两条路：给产品加一个
//! 注入注册表的开关（`AGENTS.md` 明令"不给出厂二进制留后门开关"），或者让
//! `path diff` 接受两份快照文件（那是另一件事，票据没有）。所以这里如实记下：
//!
//! * **本文件只读注册表，绝不写**：每一条触及注册表的用例结尾都断言
//!   **跑前跑后两个作用域的 `Path` 逐字节相同**（[`registry_snapshot`] /
//!   [`assert_registry_unchanged`]）。**真正的写只发生在真机验收脚本里**
//!   （`scripts/acceptance-L1-15.ps1`，它自己备份 + 还原 + 哈希证明）。
//! * 判据（"注册表必须走假后端"）由 core / platform 的用例承担 —— 那里有真夹具。
//!
//! # 为什么每一处都用 [`IsolatedHome`] + 一个固定的 `USERPROFILE`
//!
//! `LOCALAPPDATA` / `APPDATA` 隔离的是**我们会写的东西**（存储、shim 目录）；
//! `USERPROFILE` 隔离的是**当前用户名的来源**（决策 145 的主来源是进程环境）——
//! 不固定它，`currentUsername` 就是开发者的用户名，而"旧名可换的重写"这条用例
//! 的期望值只能靠机器状态凑出来。
//!
//! # 一条不能失败的断言不是断言
//!
//! "跑前跑后逐字节相同"最容易退化成"两份空串相同"（`AGENTS.md` 规矩五）——
//! 所以 [`the_registry_helper_really_reads_both_scopes`] 先证明这个助手真的读到了东西。

mod common;

use std::path::{Path, PathBuf};
use std::process::Output;

use common::{IsolatedHome, json, stdout};
use serde_json::{Value, json as value};
use tuoen_core::capture::SCHEMA_VERSION;
use tuoen_core::pathdiff::{DiffClass, DiffReason};
use tuoen_platform::{EnvBlock, EnvScope, RealEnvBlock, RealFileSystem, RealRegistry};

/// 隔离的 `USERPROFILE` 的最后一段 —— 于是 `currentUsername` 是确定的。
///
/// 它**不是**真机上的用户名，这正是重点：所有"旧名可换"的期望值都从它算出来。
const PROFILE_USER: &str = "tuoen-pathdiff-user";

/// 两条**只存在于自造快照里**的目录。它们不可能是本机 `PATH` 上的东西，
/// 所以它们的类必然是 `add`（决策 127 的存在性优先级）。
const UNIQUE_A: &str = r"C:\tuoen-pathdiff-contract\alpha\bin";
const UNIQUE_B: &str = r"C:\tuoen-pathdiff-contract\beta\bin";

// ─────────────────────────────────────────────────────────────────────────────
// 测试基础设施
// ─────────────────────────────────────────────────────────────────────────────

/// 隔离的家目录 + 一个确定的 `USERPROFILE`。
struct Fixture {
    home: IsolatedHome,
}

impl Fixture {
    fn new(label: &str) -> Self {
        let home = IsolatedHome::new(label);
        std::fs::create_dir_all(home.tuoen_home().join("profile").join(PROFILE_USER))
            .expect("造隔离的 USERPROFILE");
        Self { home }
    }

    /// 跑一次 `tuoen`，环境指向隔离的家目录与固定的 `USERPROFILE`。
    fn run(&self, args: &[&str]) -> Output {
        self.home
            .command()
            .env("USERPROFILE", self.profile())
            .args(args)
            .output()
            .expect("运行 tuoen 应当成功")
    }

    /// `tuoen` 会看到的 `%USERPROFILE%`。
    fn profile(&self) -> PathBuf {
        self.home.tuoen_home().join("profile").join(PROFILE_USER)
    }

    /// 写快照、放临时文件的目录。**在隔离的家目录里面**：一次都不碰真实的家目录。
    fn scratch(&self) -> PathBuf {
        let dir = self.home.tuoen_home().join("scratch");
        std::fs::create_dir_all(&dir).expect("造 scratch 目录");
        dir
    }
}

/// 人类输出里 `[用户级]` 那一行。
///
/// **逐行找**，不靠"整段里含不含"：机器级那一行也带"会写"两个字
/// （`**不会写：机器级要提权**`），整段搜索会把两条结论混成一个。
fn user_scope_line(output: &Output) -> String {
    stdout(output)
        .lines()
        .find(|line| line.contains("[用户级]"))
        .unwrap_or_else(|| panic!("人类输出里没有 `[用户级]` 那一行：{}", describe(output)))
        .to_owned()
}

/// 一次调用的两路输出，用于断言失败时的可读信息。
fn describe(output: &Output) -> String {
    format!(
        "exit={:?} stdout：{} stderr：{}",
        output.status.code(),
        stdout(output),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn contains_cjk(text: &str) -> bool {
    text.chars()
        .any(|c| (0x4E00..=0x9FFF).contains(&(u32::from(c))))
}

/// `--json` 里不许出现时间戳（两次调用必须逐字节相同）。
///
/// 判据是 `YYYY-MM-DDTHH…` 这个**形状**，不是 `captured_at` 这个名字：
/// 换个字段名就能绕过的检查等于没有检查。
fn looks_like_a_timestamp(text: &str) -> bool {
    text.as_bytes().windows(11).any(|window| {
        window[..4].iter().all(|b| b.is_ascii_digit())
            && window[4] == b'-'
            && window[5..7].iter().all(|b| b.is_ascii_digit())
            && window[7] == b'-'
            && window[8..10].iter().all(|b| b.is_ascii_digit())
            && window[10] == b'T'
    })
}

// ─────────────────────────────────────────────────────────────────────────────
// 注册表助手：**只读**，而且必须自证读到了东西
// ─────────────────────────────────────────────────────────────────────────────

/// 两个作用域 `Path` 的**原文 + 类型**快照。
///
/// 逐字节：类型（`sz` / `expand-sz`）与原文一起比 —— 只比原文会漏掉"值没变但类型
/// 被改成了 `REG_EXPAND_SZ`"这种改法，而它会让下一次读取的展开行为改变。
fn registry_snapshot() -> (String, String) {
    let block = RealEnvBlock::new(RealRegistry, RealFileSystem);
    let read = |scope| match block.get(scope, "Path") {
        Some(var) => format!("{:?}|{}", var.reg_type, var.value_raw),
        None => "<没有这个值>".to_owned(),
    };
    (read(EnvScope::User), read(EnvScope::Machine))
}

/// 跑完必须证明**没碰过**。**声称没碰 ≠ 证明没碰**（`AGENTS.md` 规矩四）。
fn assert_registry_unchanged(before: (String, String), what: &str) {
    let after = registry_snapshot();
    assert_eq!(after.0, before.0, "用户级 Path 被改了（{what}）");
    assert_eq!(after.1, before.1, "机器级 Path 被改了（{what}）");
}

// ─────────────────────────────────────────────────────────────────────────────
// 自造快照
// ─────────────────────────────────────────────────────────────────────────────

/// 一份最小的合法 `path.toml`：只带给定的条目（`(作用域, 下标, 值, 是否硬编码用户名)`）。
///
/// **不依赖本机 `PATH` 的具体内容**：选择性应用的断言必须能被任何机器复现。
fn write_snapshot(dir: &Path, name: &str, entries: &[(&str, usize, &str, bool)]) -> PathBuf {
    let mut text = format!(
        "schema_version = {SCHEMA_VERSION}\n\
         captured_at = \"2026-01-01T00:00:00Z\"\n\
         \n\
         [budget]\n\
         raw_user_chars = 0\n\
         raw_machine_chars = 0\n\
         effective_chars = 0\n\
         cliff = 8191\n\
         remaining = 8191\n\
         level = \"ok\"\n"
    );
    for (scope, index, value, has_username) in entries {
        // 单引号字面量：TOML 里反斜杠不需要转义（`C:\a` 就是 `C:\a`）。
        text.push_str(&format!(
            "\n[[entry]]\n\
             scope = '{scope}'\n\
             index = {index}\n\
             owner = \"unknown\"\n\
             raw = '{value}'\n\
             expanded = '{value}'\n\
             quoted = false\n\
             empty = false\n\
             exists = \"no\"\n\
             reparse = \"none\"\n\
             has_vars = false\n\
             has_username = {has_username}\n\
             dup_index = 0\n\
             reg_type = \"sz\"\n"
        ));
    }
    std::fs::write(dir.join(name), text).expect("写自造快照");
    dir.join(name)
}

fn small_snapshot(fixture: &Fixture) -> PathBuf {
    write_snapshot(
        &fixture.scratch(),
        "small.toml",
        &[("user", 0, UNIQUE_A, false), ("user", 1, UNIQUE_B, false)],
    )
}

/// 一份**撑过 8191 悬崖**的快照：三条各 2994 字符的**机器级**条目。
///
/// # 为什么用机器级条目来造这个场景（这一条很重要）
///
/// 机器级**从不落盘**（决策 136）：`path apply` 只写用户级。于是这一条用例即便在
/// `too-long` 守卫被写坏的情况下也**碰不到真机注册表** —— 真跑下去只会走到
/// `apply_rewrite`，而用户级那一次的 `after_entries` 与现状逐条相同，
/// `will_write() == false`，`apply()` 直接返回 `wrote = false` 且不广播。
/// 反过来，用用户级的长条目造这个场景，"守卫被写坏"就等于"真机 `PATH` 被写坏"
/// （超过 8191 之后 `cmd.exe` 忽略整条 `PATH`）—— 一条测试不该有那种失效模式。
fn over_long_snapshot(fixture: &Fixture) -> PathBuf {
    let values: Vec<String> = (0..3)
        .map(|i| format!(r"C:\{}{i}", "a".repeat(2_990)))
        .collect();
    let entries: Vec<(&str, usize, &str, bool)> = values
        .iter()
        .enumerate()
        .map(|(index, value)| ("machine", index, value.as_str(), false))
        .collect();
    write_snapshot(&fixture.scratch(), "over-long.toml", &entries)
}

// ─────────────────────────────────────────────────────────────────────────────
// `--json` 的取用
// ─────────────────────────────────────────────────────────────────────────────

/// `path diff <snapshot> --json` 的 `data`。
fn diff_data(fixture: &Fixture, snapshot: &Path) -> Value {
    let output = fixture.run(&["path", "diff", &snapshot.display().to_string(), "--json"]);
    assert_eq!(output.status.code(), Some(0), "{}", describe(&output));
    let envelope = json(&output);
    assert!(envelope.ok, "diff 必须是成功信封：{:?}", envelope.error);
    assert_eq!(envelope.command, "path.diff");
    envelope.data.expect("成功必须有 data")
}

/// `path apply … --json` 的 `(退出码, data)`。
fn apply_data(fixture: &Fixture, snapshot: &Path, extra: &[&str]) -> (Option<i32>, Value) {
    let path = snapshot.display().to_string();
    let mut args = vec!["path", "apply", path.as_str()];
    args.extend_from_slice(extra);
    let output = fixture.run(&args);
    let envelope = json(&output);
    assert!(envelope.ok, "apply 必须是成功信封：{:?}", envelope.error);
    (
        output.status.code(),
        envelope.data.expect("成功必须有 data"),
    )
}

/// 一个作用域在 `apply --json` 里的那一节。
fn scope_of<'a>(data: &'a Value, scope: &str) -> &'a Value {
    data["scopes"]
        .as_array()
        .expect("scopes 必须是数组")
        .iter()
        .find(|row| row["scope"] == value!(scope))
        .unwrap_or_else(|| panic!("报告里没有 `{scope}` 作用域：{data}"))
}

/// `(scope, index, raw)` —— 本机侧的行，**排好序**（比较两个集合时两边都要先排序）。
fn local_rows(rows: &[Value]) -> Vec<(String, u64, String)> {
    let mut out: Vec<(String, u64, String)> = rows
        .iter()
        .filter(|row| !row["local"].is_null())
        .map(|row| {
            (
                row["local"]["scope"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned(),
                row["local"]["index"].as_u64().unwrap_or_default(),
                row["local"]["raw"].as_str().unwrap_or_default().to_owned(),
            )
        })
        .collect();
    out.sort();
    out
}

// ─────────────────────────────────────────────────────────────────────────────
// 0. 助手自证
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn the_registry_helper_really_reads_both_scopes() {
    // **这一条是全部"没变"断言的护栏**：一个读不到东西的助手会让每一条
    // `before == after` 都退化成"两份空串相同"（`AGENTS.md` 规矩五：
    // "这条断言失败过吗？"）。所以先证明它读到了真实的 `Path`。
    //
    // 判据是平台事实，不是开发机事实：任何 Windows 的机器级 `Path` 都存在且非空。
    let (user, machine) = registry_snapshot();
    assert!(!machine.starts_with("<没有这个值>"), "机器级 Path 必然存在");
    assert!(
        machine.len() > "<没有这个值>".len(),
        "机器级 Path 不可能是空的：{machine}"
    );
    // 用户级可能不存在（那也是一个合法状态）—— 但它必须**有类型前缀**。
    assert!(user.contains('|'), "用户级的判据必须带类型：{user}");
}

// ─────────────────────────────────────────────────────────────────────────────
// 1. 空选择：拒绝执行，而且**在读注册表之前**
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn an_empty_selection_is_refused_before_anything_is_read() {
    let fixture = Fixture::new("pathdiff-empty-selection");
    let before = registry_snapshot();
    // **给一个根本不存在的快照路径**：如果实现先读了注册表或先读了快照，
    // 这里拿到的就会是另一个错误码。判据是"拒绝的理由在任何机器上都成立，
    // 所以没有理由先去做任何 I/O"。
    let missing = fixture.scratch().join("nope").join("path.toml");

    let output = fixture.run(&["path", "apply", &missing.display().to_string(), "--json"]);
    assert_eq!(output.status.code(), Some(1), "{}", describe(&output));
    let envelope = json(&output);
    assert!(!envelope.ok);
    let error = envelope.error.expect("失败必须有 error");
    assert_eq!(error.code, "nothing-selected");

    let data = envelope.data.expect("拒绝要带 data（理由就在里面）");
    assert!(
        data["scopes"].is_null(),
        "**没执行**不是「什么都没做」：data 里不许有 scopes：{data}"
    );
    assert!(data["rows"].is_null(), "data 里不许有 rows：{data}");
    assert_eq!(data["dryRun"], value!(false));
    assert_eq!(data["selection"]["classes"], value!([]));
    assert_eq!(data["selection"]["picks"], value!([]));
    assert_registry_unchanged(before, "空选择");
}

// ─────────────────────────────────────────────────────────────────────────────
// 2. 用法错误：不认识的类由 clap 拒绝（退出码 2）
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn an_unknown_class_is_a_usage_error() {
    let fixture = Fixture::new("pathdiff-unknown-class");
    let snapshot = small_snapshot(&fixture);
    let before = registry_snapshot();

    let output = fixture.run(&[
        "path",
        "apply",
        &snapshot.display().to_string(),
        "--only",
        "nonsense",
        "--dry-run",
    ]);
    assert_eq!(output.status.code(), Some(2), "{}", describe(&output));
    // 退出码 2 是 clap 的用法错误 —— 它必须**一个字都不写**。
    assert_registry_unchanged(before, "--only nonsense");
}

// ─────────────────────────────────────────────────────────────────────────────
// 3–4. 读目标快照失败：两个错误码必须分得开
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn a_missing_snapshot_is_reported_as_path_snapshot_io() {
    let fixture = Fixture::new("pathdiff-missing-snapshot");
    let before = registry_snapshot();
    let missing = fixture.scratch().join("nope").join("path.toml");

    let output = fixture.run(&["path", "diff", &missing.display().to_string(), "--json"]);
    assert_eq!(output.status.code(), Some(1), "{}", describe(&output));
    let envelope = json(&output);
    assert_eq!(envelope.error.expect("error").code, "path-snapshot-io");
    assert_registry_unchanged(before, "快照不存在");
}

#[test]
fn a_broken_toml_is_reported_as_path_snapshot_toml() {
    let fixture = Fixture::new("pathdiff-broken-toml");
    let before = registry_snapshot();
    // 读得到，但不是一份合法的 `PathFile` —— 与"文件不在"要做的下一步完全不同。
    let bad = fixture.scratch().join("bad.toml");
    std::fs::write(&bad, "[[entry]").expect("写坏 TOML");

    let output = fixture.run(&["path", "diff", &bad.display().to_string(), "--json"]);
    assert_eq!(output.status.code(), Some(1), "{}", describe(&output));
    let envelope = json(&output);
    assert_eq!(envelope.error.expect("error").code, "path-snapshot-toml");
    assert_registry_unchanged(before, "坏 TOML");
}

// ─────────────────────────────────────────────────────────────────────────────
// 5–6. 成功载荷的形状：无中文、无 `message`、无时间戳、逐字节稳定
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn the_success_payload_has_no_chinese_no_message_and_no_timestamp() {
    let fixture = Fixture::new("pathdiff-payload-shape");
    let before = registry_snapshot();
    let snapshot = small_snapshot(&fixture);

    let output = fixture.run(&["path", "diff", &snapshot.display().to_string(), "--json"]);
    assert_eq!(output.status.code(), Some(0), "{}", describe(&output));
    let text = stdout(&output);

    assert!(
        !contains_cjk(&text),
        "成功载荷里不许有中文（决策 35）：{text}"
    );
    assert!(
        !text.contains("\"message\""),
        "成功载荷里不许有 message 键：{text}"
    );
    assert!(
        !looks_like_a_timestamp(&text),
        "`--json` 里不许有时间戳（两次调用必须逐字节相同）：{text}"
    );
    assert_registry_unchanged(before, "成功载荷");
}

#[test]
fn two_diffs_in_a_row_are_byte_identical() {
    let fixture = Fixture::new("pathdiff-idempotent");
    let before = registry_snapshot();
    let snapshot = small_snapshot(&fixture);
    let path = snapshot.display().to_string();

    let first = fixture.run(&["path", "diff", &path, "--json"]);
    let second = fixture.run(&["path", "diff", &path, "--json"]);
    assert_eq!(first.status.code(), Some(0), "{}", describe(&first));
    assert_eq!(
        stdout(&first),
        stdout(&second),
        "同一台机器上的两次 diff 必须逐字节相同"
    );
    assert_registry_unchanged(before, "两次 diff");
}

// ─────────────────────────────────────────────────────────────────────────────
// 7–9. 逐条不变量：计数、类与理由、id
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn counts_match_the_rows_they_describe() {
    let fixture = Fixture::new("pathdiff-counts");
    let before = registry_snapshot();
    let snapshot = small_snapshot(&fixture);
    let data = diff_data(&fixture, &snapshot);
    let rows = data["rows"].as_array().expect("rows 必须是数组");

    let mut tally: std::collections::BTreeMap<&str, usize> = std::collections::BTreeMap::new();
    for row in rows {
        *tally
            .entry(row["class"].as_str().expect("class 必须是字符串"))
            .or_default() += 1;
    }

    let counts = data["counts"].as_object().expect("counts 必须是对象");
    assert_eq!(
        counts.len(),
        DiffClass::ALL.len(),
        "六个键一个不多一个不少：{counts:?}"
    );
    for (key, got) in counts {
        // `case-only` 的键是 `caseOnly`（camelCase）—— 除这一处改名之外，
        // 键与 slug 逐字相同，而 slug 由 core 拥有。
        let slug = if key == "caseOnly" { "case-only" } else { key };
        let class = DiffClass::parse(slug).unwrap_or_else(|| panic!("`{key}` 不是一个类"));
        assert_eq!(
            got.as_u64().unwrap_or_default() as usize,
            tally.get(class.as_str()).copied().unwrap_or(0),
            "counts.{key} 与逐条数出来的不一致"
        );
    }
    assert_eq!(
        counts
            .values()
            .map(|v| v.as_u64().unwrap_or(0) as usize)
            .sum::<usize>(),
        rows.len(),
        "`sum(counts) == rows.len()`"
    );
    assert_registry_unchanged(before, "counts 与 rows 对照");
}

#[test]
fn every_class_equals_its_reasons_class() {
    let fixture = Fixture::new("pathdiff-class-reason");
    let before = registry_snapshot();
    let snapshot = small_snapshot(&fixture);
    let data = diff_data(&fixture, &snapshot);
    let rows = data["rows"].as_array().expect("rows");

    // **不抄一张 slug 表**：九个理由由 core 的类型列出，`class()` 也是 core 的。
    // 抄一份的那天起，core 加了一个理由而这里还照着旧表断言。
    const REASONS: [DiffReason; 9] = [
        DiffReason::Identical,
        DiffReason::OnlyInTarget,
        DiffReason::OnlyInLocal,
        DiffReason::PositionDiffers,
        DiffReason::CaseDiffers,
        DiffReason::EmptySegment,
        DiffReason::Duplicate,
        DiffReason::Dangling,
        DiffReason::UsernameHardcoded,
    ];

    for row in rows {
        let slug = row["reason"].as_str().expect("reason 必须是字符串");
        let reason = REASONS
            .iter()
            .find(|reason| reason.as_str() == slug)
            .unwrap_or_else(|| panic!("`{slug}` 不是 core 的理由之一"));
        assert_eq!(
            row["class"],
            value!(reason.class().as_str()),
            "class == reason.class() 是硬不变量（决策 127）：{row}"
        );
    }
    assert_registry_unchanged(before, "类与理由对照");
}

#[test]
fn ids_are_unique_and_shaped_like_a_scope_and_an_index() {
    let fixture = Fixture::new("pathdiff-ids");
    let before = registry_snapshot();
    let snapshot = small_snapshot(&fixture);
    let data = diff_data(&fixture, &snapshot);
    let rows = data["rows"].as_array().expect("rows");

    let mut seen: Vec<&str> = Vec::new();
    for row in rows {
        let id = row["id"].as_str().expect("id 必须是字符串");
        let (scope, index) = id
            .split_once(':')
            .unwrap_or_else(|| panic!("id `{id}` 里没有 `:`"));
        assert!(
            scope == "machine" || scope == "user",
            "id `{id}` 的作用域不是 machine/user"
        );
        let digits = index.strip_prefix('+').unwrap_or(index);
        assert!(
            !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit()),
            "id `{id}` 的序号不是数字"
        );
        // `+` 那半边是必需的（决策 143）：同一下标既可能有一条 remove、又可能有一条 add。
        let local_index = row["local"]["index"].as_u64();
        let has_plus = index.starts_with('+');
        assert_eq!(
            has_plus,
            local_index.is_none(),
            "只有目标侧才有的行才带 `+`：{id} / {row}"
        );
        seen.push(id);
    }
    let mut sorted = seen.clone();
    sorted.sort_unstable();
    let len_before = sorted.len();
    sorted.dedup();
    assert_eq!(
        sorted.len(),
        len_before,
        "所有 id 必须互不相同（否则一次 `--pick` 会选中两条）：{seen:?}"
    );
    assert!(!seen.is_empty(), "本机 PATH 不可能一条都没有");
    assert_registry_unchanged(before, "id 形状");
}

// ─────────────────────────────────────────────────────────────────────────────
// 10. `--pick` 只影响那一行
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn picking_one_id_applies_exactly_that_row() {
    let fixture = Fixture::new("pathdiff-pick");
    let before = registry_snapshot();
    let snapshot = small_snapshot(&fixture);

    let data = diff_data(&fixture, &snapshot);
    let rows = data["rows"].as_array().expect("rows");
    let adds: Vec<&Value> = rows
        .iter()
        .filter(|row| row["class"] == value!("add"))
        .collect();
    assert_eq!(adds.len(), 2, "自造快照里有两条本机没有的目录：{rows:#?}");
    let picked = adds[0]["id"].as_str().expect("id").to_owned();
    assert!(
        picked.starts_with("user:+"),
        "只有目标侧才有的行带 `+`（决策 143）：{picked}"
    );

    let (code, payload) = apply_data(
        &fixture,
        &snapshot,
        &["--pick", &picked, "--dry-run", "--json"],
    );
    assert_eq!(code, Some(0));

    let user = scope_of(&payload, "user");
    let applied = user["applied"].as_array().expect("applied");
    assert_eq!(applied.len(), 1, "`--pick` 一条就只该动一条：{applied:#?}");
    assert_eq!(applied[0]["id"], value!(picked.as_str()));
    assert_eq!(applied[0]["class"], value!("add"));
    assert_eq!(applied[0]["value"], value!(UNIQUE_A));

    // **另一条 `add` 没有被插进来** —— 那才是"只影响那一行"的证据。
    let after = user["afterEntries"].as_array().expect("afterEntries");
    assert!(
        !after.iter().any(|entry| entry == &value!(UNIQUE_B)),
        "没被选中的 add 不许被插进去：{after:#?}"
    );
    assert_registry_unchanged(before, "--pick --dry-run");
}

// ─────────────────────────────────────────────────────────────────────────────
// 11. `--dry-run` 一个字节都不写
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn a_dry_run_writes_nothing() {
    let fixture = Fixture::new("pathdiff-dry-run");
    let before = registry_snapshot();
    let snapshot = small_snapshot(&fixture);

    let (code, payload) = apply_data(
        &fixture,
        &snapshot,
        &["--only", "add", "--dry-run", "--json"],
    );
    assert_eq!(code, Some(0));
    assert_eq!(payload["dryRun"], value!(true));
    assert_eq!(payload["wrote"], value!(false), "演练没有落盘结果");
    assert_eq!(payload["broadcastReplies"], value!(0), "演练一次广播都不发");
    assert_eq!(payload["visibility"], value!("restart-required"));
    // 演练的那几个数字来自**计划**（决策 139），所以它们必须与 afterRaw 对得上。
    let user = scope_of(&payload, "user");
    let after_raw = user["afterRaw"].as_str().expect("afterRaw");
    assert_eq!(
        payload["chars"].as_u64().unwrap_or_default() as usize,
        after_raw.chars().count(),
        "演练的 chars 必须就是 afterRaw 的长度"
    );
    assert_eq!(
        payload["budget"]["rawUserChars"]
            .as_u64()
            .unwrap_or_default() as usize,
        after_raw.chars().count(),
        "预算的 rawUserChars = 重建后的用户级原文长度"
    );
    assert_eq!(payload["budget"]["cliff"], value!(8191));
    assert_registry_unchanged(before, "--only add --dry-run");
}

// ─────────────────────────────────────────────────────────────────────────────
// 12. `--only fix`：remove 类条目连位置都不动
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn selecting_only_fix_does_not_move_remove_rows() {
    let fixture = Fixture::new("pathdiff-only-fix");
    let before = registry_snapshot();
    let snapshot = small_snapshot(&fixture);

    let data = diff_data(&fixture, &snapshot);
    let rows = data["rows"].as_array().expect("rows");
    // 只看**用户级**：那才是会被写回的那一侧（机器级只算不写，决策 136）。
    // 基底 = 本机用户级的行，按 index 排好 —— 那就是重建的起点。
    let base: Vec<(u64, String)> = local_rows(rows)
        .into_iter()
        .filter(|(scope, _, _)| scope == "user")
        .map(|(_, index, raw)| (index, raw))
        .collect();
    assert!(
        base.len() > 5,
        "本机用户级 `PATH` 不可能只有几条：{base:#?}"
    );
    let remove_raws: Vec<String> = base
        .iter()
        .filter(|(index, _)| {
            rows.iter().any(|row| {
                row["class"] == value!("remove")
                    && row["local"]["scope"] == value!("user")
                    && row["local"]["index"].as_u64() == Some(*index)
            })
        })
        .map(|(_, raw)| raw.clone())
        .collect();
    assert!(
        remove_raws.len() > 5,
        "这个小快照下本机绝大多数条目都该是 remove，实际 {}",
        remove_raws.len()
    );

    let (code, payload) = apply_data(
        &fixture,
        &snapshot,
        &["--only", "fix", "--dry-run", "--json"],
    );
    assert_eq!(code, Some(0));
    let user = scope_of(&payload, "user");
    let after: Vec<String> = user["afterEntries"]
        .as_array()
        .expect("afterEntries")
        .iter()
        .map(|entry| entry.as_str().unwrap_or_default().to_owned())
        .collect();

    // "位置不动"的诚实口径（决策 131）：**不丢、不挪**。在它前面丢掉一条 fix 行，
    // 它的**下标**当然会变 —— 那是"在一个有序列表里删除"的必然结果。所以判据是
    // "每一条都还在，而且相对顺序与基底一致"。
    let mut cursor = 0;
    for raw in &remove_raws {
        let found = after[cursor..]
            .iter()
            .position(|entry| entry == raw)
            .unwrap_or_else(|| panic!("remove 行 `{raw}` 被删掉了、或被挪到了前面：{after:#?}"));
        cursor += found + 1;
    }

    // 而且 `applied` 里只有 fix 类（选中的类就是它）。
    for row in user["applied"].as_array().expect("applied") {
        assert_eq!(row["class"], value!("fix"), "只选 fix，别的类不许动：{row}");
    }
    assert_registry_unchanged(before, "--only fix --dry-run");
}

// ─────────────────────────────────────────────────────────────────────────────
// 13. 机器级的改动：**算出来但不写**
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn machine_level_changes_are_computed_but_never_written() {
    let fixture = Fixture::new("pathdiff-elevation");
    let before = registry_snapshot();
    // 自造快照**只有用户级条目**，所以本机机器级的每一条都是 `remove` ——
    // 于是"机器级一定有选中的改动"是**确定的**，不依赖这台机器的 `PATH` 长什么样。
    let snapshot = small_snapshot(&fixture);

    let (code, payload) = apply_data(
        &fixture,
        &snapshot,
        &["--only", "remove", "--dry-run", "--json"],
    );
    assert_eq!(code, Some(0));
    let machine = scope_of(&payload, "machine");
    let applied = machine["applied"].as_array().expect("applied");
    assert!(
        !applied.is_empty(),
        "机器级的每一条都该是 remove：{machine:#?}"
    );
    assert_eq!(
        machine["requiresElevation"],
        value!(true),
        "机器级有改动就要标记提权（决策 136）"
    );
    assert!(
        machine["afterRaw"].is_string(),
        "机器级的 afterRaw 也要被算出来（哪怕结果是空串）：{machine:#?}"
    );
    // 自造快照一条机器级条目都没有，所以本机机器级**几乎**全部是 remove
    // （唯一的例外是空条目：决策 130 把 `empty-segment` 排在存在性之前）。
    let classes: Vec<&str> = applied
        .iter()
        .filter_map(|row| row["class"].as_str())
        .collect();
    assert!(
        classes
            .iter()
            .all(|class| *class == "remove" || *class == "fix"),
        "机器级不该出现别的类：{classes:?}"
    );
    assert!(
        classes.iter().filter(|class| **class == "remove").count() > 5,
        "机器级绝大多数条目都该是 remove：{classes:?}"
    );
    let top = payload["requiresElevation"]
        .as_array()
        .expect("requiresElevation");
    assert_eq!(
        top.len(),
        applied.len(),
        "顶层 requiresElevation 要逐条列出机器级的改动"
    );
    for row in top {
        assert!(
            row["id"]
                .as_str()
                .is_some_and(|id| id.starts_with("machine:"))
        );
        assert!(row["class"].as_str().is_some_and(|class| class == "remove"));
    }
    // **算出来了 ≠ 写下去了**：两个作用域逐字节未变。
    assert_registry_unchanged(before, "--only remove --dry-run（机器级只算不写）");
}

// ─────────────────────────────────────────────────────────────────────────────
// 14. 选了但什么都没做的类，必须被说出来
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn a_selected_class_that_does_nothing_is_reported() {
    let fixture = Fixture::new("pathdiff-noop-classes");
    let before = registry_snapshot();
    let snapshot = small_snapshot(&fixture);

    let (code, payload) = apply_data(
        &fixture,
        &snapshot,
        &["--only", "case-only", "--dry-run", "--json"],
    );
    assert_eq!(code, Some(0), "选了 `case-only` 是成功，不是失败");
    let noop = payload["noOpClasses"].as_array().expect("noOpClasses");
    assert!(
        noop.contains(&value!("case-only")),
        "决策 128：改大小写零收益、纯风险 —— 选中它什么都不做，但**必须说出来**：{noop:?}"
    );
    let user = scope_of(&payload, "user");
    assert!(
        user["applied"].as_array().expect("applied").is_empty(),
        "`case-only` 不做任何变换"
    );
    assert_registry_unchanged(before, "--only case-only --dry-run");
}

// ─────────────────────────────────────────────────────────────────────────────
// 15. 一个没匹配上的 `--pick` 不许静默
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn a_pick_that_matches_nothing_is_said_out_loud() {
    let fixture = Fixture::new("pathdiff-pick-unknown");
    let before = registry_snapshot();
    let snapshot = small_snapshot(&fixture);
    let path = snapshot.display().to_string();

    let output = fixture.run(&[
        "path",
        "apply",
        &path,
        "--pick",
        "user:9999",
        "--dry-run",
        "--json",
    ]);
    assert_eq!(output.status.code(), Some(0), "{}", describe(&output));
    // stdout 上的契约一个字节都没变：仍然是一个成功的信封、空的 `applied`。
    let envelope = json(&output);
    assert!(envelope.ok, "{:?}", envelope.error);
    let data = envelope.data.expect("data");
    assert!(
        scope_of(&data, "user")["applied"]
            .as_array()
            .expect("applied")
            .is_empty()
    );

    // 但那个 id 必须被**说出来**：一个不存在的 id 会让 `applied` 为空而退出码是 0 ——
    // 那正是"看起来在工作"（`AGENTS.md`：一句看起来完全合理的错话比崩溃更难发现）。
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("user:9999"),
        "没匹配上的 id 必须点名：{stderr}"
    );
    assert_registry_unchanged(before, "--pick 一个不存在的 id");
}

// ─────────────────────────────────────────────────────────────────────────────
// 16. 真机：`capture` 出来的快照能真的 diff 通
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn the_real_machine_can_be_captured_and_diffed() {
    // 这一条断言的是**真实现的接线是通的**：本机侧来自真实注册表（`RegEnumValueW`、
    // 不展开读），目标侧来自一份真的写出来的 `path.toml`。任何固定装置都发现不了
    // "注册表路径拼错了 → 静默读到空"。
    let fixture = Fixture::new("pathdiff-real-machine");
    let before = registry_snapshot();
    let out = fixture.scratch().join("tuoen.d");

    let capture = fixture.run(&[
        "capture",
        "--only",
        "path",
        "--out",
        &out.display().to_string(),
    ]);
    assert_eq!(capture.status.code(), Some(0), "{}", describe(&capture));
    let snapshot = out.join("path.toml");
    assert!(snapshot.exists(), "capture 必须写出 path.toml");

    let data = diff_data(&fixture, &snapshot);
    let rows = data["rows"].as_array().expect("rows");
    assert!(
        !rows.is_empty(),
        "一台能编译这个仓库的机器不可能 `PATH` 一条都没有 —— \
         这几乎一定意味着注册表那一侧静默读到了空"
    );
    let counts = data["counts"].as_object().expect("counts");
    assert_eq!(counts.len(), 6, "六个类一个都不能少：{counts:?}");
    // 自己刚从这台机器采集的、自己 diff 自己：应当**一条 add / remove 都没有**。
    // （它同时证明"两侧同一形状"真的成立 —— 形状不同必然出现假的 add/remove。）
    assert_eq!(counts["add"], value!(0), "自己对自己不该有 add：{counts:?}");
    assert_eq!(
        counts["remove"],
        value!(0),
        "自己对自己不该有 remove：{counts:?}"
    );
    assert_eq!(data["currentUsername"], value!(PROFILE_USER));
    assert_registry_unchanged(before, "capture + diff");
}

// ─────────────────────────────────────────────────────────────────────────────
// 16–17. 8191 悬崖：预览照给、真写拒绝（Lead 的补充裁决）
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn an_over_long_dry_run_still_shows_the_whole_plan() {
    // **`--dry-run` 不许被 `exceeded` 拒绝**：`path apply` 是唯一能产出"重建后的完整
    // 列表"的命令（`path diff` 只给逐条分类）。把一个 `PATH` 已经超悬崖的用户的预览
    // 拿走，他就连"要删哪几条才能降下来"都无从知道 —— 那不是"预览撒谎"，是"预览被拿走"。
    let fixture = Fixture::new("pathdiff-too-long-dry-run");
    let before = registry_snapshot();
    let snapshot = over_long_snapshot(&fixture);

    let (code, payload) = apply_data(
        &fixture,
        &snapshot,
        &["--only", "add", "--dry-run", "--json"],
    );
    assert_eq!(code, Some(0), "预览在 `exceeded` 时必须成功（真写才拒绝）");
    assert_eq!(payload["dryRun"], value!(true));
    assert_eq!(payload["budget"]["level"], value!("exceeded"));
    assert!(
        payload["budget"]["effectiveChars"]
            .as_u64()
            .unwrap_or_default()
            > 8191,
        "这份快照必须真的把它顶过悬崖：{}",
        payload["budget"]
    );
    // 计划完整：机器级那三条长条目要**看得见**（它们就是超悬崖的原因）。
    let machine = scope_of(&payload, "machine");
    assert_eq!(
        machine["applied"].as_array().expect("applied").len(),
        3,
        "三条机器级长条目都该被算进计划：{machine:#?}"
    );
    assert_eq!(
        machine["afterRaw"]
            .as_str()
            .unwrap_or_default()
            .chars()
            .count(),
        payload["budget"]["rawMachineChars"]
            .as_u64()
            .unwrap_or_default() as usize,
        "`afterRaw` 就是预算里那个机器级原文 —— 用户照着它才能决定少选哪几条"
    );
    assert_registry_unchanged(before, "exceeded 的 dry-run");
}

#[test]
fn an_over_long_write_is_refused_but_the_plan_is_still_there() {
    // **真写**在 `exceeded` 时拒绝（`too-long`，退出 1），但**载荷里仍然是完整计划**：
    // 拒绝的理由就在那份计划里。这一条走的是"不给 `--dry-run`"的真路径 ——
    // 它之所以**不可能**碰真机注册表，是因为超悬崖的那三条是**机器级**条目
    // （见 [`over_long_snapshot`] 的文档）：用户级那一次 `will_write() == false`。
    let fixture = Fixture::new("pathdiff-too-long-write");
    let before = registry_snapshot();
    let snapshot = over_long_snapshot(&fixture);

    let output = fixture.run(&[
        "path",
        "apply",
        &snapshot.display().to_string(),
        "--only",
        "add",
        "--json",
    ]);
    assert_eq!(output.status.code(), Some(1), "{}", describe(&output));
    let envelope = json(&output);
    assert!(!envelope.ok);
    let error = envelope.error.expect("失败必须有 error");
    assert_eq!(error.code, "too-long");
    let data = envelope.data.expect("拒绝也要带**完整计划**");
    assert!(
        data["scopes"].is_array(),
        "`data.scopes` 必须在场：拒绝的理由就在那份计划里"
    );
    assert_eq!(data["budget"]["level"], value!("exceeded"));
    assert_eq!(data["wrote"], value!(false), "拒绝了就没有落盘结果");
    assert_eq!(data["broadcastReplies"], value!(0), "拒绝了就不许广播");
    assert_eq!(
        scope_of(&data, "machine")["applied"]
            .as_array()
            .expect("applied")
            .len(),
        3
    );
    // 这一句同时钉住"消息里说清要删掉几条 / 当前多少字符 / 悬崖 8191"。
    let message = error.message.as_str();
    for needle in ["至少要少", "8191", "拒绝写入"] {
        assert!(
            message.contains(needle),
            "`too-long` 的消息里必须有 `{needle}`：{message}"
        );
    }
    assert_registry_unchanged(before, "exceeded 的真写（被拒绝）");
}

// ─────────────────────────────────────────────────────────────────────────────
// 18. 用户级那一行的"会写 / 不会写"必须与计划一致
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn the_user_scope_line_says_whether_it_will_really_be_written() {
    // 判据是 `PathPlan::will_write()`，**不是"这个作用域可写"**：恒印"会写"在
    // `applied` 为空、`wrote == false` 的形态下是一句与事实相反的话，而下面
    // `print_visibility` 那句"计划与现状一致"只把它盖住了一半。
    let fixture = Fixture::new("pathdiff-will-write-line");
    let before = registry_snapshot();
    let snapshot = small_snapshot(&fixture);
    let path = snapshot.display().to_string();

    // ① no-op 形态：`case-only` 一个 `applied` 都不产出（决策 128），所以用户级
    //    **真的不会写** —— 标签不许印"会写"，而且要与下面那句话一致。
    let noop = fixture.run(&["path", "apply", &path, "--only", "case-only", "--dry-run"]);
    assert_eq!(noop.status.code(), Some(0), "{}", describe(&noop));
    let line = user_scope_line(&noop);
    assert!(
        !line.contains("**会写**"),
        "用户级这一节什么都没有改，标签不许印「会写」：{line}"
    );
    assert!(
        line.contains("**不会写（计划与现状一致）**"),
        "标签要说清为什么不写：{line}"
    );
    let text = stdout(&noop);
    assert!(
        text.contains("以上是计划，**什么都没有写**"),
        "标签与 `print_visibility` 那句不许自相矛盾：{text}"
    );

    // ② 反例：用户级**真的会写**的形态必须照旧印"会写"。
    //
    //    这里用的是 `--only add`（自造快照里那两条新目录会真的插进用户级）。
    //    **不能拿 `--only fix` 配这份小快照**：那份快照里本机用户级的每一条都是
    //    `only-in-local`（名次 2 在名次 3/5 之前），所以 `fix` 在用户级**一条都没有**
    //    —— 那时"不会写"才是真话。真机上 `--only fix` 会写，见下面的 ③。
    let add = fixture.run(&["path", "apply", &path, "--only", "add", "--dry-run"]);
    assert_eq!(add.status.code(), Some(0), "{}", describe(&add));
    let line = user_scope_line(&add);
    assert!(
        line.contains("**会写**"),
        "用户级真的会写（有 applied 条目），标签必须说会写：{line}"
    );
    assert!(
        !line.contains("**不会写"),
        "有改动却印「不会写」是另一种撒谎：{line}"
    );

    // ③ 真机快照 + `--only fix`：标签必须与**同一份输出里的 `applied` 行**一致。
    //    这条判据在任何机器上都成立（干净机器上用户级可能没有 `fix` 行，那就该印
    //    "不会写"），而本机上有 9 条 —— 它证明真机上那个"会写"的形态没被改坏。
    let out = fixture.scratch().join("tuoen.d");
    let capture = fixture.run(&[
        "capture",
        "--only",
        "path",
        "--out",
        &out.display().to_string(),
    ]);
    assert_eq!(capture.status.code(), Some(0), "{}", describe(&capture));
    let real = out.join("path.toml").display().to_string();
    let fix = fixture.run(&["path", "apply", &real, "--only", "fix", "--dry-run"]);
    assert_eq!(fix.status.code(), Some(0), "{}", describe(&fix));
    let line = user_scope_line(&fix);
    let user_applied = stdout(&fix)
        .lines()
        .any(|row| row.trim_start().starts_with("· [user:"));
    assert_eq!(
        user_applied,
        line.contains("**会写**"),
        "标签必须与同一份输出里的 `applied` 行一致（本机有 user applied = {user_applied}）：{line}"
    );
    assert_registry_unchanged(before, "用户级标签");
}
