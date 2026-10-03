//! `tuoen capture --only globals` / `--only configs` 的**独立验证**（票据 #17，决策 165–185）。
//!
//! # 这一份为什么独立
//!
//! `globals` / `configs` 这两个 section 的单测在 `crates/core/src/capture/collect/**` 里，
//! 由**写实现的人**维护。这一份的价值在于：它**不读实现**，只读 `fixtures/capture/**`
//! 那几份**手写的机器描述**，然后：
//!
//! - 期望值**从固定装置自己数出来**：包数从 fixture 里那份 `npm ls` / `pip list` 的
//!   **JSON 文本**里数、跳过项从声明的路径按决策 179/180/181/182 的判据表推出来、
//!   哈希用测试自己算的 sha256、`prefix_inside_version_dir` 按决策 173 的四支判据自己判；
//! - **绝不从产品输出反推期望** —— 那等于拿产品自己的输出当答案，两边一起错的时候它
//!   永远是绿的；
//! - **不写死数字**：一个写死的"应该是 7 个包"在 fixture 改一个字之后照样通过。
//!
//! # 只碰假机器
//!
//! 每个用例都走 `CaptureFixture::build(&MachineFixture)`（`crates/core/src/capture/test_support.rs`）
//! 与 `toml::from_str::<MachineFixture>`。**假文件系统对任何未声明的路径都答"不存在"**，
//! 所以一个忘了写固定装置的用例会失败，而不是静默读到这台开发机的真实磁盘。
//! 读真实用户目录在这份文件里一次都不出现。
//!
//! # 两条变异测试
//!
//! [`mutation_removing_the_credential_shape_flips_the_verdict`] 与
//! [`mutation_moving_the_prefix_out_of_a_version_directory_flips_the_flag`] **在内存里**
//! 改固定装置（磁盘上的文件不动），断言"改动之后结论必须变" —— 那是"这条用例真的会红"
//! 的证据，而不是"它一直是绿的"。用 `--nocapture` 跑可以看到它们打印的对照。
//!
//! # 假 token
//!
//! `credential-shape` 那一份里的 PAT 是假的，但形状是真的（GitLab PAT 前缀 + 16 位）。
//! 固定装置里用 `\u002D` 写那个连字符（于是**文件文本里没有前缀字面量**：本仓库被
//! GitHub push protection GH013 拦过一次），测试里用 `concat!("glpat", "-")` 拼。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use tuoen_core::capture::test_support::CaptureFixture;
use tuoen_core::capture::{
    CaptureBundle, ConfigRow, ConfigsFile, GlobalRow, GlobalsFile, SchemaFile, Section, SkipEntry,
    SkippedFile, render,
};
use tuoen_platform::ReparseKind;
use tuoen_platform::fixture::{FixturePath, FixtureProcess, MachineFixture};

// ─────────────────────────────────────────────────────────────────────────────
// 固定装置的清单与读取
// ─────────────────────────────────────────────────────────────────────────────

/// 固定装置的根。`CARGO_MANIFEST_DIR` 是 `crates/core`，往上两级才是仓库根。
const ROOT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/capture");

/// 所有用例共用的捕获时间。**固定值**：幂等那条用例要求两次捕获逐字节相同。
const AT: &str = "2026-10-02T12:00:00Z";

/// 决策 176/179 的上限（`collect::configs` 的 `MAX_CONFIG_BYTES`）。
///
/// 这里**自己写一遍**而不是 `use` 产品的常量：它是"设计说的数"（256 KiB），
/// 用它来推"这份 262145 字节的文件必须被跳过"的期望 —— 抄产品的常量会让
/// "产品把上限改成 1 MB"这件事在测试里一起被改掉。
const MAX_CONFIG_BYTES: u64 = 256 * 1024;

/// 场景目录 → 里面的机器文件（**顺序固定**，失败信息里的顺序也就固定了）。
///
/// 这张表是"夹具与测试不许漂移"的那一半：目录里多一个/少一个 `*.toml` 都会当场红。
const SCENARIOS: [(&str, &[&str]); 8] = [
    (
        "npm-inside-version-dir",
        &[
            "ancestor-junction.toml",
            "machine.toml",
            "version-segment-prefix.toml",
        ],
    ),
    ("two-node-versions", &["node-v22.toml", "node-v24.toml"]),
    (
        "enumeration-fails",
        &["bad-json.toml", "command-failed.toml", "timed-out.toml"],
    ),
    (
        "git-identity-layers",
        &[
            "git-unavailable.toml",
            "global-only.toml",
            "global-wins.toml",
            "missing.toml",
            "system-only.toml",
        ],
    ),
    ("credential-shape", &["machine.toml"]),
    ("unreadable-too-large-binary", &["machine.toml"]),
    ("caches-and-keys", &["machine.toml"]),
    ("idempotent", &["machine.toml"]),
];

/// 七条枚举/探测命令的 `program` 与 `args`（与 `fixtures/capture/README.md` 的表逐字一致）。
///
/// 固定装置里的 `program` 是**裸名字**（决策 168 + task-7 的落地口径），
/// 而 `FixtureProcess` 的匹配规则是决策 185：`program` 相等 **且**（声明 args 为空
/// **或** 调用 args 以它开头）；多命中时 args 最长的赢。
const NODE_V: &[&str] = &["-v"];
const NPM_PREFIX: &[&str] = &["/C", "npm.cmd", "config", "get", "prefix"];
const NPM_LS: &[&str] = &[
    "/C",
    "npm.cmd",
    "ls",
    "-g",
    "--json",
    "--depth=0",
    "--offline",
];
const PIP_VERSION: &[&str] = &["--version"];
const PIP_LIST: &[&str] = &["list", "--format=json", "--disable-pip-version-check"];
const GIT_SYSTEM: &[&str] = &["config", "--system", "--list", "--show-origin"];
const GIT_GLOBAL: &[&str] = &["config", "--global", "--list", "--show-origin"];

fn scenario_dir(scenario: &str) -> PathBuf {
    Path::new(ROOT).join(scenario)
}

/// 读一份固定装置。读不到/解析不了就**指名道姓**地炸 —— 固定装置没了不该表现为一条
/// 看不懂的断言失败。
fn machine(scenario: &str, file: &str) -> MachineFixture {
    let path = scenario_dir(scenario).join(file);
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("读不到固定装置 {}：{err}", path.display()));
    toml::from_str(&text).unwrap_or_else(|err| {
        panic!(
            "固定装置 {} 不是合法的 MachineFixture（`deny_unknown_fields` 会抓住字段名漂移）：{err}",
            path.display()
        )
    })
}

fn fixture(scenario: &str, file: &str) -> CaptureFixture {
    CaptureFixture::build(&machine(scenario, file))
}

fn bundle(scenario: &str, file: &str, sections: &[Section]) -> CaptureBundle {
    fixture(scenario, file).capture(sections, AT)
}

fn globals(scenario: &str, file: &str) -> GlobalsFile {
    bundle(scenario, file, &[Section::Globals])
        .globals
        .unwrap_or_else(|| panic!("{scenario}/{file}：`--only globals` 必须产出 globals 这一节"))
}

fn configs(scenario: &str, file: &str) -> ConfigsFile {
    bundle(scenario, file, &[Section::Configs])
        .configs
        .unwrap_or_else(|| panic!("{scenario}/{file}：`--only configs` 必须产出 configs 这一节"))
}

fn skips(scenario: &str, file: &str) -> SkippedFile {
    bundle(scenario, file, &[Section::Configs])
        .skipped
        .unwrap_or_else(|| {
            panic!("{scenario}/{file}：扫过 configs 就必须产出 skipped.toml（决策 179）")
        })
}

// ─────────────────────────────────────────────────────────────────────────────
// 路径与固定装置的小工具
// ─────────────────────────────────────────────────────────────────────────────

fn norm(path: &str) -> String {
    path.replace('/', "\\")
        .trim_end_matches('\\')
        .to_lowercase()
}

fn same_path(a: &str, b: &str) -> bool {
    norm(a) == norm(b)
}

fn join_path(dir: &str, name: &str) -> String {
    format!("{}\\{}", dir.trim_end_matches('\\'), name)
}

/// 固定装置里声明的**所有条目**：`(完整路径, 条目)`。
///
/// `dirs[].entries[]` 的 `path` 是**相对该目录的名字**（单层，不嵌套），所以这里拼成
/// 完整路径 —— 采集器看到的也是完整路径。
fn entries(m: &MachineFixture) -> Vec<(String, FixturePath)> {
    let mut out = Vec::new();
    for dir in &m.dirs {
        for entry in &dir.entries {
            out.push((join_path(&dir.path, &entry.path), entry.clone()));
        }
    }
    for entry in &m.paths {
        out.push((entry.path.clone(), entry.clone()));
    }
    out
}

/// 一个路径在固定装置里的条目。
///
/// **两种声明都要看**：`dirs[].entries[]` / `paths[]` 里的条目，以及一个**裸的
/// `[[dirs]]` 节点** —— 后者只回答"这个目录在不在"（缓存目录、`C:\Python312`
/// 这类只作为前缀/祖先出现的目录就是这样声明的），没有 `size` / `content`。
fn entry_of(m: &MachineFixture, path: &str) -> Option<FixturePath> {
    if let Some(entry) = entries(m)
        .into_iter()
        .find(|(candidate, _)| same_path(candidate, path))
        .map(|(_, entry)| entry)
    {
        return Some(entry);
    }
    m.dirs
        .iter()
        .find(|dir| same_path(&dir.path, path))
        .map(|dir| FixturePath {
            path: dir.path.clone(),
            exists: dir.exists,
            is_dir: true,
            ..FixturePath::default()
        })
}

fn content_of(m: &MachineFixture, path: &str) -> Option<String> {
    entry_of(m, path).and_then(|entry| entry.content)
}

/// 固定装置声明的路径存在吗（`exists` 默认 `true`）。
fn exists(m: &MachineFixture, path: &str) -> bool {
    entry_of(m, path).is_some_and(|entry| entry.exists)
}

fn env_var(m: &MachineFixture, name: &str) -> Option<String> {
    m.env
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.clone())
}

/// 进程环境 `Path` 上的目录（按声明顺序）。
fn path_dirs(m: &MachineFixture) -> Vec<String> {
    env_var(m, "Path")
        .unwrap_or_default()
        .split(';')
        .map(str::trim)
        .filter(|dir| !dir.is_empty())
        .map(str::to_owned)
        .collect()
}

/// 在进程 `Path` 上找某个可执行文件 —— 这是决策 171"找不到可执行文件就不产生行"的
/// 前提，也是固定装置自证的一半（声明了 PATH 却忘了放 `npm.cmd`，globals 那一行
/// 根本不会出现，用例会以一条看不懂的方式红）。
fn find_on_path(m: &MachineFixture, name: &str) -> Option<String> {
    for dir in path_dirs(m) {
        let full = join_path(&dir, name);
        if entries(m)
            .iter()
            .any(|(path, entry)| same_path(path, &full) && entry.exists)
        {
            return Some(full);
        }
    }
    None
}

/// 决策 185 的匹配规则（测试自己也用它来找固定装置里的进程条目）。
fn probe<'a>(m: &'a MachineFixture, program: &str, args: &[&str]) -> &'a FixtureProcess {
    let mut best: Option<&FixtureProcess> = None;
    for entry in &m.processes {
        if !entry.program.eq_ignore_ascii_case(program) {
            continue;
        }
        if !entry.args.is_empty()
            && !args.starts_with(
                entry
                    .args
                    .iter()
                    .map(String::as_str)
                    .collect::<Vec<_>>()
                    .as_slice(),
            )
        {
            continue;
        }
        if best.is_none_or(|current| entry.args.len() > current.args.len()) {
            best = Some(entry);
        }
    }
    best.unwrap_or_else(|| {
        panic!(
            "固定装置里没有 `{program} {args:?}` 这条进程条目 —— 采集器会答 not_spawned，\
             用例就测错了东西（查 fixtures/capture/README.md 的那张表）"
        )
    })
}

fn sha256_of(text: &str) -> String {
    format!("sha256:{}", tuoen_download::sha256_hex(text.as_bytes()))
}

/// 决策 173 ③ 支的"像版本号的一段"：`v` + 数字（`v24.19.0`），或纯数字点分（`3.12`）。
fn looks_like_version_segment(path: &str) -> bool {
    path.split(['\\', '/']).any(|segment| {
        let lower = segment.to_lowercase();
        let digits = lower.strip_prefix('v').unwrap_or(&lower);
        !digits.is_empty()
            && digits
                .split('.')
                .all(|part| !part.is_empty() && part.chars().all(|c| c.is_ascii_digit()))
    })
}

// ─────────────────────────────────────────────────────────────────────────────
// 期望值：全部从固定装置数出来
// ─────────────────────────────────────────────────────────────────────────────

/// `npm ls -g --json` 的 JSON 文本里数出来的包（`dependencies` 的 name → version）。
fn npm_packages(m: &MachineFixture) -> BTreeMap<String, String> {
    let text = &probe(m, "cmd.exe", NPM_LS).stdout;
    let json: serde_json::Value = serde_json::from_str(text)
        .unwrap_or_else(|err| panic!("固定装置里的 `npm ls` 输出不是 JSON：{err}"));
    json["dependencies"]
        .as_object()
        .expect("npm ls 的 JSON 必须有 dependencies")
        .iter()
        .map(|(name, facts)| {
            (
                name.clone(),
                facts["version"].as_str().unwrap_or("unknown").to_owned(),
            )
        })
        .collect()
}

/// `pip list --format=json` 的 JSON 文本里数出来的包。
fn pip_packages(m: &MachineFixture) -> BTreeMap<String, String> {
    let text = &probe(m, "pip.exe", PIP_LIST).stdout;
    let json: serde_json::Value = serde_json::from_str(text)
        .unwrap_or_else(|err| panic!("固定装置里的 `pip list` 输出不是 JSON：{err}"));
    json.as_array()
        .expect("pip list 的 JSON 必须是数组")
        .iter()
        .map(|item| {
            (
                item["name"].as_str().expect("name").to_owned(),
                item["version"].as_str().expect("version").to_owned(),
            )
        })
        .collect()
}

fn npm_prefix(m: &MachineFixture) -> String {
    probe(m, "cmd.exe", NPM_PREFIX).stdout.trim().to_owned()
}

fn node_version(m: &MachineFixture) -> String {
    probe(m, "node.exe", NODE_V).stdout.trim().to_owned()
}

/// `pip --version` 里 `from <路径>` 的那条路径（决策 172：pip 的版本与前缀都来自它）。
fn pip_install_path(m: &MachineFixture) -> String {
    let out = &probe(m, "pip.exe", PIP_VERSION).stdout;
    out.split(" from ")
        .nth(1)
        .unwrap_or_else(|| panic!("`pip --version` 的输出里要有 ` from `：{out}"))
        .split_whitespace()
        .next()
        .expect("`from` 后面要有路径")
        .to_owned()
}

/// pip 的安装根（去掉 `\Lib\site-packages\pip` 之后的那个目录）—— 决策 174 的 `prefix`。
fn pip_prefix(m: &MachineFixture) -> String {
    let install = pip_install_path(m);
    let cut = install
        .to_lowercase()
        .find("\\lib\\site-packages")
        .unwrap_or_else(|| panic!("pip 的安装路径里要有 `\\Lib\\site-packages`：{install}"));
    install[..cut].to_owned()
}

/// `pip --version` 里那个**运行时版本号** —— 决策 172 的 pip `tool_version`。
///
/// `pip --version` 的输出是 `pip 24.0 from …\pip (python 3.12)`：**括号与 `python`
/// 是 pip 的排版，不是版本字符串的一部分**（字段名是 `tool_version`，值必须是一个版本），
/// 所以取括号里 `python ` 之后的那一段 —— `3.12`。node 那边相反：`node -v` 报的
/// 就是 `v24.19.0`，**原样**存（决策 172 的复核期澄清）。
fn pip_tool_version(m: &MachineFixture) -> String {
    let out = &probe(m, "pip.exe", PIP_VERSION).stdout;
    let start = out.find('(').expect("`pip --version` 里有括号段");
    let end = out[start..].find(')').expect("括号要闭合") + start;
    let inner = &out[start + 1..end];
    inner
        .split_whitespace()
        .nth(1)
        .unwrap_or_else(|| panic!("括号段里要有 `python <版本>`：{inner}"))
        .to_owned()
}

/// 从 `--show-origin` 的输出里数出某一层的 `user.name` / `user.email`
/// （**数固定装置，不看产品**）。
fn identity_in(m: &MachineFixture, args: &[&str]) -> Option<(String, String)> {
    let out = &probe(m, "git.exe", args).stdout;
    let mut name = None;
    let mut email = None;
    for line in out.lines() {
        let Some((_, key_value)) = line.split_once('\t') else {
            continue;
        };
        if let Some(value) = key_value.strip_prefix("user.name=") {
            name = Some(value.to_owned());
        }
        if let Some(value) = key_value.strip_prefix("user.email=") {
            email = Some(value.to_owned());
        }
    }
    name.zip(email)
}

/// `--show-origin` 第一行里 `file:` 后面的路径（**原样**，不归一化分隔符）。
fn origin_path(m: &MachineFixture, args: &[&str]) -> String {
    let out = &probe(m, "git.exe", args).stdout;
    let first = out.lines().next().expect("至少一行");
    let (origin, _) = first
        .split_once('\t')
        .unwrap_or_else(|| panic!("`--show-origin` 是 TAB 分隔：{first}"));
    origin
        .strip_prefix("file:")
        .unwrap_or_else(|| panic!("`--show-origin` 的第一段是 `file:<路径>`：{origin}"))
        .to_owned()
}

/// 决策 180 的常量表：`(env 变量, 相对后缀)`。命中前缀**且存在**才是一条 `cache-directory`。
const CACHE_RELATIVE: [(&str, &str); 6] = [
    ("LOCALAPPDATA", r"\pnpm\store"),
    ("LOCALAPPDATA", r"\npm-cache"),
    ("LOCALAPPDATA", r"\pip\Cache"),
    ("LOCALAPPDATA", r"\uv\cache"),
    ("USERPROFILE", r"\.cache\codex-runtimes"),
    ("USERPROFILE", r"\.m2\repository"),
];

/// 固定装置里**存在**的那几个缓存目录（期望条数 = 这个长度，不是 6）。
fn expected_cache_dirs(m: &MachineFixture) -> Vec<String> {
    CACHE_RELATIVE
        .iter()
        .map(|(env_name, relative)| {
            let root = env_var(m, env_name)
                .unwrap_or_else(|| panic!("固定装置缺 {env_name} —— 缓存目录的判据拼不出来"));
            format!("{root}{relative}")
        })
        .filter(|full| exists(m, full))
        .collect()
}

/// 决策 181 的私钥形状：`id_*`（不是 `.pub`）/ `*.pem` / `*.ppk` / `*.key`。
fn is_private_key_shape(name: &str) -> bool {
    let lower = name.to_lowercase();
    if lower.ends_with(".pub") {
        return false;
    }
    lower.starts_with("id_")
        || lower.ends_with(".pem")
        || lower.ends_with(".ppk")
        || lower.ends_with(".key")
}

/// 固定装置里那个假 token 必须**真的是**一个厂商凭据形状：GitLab PAT 前缀 + ≥16 位
/// `[A-Za-z0-9_-]`（真机验收脚本那条独立判据是 `glpat-[A-Za-z0-9_\-]{8,}`，
/// 这里取更严的 16）。
///
/// **这条判据是测试自己写的，不看产品的扫描器**：固定装置里的靶子如果退化成
/// "看起来像、其实扫不到的字符串"，这条用例会红 —— 否则"它一个片段都没漏出去"
/// 就变成了在空靶子上做的空测。
fn looks_like_gitlab_pat(text: &str) -> bool {
    let prefix = concat!("glpat", "-");
    text.match_indices(prefix).any(|(at, _)| {
        text[at + prefix.len()..]
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
            .count()
            >= 16
    })
}

/// 决策 182：JetBrains 的凭据库/私钥文件 → 跳过项 kind。
fn jetbrains_kind(name: &str) -> Option<&'static str> {
    match name.to_lowercase().as_str() {
        "c.kdbx" | "c.pwd" => Some("credential-database"),
        "idea.key" => Some("private-key-file"),
        _ => None,
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// 从产品输出里取一行的工具（只用来**定位**，期望值仍然来自固定装置）
// ─────────────────────────────────────────────────────────────────────────────

fn global_row<'a>(file: &'a GlobalsFile, tool: &str) -> &'a GlobalRow {
    let hits: Vec<&GlobalRow> = file.global.iter().filter(|row| row.tool == tool).collect();
    assert_eq!(hits.len(), 1, "`{tool}` 必须恰好一行：{:?}", file.global);
    hits[0]
}

fn config_row<'a>(file: &'a ConfigsFile, path: &str) -> Option<&'a ConfigRow> {
    file.config.iter().find(|row| same_path(&row.path, path))
}

/// 跳过清单里的一条。**先按完整路径找，再退回按最后一段找**（`SkipEntry.name` 是
/// "文件的相对位置"，实现可以选择印完整路径或相对位置）。
fn skip_entry<'a>(file: &'a SkippedFile, path: &str) -> Option<&'a SkipEntry> {
    let full = norm(path);
    let base = full.rsplit('\\').next().unwrap_or(&full).to_owned();
    let by_full: Vec<&SkipEntry> = file
        .skipped
        .iter()
        .filter(|entry| {
            let name = norm(&entry.name);
            name == full || name.ends_with(&format!("\\{full}"))
        })
        .collect();
    if let [only] = by_full.as_slice() {
        return Some(only);
    }
    let by_base: Vec<&SkipEntry> = file
        .skipped
        .iter()
        .filter(|entry| {
            let name = norm(&entry.name);
            name == base || name.ends_with(&format!("\\{base}"))
        })
        .collect();
    match by_base.as_slice() {
        [] => None,
        [only] => Some(only),
        many => panic!("`{path}` 在跳过清单里有多条匹配：{many:?}"),
    }
}

fn skip_entries_named<'a>(file: &'a SkippedFile, needle: &str) -> Vec<&'a SkipEntry> {
    let needle = norm(needle);
    file.skipped
        .iter()
        .filter(|entry| {
            let name = norm(&entry.name);
            name == needle || name.ends_with(&format!("\\{needle}"))
        })
        .collect()
}

fn package_map(row: &GlobalRow) -> BTreeMap<String, String> {
    row.packages
        .iter()
        .map(|package| (package.name.clone(), package.version.clone()))
        .collect()
}

// ─────────────────────────────────────────────────────────────────────────────
// 变异：**在内存里**改固定装置（磁盘上的文件一个字节都不动）
// ─────────────────────────────────────────────────────────────────────────────

fn visit_entry<F: FnMut(&mut FixturePath)>(m: &mut MachineFixture, path: &str, mut f: F) -> bool {
    let mut found = false;
    for dir in &mut m.dirs {
        let dir_path = dir.path.clone();
        for entry in &mut dir.entries {
            if same_path(&join_path(&dir_path, &entry.path), path) {
                f(entry);
                found = true;
            }
        }
    }
    for entry in &mut m.paths {
        if same_path(&entry.path, path) {
            f(entry);
            found = true;
        }
    }
    found
}

/// 改一个条目的 `content`（同时把 `size` 跟着改，免得 `inspect` 与 `read` 说两套话）。
fn mutate_content(m: &mut MachineFixture, path: &str, f: impl FnOnce(&str) -> String) {
    let current = content_of(m, path).unwrap_or_default();
    let text = f(&current);
    let size = text.len() as u64;
    let found = visit_entry(m, path, |entry| {
        entry.size = size;
        entry.content = Some(text.clone());
    });
    assert!(found, "固定装置里没有 `{path}` 可以变异");
}

/// 决策 179 的 `too-large`：固定装置里**只写 `size`**（256 KiB 的填充不是信息，
/// 不该进仓库、也不该让 `git diff` 不可看），内容在这里补成**等长**的字符串。
///
/// 为什么必须补：`too-large` 的判据是 `FileSystem::read` 读到的**内容长度**超过
/// `MAX_CONFIG_BYTES`，而不是声明的 `size` —— 只写 `size` 会落到 `unreadable`
/// （假文件系统不肯替我们编内容，那是故意的）。
fn with_synthetic_large_content(m: &mut MachineFixture, path: &str) {
    let size = entry_of(m, path)
        .unwrap_or_else(|| panic!("固定装置里没有 `{path}`"))
        .size;
    assert!(
        size > MAX_CONFIG_BYTES,
        "`{path}` 声明了 {size} 字节，没超过 {MAX_CONFIG_BYTES} —— 这条不是 too-large"
    );
    assert!(
        visit_entry(m, path, |entry| entry.content =
            Some("x".repeat(size as usize))),
        "`{path}` 要在固定装置里"
    );
    assert_eq!(
        content_of(m, path).map(|text| text.len() as u64),
        Some(size),
        "补出来的内容必须与 `size` 等长（决策 186 的构造期校验也会查这一条）"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// 一、固定装置本身（夹具与测试不许漂移）
// ─────────────────────────────────────────────────────────────────────────────

/// 八个目录、十五份机器文件，一份不多一份不少，而且每一份都真的描述了一台机器。
#[test]
fn the_fixture_directories_hold_exactly_the_machines_this_file_reads() {
    let mut total = 0;
    for (scenario, files) in SCENARIOS {
        let dir = scenario_dir(scenario);
        assert!(dir.is_dir(), "固定装置目录不存在：{}", dir.display());

        let mut actual: Vec<String> = std::fs::read_dir(&dir)
            .unwrap_or_else(|err| panic!("列不了 {}：{err}", dir.display()))
            .map(|entry| {
                entry
                    .expect("目录项")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .filter(|name| name.ends_with(".toml"))
            .collect();
        actual.sort();
        assert_eq!(actual, files.to_vec(), "{scenario}/ 里的机器文件清单");

        for file in files {
            total += 1;
            let m = machine(scenario, file);
            assert!(
                !m.dirs.is_empty() || !m.paths.is_empty(),
                "{scenario}/{file} 一个路径都没声明 —— 它描述的不是一台机器"
            );
            assert!(
                !m.env.is_empty(),
                "{scenario}/{file} 连 env 都没有：候选路径拼不出来"
            );
            // 夹具自证：`size` 与 `content` 的字节数必须逐条相等（不一致会让
            // `inspect` 与 `read` 说两套话，而 `too-large` 的判据来自后者）。
            for (path, entry) in entries(&m) {
                if let Some(content) = &entry.content {
                    assert_eq!(
                        entry.size,
                        content.len() as u64,
                        "{scenario}/{file} 的 {path}：size 与 content 不一致"
                    );
                }
            }
        }
    }
    assert_eq!(total, 17, "八个场景里一共十七份机器文件");
}

/// 固定装置里的字符串**不许含 `\r`**，也**不许用多行 TOML 字符串**。
///
/// 理由（踩过一次，代价是 11 条用例一起红）：`.gitattributes` 是 `*.toml text` 而本机
/// `core.autocrlf=true` ⇒ 工作树是 **CRLF**，而 `toml` crate **不**把多行字符串里的
/// `\r\n` 归一化 —— 于是解析出来的长度比仓库里的 LF 版多"换行数"个字节（实测 `.npmrc`
/// 声明 54、解析出 56），直接撞上决策 186 的 `content.is_some() ⇒ size == content.len()`。
/// `stdout` 同理：git 的输出是按行解析的，行尾的 `\r` 会进到值里。
///
/// 规矩是**单行基本字符串 + `\n` / `\t` 转义**（`content = "a\nb\tc"`）—— 它与工作树
/// 行尾无关。这条用例把那条规矩变成被强制的东西，而不是一句写在 README 里的建议。
#[test]
fn no_fixture_string_depends_on_the_working_tree_line_endings() {
    for (scenario, files) in SCENARIOS {
        for file in files {
            let path = scenario_dir(scenario).join(file);
            let raw = std::fs::read_to_string(&path)
                .unwrap_or_else(|err| panic!("读不到 {}：{err}", path.display()));
            assert!(
                !raw.contains("\"\"\""),
                "{scenario}/{file} 用了多行 TOML 字符串 —— 改成单行基本字符串 + `\\n` 转义"
            );
            let m = machine(scenario, file);
            for (entry_path, entry) in entries(&m) {
                if let Some(content) = &entry.content {
                    assert!(
                        !content.contains('\r'),
                        "{scenario}/{file} 的 {entry_path}：content 里有 `\\r`"
                    );
                }
            }
            for process in &m.processes {
                for (label, text) in [("stdout", &process.stdout), ("stderr", &process.stderr)] {
                    assert!(
                        !text.contains('\r'),
                        "{scenario}/{file} 的 `{}` {label} 里有 `\\r`",
                        process.program
                    );
                }
            }
        }
    }
}

/// 固定装置里那些"故意造出来"的形状必须真的在（否则依赖它们的用例是空测）。
#[test]
fn the_fixtures_really_hold_the_shapes_the_tests_count_on() {
    // 决策 173 的"一真一假"：npm 的前缀是联接、目标是版本目录；pip 的前缀是普通目录。
    let m = machine("npm-inside-version-dir", "machine.toml");
    let prefix = npm_prefix(&m);
    let node = entry_of(&m, &prefix).expect("npm 的 prefix 必须在固定装置里声明");
    assert_eq!(
        node.reparse,
        ReparseKind::SymlinkDir,
        "npm 的 prefix 是目录联接"
    );
    let target = node.link_target.expect("联接要有目标");
    assert!(
        looks_like_version_segment(&target),
        "联接目标里必须有版本段：{target}"
    );
    let pip = pip_prefix(&m);
    let pip_node = entry_of(&m, &pip).expect("pip 的 prefix 必须在固定装置里声明");
    assert_eq!(pip_node.reparse, ReparseKind::None);
    assert!(!looks_like_version_segment(&pip), "{pip} 里不该有版本段");
    assert_ne!(prefix, pip, "两个前缀必须真的不同，否则'一真一假'无从谈起");

    // 决策 171 的前提：两个工具的可执行文件都在 PATH 上
    assert!(
        find_on_path(&m, "npm.cmd").is_some(),
        "npm.cmd 必须在 PATH 上"
    );
    assert!(
        find_on_path(&m, "pip.exe").is_some(),
        "pip.exe 必须在 PATH 上"
    );

    // 决策 178：假 token 真的在固定装置里（**拼出来**，不写字面量）
    let xml = content_of(&m, r"C:\Users\dev\.m2\settings.xml");
    let _ = xml; // 本用例不查它，下面那一份才是
    let credential = machine("credential-shape", "machine.toml");
    let xml = content_of(&credential, r"C:\Users\dev\.m2\settings.xml")
        .expect("settings.xml 要有 content");
    let token = format!("{}0000000000000000", concat!("glpat", "-"));
    assert!(
        xml.contains(&token),
        "假 token 必须在固定装置里 —— 否则'它一个片段都没漏出去'是空测"
    );

    // 决策 179：三种 skip_reason 的形状
    let mut hard = machine("unreadable-too-large-binary", "machine.toml");
    let declared = entry_of(&hard, r"C:\Users\dev\.m2\settings.xml")
        .expect("too-large 那条要在固定装置里")
        .size;
    assert!(
        declared > MAX_CONFIG_BYTES,
        "too-large 那条必须声明超过 {MAX_CONFIG_BYTES} 字节，实际 {declared}"
    );
    assert!(
        content_of(&hard, r"C:\Users\dev\.m2\settings.xml").is_none(),
        "固定装置里**不带** 256 KiB 填充（内容由测试补），只留 `size` 与那句注释"
    );
    with_synthetic_large_content(&mut hard, r"C:\Users\dev\.m2\settings.xml");
    let big = content_of(&hard, r"C:\Users\dev\.m2\settings.xml").expect("补出来的内容");
    assert_eq!(big.len() as u64, declared, "补出来的内容与 size 等长");
    let binary =
        content_of(&hard, r"C:\Users\dev\.docker\config.json").expect("binary 那条要有 content");
    assert!(binary.contains('\u{0}'), "binary 那条的内容里要有 NUL");
    assert!(
        content_of(&hard, r"C:\Users\dev\.gitconfig").is_none(),
        "unreadable 那条**不许**有 content（假文件系统不肯替我们编内容）"
    );

    // 决策 180：六个缓存目录都在固定装置里，其中一个**不存在**
    let caches = machine("caches-and-keys", "machine.toml");
    let declared = expected_cache_dirs(&caches);
    assert_eq!(declared.len(), 5, "六个缓存目录里恰好五个存在");
    let all_six = CACHE_RELATIVE.len();
    assert_eq!(all_six, 6);

    // 决策 182：JetBrains 的产品目录与三个凭据文件都在
    let product = format!(
        "{}\\JetBrains\\IntelliJIdea2026.1",
        env_var(&caches, "APPDATA").expect("APPDATA")
    );
    assert!(exists(&caches, &product), "{product} 必须在固定装置里");
    for name in ["c.kdbx", "c.pwd", "idea.key"] {
        assert!(
            exists(&caches, &join_path(&product, name)),
            "{name} 必须在产品目录里"
        );
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// 二、决策 173：前缀是不是落在"按版本隔离"的目录里（一真一假同机）
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn a_prefix_that_resolves_into_a_version_directory_is_flagged_and_a_plain_one_is_not() {
    const SCENARIO: &str = "npm-inside-version-dir";
    let m = machine(SCENARIO, "machine.toml");
    let file = globals(SCENARIO, "machine.toml");

    let npm = global_row(&file, "npm");
    assert_eq!(
        npm.prefix.as_deref(),
        Some(npm_prefix(&m).as_str()),
        "prefix 原样来自 `npm config get prefix`"
    );
    assert_eq!(
        npm.prefix_inside_version_dir,
        Some(true),
        "npm 的前缀是联接、目标是 `…\\nvm\\v24.19.0` —— 决策 173 的 ①④ 支"
    );

    let pip = global_row(&file, "pip");
    assert_eq!(pip.prefix.as_deref(), Some(pip_prefix(&m).as_str()));
    assert_eq!(
        pip.prefix_inside_version_dir,
        Some(false),
        "pip 的前缀是普通目录、路径里没有版本段 —— 四支全假"
    );

    // 两个答案真的不同：一个"恒 true"（或"恒 false"）的实现在这里必然红一条。
    assert_ne!(
        npm.prefix_inside_version_dir, pip.prefix_inside_version_dir,
        "同一次采集里必须一真一假"
    );

    // 决策 174：`prefix` 与 `prefix_inside_version_dir` 同生共死
    for row in &file.global {
        assert_eq!(
            row.prefix.is_some(),
            row.prefix_inside_version_dir.is_some(),
            "{}：两个键必须都出或都不出",
            row.tool
        );
    }
}

/// 决策 173 的 ② 与 ③ 支**各自单独为真**时也必须判 `true`。
///
/// 上面那条用例的两台机器只覆盖"①+④ 为真"与"四支全假"。这两份装置里除"祖先是一个
/// 目录联接"（②）与"路径里有一段版本号"（③）之外**没有任何 reparse point** ——
/// 所以一个只判 reparse（①/④）的实现在这里会红。
#[test]
fn the_ancestor_and_version_segment_branches_are_checked_on_their_own() {
    const SCENARIO: &str = "npm-inside-version-dir";

    // ③ 支单独为真：prefix 是普通目录，但路径里有一段 `v24.19.0`。
    let segment = machine(SCENARIO, "version-segment-prefix.toml");
    let prefix = npm_prefix(&segment);
    let facts = entry_of(&segment, &prefix).expect("prefix 要在固定装置里声明");
    assert_eq!(facts.reparse, ReparseKind::None, "前提：① 支必须是假");
    assert!(
        facts.link_target.is_none(),
        "前提：④ 支必须是假（它自己没有 reparse 目标）"
    );
    assert!(
        looks_like_version_segment(&prefix),
        "前提：③ 支必须为真（{prefix} 里要有一段像版本号）"
    );
    let file = globals(SCENARIO, "version-segment-prefix.toml");
    assert_eq!(
        global_row(&file, "npm").prefix_inside_version_dir,
        Some(true),
        "只有 ③ 支为真，结论也必须是 true（只判 reparse 的实现会在这里红）"
    );
    assert_eq!(
        global_row(&file, "pip").prefix_inside_version_dir,
        Some(false),
        "同一台机器上的对照：pip 的前缀四支全假"
    );

    // ② 支单独为真：prefix 自己是普通目录，但它的**祖先**是一个目录联接。
    let ancestor = machine(SCENARIO, "ancestor-junction.toml");
    let prefix = npm_prefix(&ancestor);
    let facts = entry_of(&ancestor, &prefix).expect("prefix 要在固定装置里声明");
    assert_eq!(facts.reparse, ReparseKind::None, "前提：① 支必须是假");
    assert!(!looks_like_version_segment(&prefix), "前提：③ 支必须是假");
    let parent = prefix.rsplit_once('\\').expect("有父目录").0.to_owned();
    let parent_facts = entry_of(&ancestor, &parent).expect("祖先要在固定装置里声明");
    assert_eq!(
        parent_facts.reparse,
        ReparseKind::SymlinkDir,
        "前提：② 支必须为真（{parent} 是目录联接）"
    );
    let file = globals(SCENARIO, "ancestor-junction.toml");
    assert_eq!(
        global_row(&file, "npm").prefix_inside_version_dir,
        Some(true),
        "只有 ② 支为真，结论也必须是 true（只看 prefix 自己的实现会在这里红）"
    );
    assert_eq!(
        global_row(&file, "pip").prefix_inside_version_dir,
        Some(false),
        "同一台机器上的对照"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// 三、决策 172/174：同一个前缀、两个 Node 版本 → 两份独立清单
// ─────────────────────────────────────────────────────────────────────────────
#[test]
fn the_same_prefix_under_two_node_versions_yields_two_independent_lists() {
    const SCENARIO: &str = "two-node-versions";
    let (v22, v24) = (
        machine(SCENARIO, "node-v22.toml"),
        machine(SCENARIO, "node-v24.toml"),
    );

    // 前提：两份装置的 prefix **逐字相同**，只有版本与包集合不同。
    assert_eq!(
        npm_prefix(&v22),
        npm_prefix(&v24),
        "前提：切 Node 版本时 prefix 不变，变的只是它指向哪里"
    );
    assert_ne!(
        npm_packages(&v22),
        npm_packages(&v24),
        "前提：包集合必须不同"
    );
    assert_ne!(node_version(&v22), node_version(&v24), "前提：版本必须不同");

    for (file_name, m) in [("node-v22.toml", &v22), ("node-v24.toml", &v24)] {
        let file = globals(SCENARIO, file_name);
        assert_eq!(file.global.len(), 1, "{file_name}：只有 npm 一个工具");
        let npm = global_row(&file, "npm");
        assert_eq!(
            npm.tool_version,
            node_version(m),
            "{file_name}：`tool_version` 原样来自 `node -v`"
        );
        assert_eq!(
            package_map(npm),
            npm_packages(m),
            "{file_name}：包集合必须等于它自己那份固定装置里的 JSON"
        );
        // 决策 174：`packages` 按 name 排序
        assert!(
            npm.packages.windows(2).all(|w| w[0].name <= w[1].name),
            "{file_name}：包清单必须按 name 排序"
        );
    }

    // 两份清单**不许被合并**：版本不同、包集合不同、各自的包不相等。
    let a = globals(SCENARIO, "node-v22.toml");
    let b = globals(SCENARIO, "node-v24.toml");
    assert_ne!(
        global_row(&a, "npm").tool_version,
        global_row(&b, "npm").tool_version
    );
    assert_ne!(
        package_map(global_row(&a, "npm")),
        package_map(global_row(&b, "npm"))
    );
    assert!(
        !package_map(global_row(&a, "npm"))
            .keys()
            .all(|name| package_map(global_row(&b, "npm")).contains_key(name)),
        "两份清单不该是同一份"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// 四、决策 175：枚举失败的三种形态
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn a_failed_enumeration_still_reports_the_row_and_names_the_concrete_reason() {
    const SCENARIO: &str = "enumeration-fails";
    let cases = [
        ("bad-json.toml", "bad-json"),
        ("command-failed.toml", "command-failed"),
        ("timed-out.toml", "timed-out"),
    ];

    for (file, slug) in cases {
        let m = machine(SCENARIO, file);

        // 票据原文：枚举失败**不报错退出**。在这一层就是"捕获本身不许失败"。
        let files = fixture(SCENARIO, file)
            .try_files(&[Section::Globals], AT)
            .unwrap_or_else(|err| panic!("{file}：枚举失败不许让捕获失败：{err}"));
        assert!(files.contains_key("globals.toml"), "{file}");

        let globals = globals(SCENARIO, file);
        let npm = global_row(&globals, "npm");
        assert_eq!(
            npm.enumerate_error.as_deref(),
            Some(slug),
            "{file}：具体的失败 slug"
        );
        // 行本身照样写出去，带着 prefix 与 tool_version（决策 175）
        assert_eq!(npm.tool_version, node_version(&m), "{file}：失败也要带版本");
        assert_eq!(
            npm.prefix.as_deref(),
            Some(npm_prefix(&m).as_str()),
            "{file}：失败也要带前缀"
        );
        assert!(npm.packages.is_empty(), "{file}：数不出包");

        // 枚举失败**不走 skipped.toml**（决策 175：同一个事实写两处就是第二份真相）
        let skipped = fixture(SCENARIO, file)
            .capture(&[Section::Globals], AT)
            .skipped;
        assert!(
            skipped.is_none(),
            "{file}：只扫 globals 时不该有跳过清单，更不该把枚举失败写进去"
        );
    }

    // 三个 slug 必须互不相同 —— "都是失败"不是分类。
    let slugs: Vec<String> = cases
        .iter()
        .map(|(file, _)| {
            globals(SCENARIO, file)
                .global
                .iter()
                .filter_map(|row| row.enumerate_error.clone())
                .collect::<Vec<_>>()
                .join(",")
        })
        .collect();
    let unique: std::collections::BTreeSet<&String> = slugs.iter().collect();
    assert_eq!(
        unique.len(),
        3,
        "三种失败形态必须是三个不同的 slug：{slugs:?}"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// 五、决策 183：Git 身份的层级
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn the_git_identity_names_the_layer_it_came_from() {
    const SCENARIO: &str = "git-identity-layers";
    let cases = [
        ("system-only.toml", "system"),
        ("global-only.toml", "global"),
        ("global-wins.toml", "global"),
        ("missing.toml", "missing"),
        ("git-unavailable.toml", "unknown"),
    ];

    for (file, source) in cases {
        let m = machine(SCENARIO, file);
        let configs = configs(SCENARIO, file);
        let git = configs.git.as_ref().unwrap_or_else(|| {
            panic!(
                "{file}：`[git]` 表必须存在 —— 决策 183 说省略整张表会让'没问'与\
                 '没装 git'长得一样（identity_source = {source} 才是那句话）"
            )
        });
        assert_eq!(git.identity_source, source, "{file}");

        match source {
            "system" | "global" => {
                let args = if source == "system" {
                    GIT_SYSTEM
                } else {
                    GIT_GLOBAL
                };
                let (name, email) = identity_in(&m, args).unwrap_or_else(|| {
                    panic!("{file}：这一层该有身份 —— 固定装置里没数出来，夹具坏了")
                });
                assert_eq!(git.user_name.as_deref(), Some(name.as_str()), "{file}");
                assert_eq!(git.user_email.as_deref(), Some(email.as_str()), "{file}");
                assert_eq!(
                    git.system_config.as_deref(),
                    Some(origin_path(&m, GIT_SYSTEM).as_str()),
                    "{file}：系统级路径来自 git 自己的回答，**原样**"
                );
                assert_eq!(
                    git.global_config.as_deref(),
                    Some(origin_path(&m, GIT_GLOBAL).as_str()),
                    "{file}：全局级路径来自 git 自己的回答，**原样**"
                );
            }
            "missing" => {
                assert!(
                    git.user_name.is_none() && git.user_email.is_none(),
                    "{file}：两层都没有身份时两个键都不出（'没有身份'与'身份是空串'是两句话）"
                );
                assert_eq!(
                    git.system_config.as_deref(),
                    Some(origin_path(&m, GIT_SYSTEM).as_str()),
                    "{file}：我们问得到这两份文件在哪，只是里面没有身份"
                );
                assert_eq!(
                    git.global_config.as_deref(),
                    Some(origin_path(&m, GIT_GLOBAL).as_str()),
                    "{file}"
                );
            }
            "unknown" => {
                assert!(
                    git.system_config.is_none() && git.global_config.is_none(),
                    "{file}：git 不可用时连路径都问不到，两个键都不出"
                );
                assert!(
                    git.user_name.is_none() && git.user_email.is_none(),
                    "{file}"
                );
            }
            other => panic!("没见过的 identity_source：{other}"),
        }
    }

    // 全局赢：两层都有身份、值**不同**，取的是全局级那一对。
    let wins = machine(SCENARIO, "global-wins.toml");
    let system = identity_in(&wins, GIT_SYSTEM).expect("系统级有身份");
    let global = identity_in(&wins, GIT_GLOBAL).expect("全局级有身份");
    assert_ne!(system, global, "前提：两层的值必须不同，否则'谁赢'分不出来");
    let git = configs(SCENARIO, "global-wins.toml").git.expect("[git]");
    assert_eq!(git.user_name.as_deref(), Some(global.0.as_str()));
    assert_eq!(git.user_email.as_deref(), Some(global.1.as_str()));

    // 系统级独有：全局级没有身份时，系统级的身份才被采纳。
    let only_system = machine(SCENARIO, "system-only.toml");
    assert!(
        identity_in(&only_system, GIT_GLOBAL).is_none(),
        "前提：system-only 的全局级必须没有身份"
    );
    assert!(identity_in(&only_system, GIT_SYSTEM).is_some());
}

/// 决策 176：`layer` **只在有层级概念的文件上出键**（git 的 system / global）。
/// 而且系统级那一份**只从 git 的回答里来**：它的路径就是 `--show-origin` 第一行的
/// `file:` 路径（**原样**，正斜杠），git 不可用时**连这条行都没有** ——
/// 但 `~/.gitconfig` 那条**照旧在**（它是 env 候选，与 git 可不可用无关）。
#[test]
fn the_config_layer_appears_only_where_it_means_something() {
    const SCENARIO: &str = "git-identity-layers";
    let m = machine(SCENARIO, "system-only.toml");
    let configs_file = configs(SCENARIO, "system-only.toml");

    let global =
        config_row(&configs_file, r"C:\Users\dev\.gitconfig").expect("~/.gitconfig 是候选");
    assert_eq!(global.kind, "git");
    assert_eq!(global.layer.as_deref(), Some("global"));
    assert!(global.captured, "这一份在固定装置里有内容");

    let system_path = origin_path(&m, GIT_SYSTEM);
    let system = config_row(&configs_file, &system_path).unwrap_or_else(|| {
        panic!(
            "{system_path}（git 自己报的路径）必须是一条 [[config]] 行：{:?}",
            configs_file.config
        )
    });
    assert_eq!(system.kind, "git");
    assert_eq!(system.layer.as_deref(), Some("system"));
    assert!(system.captured);
    let content = content_of(&m, &system_path).expect("系统级 gitconfig 要有 content");
    assert_eq!(
        system.content_hash.as_deref(),
        Some(sha256_of(&content).as_str()),
        "哈希是测试自己算的"
    );
    assert_eq!(system.bytes, Some(content.len() as u64));

    // 只有 git 的两份有 layer；其余候选都不出这个键。
    for row in &configs_file.config {
        if row.kind != "git" {
            assert!(row.layer.is_none(), "{} 不该有 layer", row.path);
        }
    }

    // git 不可用：系统级那条行不存在，而 `~/.gitconfig` 照旧在。
    let unavailable = configs(SCENARIO, "git-unavailable.toml");
    assert!(
        config_row(&unavailable, r"C:\Users\dev\.gitconfig").is_some(),
        "`~/.gitconfig` 是 env 候选 —— 与 git 可不可用无关"
    );
    assert!(
        !unavailable
            .config
            .iter()
            .any(|row| row.layer.as_deref() == Some("system")),
        "系统级那条行只从 git 的回答里来：git 不可用时它不该存在"
    );
}

/// 决策 171：**找不到可执行文件的工具不产生行**（不是"报了个空行"）。
#[test]
fn a_tool_whose_executable_is_not_on_path_produces_no_row() {
    const SCENARIO: &str = "git-identity-layers";
    let m = machine(SCENARIO, "git-unavailable.toml");
    assert!(
        find_on_path(&m, "npm.cmd").is_none(),
        "前提：这份装置上没有 npm"
    );
    assert!(
        find_on_path(&m, "pip.exe").is_none(),
        "前提：这份装置上没有 pip"
    );

    let file = globals(SCENARIO, "git-unavailable.toml");
    assert!(
        file.global.is_empty(),
        "没有可执行文件的工具不许产生行：{:?}",
        file.global
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// 六、决策 178：内容级凭据形状（本票的安全红线）
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn a_credential_shaped_config_file_is_skipped_and_no_fragment_of_it_leaves_the_machine() {
    const SCENARIO: &str = "credential-shape";
    const SETTINGS: &str = r"C:\Users\dev\.m2\settings.xml";
    let m = machine(SCENARIO, "machine.toml");
    let xml = content_of(&m, SETTINGS).expect("settings.xml 要有 content");
    let token = format!("{}0000000000000000", concat!("glpat", "-"));
    assert!(
        xml.contains(&token),
        "前提：假 token 真的在固定装置里（否则这条用例什么都没测）"
    );
    assert!(
        looks_like_gitlab_pat(&xml),
        "固定装置里的靶子必须是一个**真形状**（前缀 + ≥16 位）—— 判据是测试自己写的"
    );

    let configs = configs(SCENARIO, "machine.toml");
    let row = config_row(&configs, SETTINGS)
        .expect("被跳过的文件**也必须**在清单里 —— 否则'跳过了什么'无处可查");
    assert!(!row.captured, "带凭据形状的文件不许捕获");
    assert_eq!(
        row.skip_reason.as_deref(),
        Some("contains-credential-shape")
    );
    assert!(
        row.bytes.is_none() && row.content_hash.is_none(),
        "跳过的行**既不出 bytes 也不出 content_hash**（没读到内容，两个都无从谈起）"
    );

    // 决策 179：跳过清单里必须有它，且 `reason` 具体。
    let skipped = skips(SCENARIO, "machine.toml");
    let entry = skip_entry(&skipped, SETTINGS).unwrap_or_else(|| {
        panic!(
            "被跳过的候选必须在 skipped.toml 里（决策 179：静默跳过是 bug）；\
             实际清单：{:?}",
            skipped.skipped
        )
    });
    assert_eq!(entry.section, "configs");
    assert_eq!(entry.kind, "contains-credential-shape");
    assert!(!entry.reason.trim().is_empty(), "理由必须具体");

    // ── 安全红线：渲染出来的**每一个** String 里都不许有 token 的任何 8 字符窗口 ──
    let bundle = fixture(SCENARIO, "machine.toml").capture(&[Section::Configs], AT);
    let rendered = render(&bundle).expect("渲染");
    assert!(
        rendered.len() >= 3,
        "至少要有 schema.toml / configs.toml / skipped.toml 三份：{:?}",
        rendered.iter().map(|(name, _)| *name).collect::<Vec<_>>()
    );
    for (name, text) in &rendered {
        for window in token.as_bytes().windows(8) {
            let window = std::str::from_utf8(window).expect("固定装置是 UTF-8");
            assert!(
                !text.contains(window),
                "{name} 里出现了凭据材料的片段：{window}"
            );
        }
    }

    // 对照组：这三份一个凭据形状都没有，**必须**被捕获（否则"全部跳过"也能过）。
    for path in [
        r"C:\Users\dev\.gitconfig",
        r"C:\Users\dev\.npmrc",
        r"C:\Users\dev\.docker\config.json",
    ] {
        let content = content_of(&m, path).unwrap_or_else(|| panic!("{path} 要有 content"));
        let row = config_row(&configs, path).unwrap_or_else(|| panic!("{path} 必须是候选"));
        assert!(row.captured, "{path} 没有凭据形状，必须捕获");
        assert_eq!(row.bytes, Some(content.len() as u64), "{path}");
        assert_eq!(
            row.content_hash.as_deref(),
            Some(sha256_of(&content).as_str()),
            "{path}：哈希是测试自己算的"
        );
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// 七、决策 179：unreadable / too-large / binary
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn unreadable_too_large_and_binary_files_are_each_skipped_for_a_specific_reason() {
    const SCENARIO: &str = "unreadable-too-large-binary";
    // 固定装置里只写了 `size = 262145`（256 KiB 的填充不是信息，不进仓库）——
    // 内容由 `with_synthetic_large_content` 在内存里补，所以这一份的夹具必须从
    // **改过的**机器造，而不是从磁盘上的 `machine()` 直接造。
    let mut m = machine(SCENARIO, "machine.toml");
    with_synthetic_large_content(&mut m, r"C:\Users\dev\.m2\settings.xml");
    let bundle = CaptureFixture::build(&m).capture(&[Section::Configs], AT);
    let configs = bundle.configs.expect("configs 这一节");
    let skipped = bundle
        .skipped
        .expect("扫过 configs 就必须产出 skipped.toml（决策 179）");

    let cases = [
        (r"C:\Users\dev\.gitconfig", "unreadable"),
        (r"C:\Users\dev\.m2\settings.xml", "too-large"),
        (r"C:\Users\dev\.docker\config.json", "binary"),
    ];

    for (path, slug) in cases {
        let row = config_row(&configs, path).unwrap_or_else(|| panic!("{path} 必须是候选"));
        assert!(!row.captured, "{path}：没写进 configs 的内容就不算捕获");
        assert_eq!(row.skip_reason.as_deref(), Some(slug), "{path}");
        assert!(
            row.bytes.is_none() && row.content_hash.is_none(),
            "{path}：跳过的行既不出 bytes 也不出 content_hash"
        );

        let entry = skip_entry(&skipped, path)
            .unwrap_or_else(|| panic!("{path} 必须在 skipped.toml 里（决策 179）"));
        assert_eq!(entry.section, "configs", "{path}");
        assert_eq!(entry.kind, slug, "{path}");
        assert!(!entry.reason.trim().is_empty(), "{path}：理由必须具体");
    }

    // 对照：`.npmrc` 必须被捕获，且哈希/字节数是我自己从 content 算的。
    let npmrc = content_of(&m, r"C:\Users\dev\.npmrc").expect(".npmrc 要有 content");
    let row = config_row(&configs, r"C:\Users\dev\.npmrc").expect("候选");
    assert!(row.captured, "没有问题的文件必须捕获");
    assert_eq!(row.bytes, Some(npmrc.len() as u64));
    assert_eq!(
        row.content_hash.as_deref(),
        Some(sha256_of(&npmrc).as_str())
    );

    // 三个 slug 互不相同 —— 一个"读不了就说读不了"的实现会在这里红。
    let unique: std::collections::BTreeSet<&str> = cases.iter().map(|(_, slug)| *slug).collect();
    assert_eq!(unique.len(), 3);
}

// ─────────────────────────────────────────────────────────────────────────────
// 八、决策 180/181/182：缓存目录、私钥形状、JetBrains
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn cache_directories_private_keys_and_jetbrains_entries_are_classified() {
    const SCENARIO: &str = "caches-and-keys";
    let m = machine(SCENARIO, "machine.toml");
    let configs = configs(SCENARIO, "machine.toml");
    let skipped = skips(SCENARIO, "machine.toml");

    // ── 决策 180：常量表里**存在**的那几个各一条；不存在的那一个一条都不许有 ──
    let existing = expected_cache_dirs(&m);
    assert_eq!(existing.len(), 5, "六个里恰好五个存在");
    for dir in &existing {
        let entry =
            skip_entry(&skipped, dir).unwrap_or_else(|| panic!("缓存目录 {dir} 必须在跳过清单里"));
        assert_eq!(entry.kind, "cache-directory", "{dir}");
        assert_eq!(entry.section, "configs", "{dir}");
        // 缓存目录**只在跳过清单里**：不进 configs.toml，也不报体积。
        assert!(
            config_row(&configs, dir).is_none(),
            "{dir} 只该出现在跳过清单里"
        );
    }
    let missing_cache = format!(
        "{}{}",
        env_var(&m, "LOCALAPPDATA").expect("LOCALAPPDATA"),
        r"\uv\cache"
    );
    assert!(!exists(&m, &missing_cache), "前提：这一条声明为不存在");
    assert!(
        skip_entries_named(&skipped, &missing_cache).is_empty(),
        "命中前缀但**不存在**的缓存目录不该出现在跳过清单里"
    );

    // ── 决策 181：~/.ssh ──
    let ssh = format!(
        "{}\\{}",
        env_var(&m, "USERPROFILE").expect("USERPROFILE"),
        ".ssh"
    );
    let ssh_dir = m
        .dirs
        .iter()
        .find(|dir| same_path(&dir.path, &ssh))
        .unwrap_or_else(|| panic!("{ssh} 必须声明为目录（私钥是靠 list_dir 发现的）"));
    let mut private_keys = 0;
    for entry in &ssh_dir.entries {
        let full = join_path(&ssh, &entry.path);
        if is_private_key_shape(&entry.path) {
            private_keys += 1;
            let skip = skip_entry(&skipped, &full)
                .unwrap_or_else(|| panic!("{full} 是私钥形状，必须在跳过清单里"));
            assert_eq!(skip.kind, "private-key-file", "{full}");
        }
        if entry.path.eq_ignore_ascii_case("known_hosts") {
            let skip = skip_entry(&skipped, &full).expect("known_hosts 必须在跳过清单里");
            assert_eq!(skip.kind, "host-keys-not-captured", "{full}");
        }
        if entry.path.eq_ignore_ascii_case("config") {
            let content = content_of(&m, &full).expect("~/.ssh/config 要有 content");
            let row = config_row(&configs, &full).expect("~/.ssh/config 是候选");
            assert!(row.captured, "~/.ssh/config 是票据点名要捕获的");
            assert_eq!(
                row.content_hash.as_deref(),
                Some(sha256_of(&content).as_str())
            );
        }
    }
    assert_eq!(private_keys, 4, "私钥形状恰好四条");
    // "没看"不是"跳过"：这两个不是候选，一个字都不该出现。
    for name in ["id_rsa.pub", "authorized_keys"] {
        assert!(
            skip_entries_named(&skipped, name).is_empty(),
            "{name} 不在候选集里 —— '没看'不是'跳过'"
        );
    }

    // ── 决策 182：JetBrains ──
    let product = format!(
        "{}\\JetBrains\\IntelliJIdea2026.1",
        env_var(&m, "APPDATA").expect("APPDATA")
    );
    let row = config_row(&configs, &product).expect("产品目录要出一条标记行");
    assert!(row.captured, "{product}：标记行");
    assert_eq!(row.kind, "jetbrains");
    assert!(
        row.bytes.is_none() && row.content_hash.is_none(),
        "目录不是文件：标记行没有 bytes / content_hash（决策 182 的唯一例外）"
    );
    // 目录里的条目**按固定装置自己数**：命中凭据库/私钥形状的要有跳过项，
    // 其余（`consentOptions` / `acp-agents`）不在候选集里，一个字都不许出现。
    let product_dir = m
        .dirs
        .iter()
        .find(|dir| same_path(&dir.path, &product))
        .unwrap_or_else(|| panic!("{product} 要声明为目录（里面的凭据文件靠 list_dir 发现）"));
    let mut jetbrains_keys = 0;
    for entry in &product_dir.entries {
        let full = join_path(&product, &entry.path);
        if let Some(slug) = jetbrains_kind(&entry.path) {
            jetbrains_keys += 1;
            let skip =
                skip_entry(&skipped, &full).unwrap_or_else(|| panic!("{full} 必须在跳过清单里"));
            assert_eq!(skip.kind, slug, "{full}");
        } else {
            assert!(
                skip_entries_named(&skipped, &entry.path).is_empty(),
                "{} 不在候选集里 —— '没看'不是'跳过'",
                entry.path
            );
        }
    }
    assert_eq!(jetbrains_keys, 3, "产品目录里凭据库/私钥恰好三条");
    assert!(
        skip_entries_named(&skipped, ".bashrc").is_empty(),
        "`.bashrc` 不是候选 —— '没看'不是'跳过'"
    );

    // ── 决策 184：`captured` 与 `skip_reason` 是两个互斥子集 ──
    for row in &configs.config {
        if row.kind == "jetbrains" {
            continue; // 目录标记行是唯一的例外
        }
        if row.captured {
            assert!(
                row.bytes.is_some() && row.content_hash.is_some(),
                "{}：捕获了就要有 bytes 与哈希",
                row.path
            );
            assert!(row.skip_reason.is_none(), "{}：不许既捕获又跳过", row.path);
        } else {
            assert!(
                row.skip_reason
                    .as_deref()
                    .is_some_and(|reason| !reason.trim().is_empty()),
                "{}：跳过了就要有具体理由",
                row.path
            );
            assert!(
                row.bytes.is_none() && row.content_hash.is_none(),
                "{}：跳过的行既不出 bytes 也不出 content_hash",
                row.path
            );
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// 九、幂等与文件形状（决策 165/166）
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn capturing_the_same_machine_twice_is_byte_identical() {
    const SCENARIO: &str = "idempotent";
    let m = machine(SCENARIO, "machine.toml");
    let sections = [Section::Globals, Section::Configs];

    let first = fixture(SCENARIO, "machine.toml").files(&sections, AT);
    let second = fixture(SCENARIO, "machine.toml").files(&sections, AT);
    assert_eq!(
        first, second,
        "同一份装置、同一个 captured_at 两次捕获必须逐字节相同"
    );
    for name in ["globals.toml", "configs.toml", "skipped.toml"] {
        assert!(first.contains_key(name), "缺 {name}：{:?}", first.keys());
    }

    // 两次捕获的对象也逐条相同，且哈希两次都等于我自己算的。
    let a = configs(SCENARIO, "machine.toml");
    let b = configs(SCENARIO, "machine.toml");
    assert_eq!(a, b);
    let mut checked = 0;
    for row in a
        .config
        .iter()
        .filter(|row| row.captured && row.kind != "jetbrains")
    {
        let content = content_of(&m, &row.path)
            .unwrap_or_else(|| panic!("{} 的 content 必须在固定装置里", row.path));
        assert_eq!(row.bytes, Some(content.len() as u64), "{}", row.path);
        assert_eq!(
            row.content_hash.as_deref(),
            Some(sha256_of(&content).as_str()),
            "{}",
            row.path
        );
        checked += 1;
    }
    assert!(checked >= 4, "这一份装置该有多份被捕获的配置：{checked}");

    // globals 那一半也要稳定，而且包清单来自固定装置自己的 JSON。
    let g = globals(SCENARIO, "machine.toml");
    assert_eq!(g, globals(SCENARIO, "machine.toml"));
    // 行序按 `tool` 排序（决策 174：确定性是幂等性的一部分）——
    // 这一份装置两个工具都在（npm + pip），所以顺序是可断言的。
    let tools: Vec<&str> = g.global.iter().map(|row| row.tool.as_str()).collect();
    assert_eq!(tools, ["npm", "pip"], "工具行按 tool 排序");
    assert_eq!(package_map(global_row(&g, "npm")), npm_packages(&m));
    assert_eq!(package_map(global_row(&g, "pip")), pip_packages(&m));
    assert_eq!(global_row(&g, "pip").tool_version, pip_tool_version(&m));
}

/// 决策 165/166：section 的顺序、文件写入的顺序、`schema.toml` 里那份清单。
#[test]
fn the_section_order_and_the_file_order_are_the_frozen_ones() {
    assert_eq!(
        Section::ALL,
        [
            Section::Tools,
            Section::Path,
            Section::Env,
            Section::Wsl,
            Section::Globals,
            Section::Configs
        ],
        "两个新 section 追加在末尾（决策 165）"
    );

    let bundle = fixture("idempotent", "machine.toml").capture_all(AT);
    let rendered = render(&bundle).expect("渲染");
    let names: Vec<&str> = rendered.iter().map(|(name, _)| *name).collect();
    assert_eq!(
        names,
        [
            "schema.toml",
            "tools.toml",
            "path.toml",
            "env.toml",
            "wsl.toml",
            "globals.toml",
            "configs.toml",
            "skipped.toml"
        ],
        "写入顺序 = schema 在最前、skipped 在最后（决策 165）"
    );

    let schema_text = rendered
        .iter()
        .find(|(name, _)| *name == "schema.toml")
        .map(|(_, text)| text.clone())
        .expect("schema.toml");
    let schema: SchemaFile = toml::from_str(&schema_text).expect("schema.toml 要能读回来");
    // `schema.toml` 里那份清单是**排序过的**（`SchemaFile::new` 的文档写着"sections
    // 会被排序，所以调用方的顺序不影响文件内容"）。这与"写入顺序 = `Section::ALL`"
    // 是**两件事**，而决策 165 把它们写在同一句里 —— 稳定性的要求（两次运行逐字节相同、
    // 老快照的顺序不变）由排序同样满足。这里断言排序后的六个 slug，
    // 并把这条措辞差异记进交付报告（不改产品，也不改期望去迎合它）。
    assert_eq!(
        schema.sections,
        ["configs", "env", "globals", "path", "tools", "wsl"],
        "schema.toml 里的 section slug：六个都在、按字典序（排序即稳定）"
    );

    // 决策 166：加 section 不递增 schemaVersion。
    assert_eq!(
        schema.schema_version, 1,
        "追加 section 不是删改（决策 166）"
    );
}

/// 决策 179：跳过清单的触发条件是"扫过 env 或 configs"。
#[test]
fn only_globals_has_no_skip_list_but_only_configs_does() {
    let globals_only = fixture("caches-and-keys", "machine.toml").files(&[Section::Globals], AT);
    assert_eq!(
        globals_only.keys().copied().collect::<Vec<_>>(),
        ["globals.toml", "schema.toml"],
        "只扫 globals 时不该有跳过清单"
    );

    let configs_only = fixture("caches-and-keys", "machine.toml").files(&[Section::Configs], AT);
    assert_eq!(
        configs_only.keys().copied().collect::<Vec<_>>(),
        ["configs.toml", "schema.toml", "skipped.toml"],
        "扫过 configs 就必须有跳过清单"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// 十、两条变异测试：证明上面那些断言**真的会红**
// ─────────────────────────────────────────────────────────────────────────────

/// 变异①：把固定装置里的假 token 换成普通文本 → "必须被跳过"那条结论必须翻转。
///
/// 只在**内存里**改（磁盘上的 `fixtures/capture/**` 一个字节都不动）：
/// 把 `settings.xml` 的 `content` 里的 token 换成 `not-a-credential`，
/// 再跑同一段判据 —— 它必须从"跳过"变成"捕获"。
#[test]
fn mutation_removing_the_credential_shape_flips_the_verdict() {
    const SCENARIO: &str = "credential-shape";
    const SETTINGS: &str = r"C:\Users\dev\.m2\settings.xml";
    let token = format!("{}0000000000000000", concat!("glpat", "-"));

    // 原始（磁盘上的固定装置）：跳过，理由是内容级凭据形状。
    let original = configs(SCENARIO, "machine.toml");
    let row = config_row(&original, SETTINGS).expect("候选");
    println!(
        "[变异①·原始] captured={} skip_reason={:?} content_hash={:?}",
        row.captured, row.skip_reason, row.content_hash
    );
    assert_eq!(
        row.skip_reason.as_deref(),
        Some("contains-credential-shape")
    );

    // 变异：token → 普通文本（`size` 跟着改，免得 inspect 与 read 说两套话）。
    let mut mutated = machine(SCENARIO, "machine.toml");
    mutate_content(&mut mutated, SETTINGS, |text| {
        assert!(text.contains(&token), "前提：变异前 token 在内容里");
        text.replace(&token, "not-a-credential")
    });
    let after = CaptureFixture::build(&mutated)
        .capture(&[Section::Configs], AT)
        .configs
        .expect("configs 这一节");
    let row = config_row(&after, SETTINGS).expect("候选");
    let expected_hash = sha256_of(&content_of(&mutated, SETTINGS).expect("变异后的 content"));
    println!(
        "[变异①·变异后] captured={} skip_reason={:?} content_hash={:?}",
        row.captured, row.skip_reason, row.content_hash
    );
    assert!(
        row.captured,
        "token 换成普通文本之后必须**捕获**它 —— 这一条就是'原用例会红'的证据"
    );
    assert!(row.skip_reason.is_none(), "没有凭据形状就不该有跳过理由");
    assert_eq!(
        row.content_hash.as_deref(),
        Some(expected_hash.as_str()),
        "变异后它是一份普通配置：哈希来自变异后的内容"
    );
}

/// 变异②：把前缀从"版本目录"里挪出来 → `prefix_inside_version_dir` 必须翻转。
///
/// 这里刻意做**两步**，因为决策 173 有四支判据，而"改目标"只动得了第 ④ 支：
///
/// 1. 只把联接目标换成普通目录（`…\nvm\current`）：① 支仍然为真（前缀**本身**还是
///    reparse point）⇒ 结论**仍然是 true**。这不是 bug，正是"四支任一为真"的含义；
///    一个只判 ④ 支的实现在这一步就会红。
/// 2. 再把前缀本身恢复成普通目录（去掉 reparse）：四支全假 ⇒ 结论必须是 **false**。
#[test]
fn mutation_moving_the_prefix_out_of_a_version_directory_flips_the_flag() {
    const SCENARIO: &str = "npm-inside-version-dir";
    let m = machine(SCENARIO, "machine.toml");
    let prefix = npm_prefix(&m);
    let target = entry_of(&m, &prefix)
        .and_then(|entry| entry.link_target)
        .expect("前缀是联接，有目标");

    let original = globals(SCENARIO, "machine.toml");
    let row = global_row(&original, "npm");
    println!(
        "[变异②·原始] prefix={:?} link_target={target} prefix_inside_version_dir={:?}",
        row.prefix, row.prefix_inside_version_dir
    );
    assert_eq!(row.prefix_inside_version_dir, Some(true));

    // 第一步：目标改成普通目录（① 支还在）。
    let mut step1 = machine(SCENARIO, "machine.toml");
    assert!(
        visit_entry(&mut step1, &prefix, |entry| {
            entry.link_target = Some(r"C:\Users\dev\AppData\Local\nvm\current".to_owned());
        }),
        "前缀那条要在固定装置里"
    );
    let after1 = CaptureFixture::build(&step1)
        .capture(&[Section::Globals], AT)
        .globals
        .expect("globals");
    let row1 = global_row(&after1, "npm");
    println!(
        "[变异②·只改目标] prefix_inside_version_dir={:?}（① 支仍为真，所以仍然是 true）",
        row1.prefix_inside_version_dir
    );
    assert_eq!(
        row1.prefix_inside_version_dir,
        Some(true),
        "前缀**本身**还是 reparse point ⇒ 决策 173 的 ① 支为真 ⇒ 仍然是 true"
    );

    // 第二步：连 reparse 一起去掉 —— 四支全假。
    let mut step2 = machine(SCENARIO, "machine.toml");
    assert!(
        visit_entry(&mut step2, &prefix, |entry| {
            entry.reparse = ReparseKind::None;
            entry.link_target = None;
            entry.is_dir = true;
        }),
        "前缀那条要在固定装置里"
    );
    let after2 = CaptureFixture::build(&step2)
        .capture(&[Section::Globals], AT)
        .globals
        .expect("globals");
    let row2 = global_row(&after2, "npm");
    println!(
        "[变异②·去掉 reparse] prefix={:?} prefix_inside_version_dir={:?}",
        row2.prefix, row2.prefix_inside_version_dir
    );
    assert_eq!(
        row2.prefix_inside_version_dir,
        Some(false),
        "前缀不再是 reparse point、路径里也没有版本段 ⇒ 必须翻成 false"
    );
    assert_eq!(
        row2.prefix.as_deref(),
        Some(prefix.as_str()),
        "前缀本身没变（变的是它的性质）"
    );
}
