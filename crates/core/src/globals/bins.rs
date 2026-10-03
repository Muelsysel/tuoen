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
//! # pip 的归属只做**精确名字匹配**
//!
//! `pip list` **不说**哪个脚本属于哪个包（那是 `pip show -f` 的活，一个包一次进程）。
//! 所以这里的规则写死为一句话：**包名与文件名（去掉 `.exe`）逐字相等**才算它的，
//! 大小写不敏感、`-` 与 `_` 折叠（`pypinyin` → `pypinyin.exe`；`pip` → `pip.exe`）；
//! 对不上就没有。**不按前缀猜、不把整个目录的名字贴到每一个包上**：
//! 那会让每个包都声称自己提供别人的命令 —— 一句看起来完全合理的错话。

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
    let manifest = prefix
        .join("node_modules")
        .join(package)
        .join("package.json");
    let Some(text) = read_text(ctx, &manifest) else {
        return Vec::new();
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
        return Vec::new();
    };
    match value.get("bin") {
        // 对象 → 键。顺序由 `serde_json::Map`（`BTreeMap`）给出，调用方再排一次。
        Some(serde_json::Value::Object(entries)) => entries.keys().cloned().collect(),
        // 字符串 → 包名（不带 scope）。npm 的约定：`bin` 是字符串时命令名 = 包名。
        Some(serde_json::Value::String(_)) => {
            vec![unscoped(package).to_owned()]
        }
        _ => Vec::new(),
    }
}

/// pip：`Scripts\*.exe` 里**名字与包名逐字对上**的那一个。
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
    ctx.fs
        .list_dir(&scripts)
        .into_iter()
        // 目录不是命令；`is_executable` 的判据这里只有一条：文件名以 `.exe` 结尾。
        .filter(|entry| !entry.is_dir && has_exe_extension(&entry.name))
        .filter(|entry| fold(strip_exe_extension(&entry.name)) == wanted)
        .map(|entry| entry.name)
        .collect()
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

fn strip_exe_extension(name: &str) -> &str {
    &name[..name.len() - ".exe".len()]
}

/// 读一个文本文件。**读不到 / 太大 / 不是 UTF-8 都是 `None`** ——
/// 这三件事对调用方的含义完全相同："拿不到"，于是不出那个键。
fn read_text(ctx: &DetectContext<'_>, path: &Path) -> Option<String> {
    match ctx.fs.read(path, MAX_PACKAGE_JSON_BYTES) {
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
}
