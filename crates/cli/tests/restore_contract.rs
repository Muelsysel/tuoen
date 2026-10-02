//! `tuoen restore` 的**进程边界**契约测试（票据 #16）。
//!
//! # 这一层能证明什么、不能证明什么
//!
//! `restore` 的本机侧是**现场采集**出来的（与 `path diff` 同一条：决策 126 的沿用），
//! 而 `capture` 必须在进程边界之外读真实注册表 —— 没有注入点。所以这里如实记下：
//!
//! * **本文件只读注册表，绝不写**：每一条触及注册表的用例结尾都断言
//!   **跑前跑后两个作用域的 `Path` 逐字节相同**（[`registry_snapshot`] /
//!   [`assert_registry_unchanged`]）。**真正的写只发生在真机验收脚本里**
//!   （`scripts/acceptance-L1-16.ps1`，它自己备份 + 还原 + 哈希证明）。
//! * **一次 `--apply` 都不跑**：`--apply` 会写 `HKCU\Environment`（环境变量段）
//!   或下载安装（工具段），两者都不该发生在 `cargo test` 里。
//!   计划形态（默认 / `--dry-run`）与 `--apply` 读的是**同一份计划**
//!   （决策 150：`plan()` 是纯函数），所以"计划对不对"在这里能证明；
//!   "写下去对不对"由验收脚本证明。
//! * **不访问网络**：`--apply` 之外没有任何网络路径，而计划**永远不需要网络**
//!   （决策 157）。
//!
//! # 为什么每一处都用 [`IsolatedHome`] + 一个固定的 `USERPROFILE`
//!
//! `LOCALAPPDATA` / `APPDATA` 隔离的是**我们会写的东西**（存储、shim 目录）；
//! `USERPROFILE` 隔离的是**当前用户名的来源**（决策 145 的主来源是进程环境）。
//!
//! # 一条不能失败的断言不是断言
//!
//! "跑前跑后逐字节相同"最容易退化成"两份空串相同"（`AGENTS.md` 规矩五）——
//! 所以 [`the_registry_helper_really_reads_both_scopes`] 先证明这个助手真的读到了东西。

mod common;

use std::path::{Path, PathBuf};
use std::process::Output;

use common::{IsolatedHome, json, stderr, stdout};
use serde_json::Value;
use tuoen_core::capture::SCHEMA_VERSION;
use tuoen_platform::{EnvBlock, EnvScope, RealEnvBlock, RealFileSystem, RealRegistry};

/// 隔离的 `USERPROFILE` 的最后一段 —— 于是 `currentUsername` 是确定的。
const PROFILE_USER: &str = "tuoen-restore-user";

/// 一条**只存在于自造快照里**的目录：它必然是 `add`（决策 127 的存在性优先级）。
const UNIQUE_DIR: &str = r"C:\tuoen-restore-contract\only-in-snapshot\bin";

/// 一条**机器级**的目录：`add` 在机器级 → 要提权、只报告（决策 136）。
const MACHINE_DIR: &str = r"C:\tuoen-restore-contract\machine-scope\bin";

/// 一个凭据形状的串。**它绝不许出现在任何输出里**（决策 153）。
///
/// # 为什么是 `concat!` 拼出来的，而不是一个字面量
///
/// `glpat-` 后面直接跟一长串字符的**字面量**会被 GitHub push protection 当成真 token
/// （GH013）—— 本仓库已经被拦过一次（见 `crates/core/src/capture/secrets.rs` 里那次
/// 的记载）。拼接保住"形状正确"（前缀 + 长度都像真的，所以"不泄露"这条断言仍然有意义），
/// 又不给扫描器一个候选。
const SECRET: &str = concat!("glpat-", "AbCdEfGhIjKlMnOpQrSt");

/// 第三方版本管理器自己的文件。restore **一个字都不许写**（决策 154）。
const NVM_SETTINGS: &str = "root: C:\\nvm\r\npath: C:\\nvm\\nodejs\r\n";

// ─────────────────────────────────────────────────────────────────────────────
// 测试基础设施
// ─────────────────────────────────────────────────────────────────────────────

/// 隔离的家目录 + 一个确定的 `USERPROFILE` + 一份 nvm4w 的 `settings.txt`。
struct Fixture {
    home: IsolatedHome,
}

impl Fixture {
    fn new(label: &str) -> Self {
        let home = IsolatedHome::new(label);
        std::fs::create_dir_all(home.tuoen_home().join("profile").join(PROFILE_USER))
            .expect("造隔离的 USERPROFILE");
        // nvm4w 的地盘：`%APPDATA%\nvm\settings.txt`。
        let nvm = home.roaming_app_data().join("nvm");
        std::fs::create_dir_all(&nvm).expect("造 nvm 目录");
        std::fs::write(nvm.join("settings.txt"), NVM_SETTINGS).expect("写 settings.txt");
        Self { home }
    }

    fn command(&self) -> std::process::Command {
        let mut command = self.home.command();
        command.env("USERPROFILE", self.profile());
        command
    }

    fn run(&self, args: &[&str]) -> Output {
        self.command()
            .args(args)
            .output()
            .expect("运行 tuoen 应当成功")
    }

    /// 在指定的工作目录里跑（`restore` 的默认 `<dir>` 是相对路径 `tuoen.d`，
    /// 所以"在哪个目录里跑"是一个真实的输入维度）。
    fn run_in(&self, dir: &Path, args: &[&str]) -> Output {
        self.command()
            .current_dir(dir)
            .args(args)
            .output()
            .expect("运行 tuoen 应当成功")
    }

    fn profile(&self) -> PathBuf {
        self.home.tuoen_home().join("profile").join(PROFILE_USER)
    }

    fn scratch(&self) -> PathBuf {
        let dir = self.home.tuoen_home().join("scratch");
        std::fs::create_dir_all(&dir).expect("造 scratch 目录");
        dir
    }

    /// 造一份快照目录（`<scratch>/<name>`），写进给定的文件。
    fn snapshot(&self, name: &str, files: &[(&str, String)]) -> PathBuf {
        let dir = self.scratch().join(name);
        std::fs::create_dir_all(&dir).expect("造快照目录");
        for (file, text) in files {
            std::fs::write(dir.join(file), text).expect("写快照文件");
        }
        dir
    }

    fn nvm_settings(&self) -> String {
        std::fs::read_to_string(
            self.home
                .roaming_app_data()
                .join("nvm")
                .join("settings.txt"),
        )
        .expect("读 settings.txt")
    }
}

/// 出错时把三个流都打出来 —— 断言消息里只有一句"期望 0 实际 1"是查不动的。
fn describe(output: &Output) -> String {
    format!(
        "退出码 {:?}\n--- stdout ---\n{}\n--- stderr ---\n{}",
        output.status.code(),
        stdout(output),
        stderr(output)
    )
}

// ─────────────────────────────────────────────────────────────────────────────
// 注册表护栏
// ─────────────────────────────────────────────────────────────────────────────

/// 两个作用域的 `Path`（类型 + 原文）。**判据是平台事实，不是开发机事实**。
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

#[test]
fn the_registry_helper_really_reads_both_scopes() {
    // **这一条是全部"没变"断言的护栏**：一个读不到东西的助手会让每一条
    // `before == after` 都退化成"两份空串相同"（`AGENTS.md` 规矩五：
    // "这条断言失败过吗？"）。所以先证明它读到了真实的 `Path`。
    let (user, machine) = registry_snapshot();
    assert!(!machine.starts_with("<没有这个值>"), "机器级 Path 必然存在");
    assert!(
        machine.len() > "<没有这个值>".len(),
        "机器级 Path 不可能是空的：{machine}"
    );
    assert!(user.contains('|'), "用户级的判据必须带类型：{user}");
}

// ─────────────────────────────────────────────────────────────────────────────
// 自造快照（TOML 的形状照抄 `tuoen capture` 的产物）
// ─────────────────────────────────────────────────────────────────────────────

fn schema_toml(sections: &[&str]) -> String {
    let list: String = sections
        .iter()
        .map(|section| format!("    \"{section}\",\n"))
        .collect();
    format!(
        "schema_version = {SCHEMA_VERSION}\n\
         captured_at = \"2026-01-01T00:00:00Z\"\n\
         tuoen_version = \"0.1.0\"\n\
         sections = [\n{list}]\n"
    )
}

/// 一行工具。字段与 `tuoen_core::capture::files::ToolRow` 逐字对应。
struct Tool<'a> {
    name: &'a str,
    version: Option<&'a str>,
    /// 有管理器 = 第三方版本管理器管的（决策 154）。
    manager: Option<&'a str>,
    confidence: &'a str,
    reproducible: bool,
}

fn tools_toml(rows: &[Tool<'_>]) -> String {
    let mut text =
        format!("schema_version = {SCHEMA_VERSION}\ncaptured_at = \"2026-01-01T00:00:00Z\"\n");
    for row in rows {
        text.push_str("\n[[tool]]\n");
        text.push_str(&format!("name = \"{}\"\n", row.name));
        if let Some(version) = row.version {
            text.push_str(&format!("version = \"{version}\"\n"));
        }
        // 单引号字面量：TOML 里反斜杠不用转义。
        text.push_str(&format!(
            "path = 'C:\\tuoen-restore-contract\\{}\\bin'\n",
            row.name
        ));
        text.push_str("source = \"registry-arp\"\n");
        text.push_str(&format!("confidence = \"{}\"\n", row.confidence));
        if let Some(manager) = row.manager {
            text.push_str(&format!("manager = \"{manager}\"\n"));
        }
        text.push_str(&format!("evidence = \"契约测试自造：{}\"\n", row.name));
        text.push_str(&format!("reproducible = {}\n", row.reproducible));
    }
    text
}

/// 一行 `PATH`。`(作用域, 下标, 原文)`。
fn path_toml(entries: &[(&str, usize, &str)]) -> String {
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
    for (scope, index, raw) in entries {
        text.push_str(&format!(
            "\n[[entry]]\n\
             scope = \"{scope}\"\n\
             index = {index}\n\
             owner = \"third-party\"\n\
             raw = '{raw}'\n\
             expanded = '{raw}'\n\
             quoted = false\n\
             empty = false\n\
             exists = \"no\"\n\
             reparse = \"none\"\n\
             has_vars = false\n\
             has_username = false\n\
             dup_index = 0\n\
             reg_type = \"sz\"\n"
        ));
    }
    text
}

/// 一行环境变量。`(名字, 作用域, 原文, 注册表类型)`。
fn env_toml(vars: &[(&str, &str, &str, &str)]) -> String {
    let mut text =
        format!("schema_version = {SCHEMA_VERSION}\ncaptured_at = \"2026-01-01T00:00:00Z\"\n");
    for (name, scope, value, reg_type) in vars {
        // 值用**单引号字面量**：`C:\x` 里的反斜杠在 TOML 基本字符串里是转义符。
        text.push_str(&format!(
            "\n[[var]]\n\
             name = \"{name}\"\n\
             scope = \"{scope}\"\n\
             value_raw = '{value}'\n\
             value_expanded = '{value}'\n\
             reg_type = \"{reg_type}\"\n\
             target_exists = \"not-a-path\"\n"
        ));
    }
    text
}

fn wsl_toml(names: &[&str]) -> String {
    let mut text =
        format!("schema_version = {SCHEMA_VERSION}\ncaptured_at = \"2026-01-01T00:00:00Z\"\n");
    for name in names {
        text.push_str(&format!(
            "\n[[distribution]]\n\
             name = \"{name}\"\n\
             guid = \"{{00000000-0000-0000-0000-000000000000}}\"\n\
             base_path = 'C:\\tuoen-restore-contract\\wsl\\{name}'\n\
             non_standard_path = false\n\
             vhdx_path = 'C:\\tuoen-restore-contract\\wsl\\{name}\\ext4.vhdx'\n\
             vhdx_exists = \"yes\"\n"
        ));
    }
    text
}

/// 一行"看见了但故意没写进快照"的东西。`(section, 作用域, 名字, 类别)`。
fn skipped_toml(rows: &[(&str, &str, &str, &str)]) -> String {
    let mut text =
        format!("schema_version = {SCHEMA_VERSION}\ncaptured_at = \"2026-01-01T00:00:00Z\"\n");
    for (section, scope, name, kind) in rows {
        text.push_str(&format!(
            "\n[[skipped]]\n\
             section = \"{section}\"\n\
             scope = \"{scope}\"\n\
             name = \"{name}\"\n\
             kind = \"{kind}\"\n\
             reason = \"契约测试自造\"\n"
        ));
    }
    text
}

/// 四节齐全的一份快照（用来测 `--only` 与"快照里没有它"这两句话的差别）。
fn full_snapshot(fixture: &Fixture, name: &str) -> PathBuf {
    fixture.snapshot(
        name,
        &[
            ("schema.toml", schema_toml(&["env", "path", "tools", "wsl"])),
            (
                "tools.toml",
                tools_toml(&[Tool {
                    name: "cargo",
                    version: Some("1.99.0"),
                    manager: None,
                    confidence: "executable",
                    reproducible: true,
                }]),
            ),
            ("path.toml", path_toml(&[("user", 0, UNIQUE_DIR)])),
            (
                "env.toml",
                env_toml(&[("CARGO_HOME", "user", r"C:\cargo", "sz")]),
            ),
            ("wsl.toml", wsl_toml(&["Ubuntu"])),
        ],
    )
}

/// 只有 `path.toml` 的一份快照（另外三节**快照里就没有**）。
fn path_only_snapshot(fixture: &Fixture, name: &str) -> PathBuf {
    fixture.snapshot(
        name,
        &[
            ("schema.toml", schema_toml(&["path"])),
            ("path.toml", path_toml(&[("user", 0, UNIQUE_DIR)])),
        ],
    )
}

/// 五类手动待办各一条的那份快照（票据点名的那条用例）。
fn five_codes_snapshot(fixture: &Fixture, name: &str) -> PathBuf {
    fixture.snapshot(
        name,
        &[
            ("schema.toml", schema_toml(&["env", "path", "tools", "wsl"])),
            (
                "tools.toml",
                tools_toml(&[
                    // 许可不允许再分发（内置目录里 `oracle-jdk` 是 prohibited）。
                    //
                    // **`reproducible = false` 是必须的**：core 的四步顺序里
                    // "可复现 → 装它"排在许可表**之前**（②先于③），所以一条
                    // 可复现的 `oracle-jdk` 会变成 `install`，而不是 `licence-blocked`。
                    Tool {
                        name: "oracle-jdk",
                        version: Some("17.0.9"),
                        manager: None,
                        confidence: "registered",
                        reproducible: false,
                    },
                    // 第三方版本管理器管的（决策 154）。
                    Tool {
                        name: "tuoen-contract-nvm-node",
                        version: Some("24.19.0"),
                        manager: Some("nvm4w"),
                        confidence: "manager-owned",
                        reproducible: true,
                    },
                    // 不可复现（快照里没留下配方）。
                    Tool {
                        name: "tuoen-contract-ghost",
                        version: None,
                        manager: None,
                        confidence: "registered",
                        reproducible: false,
                    },
                ]),
            ),
            (
                "path.toml",
                path_toml(&[("machine", 0, MACHINE_DIR), ("user", 0, UNIQUE_DIR)]),
            ),
            (
                "env.toml",
                env_toml(&[("TUOEN_CONTRACT_MACHINE_VAR", "machine", r"C:\machine", "sz")]),
            ),
            ("wsl.toml", wsl_toml(&["Ubuntu"])),
            (
                "skipped.toml",
                skipped_toml(&[("env", "user", "TUOEN_CONTRACT_TOKEN", "credential-named")]),
            ),
        ],
    )
}

// ─────────────────────────────────────────────────────────────────────────────
// `--json` 的取用
// ─────────────────────────────────────────────────────────────────────────────

/// `restore <dir> --json` 的 `data`（计划形态）。
fn plan_data(fixture: &Fixture, dir: &Path, extra: &[&str]) -> Value {
    let path = dir.display().to_string();
    let mut args = vec!["restore", path.as_str()];
    args.extend_from_slice(extra);
    args.push("--json");
    let output = fixture.run(&args);
    assert_eq!(output.status.code(), Some(0), "{}", describe(&output));
    let envelope = json(&output);
    assert!(envelope.ok, "计划必须是成功信封：{:?}", envelope.error);
    assert_eq!(envelope.command, "restore.plan");
    envelope.data.expect("成功必须有 data")
}

/// 计划里某一节。
fn section<'a>(data: &'a Value, id: &str) -> &'a Value {
    data["sections"]
        .as_array()
        .expect("sections 必须是数组")
        .iter()
        .find(|section| section["id"] == id)
        .unwrap_or_else(|| panic!("计划里必须有 {id} 这一节"))
}

/// 计划里所有 `manualActions[].code`。
fn manual_codes(data: &Value) -> Vec<String> {
    data["manualActions"]
        .as_array()
        .expect("manualActions 必须是数组")
        .iter()
        .map(|action| {
            action["code"]
                .as_str()
                .expect("code 必须是字符串")
                .to_owned()
        })
        .collect()
}

// ─────────────────────────────────────────────────────────────────────────────
// 1. 默认形态：只出计划，什么都不写
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn a_bare_restore_is_a_plan_and_writes_nothing() {
    let fixture = Fixture::new("restore-bare-plan");
    let dir = full_snapshot(&fixture, "tuoen.d");
    assert!(dir.is_dir(), "快照目录必须先存在");
    let before = registry_snapshot();

    let output = fixture.run_in(&fixture.scratch(), &["restore", "tuoen.d"]);
    assert_eq!(output.status.code(), Some(0), "{}", describe(&output));
    let text = stdout(&output);
    assert!(text.contains("恢复计划"), "{text}");
    assert!(text.contains("什么都没有写"), "{text}");
    // 计划里逐条点名了会做什么 —— 一句"有 N 处差异"不算计划。
    assert!(text.contains(UNIQUE_DIR), "计划里要点名那条新增：{text}");
    assert!(
        text.contains("人工待办"),
        "无变更也要打人工待办那一段：{text}"
    );

    assert_registry_unchanged(before, "restore 默认形态");
    assert_eq!(
        fixture.nvm_settings(),
        NVM_SETTINGS,
        "nvm 的 settings.txt 被动了"
    );
}

#[test]
fn the_default_form_and_the_explicit_dry_run_are_byte_identical() {
    // 决策 150：`--dry-run` 是默认行为的**显式别名**，不是第二条代码路径。
    // 逐字节相同是"它们本来就是同一次调用"的直接证据。
    let fixture = Fixture::new("restore-default-equals-dry-run");
    let dir = full_snapshot(&fixture, "tuoen.d");
    let path = dir.display().to_string();

    let bare = fixture.run(&["restore", &path, "--json"]);
    let dry = fixture.run(&["restore", &path, "--dry-run", "--json"]);
    assert_eq!(bare.status.code(), Some(0), "{}", describe(&bare));
    assert_eq!(dry.status.code(), Some(0), "{}", describe(&dry));
    assert_eq!(
        stdout(&bare),
        stdout(&dry),
        "默认形态与 --dry-run 的 JSON 必须逐字节相同"
    );

    let bare_human = fixture.run(&["restore", &path]);
    let dry_human = fixture.run(&["restore", &path, "--dry-run"]);
    assert_eq!(
        stdout(&bare_human),
        stdout(&dry_human),
        "默认形态与 --dry-run 的人类输出必须逐字节相同"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// 2. 用法错误：退出码 2，在解析期就被拒
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn apply_and_dry_run_together_are_a_usage_error() {
    let fixture = Fixture::new("restore-apply-and-dry-run");
    let output = fixture.run(&["restore", "tuoen.d", "--apply", "--dry-run"]);
    assert_eq!(output.status.code(), Some(2), "{}", describe(&output));
    assert!(
        stdout(&output).is_empty(),
        "--apply 与 --dry-run 矛盾时不该有 stdout 输出"
    );
}

#[test]
fn an_unknown_section_is_a_usage_error() {
    let fixture = Fixture::new("restore-unknown-section");
    let output = fixture.run(&["restore", "tuoen.d", "--only", "toolsx"]);
    assert_eq!(output.status.code(), Some(2), "{}", describe(&output));
    let text = stderr(&output);
    assert!(text.contains("toolsx"), "错误里要点名那个取值：{text}");
    assert!(text.contains("path"), "错误里要列出合法取值：{text}");
}

// ─────────────────────────────────────────────────────────────────────────────
// 3. 快照读不了：三个错误码必须分得开
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn a_snapshot_that_is_not_there_is_snapshot_io() {
    let fixture = Fixture::new("restore-missing-snapshot");
    let output = fixture.run(&["restore", "no-such-dir", "--json"]);
    assert_eq!(output.status.code(), Some(1), "{}", describe(&output));
    let envelope = json(&output);
    assert!(!envelope.ok);
    assert_eq!(
        envelope.error.expect("失败必须有 error").code,
        "snapshot-io"
    );
    assert!(envelope.data.is_none(), "失败时不该有 data");
}

#[test]
fn a_snapshot_that_is_not_toml_is_snapshot_toml() {
    let fixture = Fixture::new("restore-bad-toml");
    let dir = fixture.snapshot("broken", &[("path.toml", "这不是 TOML [[[".to_owned())]);
    let output = fixture.run(&["restore", &dir.display().to_string(), "--json"]);
    assert_eq!(output.status.code(), Some(1), "{}", describe(&output));
    let envelope = json(&output);
    assert_eq!(
        envelope.error.expect("失败必须有 error").code,
        "snapshot-toml",
        "文件在那儿但内容变了 —— 与「文件不在」是两个错误码"
    );
}

#[test]
fn a_directory_with_no_section_file_is_an_empty_snapshot() {
    // 决策 156：空快照**明确报错**。四节全 `skipped` 的计划看起来像
    // "本机已经全都对上了"，而那份快照其实什么都没说。
    let fixture = Fixture::new("restore-empty-snapshot");
    let dir = fixture.snapshot("empty", &[("schema.toml", schema_toml(&[]))]);

    let output = fixture.run(&["restore", &dir.display().to_string(), "--json"]);
    assert_eq!(output.status.code(), Some(1), "{}", describe(&output));
    let envelope = json(&output);
    assert_eq!(
        envelope.error.expect("失败必须有 error").code,
        "empty-snapshot"
    );
    assert!(envelope.data.is_none(), "空快照不该给出一个计划");

    // 人类输出也要说清楚，而且不能说成"无变更"。
    let human = fixture.run(&["restore", &dir.display().to_string()]);
    assert_eq!(human.status.code(), Some(1), "{}", describe(&human));
    assert!(!stdout(&human).contains("无变更"), "{}", stdout(&human));
}

// ─────────────────────────────────────────────────────────────────────────────
// 4. 成功载荷的形状
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn the_success_payload_is_ascii_only_and_carries_no_timestamp() {
    let fixture = Fixture::new("restore-payload-shape");
    let dir = full_snapshot(&fixture, "tuoen.d");
    let data = plan_data(&fixture, &dir, &[]);

    // 四个键，一个不多一个不少。
    let keys: Vec<&str> = data
        .as_object()
        .expect("data 必须是对象")
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        keys,
        vec!["manualActions", "sections", "snapshot", "summary"]
    );

    // 整份载荷**纯 ASCII**（中文只进人类输出）。
    let text = data.to_string();
    assert!(text.is_ascii(), "成功载荷里出现了非 ASCII：{text}");
    // 没有时间戳（决策 126 的沿用：快照的 `captured_at` 不许进 JSON）。
    assert!(!text.contains("2026-"), "载荷里不许有时间戳：{text}");
    assert!(!text.contains("capturedAt"), "{text}");
    // 没有 `message` 键（那是人类输出的事）。
    assert!(!text.contains("\"message\""), "{text}");
}

#[test]
fn two_runs_in_the_same_directory_are_byte_identical() {
    let fixture = Fixture::new("restore-twice");
    let dir = full_snapshot(&fixture, "tuoen.d");
    let path = dir.display().to_string();
    let first = fixture.run(&["restore", &path, "--json"]);
    let second = fixture.run(&["restore", &path, "--json"]);
    assert_eq!(first.status.code(), Some(0), "{}", describe(&first));
    assert_eq!(
        stdout(&first),
        stdout(&second),
        "同一条命令两次必须逐字节相同"
    );
}

#[test]
fn summary_is_the_status_histogram() {
    let fixture = Fixture::new("restore-summary");
    let dir = full_snapshot(&fixture, "tuoen.d");
    let data = plan_data(&fixture, &dir, &[]);

    let summary = &data["summary"];
    let sections = data["sections"].as_array().expect("sections").len() as u64;
    assert_eq!(summary["sections"].as_u64(), Some(sections));
    let histogram = [
        "noChange",
        "wouldChange",
        "requiresElevation",
        "needsNetwork",
        "unsupported",
        "skipped",
    ]
    .iter()
    .map(|key| summary[*key].as_u64().unwrap_or_else(|| panic!("缺 {key}")))
    .sum::<u64>();
    assert_eq!(
        histogram, sections,
        "六个计数器相加必须等于 sections（决策 161）"
    );

    // 直方图与逐节的 `status` 一致（自己数一遍，不信 summary）。
    let mut counted = std::collections::BTreeMap::new();
    for section in data["sections"].as_array().expect("sections") {
        *counted
            .entry(section["status"].as_str().expect("status").to_owned())
            .or_insert(0u64) += 1;
    }
    let from_sections = |status: &str| counted.get(status).copied().unwrap_or(0);
    assert_eq!(
        summary["noChange"].as_u64(),
        Some(from_sections("no-change"))
    );
    assert_eq!(
        summary["wouldChange"].as_u64(),
        Some(from_sections("would-change"))
    );
    assert_eq!(
        summary["requiresElevation"].as_u64(),
        Some(from_sections("requires-elevation"))
    );
    assert_eq!(
        summary["needsNetwork"].as_u64(),
        Some(from_sections("needs-network"))
    );
    assert_eq!(
        summary["unsupported"].as_u64(),
        Some(from_sections("unsupported"))
    );
    assert_eq!(summary["skipped"].as_u64(), Some(from_sections("skipped")));

    let manual = data["manualActions"]
        .as_array()
        .expect("manualActions")
        .len() as u64;
    assert_eq!(summary["manualActions"].as_u64(), Some(manual));
}

// ─────────────────────────────────────────────────────────────────────────────
// 5. `--only`：没选中的节**根本走不到写代码**
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn only_path_leaves_the_other_three_sections_untouched() {
    // 票据点名的证据："`--only path` → `tools` / `env` 未被改动"。
    // 在进程边界之外"未被改动"没法直接证明，所以判据是**计划里那三节
    // 连一个动作都没有**（`actions` 空 + `skipped`），而"会碰谁"由
    // `restore_cmd::sections_to_apply` 这个纯函数决定（它的两条单测在那边）。
    let fixture = Fixture::new("restore-only-path");
    let dir = full_snapshot(&fixture, "tuoen.d");
    let before = registry_snapshot();

    let data = plan_data(&fixture, &dir, &["--only", "path"]);

    // `path` 被选中了，所以它**不是** `skipped`；它具体是哪一个"有变更"状态
    // 取决于本机（自造快照与本机 `PATH` 的差异里有没有机器级那一段），
    // 所以判据是"它进了决策 161 那三个状态之一"，而不是钉死某一个。
    let path_status = section(&data, "path")["status"]
        .as_str()
        .expect("status")
        .to_owned();
    assert!(
        ["would-change", "requires-elevation", "needs-network"].contains(&path_status.as_str()),
        "被选中的 path 必须是一个「有变更」状态，实际是 {path_status}"
    );
    for id in ["tools", "env", "wsl"] {
        let other = section(&data, id);
        assert_eq!(other["status"], "skipped", "{id} 必须是 skipped");
        assert_eq!(other["note"], "not-selected", "{id} 的理由是「你没选它」");
        assert!(
            other["actions"].as_array().expect("actions").is_empty(),
            "{id} 不许有任何动作"
        );
    }

    assert_registry_unchanged(before, "--only path 的计划形态");
}

#[test]
fn a_section_missing_from_the_snapshot_says_a_different_sentence() {
    // 决策 159/161：**"快照里没有它"** 与 **"你没选它"** 是两句不同的话 ——
    // 用户要做的下一步完全不同。
    let fixture = Fixture::new("restore-missing-section");
    let dir = path_only_snapshot(&fixture, "tuoen.d");

    let all = plan_data(&fixture, &dir, &[]);
    for id in ["tools", "env", "wsl"] {
        assert_eq!(section(&all, id)["note"], "section-not-in-snapshot", "{id}");
    }

    let selected = plan_data(&fixture, &dir, &["--only", "path"]);
    for id in ["tools", "env", "wsl"] {
        assert_eq!(
            section(&selected, id)["note"],
            "not-selected",
            "{id} 这一次是「你没选它」，不是「快照里没有它」"
        );
    }

    // 人类输出也要分开说。
    let human = fixture.run(&["restore", &dir.display().to_string()]);
    let text = stdout(&human);
    assert!(text.contains("这份快照里"), "{text}");
    assert!(
        text.contains("没有选它") || !text.contains("--only"),
        "{text}"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// 6. 五类手动待办：每一类都要具体到 slug
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn the_five_manual_codes_each_get_a_line() {
    let fixture = Fixture::new("restore-five-codes");
    let dir = five_codes_snapshot(&fixture, "tuoen.d");
    let data = plan_data(&fixture, &dir, &[]);
    let codes = manual_codes(&data);

    for code in [
        "requires-elevation",
        "credential-reconfigure",
        "licence-blocked",
        "third-party-manager",
        "unsupported",
    ] {
        assert!(codes.contains(&code.to_owned()), "缺 {code}：{codes:?}");
    }

    let action = |code: &str| {
        data["manualActions"]
            .as_array()
            .expect("manualActions")
            .iter()
            .find(|action| action["code"] == code)
            .unwrap_or_else(|| panic!("缺 {code}"))
    };

    // **具体到 slug**（决策 160）：不许是"许可问题"这种废话。
    assert_eq!(
        action("licence-blocked")["detail"],
        "oracle-jdk-redistribution-not-permitted"
    );
    assert_eq!(action("licence-blocked")["subject"], "oracle-jdk");
    assert_eq!(action("unsupported")["detail"], "not-reproducible");
    assert_eq!(action("third-party-manager")["subject"], "nvm4w");
    assert_eq!(
        action("third-party-manager")["remediation"],
        "use-the-manager"
    );
    assert_eq!(
        action("credential-reconfigure")["remediation"],
        "reconfigure-manually"
    );
    assert_eq!(
        action("requires-elevation")["remediation"],
        "run-as-administrator"
    );

    // 人类输出：`detail` 那个 slug 要被翻成中文散文，而不是原样打出来。
    let human = fixture.run(&["restore", &dir.display().to_string()]);
    let text = stdout(&human);
    assert!(
        text.contains("Oracle JDK"),
        "许可那一类要说清是哪个许可：{text}"
    );
    assert!(text.contains("nvm4w"), "{text}");
    assert!(text.contains("凭据"), "{text}");
}

#[test]
fn a_credential_never_leaks_a_material() {
    // 决策 153：`manualActions` 里**只有标识、没有材料**。
    // 这里把材料真的放进输入（`env.toml` 的一个值 + 一条 credential 记录），
    // 然后断言它**一个片段都没有**出现在任何输出流里。
    let fixture = Fixture::new("restore-credential");
    let dir = fixture.snapshot(
        "tuoen.d",
        &[
            ("schema.toml", schema_toml(&["env", "path"])),
            ("path.toml", path_toml(&[("user", 0, UNIQUE_DIR)])),
            (
                "env.toml",
                env_toml(&[("TUOEN_CONTRACT_TOKEN", "user", SECRET, "sz")]),
            ),
            (
                "skipped.toml",
                skipped_toml(&[("env", "user", "TUOEN_CONTRACT_TOKEN", "credential-named")]),
            ),
        ],
    );

    for extra in [vec![], vec!["--only", "env"]] {
        let path = dir.display().to_string();
        let mut args = vec!["restore", path.as_str()];
        args.extend_from_slice(&extra);
        let json_run = fixture.run(&args);
        let human_run = fixture.run(&args);
        for output in [&json_run, &human_run] {
            let all = format!("{}{}", stdout(output), stderr(output));
            assert!(!all.contains("glpat-"), "输出里出现了凭据前缀：{all}");
            assert!(!all.contains(SECRET), "输出里出现了凭据材料：{all}");
            // 名字**要**出现（那是"需要哪个凭据"这件必要的信息）。
            assert!(all.contains("TUOEN_CONTRACT_TOKEN"), "{all}");
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// 7. 第三方管理器与机器级：算出来，但不碰
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn a_third_party_manager_s_own_files_are_never_touched() {
    let fixture = Fixture::new("restore-third-party");
    let dir = five_codes_snapshot(&fixture, "tuoen.d");
    let before = registry_snapshot();

    let data = plan_data(&fixture, &dir, &[]);
    let tools = section(&data, "tools");
    let actions = tools["actions"].as_array().expect("actions");
    // 管理器管的那一行**不许有 `install` 动作**：我们不接管它。
    assert!(
        !actions.iter().any(|action| {
            action["kind"] == "install"
                && action["subject"]
                    .as_str()
                    .is_some_and(|subject| subject.contains("nvm"))
        }),
        "第三方管理器管的工具不该出现在 install 里：{actions:?}"
    );
    // 而它**要**在手动待办里被点名（"你自己用它自己的命令"）。
    assert!(manual_codes(&data).contains(&"third-party-manager".to_owned()));

    // 计划形态跑完，管理器自己的文件逐字节未变。
    assert_eq!(
        fixture.nvm_settings(),
        NVM_SETTINGS,
        "nvm 的 settings.txt 被动了"
    );
    assert_registry_unchanged(before, "第三方管理器那一节");
}

#[test]
fn a_machine_scope_change_is_planned_but_never_written() {
    let fixture = Fixture::new("restore-machine-scope");
    let dir = five_codes_snapshot(&fixture, "tuoen.d");
    let before = registry_snapshot();

    let data = plan_data(&fixture, &dir, &[]);
    let path = section(&data, "path");
    // 机器级那一条要在计划里出现（`requiresElevation` 是逐条说的），
    // 而它**只能**出现在需要提权那一类里。
    assert_eq!(path["requiresElevation"], true, "机器级改动要如实报出来");
    assert!(
        manual_codes(&data).contains(&"requires-elevation".to_owned()),
        "机器级改动要进人工待办"
    );

    // 人类输出必须说"机器级要提权，tuoen 不自动提权"。
    let human = fixture.run(&["restore", &dir.display().to_string()]);
    let text = stdout(&human);
    assert!(
        text.contains("不自动提权") || text.contains("要提权"),
        "{text}"
    );

    assert_registry_unchanged(before, "机器级只算不写");
}

// ─────────────────────────────────────────────────────────────────────────────
// 8. 计划里的 `PATH` 动作复用 #15 的 id 形状
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn path_actions_use_the_snapshot_side_ids() {
    let fixture = Fixture::new("restore-path-ids");
    let dir = path_only_snapshot(&fixture, "tuoen.d");
    let data = plan_data(&fixture, &dir, &[]);

    let actions = section(&data, "path")["actions"]
        .as_array()
        .expect("actions")
        .clone();
    assert!(!actions.is_empty(), "自造快照里的那条目录必然是新增");
    for action in &actions {
        let id = action["id"].as_str().expect("id 必须是字符串");
        let (scope, index) = id
            .split_once(':')
            .unwrap_or_else(|| panic!("id 形状：{id}"));
        assert!(matches!(scope, "user" | "machine"), "{id}");
        let digits = index.strip_prefix('+').unwrap_or(index);
        assert!(
            digits.parse::<usize>().is_ok(),
            "id 的形状必须是 {{scope}}:{{index}} 或 {{scope}}:+{{index}}：{id}"
        );
        // `keep` 不进 actions（决策 158：它只是行数口径的一部分）。
        assert_ne!(action["kind"], "keep", "{action:?}");
    }

    // 用户级那条新增要在 `effective` 里说"真会落地 1 条"。
    let counts = &section(&data, "path")["counts"];
    assert_eq!(counts["rows"]["add"].as_u64(), Some(1));
    assert_eq!(counts["effective"]["add"].as_u64(), Some(1));
}

// ─────────────────────────────────────────────────────────────────────────────
// 9. 幂等：本机对着自己的快照
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn a_real_capture_of_this_machine_is_a_no_change_plan() {
    // 真机形态：现场 `capture` 一份，然后拿它当目标 —— 本机与它自己必然一致，
    // 所以四节全 `no-change`（决策 155 的幂等）。这一条同时证明
    // "整条链路在真机上跑得通"（进程边界 + 真实注册表 + 真实检测引擎）。
    let fixture = Fixture::new("restore-self");
    let dir = fixture.scratch().join("tuoen.d");
    let capture = fixture.run(&["capture", "--out", &dir.display().to_string()]);
    assert_eq!(capture.status.code(), Some(0), "{}", describe(&capture));

    let before = registry_snapshot();
    let data = plan_data(&fixture, &dir, &[]);

    for id in ["tools", "path", "env", "wsl"] {
        assert_eq!(
            section(&data, id)["status"],
            "no-change",
            "{id} 对着本机自己的快照不该有变更：{}",
            section(&data, id)
        );
    }
    assert_eq!(data["summary"]["wouldChange"].as_u64(), Some(0));
    assert_eq!(data["summary"]["requiresElevation"].as_u64(), Some(0));
    assert_eq!(data["summary"]["needsNetwork"].as_u64(), Some(0));

    assert_registry_unchanged(before, "对着本机自己的快照出计划");
}
