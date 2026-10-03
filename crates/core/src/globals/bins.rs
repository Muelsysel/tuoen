//! `bin` 名：一个全局包**提供了哪些命令**。
//!
//! # 为什么这一节要单独存在
//!
//! `npm ls -g` / `pip list` 只说"装了哪些包"，不说"敲哪个名字能跑"。而命令名是
//! 用户唯一直接感受到的东西（也是后面 `restore` 要生成 shim 的输入）。
//!
//! # 两个工具的答案来路完全不同（都实测过）
//!
//! * **npm**：`<prefix>\node_modules\<包>\package.json` 的 **`bin` 字段**。
//!   实测一个包可以给**多个**名字：`pnpm` 的 `bin` 是
//!   `{"pnpm":"bin/pnpm.mjs","pnpx":"bin/pnpx.mjs","pn":"bin/pnpm.mjs","pnx":"bin/pnpx.mjs"}`。
//!   **绝不解析 npm 自己生成的那个 `.cmd`**：它是 shell 脚本，不是契约
//!   （而且本仓库禁止发 `.cmd`，见决策 10 与 CVE-2024-27980）。
//!   实测全局树里**没有** `.bin`、也没有 `.package-lock.json`，所以"自己走目录"
//!   既数不准也拿不到这份映射 —— 只有 `package.json` 与 npm 自己的回答两个来源。
//! * **pip**：console script 已经是真 PE（实测 `<…>\Scripts\pip.exe` 是 108 KB 的启动器），
//!   所以答案是**那个目录里的文件名**。
//!
//! # 拿不到就不出这个键（`skip_serializing_if`），**不编**
//!
//! 读不到 `package.json`、JSON 坏了、`bin` 字段不在、Scripts 目录不存在 ——
//! 全都是"拿不到"，返回空表。**空表与"这个包没有 bin"在输出上是同一件事**，
//! 而它们是两件事 —— 所以调用方只在**非空**时才写 `binNames` 键：
//! 一个空的 `binNames: []` 会被读成"我查过了，它一个命令都没有"。
//!
//! # pip 的归属：**精确名字匹配** ∪ **`RECORD`**
//!
//! `pip list` **不说**哪个脚本属于哪个包（那是 `pip show -f` 的活，一个包一次进程）。
//! 所以这里的第一条规则写死为一句话：**包名与文件名（去掉 `.exe`）逐字相等**才算它的，
//! 大小写不敏感、`-` 与 `_` 折叠（`pypinyin` → `pypinyin.exe`；`pip` → `pip.exe`）；
//! 对不上就没有。**不按前缀猜、不把整个目录的名字贴到每一个包上**：
//! 那会让每个包都声称自己提供别人的命令 —— 一句看起来完全合理的错话。
//!
//! 第二条来源是 **pip 自己写的 `<名>-<版本>.dist-info\RECORD`**（PEP 376）：
//! 它是"这个包装了哪些文件"的**原始账本**，于是它认得那些名字与包名不同的启动器
//! （实测 `pypinyin` 的 RECORD 里就是 `../../Scripts/pypinyin.exe`，但别的包可能叫
//! 完全不同的名字）。用它的时候有两条纪律：
//!
//! * **路径相对 `site-packages` 解析**，`..` 要真的走一遍，然后判断**解析结果是不是
//!   恰好落在这个工具的 `Scripts` 目录里** —— **绝不硬编码 `../../`**：全机布局是
//!   `<根>\Lib\site-packages`（`../../Scripts`），用户级布局是
//!   `<root>\Python312\site-packages`（`../Scripts`），两种布局共用同一条规则。
//! * 两条来源**都拿不到**仍然是"拿不到"（空表、不出键），**不猜**。

use std::path::{Path, PathBuf};

use crate::detect::DetectContext;

use super::listing::GlobalsSource;
use super::root::GlobalsTool;

/// 读 `package.json` 的上限。
///
/// 与 `configs` 的上限同一个数（256 KiB）**不是巧合**：两者都是"一个应该很小、
/// 但没人在格式上保证它小"的文件。超过就当成拿不到 —— 宁可不出 `binNames`，
/// 也不要把一份大文件读进内存去猜。
const MAX_PACKAGE_JSON_BYTES: u64 = 256 * 1024;

/// 读 `RECORD` 的上限。
///
/// 比 `package.json` 那一条宽松得多，因为它是**账本**而不是配置：一个装了上千个文件
/// （数据文件、本地化表）的包，每一行都要记。4 MiB 足以覆盖实测见过的最坏情形
/// （本机 `pip-25.0.1.dist-info\RECORD` 854 行 ≈ 60 KB），超过就当成拿不到 ——
/// 与别处同一个口径：宁可不出名字，也不把一份大文件读进来猜。
const MAX_RECORD_BYTES: u64 = 4 * 1024 * 1024;

/// 一个包提供的命令名，**按名字排序**（确定性是幂等性的一部分）、去重。
///
/// `prefix` 是这个工具（这个来源）的全局前缀：npm 是 `<prefix>\node_modules\…`，
/// pip 是 `<…>\Scripts\…`。`tool_version` 只有 pip 用得上（`Python312\` 那一层）。
#[must_use]
pub(crate) fn bin_names(
    ctx: &DetectContext<'_>,
    tool: GlobalsTool,
    source: GlobalsSource,
    prefix: &Path,
    tool_version: &str,
    package: &str,
) -> Vec<String> {
    let mut names = match tool {
        GlobalsTool::Npm => npm_bin_names(ctx, prefix, package),
        GlobalsTool::Pip => pip_bin_names(ctx, source, prefix, tool_version, package),
    };
    names.sort();
    names.dedup();
    names
}

/// npm：读那个包的 `package.json`，取 `bin` 字段。
///
/// * `bin` 是**对象** → 它的**键**（这就是命令名）；
/// * `bin` 是**字符串** → 按 npm 自己的约定，命令名是**包名**
///   （`@scope/name` 取 `name` 那一段）；
/// * 别的形状 / 没有这个字段 / 读不到 / 不是 JSON → 空表。
fn npm_bin_names(ctx: &DetectContext<'_>, prefix: &Path, package: &str) -> Vec<String> {
    match npm_bin_field(ctx, prefix, package) {
        // 对象 → 键。顺序由 `serde_json::Map`（`BTreeMap`）给出，调用方再排一次。
        Some(serde_json::Value::Object(entries)) => entries.keys().cloned().collect(),
        // 字符串 → 包名（不带 scope）。npm 的约定：`bin` 是字符串时命令名 = 包名。
        Some(serde_json::Value::String(_)) => vec![unscoped(package).to_owned()],
        _ => Vec::new(),
    }
}

/// npm：**命令名 + `bin` 里那个相对路径**（已归一化）—— 这是 shim 那一票要的输入。
///
/// 与 `npm_bin_names` 共用同一次读取（`npm_bin_field`）：**名字与目标的来路必须是
/// 同一份 `package.json`**，否则"报了名字却没有目标"这种自相矛盾的形态就有藏身处。
///
/// 只收**字符串**值：`bin` 的值必须是路径，别的形状是坏清单（`npm_bin_names`
/// 仍然只报键 —— 那是已经冻结的字段语义，改它要递增 `schemaVersion`）。
#[must_use]
pub(crate) fn npm_bin_entries(
    ctx: &DetectContext<'_>,
    prefix: &Path,
    package: &str,
) -> Vec<(String, String)> {
    match npm_bin_field(ctx, prefix, package) {
        Some(serde_json::Value::Object(entries)) => entries
            .iter()
            .filter_map(|(name, value)| match value {
                serde_json::Value::String(raw) => {
                    normalize_bin_target(raw).map(|target| (name.clone(), target))
                }
                _ => None,
            })
            .collect(),
        Some(serde_json::Value::String(raw)) => normalize_bin_target(&raw)
            .map(|target| vec![(unscoped(package).to_owned(), target)])
            .unwrap_or_default(),
        _ => Vec::new(),
    }
}

/// 那个包的 `package.json` 里的 `bin` 字段（**唯一**的读取点）。
fn npm_bin_field(
    ctx: &DetectContext<'_>,
    prefix: &Path,
    package: &str,
) -> Option<serde_json::Value> {
    let manifest = prefix
        .join("node_modules")
        .join(package)
        .join("package.json");
    let text = read_text(ctx, &manifest)?;
    let value = serde_json::from_str::<serde_json::Value>(&text).ok()?;
    value.get("bin").cloned()
}

/// `bin` 的值 → 相对路径。**`./` 前缀要削掉**（实测 `corepack@0.35.0` 的五个值
/// 全是 `./dist/…`），分隔符统一成 `\`（这是 Windows，`Path::join` 之后再交给
/// 假文件系统比字符串时要一致）。
///
/// 拒绝的形状（返回 `None`）：空、绝对路径（`/` `\` `X:` 开头）、含 `..`
/// （那是"包外的东西"，我们不替它指路）、含 NUL（`ShimSpec::validate` 会拒，
/// 在这里先拒能报出更清楚的原因）。**这些条目不会静默消失** —— 调用方拿
/// `None` 与名字一起报出来。
fn normalize_bin_target(raw: &str) -> Option<String> {
    let mut text = raw.trim();
    while let Some(rest) = text.strip_prefix("./") {
        text = rest;
    }
    text = text.strip_prefix(".\\").unwrap_or(text);
    if text.is_empty() || text.contains('\0') {
        return None;
    }
    let absolute = text.starts_with('/')
        || text.starts_with('\\')
        || {
            // `X:` —— 盘符。`:`
            let mut chars = text.chars();
            matches!((chars.next(), chars.next()), (Some(letter), Some(':')) if letter.is_ascii_alphabetic())
        };
    if absolute {
        return None;
    }
    let normalized = text.replace('/', "\\");
    if normalized.split('\\').any(|part| part == "..") {
        return None;
    }
    Some(normalized)
}

/// pip：`Scripts\*.exe` 里**名字与包名逐字对上**的那一个 —— 再加上 pip 自己的
/// `RECORD` 账本里记的那些启动器。
///
/// 目录本身怎么来，两个来源不同：
///
/// * `source = "machine"` → `<pip --version 里的安装根>\Scripts`；
/// * `source = "tuoen"` → `<PYTHONUSERBASE>\Python<XY>\Scripts`
///   （`Python312` 那一层是 **pip 自己插的**，实测）。
fn pip_bin_names(
    ctx: &DetectContext<'_>,
    source: GlobalsSource,
    prefix: &Path,
    tool_version: &str,
    package: &str,
) -> Vec<String> {
    let Some(scripts) = pip_scripts_dir(source, prefix, tool_version) else {
        // python 版本答不上来时那一层目录的名字就是猜的 —— 不猜。
        return Vec::new();
    };

    let wanted = fold(package);
    let mut names: Vec<String> = ctx
        .fs
        .list_dir(&scripts)
        .into_iter()
        // 目录不是命令；`is_executable` 的判据这里只有一条：文件名以 `.exe` 结尾。
        .filter(|entry| !entry.is_dir && has_exe_extension(&entry.name))
        .filter(|entry| strip_exe_extension(&entry.name).is_some_and(|stem| fold(stem) == wanted))
        .map(|entry| entry.name)
        .collect();

    // 第二条来源：pip 自己写的账本。它认得名字与包名不同的启动器。
    if let Some(site) = pip_site_packages_dir(source, prefix, tool_version) {
        names.extend(record_bin_names(ctx, &site, &scripts, &wanted));
    }
    names
}

/// **pip 自己写的账本**：`<名>-<版本>.dist-info\RECORD` 里落在 `Scripts` 目录里的
/// 那些 `.exe`。
///
/// `RECORD` 的每一行是 `路径,sha256=…,大小`，路径**相对 `site-packages`**。
/// 三种"拿不到"都在这里变成空表（不变形、不猜）：没有对得上的 `dist-info`、
/// 读不到 `RECORD`、路径解析后不在 `Scripts` 里。
fn record_bin_names(
    ctx: &DetectContext<'_>,
    site: &Path,
    scripts: &Path,
    wanted: &str,
) -> Vec<String> {
    let Some(info) = dist_info_dir(ctx, site, wanted) else {
        return Vec::new();
    };
    let Some(text) = read_text_limit(ctx, &info.join("RECORD"), MAX_RECORD_BYTES) else {
        return Vec::new();
    };
    let mut names = Vec::new();
    for line in text.lines() {
        // 只要第一段（路径）；`RECORD` 自己那一行是 `<dist-info>/RECORD,,`。
        let Some(raw) = line.split(',').next() else {
            continue;
        };
        let raw = raw.trim();
        if raw.is_empty() {
            continue;
        }
        let resolved = resolve_relative(site, raw);
        // 判据是**解析之后**落在 Scripts 目录里 —— 不硬编码 `../../`。
        if !same_dir(resolved.parent(), scripts) {
            continue;
        }
        let Some(name) = resolved.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if !has_exe_extension(name) {
            continue;
        }
        // 账本说它在那儿，就再问一次盘：**"命令成功"与"东西装上了"是两件事**。
        let facts = ctx.fs.inspect(&resolved);
        if !facts.exists || facts.is_dir {
            continue;
        }
        names.push(name.to_owned());
    }
    names
}

/// `<site-packages>` 下与包名对得上的 `<名>-<版本>.dist-info` 目录。
///
/// 名字那一段是**从最后一个 `-` 往前**取的：`pypinyin-0.55.0.dist-info` → `pypinyin`。
/// 对不上就没有（`pip` 这个包自己的账本叫 `pip-25.0.1.dist-info`，它对得上）。
fn dist_info_dir(ctx: &DetectContext<'_>, site: &Path, wanted: &str) -> Option<PathBuf> {
    let suffix = ".dist-info";
    let mut hits: Vec<PathBuf> = ctx
        .fs
        .list_dir(site)
        .into_iter()
        .filter(|entry| entry.is_dir)
        .filter(|entry| entry.name.ends_with(suffix))
        .filter(|entry| {
            let stem = &entry.name[..entry.name.len() - suffix.len()];
            let name = stem.rsplit_once('-').map_or(stem, |(name, _)| name);
            fold(name) == wanted
        })
        .map(|entry| site.join(&entry.name))
        .collect();
    // 排序后取第一个：目录的枚举顺序不是契约（确定性是幂等性的一部分）。
    hits.sort();
    hits.into_iter().next()
}

/// 相对路径 → 绝对路径。**`..` 真的走一遍**（`Path::pop`），不是字符串拼接。
fn resolve_relative(base: &Path, raw: &str) -> PathBuf {
    let mut out = base.to_path_buf();
    for part in raw.split(['/', '\\']) {
        match part {
            "" | "." => {}
            ".." => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}

/// 两个目录是不是同一个（Windows：大小写不敏感）。
fn same_dir(left: Option<&Path>, right: &Path) -> bool {
    left.is_some_and(|left| {
        left.to_string_lossy()
            .eq_ignore_ascii_case(&right.to_string_lossy())
    })
}

/// `<PYTHONUSERBASE>\Python312` 里的 `Python312` —— 由 `pip --version` 报的
/// **版本号**（`3.12`）推出来。
///
/// 只认"至少两个点分段、每段都是数字"的形状（`3.12` / `3.13.1`）；
/// 别的形状返回 `None`，调用方就不出 `binNames`。`unknown` 自然落到 `None`。
fn python_layer(tool_version: &str) -> Option<String> {
    let mut parts = tool_version.split('.');
    let major = parts.next()?;
    let minor = parts.next()?;
    let numeric = |part: &str| !part.is_empty() && part.chars().all(|c| c.is_ascii_digit());
    if !numeric(major) || !numeric(minor) {
        return None;
    }
    Some(format!("Python{major}{minor}"))
}

/// `@scope/name` → `name`（npm 的约定：`bin` 是字符串时命令名是包名，不带 scope）。
fn unscoped(package: &str) -> &str {
    package.rsplit('/').next().unwrap_or(package)
}

/// 比较用的折叠：小写、`-` 与 `_` 视为同一个字符。
fn fold(text: &str) -> String {
    text.chars()
        .map(|c| match c {
            '-' | '_' => '_',
            other => other.to_ascii_lowercase(),
        })
        .collect()
}

fn has_exe_extension(name: &str) -> bool {
    name.to_ascii_lowercase().ends_with(".exe")
}

/// 去掉 `.exe` 后缀（大小写不敏感）；不以它结尾就是 `None`。
///
/// **只有一处实现**：pip 的 `Scripts` 里那批文件名与 shim 的命令名（`pip3.12`）
/// 用的是同一条判据，抄一份迟早会漂移。
pub(crate) fn strip_exe_extension(name: &str) -> Option<&str> {
    if has_exe_extension(name) {
        Some(&name[..name.len() - ".exe".len()])
    } else {
        None
    }
}

/// 读一个文本文件。**读不到 / 太大 / 不是 UTF-8 都是 `None`** ——
/// 这三件事对调用方的含义完全相同："拿不到"，于是不出那个键。
fn read_text(ctx: &DetectContext<'_>, path: &Path) -> Option<String> {
    read_text_limit(ctx, path, MAX_PACKAGE_JSON_BYTES)
}

/// 同上，上限由调用方给（`package.json` 与 `RECORD` 的大小量级不一样）。
fn read_text_limit(ctx: &DetectContext<'_>, path: &Path, limit: u64) -> Option<String> {
    match ctx.fs.read(path, limit) {
        tuoen_platform::ReadOutcome::Bytes(bytes) => String::from_utf8(bytes).ok(),
        _ => None,
    }
}

/// 一个路径是不是这个来源的 bin 目录（给人类输出与将来 shim 用）。
#[must_use]
pub(crate) fn pip_scripts_dir(
    source: GlobalsSource,
    prefix: &Path,
    tool_version: &str,
) -> Option<PathBuf> {
    match source {
        GlobalsSource::Machine => Some(prefix.join("Scripts")),
        GlobalsSource::Tuoen => {
            python_layer(tool_version).map(|layer| prefix.join(layer).join("Scripts"))
        }
    }
}

/// 一个路径是不是这个来源的 `site-packages`（`RECORD` 的所在地）。
///
/// 两个布局不同，**两个布局都由 `python_layer` 的同一套判断推导**：
///
/// * `machine` → `<安装根>\Lib\site-packages`（`Lib` 那一层是 CPython 的约定）；
/// * `tuoen` → `<PYTHONUSERBASE>\Python312\site-packages`。
///
/// 与 `pip_scripts_dir` 一样：python 版本答不上来就 `None`（不猜那一层目录的名字）。
#[must_use]
pub(crate) fn pip_site_packages_dir(
    source: GlobalsSource,
    prefix: &Path,
    tool_version: &str,
) -> Option<PathBuf> {
    match source {
        GlobalsSource::Machine => Some(prefix.join("Lib").join("site-packages")),
        GlobalsSource::Tuoen => {
            python_layer(tool_version).map(|layer| prefix.join(layer).join("site-packages"))
        }
    }
}

#[cfg(test)]
mod tests {
    use tuoen_platform::fixture::{FixtureDir, FixturePath, MachineFixture};

    use super::*;
    use crate::detect::test_support::DetectFixture;

    const NPM_PREFIX: &str = r"C:\tools\npm";
    const PIP_PREFIX: &str = r"C:\Python312";
    const USERBASE: &str = r"C:\Users\dev\AppData\Local\tuoen\globals\pip";
    /// 一个"多个 bin 名"的包 —— 形状照抄本机实测的 `pnpm`。
    const PNPM_MANIFEST: &str = r#"{"name":"pnpm","version":"11.21.0","bin":{"pnpm":"bin/pnpm.mjs","pnpx":"bin/pnpx.mjs","pn":"bin/pnpm.mjs","pnx":"bin/pnpx.mjs"}}"#;
    /// `bin` 是**字符串**的包 —— npm 的约定：命令名 = 包名。
    const STRING_BIN_MANIFEST: &str = r#"{"name":"tokentracker-cli","bin":"cli.js"}"#;
    /// 目标带 `./` 前缀的包 —— 形状照抄本机实测的 `corepack@0.35.0`（五个值全是 `./dist/…`）。
    const COREPACK_MANIFEST: &str = r#"{"name":"corepack","version":"0.35.0","bin":{"corepack":"./dist/corepack.js","pnpm":"./dist/pnpm.js","pnpx":"./dist/pnpx.js","yarn":"./dist/yarn.js","yarnpkg":"./dist/yarnpkg.js"}}"#;
    /// 目标**不是**包内相对路径的坏清单：`npm_bin_names` 只报键是已经冻结的语义，
    /// 但 shim 那一层绝不能把这些名字指向包外的某个东西。
    const BAD_TARGETS_MANIFEST: &str = r#"{"name":"badtargets","bin":{"absolute":"C:/evil/x.js","escape":"../outside.js","empty":"","good":"cli.js"}}"#;
    /// npm 自己生成的转发器：**内容故意与包清单不同**（它声称的命令叫 `ghost-cmd`）。
    ///
    /// 它存在的唯一目的就是被"**没人读它**"那条断言指着：如果哪天有人靠扫 `.bin`
    /// 来推导命令名，这里就会多出一个 `ghost-cmd`。
    const DECOY_CMD: &str =
        "@echo off\r\nrem ghost-cmd\r\nnode \"%~dp0\\..\\pnpm\\bin\\pnpm.mjs\" %*\r\n";

    fn machine() -> MachineFixture {
        MachineFixture {
            dirs: vec![
                FixtureDir::new(
                    r"C:\tools\npm\node_modules\pnpm",
                    vec![FixturePath::file_with_content(
                        "package.json",
                        PNPM_MANIFEST,
                    )],
                ),
                FixtureDir::new(
                    r"C:\tools\npm\node_modules\corepack",
                    vec![FixturePath::file_with_content(
                        "package.json",
                        COREPACK_MANIFEST,
                    )],
                ),
                FixtureDir::new(
                    r"C:\tools\npm\node_modules\badtargets",
                    vec![FixturePath::file_with_content(
                        "package.json",
                        BAD_TARGETS_MANIFEST,
                    )],
                ),
                // npm 自己生成的那一堆转发器（真机上 `.bin\pnpm.cmd` 是 538 字节）。
                // **它的内容与 `pnpm` 的清单不一样** —— 见 `DECOY_CMD`。
                FixtureDir::new(
                    r"C:\tools\npm\node_modules\.bin",
                    vec![
                        FixturePath::file_with_content("pnpm.cmd", DECOY_CMD),
                        FixturePath::file_with_content("pnpm.ps1", DECOY_CMD),
                        FixturePath::file_with_content("pnpm", DECOY_CMD),
                    ],
                ),
                FixtureDir::new(
                    r"C:\tools\npm\node_modules\@deepseek-ai\dsh",
                    vec![FixturePath::file_with_content(
                        "package.json",
                        STRING_BIN_MANIFEST,
                    )],
                ),
                // 只声明大小、**没有内容**：读它必须拿不到（假文件系统不肯替我们编）。
                FixtureDir::new(
                    r"C:\tools\npm\node_modules\broken",
                    vec![FixturePath::file("package.json", 40)],
                ),
                FixtureDir::new(
                    r"C:\tools\npm\node_modules\nobins",
                    vec![FixturePath::file_with_content(
                        "package.json",
                        r#"{"name":"nobins"}"#,
                    )],
                ),
                FixtureDir::new(
                    &format!(r"{USERBASE}\Python312\Scripts"),
                    vec![
                        FixturePath::file("pip.exe", 108_544),
                        FixturePath::file("pip3.exe", 108_544),
                        FixturePath::file("pypinyin.exe", 108_544),
                        FixturePath::file("not-an-exe.txt", 10),
                    ],
                ),
                FixtureDir::new(
                    &format!(r"{PIP_PREFIX}\Scripts"),
                    vec![
                        FixturePath::file("pip.exe", 108_544),
                        FixturePath::file("pypinyin.exe", 108_544),
                    ],
                ),
            ],
            ..MachineFixture::default()
        }
    }

    fn names(
        tool: GlobalsTool,
        source: GlobalsSource,
        prefix: &str,
        version: &str,
        package: &str,
    ) -> Vec<String> {
        let fixture = DetectFixture::build(&machine());
        bin_names(
            &fixture.context(),
            tool,
            source,
            Path::new(prefix),
            version,
            package,
        )
    }

    /// npm：`bin` 是对象 → **它的键**；一个包可以给多个名字（实测 `pnpm` 四个）。
    #[test]
    fn an_npm_package_can_provide_several_commands() {
        assert_eq!(
            names(
                GlobalsTool::Npm,
                GlobalsSource::Machine,
                NPM_PREFIX,
                "v24.19.0",
                "pnpm"
            ),
            ["pn", "pnpm", "pnpx", "pnx"],
            "名字来自 `bin` 字段的键，输出按名字排序"
        );
    }

    /// npm：`bin` 是**字符串** → 命令名是包名（`@scope/name` 取 `name` 段）。
    #[test]
    fn a_string_bin_means_the_command_is_named_after_the_package() {
        assert_eq!(
            names(
                GlobalsTool::Npm,
                GlobalsSource::Machine,
                NPM_PREFIX,
                "v24.19.0",
                "@deepseek-ai/dsh"
            ),
            ["dsh"],
            "scope 不进命令名：`@deepseek-ai/dsh` → `dsh`"
        );
    }

    /// 拿不到就是拿不到：读不出内容、没有 `bin` 字段、包不存在 —— 全是空表。
    #[test]
    fn an_unreadable_or_binless_manifest_yields_nothing_rather_than_a_guess() {
        for package in ["broken", "nobins", "does-not-exist"] {
            assert_eq!(
                names(
                    GlobalsTool::Npm,
                    GlobalsSource::Machine,
                    NPM_PREFIX,
                    "v24.19.0",
                    package
                ),
                Vec::<String>::new(),
                "{package} 不该被编出一个命令名"
            );
        }
    }

    /// pip：`<userbase>\Python312\Scripts\*.exe` 里**名字对得上**的那一个有 `-`/`_`
    /// 折叠与大小写不敏感；`.txt` 不是命令；对不上的包一个都不出。
    #[test]
    fn a_pip_command_is_matched_by_its_exact_file_name() {
        let hit = |package: &str| {
            names(
                GlobalsTool::Pip,
                GlobalsSource::Tuoen,
                USERBASE,
                "3.12",
                package,
            )
        };
        assert_eq!(hit("pypinyin"), ["pypinyin.exe"]);
        // 大小写不敏感：包名报成 `PyPinyin` 也命中同一个文件。
        assert_eq!(hit("PyPinyin"), ["pypinyin.exe"]);
        // `pip` 在真机上有三个启动器（`pip.exe` / `pip3.exe` / `pip3.12.exe`），
        // 但只有**逐字对上**的那一个是它的 —— `pip3.exe` 不归 `pip`。
        assert_eq!(hit("pip"), ["pip.exe"]);
        assert_eq!(
            hit("httpie"),
            Vec::<String>::new(),
            "Scripts 里没有它 → 不出键，而不是把这目录里的名字都贴给它"
        );
        // 包名里的 `-` 与文件名里的 `_` 是同一条命令（`python-dateutil` → `python_dateutil.exe`）。
        assert_eq!(
            names(
                GlobalsTool::Pip,
                GlobalsSource::Tuoen,
                USERBASE,
                "3.12",
                "python-dateutil"
            ),
            Vec::<String>::new(),
            "这条固定装置里没有那个文件 —— 一真一假的'假'"
        );
    }

    /// 机器自己那份 pip：Scripts 在 `<安装根>\Scripts`（不是 userbase 那一层）。
    #[test]
    fn the_machine_pip_scripts_live_under_its_install_root() {
        assert_eq!(
            names(
                GlobalsTool::Pip,
                GlobalsSource::Machine,
                PIP_PREFIX,
                "3.12",
                "pip"
            ),
            ["pip.exe"]
        );
        // 同一个包、同一份机器，换成 userbase 那一层 → 那层里没有它。
        assert_eq!(
            names(
                GlobalsTool::Pip,
                GlobalsSource::Machine,
                USERBASE,
                "3.12",
                "pip"
            ),
            Vec::<String>::new(),
            "{USERBASE} 下没有 Scripts\\pip.exe"
        );
    }

    /// python 版本那一段是**算出来的**：形状不认识就不出（不猜 `Python312`）。
    #[test]
    fn the_python_layer_comes_from_the_version_and_is_not_guessed() {
        assert_eq!(python_layer("3.12").as_deref(), Some("Python312"));
        assert_eq!(python_layer("3.13.1").as_deref(), Some("Python313"));
        assert_eq!(
            python_layer("312").as_deref(),
            None,
            "只有一个段 → 判不出来"
        );
        assert_eq!(python_layer("3.x").as_deref(), None);
        assert_eq!(python_layer("unknown").as_deref(), None);
        assert_eq!(python_layer("").as_deref(), None);

        // 版本判不出来时 tuoen 那一支**一个名字都不出**（连目录都不去列）。
        assert_eq!(
            names(
                GlobalsTool::Pip,
                GlobalsSource::Tuoen,
                USERBASE,
                "unknown",
                "pypinyin"
            ),
            Vec::<String>::new()
        );
    }

    /// 排序与去重：`serde_json::Map` 是 `BTreeMap`，但调用点不该依赖这件事。
    #[test]
    fn the_names_are_sorted_and_deduplicated() {
        let mut machine = machine();
        machine.dirs.push(FixtureDir::new(
            r"C:\tools\npm\node_modules\two",
            vec![FixturePath::file_with_content(
                "package.json",
                r#"{"name":"two","bin":{"zeta":"z.js","alpha":"a.js"}}"#,
            )],
        ));
        let fixture = DetectFixture::build(&machine);
        assert_eq!(
            bin_names(
                &fixture.context(),
                GlobalsTool::Npm,
                GlobalsSource::Machine,
                Path::new(NPM_PREFIX),
                "v24.19.0",
                "two"
            ),
            ["alpha", "zeta"]
        );
    }

    /// 一次查询返回的「名字 + 它指向的相对路径」—— shim 那一层的输入。
    fn entries(prefix: &str, package: &str) -> Vec<(String, String)> {
        let fixture = DetectFixture::build(&machine());
        npm_bin_entries(&fixture.context(), Path::new(prefix), package)
    }

    /// npm：名字与**目标**成对给出，`./` 前缀被削掉（真机上 `corepack` 的五个值
    /// 全是 `./dist/…`），分隔符统一成 `\`。
    #[test]
    fn npm_bin_entries_pair_each_name_with_a_normalized_target() {
        assert_eq!(
            entries(NPM_PREFIX, "corepack"),
            [
                ("corepack".to_owned(), r"dist\corepack.js".to_owned()),
                ("pnpm".to_owned(), r"dist\pnpm.js".to_owned()),
                ("pnpx".to_owned(), r"dist\pnpx.js".to_owned()),
                ("yarn".to_owned(), r"dist\yarn.js".to_owned()),
                ("yarnpkg".to_owned(), r"dist\yarnpkg.js".to_owned()),
            ],
            "`./` 是 npm 的写法，进路径之前必须削掉"
        );
        assert_eq!(
            entries(NPM_PREFIX, "pnpm"),
            [
                ("pn".to_owned(), r"bin\pnpm.mjs".to_owned()),
                ("pnpm".to_owned(), r"bin\pnpm.mjs".to_owned()),
                ("pnpx".to_owned(), r"bin\pnpx.mjs".to_owned()),
                ("pnx".to_owned(), r"bin\pnpx.mjs".to_owned()),
            ],
            "四个名字、两个目标：`pn` 是 `pnpm` 的别名、`pnx` 是 `pnpx` 的别名（真机逐字如此）"
        );
        assert_eq!(
            entries(NPM_PREFIX, "@deepseek-ai/dsh"),
            [("dsh".to_owned(), "cli.js".to_owned())],
            "`bin` 是字符串 → 名字是包名（不带 scope），目标是那个字符串"
        );
    }

    /// 指向包外的目标**一个都不发**（绝对路径、`..`、空），而 `binNames` 仍然报键 ——
    /// 这两个口径的差别是**故意**的：`binNames` 已经冻结，改它要递增 `schemaVersion`；
    /// 而"这个名字没有可发的目标"必须由 shim 那一层**明说**，不许静默消失。
    #[test]
    fn a_bin_target_that_points_outside_the_package_is_not_handed_on() {
        assert_eq!(
            entries(NPM_PREFIX, "badtargets"),
            [("good".to_owned(), "cli.js".to_owned())]
        );
        assert_eq!(
            names(
                GlobalsTool::Npm,
                GlobalsSource::Machine,
                NPM_PREFIX,
                "v24.19.0",
                "badtargets"
            ),
            ["absolute", "empty", "escape", "good"],
            "`binNames` 是键的投影 —— 它的语义没有变"
        );
    }

    /// **绝不许读 `node_modules\.bin` 里的 `.cmd` / `.ps1`**（npm 生成的转发器，
    /// 不是契约；本仓库绝不发那种东西）。
    ///
    /// 这条断言是**直接证据**：固定装置里那三个文件的内容都声称自己叫 `ghost-cmd`，
    /// 而账本（`FakeFileSystem::reads`）证明从来没有人打开过它们。
    /// 没有账本的话，这条断言只能退化成"产出的名字里没有 `ghost-cmd`"——
    /// 一个**间接口径**（而"间接口径"正是决策 186 要炸掉的形态）。
    #[test]
    fn the_npm_generated_cmd_forwarders_are_never_read() {
        let fixture = DetectFixture::build(&machine());
        let got = npm_bin_entries(&fixture.context(), Path::new(NPM_PREFIX), "pnpm");
        assert_eq!(
            got.iter()
                .map(|(name, _)| name.as_str())
                .collect::<Vec<_>>(),
            ["pn", "pnpm", "pnpx", "pnx"],
            "命令名只能来自包自己的 `package.json`"
        );

        let reads = fixture.machine.fs.reads();
        assert!(
            reads
                .iter()
                .any(|path| path.ends_with(r"pnpm\package.json")),
            "账本必须先证明**它真的读了该读的东西**，否则下一条断言是空真：{reads:?}"
        );
        let touched: Vec<&String> = reads
            .iter()
            .filter(|path| path.contains(r"node_modules\.bin"))
            .collect();
        assert!(
            touched.is_empty(),
            "`node_modules\\.bin` 下的转发器一个都不许读，实际读了：{touched:?}"
        );
    }

    /// 用户级布局的 `RECORD`：它认得**名字与包名不同**的启动器
    /// （`pypinyin-console.exe`），也认得用反斜杠拼的路径。
    #[test]
    fn the_record_ledger_names_scripts_the_package_name_does_not() {
        let fixture = DetectFixture::build(&record_machine());
        let ctx = fixture.context();
        assert_eq!(
            bin_names(
                &ctx,
                GlobalsTool::Pip,
                GlobalsSource::Tuoen,
                Path::new(RECORD_USERBASE),
                "3.12",
                "pypinyin"
            ),
            // 三个名字里只有 `pypinyin.exe` 是"名字与包名对上"那一条来的。
            ["backslash.exe", "pypinyin-console.exe", "pypinyin.exe"],
            "`RECORD` 是 pip 自己的账本 —— 它认得与包名不同的启动器"
        );
    }

    /// **路径相对 `site-packages` 解析**，然后判"解析结果是不是落在这个工具的
    /// `Scripts` 里" —— 两种布局共用这一条规则，**绝不硬编码 `../../`**。
    ///
    /// 全机布局是 `<根>\Lib\site-packages`（`../../Scripts`），
    /// 用户级布局是 `<root>\Python312\site-packages`（`../Scripts`）。
    #[test]
    fn a_record_path_is_resolved_relative_to_site_packages() {
        let fixture = DetectFixture::build(&record_machine());
        let ctx = fixture.context();
        assert_eq!(
            bin_names(
                &ctx,
                GlobalsTool::Pip,
                GlobalsSource::Machine,
                Path::new(RECORD_MACHINE_ROOT),
                "3.12",
                "pip"
            ),
            ["pip.exe", "pip3.12.exe", "pip3.exe"],
            // `ghost.exe` 在账本里、盘上没有；`sibling.exe` 解析到了隔壁的
            // `Lib\\Scripts`（**不是**这个工具的 Scripts）；`otherthing` 的账本不算它的。
            "账本里落在 Scripts 目录、且盘上真的有的那些才算"
        );
    }

    /// 账本**说在、盘上没有**的那些名字不算；隔壁目录里的同名文件也不算。
    #[test]
    fn a_record_entry_whose_file_is_gone_is_not_a_command() {
        let fixture = DetectFixture::build(&record_machine());
        let ctx = fixture.context();
        let got = bin_names(
            &ctx,
            GlobalsTool::Pip,
            GlobalsSource::Machine,
            Path::new(RECORD_MACHINE_ROOT),
            "3.12",
            "pip",
        );
        for absent in ["ghost.exe", "sibling.exe", "escaped.exe"] {
            assert!(
                !got.iter().any(|name| name == absent),
                "{absent} 不该被报出来：{got:?}"
            );
        }
    }

    /// 拿不到就是拿不到：`dist-info` 里没有 `RECORD`、`RECORD` 读不出来 ——
    /// 都只剩"名字与包名逐字对上"那一条来源，**不猜**。
    #[test]
    fn a_missing_or_unreadable_record_adds_nothing() {
        let fixture = DetectFixture::build(&record_machine());
        let ctx = fixture.context();
        assert_eq!(
            bin_names(
                &ctx,
                GlobalsTool::Pip,
                GlobalsSource::Tuoen,
                Path::new(RECORD_USERBASE),
                "3.12",
                "norecord"
            ),
            Vec::<String>::new(),
            "`dist-info` 在、`RECORD` 不在 → 拿不到"
        );
        assert_eq!(
            bin_names(
                &ctx,
                GlobalsTool::Pip,
                GlobalsSource::Tuoen,
                Path::new(RECORD_USERBASE),
                "3.12",
                "shortwrite"
            ),
            ["shortwrite.exe"],
            "`RECORD` 存在但读不出内容（固定装置只声明了大小）→ 只剩逐字对上那一条"
        );
        assert_eq!(
            bin_names(
                &ctx,
                GlobalsTool::Pip,
                GlobalsSource::Tuoen,
                Path::new(RECORD_USERBASE),
                "3.12",
                "otherthing"
            ),
            Vec::<String>::new(),
            "别的包的账本不许贴到它头上"
        );
    }

    /// 用户级（`RECORD` 走 `../Scripts`）与全机（`../../Scripts`）布局的固定装置。
    const RECORD_USERBASE: &str = r"C:\ub";
    const RECORD_MACHINE_ROOT: &str = r"C:\Py312";
    /// 用户级布局的账本：`..\\Scripts\\x.exe`（从 `Python312\site-packages` 往上**一层**）。
    ///
    /// 里面三种"不算"的形态**各有各的理由**，逐条写在下面的断言里：
    /// `../../Scripts/escaped.exe` 爬出了 Python312（隔壁的 `C:\ub\Scripts` 里真有那个文件 ——
    /// 一个"判据太宽就会命中"的反例）；`absent.exe` 账本说在、盘上没有；
    /// `../Scripts/backslash.exe` 用的是反斜杠（pip 写的是正斜杠，但这不是契约）。
    const USER_RECORD: &str = "pypinyin/__init__.py,sha256=AAAA,11\n\
../Scripts/pypinyin.exe,sha256=BBBB,108420\n\
../Scripts/pypinyin-console.exe,sha256=CCCC,108420\n\
../Scripts/absent.exe,sha256=DDDD,1\n\
../../Scripts/escaped.exe,sha256=EEEE,1\n\
..\\Scripts\\backslash.exe,sha256=FFFF,108420\n\
,.sha256=,1\n\
pypinyin-0.55.0.dist-info/RECORD,,\n";
    /// 全机布局的账本：`..\\..\\Scripts\\x.exe`（从 `Lib\site-packages` 往上**两层**）。
    /// `../Scripts/sibling.exe` 落进隔壁的 `Lib\Scripts`（真机上那里确实有文件）。
    const MACHINE_RECORD: &str = "../../Scripts/pip.exe,sha256=AAAA,108425\n\
../../Scripts/pip3.exe,sha256=BBBB,108425\n\
../../Scripts/pip3.12.exe,sha256=CCCC,108425\n\
../../Scripts/ghost.exe,sha256=DDDD,1\n\
../Scripts/sibling.exe,sha256=EEEE,1\n";

    fn record_machine() -> MachineFixture {
        MachineFixture {
            dirs: vec![
                FixtureDir::new(
                    &format!(r"{RECORD_USERBASE}\Python312\Scripts"),
                    vec![
                        FixturePath::file("pypinyin.exe", 108_420),
                        FixturePath::file("pypinyin-console.exe", 108_420),
                        FixturePath::file("backslash.exe", 108_420),
                        FixturePath::file("shortwrite.exe", 108_420),
                    ],
                ),
                // 隔壁的 `C:\ub\Scripts`：`../../` 那种拼法会命中它 —— 它**不是**这个工具的目录。
                FixtureDir::new(
                    &format!(r"{RECORD_USERBASE}\Scripts"),
                    vec![FixturePath::file("escaped.exe", 108_420)],
                ),
                FixtureDir::new(
                    &format!(r"{RECORD_USERBASE}\Python312\site-packages"),
                    // 父目录必须**显式声明**：`list_dir` 只答声明过的目录，
                    // 而"哪个 `dist-info` 是这个包的"正是靠列这个目录答的。
                    vec![
                        FixturePath::dir("pypinyin-0.55.0.dist-info"),
                        FixturePath::dir("norecord-1.0.dist-info"),
                        FixturePath::dir("shortwrite-1.0.dist-info"),
                        FixturePath::dir("otherthing-1.0.dist-info"),
                    ],
                ),
                FixtureDir::new(
                    &format!(
                        r"{RECORD_USERBASE}\Python312\site-packages\pypinyin-0.55.0.dist-info"
                    ),
                    vec![FixturePath::file_with_content("RECORD", USER_RECORD)],
                ),
                // `dist-info` 在、`RECORD` 不在。
                FixtureDir::new(
                    &format!(r"{RECORD_USERBASE}\Python312\site-packages\norecord-1.0.dist-info"),
                    vec![FixturePath::file("METADATA", 20)],
                ),
                // `RECORD` 在、内容拿不到（固定装置只声明了大小）。
                FixtureDir::new(
                    &format!(r"{RECORD_USERBASE}\Python312\site-packages\shortwrite-1.0.dist-info"),
                    vec![FixturePath::file("RECORD", 120)],
                ),
                // 另一个包的账本，它指向的文件**真的属于别人**。
                FixtureDir::new(
                    &format!(r"{RECORD_USERBASE}\Python312\site-packages\otherthing-1.0.dist-info"),
                    vec![FixturePath::file_with_content(
                        "RECORD",
                        "../Scripts/otherthing.exe,sha256=AAAA,1\n",
                    )],
                ),
                FixtureDir::new(
                    &format!(r"{RECORD_MACHINE_ROOT}\Scripts"),
                    vec![
                        FixturePath::file("pip.exe", 108_425),
                        FixturePath::file("pip3.exe", 108_425),
                        FixturePath::file("pip3.12.exe", 108_425),
                    ],
                ),
                FixtureDir::new(
                    &format!(r"{RECORD_MACHINE_ROOT}\Lib\Scripts"),
                    vec![FixturePath::file("sibling.exe", 108_425)],
                ),
                FixtureDir::new(
                    &format!(r"{RECORD_MACHINE_ROOT}\Lib\site-packages"),
                    vec![FixturePath::dir("pip-25.0.1.dist-info")],
                ),
                FixtureDir::new(
                    &format!(r"{RECORD_MACHINE_ROOT}\Lib\site-packages\pip-25.0.1.dist-info"),
                    vec![FixturePath::file_with_content("RECORD", MACHINE_RECORD)],
                ),
            ],
            ..MachineFixture::default()
        }
    }
}
