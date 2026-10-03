//! `tuoen globals list` 的数据面：**两个来源的根** + **两个来源的包**。
//!
//! # 为什么这个模块存在（而不是让 CLI 自己拼）
//!
//! 读机器的代码只能有一处：CLI 层不碰 `std::fs`、不碰注册表，一切经过
//! [`DetectContext`] 的注入式适配器。`globals list` 因此与 `capture` 共用
//! **同一段枚举**（`crate::capture::collect::globals::collect_rows`），
//! 只是在枚举之上多做一件事：把每个包**提供了哪些命令**（`binNames`）查出来。
//! 两份枚举迟早会漂移，而漂移的表现是"`capture` 说 7 个、`globals list` 说 6 个"。
//!
//! # 两个来源（L2 spec §2）
//!
//! * [`GlobalsSource::Machine`] —— 工具自己的全局位置（决策 167/168/169）；
//! * [`GlobalsSource::Tuoen`] —— `%LOCALAPPDATA%\tuoen\globals\` 那个由我们按
//!   运行时版本隔离出来的根（决策 26）。
//!
//! 只报一个来源就会说谎，而且是**迁移链条上最容易发生的一次**：`restore --apply`
//! 之后，新机器的 `npm ls -g` 不会列出 tuoen 装的那些包（它们在 tuoen 的 prefix 里）。
//! 那时"刚还原完再捕获一次"会得到一份少了所有 tuoen 管理的包的清单 ——
//! 一句看起来完全合理的假话。
//!
//! # `roots` 里连**空的根**都要出
//!
//! 只报"有包的根"会让"tuoen 一个包都没管"与"我们没看"长得一模一样。
//! `roots` 回答的是"根在**哪里**"，而 `exists` 回答"那个目录今天在不在" ——
//! 两者都不是包数能推出来的（这就是 [`GlobalsRootRow::exists`] 存在的理由；
//! 冻结的 `--json` 形状里没有它，那是**给人看**的那一句话的下脚料）。

use std::path::PathBuf;

use crate::detect::DetectContext;

use super::bins::bin_names;
use super::root::{GlobalsRoot, GlobalsRootError, GlobalsTool};
use crate::capture::collect::globals::{CollectedRow, collect_rows};

/// 一个全局包**从哪来**（`globals.toml` 的 `source`，L2 spec §2）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum GlobalsSource {
    /// 工具自己的全局位置 —— 今天的行为（决策 167/168/169）。
    Machine,
    /// tuoen 管理的根（决策 26 的按运行时版本隔离）。
    Tuoen,
}

impl GlobalsSource {
    /// 全部来源，**顺序即行顺序**（机器自己的在前：它是用户当下真正在用的那一份）。
    pub const ALL: [Self; 2] = [Self::Machine, Self::Tuoen];

    /// 稳定 slug（`globals.toml` 的 `source`、`--json` 的 `source`，**不本地化**）。
    #[must_use]
    pub const fn slug(self) -> &'static str {
        match self {
            Self::Machine => "machine",
            Self::Tuoen => "tuoen",
        }
    }

    /// 从 slug 解析。**不认识就是 `None`**。
    #[must_use]
    pub fn from_slug(slug: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|source| source.slug() == slug)
    }
}

impl std::fmt::Display for GlobalsSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.slug())
    }
}

/// 一个**我们管着的根**。空根也要有一条。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GlobalsRootRow {
    /// 哪个工具。
    pub tool: GlobalsTool,
    /// 根在哪（`<base>\npm\<node -v 原样>` 或 `<base>\pip`）。
    pub root: PathBuf,
    /// 这个根属于谁。**永远是 [`GlobalsSource::Tuoen`]**：机器自己的全局位置不是
    /// "我们的根"，它出现在 `packages` 里（带 `source = "machine"`）——
    /// 把两者混进同一个数组会让"我们能动哪些目录"这个问题失去答案。
    pub source: GlobalsSource,
    /// 那个目录今天在不在。**不在不是错误**：一台还没用过 tuoen 的机器就是这样。
    pub exists: bool,
}

/// 一个全局包（一行）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GlobalsPackageRow {
    /// 哪个工具。
    pub tool: GlobalsTool,
    /// 这个包的清单属于哪个**运行时**版本（`v24.19.0` / `3.12` / `unknown`）。
    ///
    /// **它不进 `--json`**（那个形状是冻结的两键 + 每包五个键）。它留在这里是因为
    /// 人的表里要印它 —— 而"这个包属于哪个运行时"正是这份清单存在的理由（决策 172）。
    pub tool_version: String,
    /// 包名（含 scope，如 `@deepseek-ai/dsh`）。
    pub name: String,
    /// 版本。工具没给版本时是 `"unknown"`（键永远在）。
    pub version: String,
    /// 这个包**从哪来**。
    pub source: GlobalsSource,
    /// 它提供了哪些命令。**拿不到就是空表** —— 调用方只在该表非空时才写
    /// `binNames` 键（`[]` 会被读成"我查过了，它一个命令都没有"）。
    pub bin_names: Vec<String>,
}

/// "为什么这里什么都没有"——**给人看**的一句话的数据面。
///
/// 它不进 `--json`（那个形状是冻结的：`roots` + `packages`）。留成结构化的 slug
/// 而不是直接拼中文，是因为中文那一句属于人类输出层（决策 35：我们写的字符串里
/// 不出现中文进 JSON，反过来中文句子也不该散落在数据层）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GlobalsNote {
    /// 关于哪个工具。
    pub tool: GlobalsTool,
    /// 哪一个原因。
    pub code: GlobalsNoteCode,
}

/// 原因本身（稳定 slug）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GlobalsNoteCode {
    /// 这个工具的可执行文件不在 `PATH` 上 → 它**一行都没有**（不是"它有零个包"）。
    ToolNotOnPath,
    /// 运行时版本判不出来 → npm 的按版本隔离的那个根**算不出来**（决策 26/172）。
    RuntimeVersionUnknown,
}

impl GlobalsNoteCode {
    /// 稳定 slug（**不进 JSON**，但人类输出按它选取句子）。
    #[must_use]
    pub const fn slug(self) -> &'static str {
        match self {
            Self::ToolNotOnPath => "tool-not-on-path",
            Self::RuntimeVersionUnknown => "runtime-version-unknown",
        }
    }
}

/// `tuoen globals list` 的全部数据。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GlobalsListing {
    /// 我们管着的根，**按工具表的顺序**（npm 在前）。**空根也在**。
    pub roots: Vec<GlobalsRootRow>,
    /// 两个来源的包，**按（工具，来源，包名）排序**（确定性是幂等性的一部分）。
    pub packages: Vec<GlobalsPackageRow>,
    /// 那些"什么都没出"的原因（给人看）。
    pub notes: Vec<GlobalsNote>,
}

impl GlobalsListing {
    /// 某个（工具，来源）下有几个包 —— 人类输出用它说"这个根里有几个"。
    #[must_use]
    pub fn package_count(&self, tool: GlobalsTool, source: GlobalsSource) -> usize {
        self.packages
            .iter()
            .filter(|row| row.tool == tool && row.source == source)
            .count()
    }

    /// 根那一行（`roots` 里按工具找）。**找不到就是 `None`** ——
    /// 那意味着这个工具的根这次算不出来（见 [`GlobalsNoteCode`]）。
    #[must_use]
    pub fn root_of(&self, tool: GlobalsTool) -> Option<&GlobalsRootRow> {
        self.roots.iter().find(|row| row.tool == tool)
    }
}

/// 从**注入上下文**里算根。
///
/// `LOCALAPPDATA` 走 `ctx.process_var`（进程环境）而**不是** `env_var`（持久环境）：
/// 决策 187 的那条理由 —— 注册表里根本没有 `LOCALAPPDATA`，去那儿读会拿到 `None`，
/// 于是整件事静默退化成"没有根"。
///
/// # Errors
///
/// 进程环境里没有 `LOCALAPPDATA`（或它不是一个绝对路径）。
pub(crate) fn root_from_context(ctx: &DetectContext<'_>) -> Result<GlobalsRoot, GlobalsRootError> {
    let value = ctx.process_var("LOCALAPPDATA");
    GlobalsRoot::from_local_app_data(value.as_deref().map(std::ffi::OsStr::new))
}

/// 两个来源的根与包。
///
/// # Errors
///
/// 算不出根（见 [`root_from_context`]）。**这一条是错误而不是"空的清单"**：
/// `tuoen globals list` 的一半答案（`roots`）在那种情况下不存在，
/// 而印一张只有机器那半边的表会被读成"tuoen 什么都没管"。
pub fn list_globals(ctx: &DetectContext<'_>) -> Result<GlobalsListing, GlobalsRootError> {
    let root = root_from_context(ctx)?;
    let collected = collect_rows(ctx, Some(&root));

    let mut packages = Vec::new();
    for row in &collected.rows {
        for package in &row.packages {
            packages.push(GlobalsPackageRow {
                tool: row.tool,
                tool_version: row.tool_version.clone(),
                name: package.name.clone(),
                version: package.version.clone(),
                source: row.source,
                // 逐包解析：`bin_names` 只认**这个包自己**的命令名 ——
                // 一个包的答案不受同工具别的包影响（pip 那一层尤其如此，
                // 那里的判据是"包名与 Scripts 里的文件名逐字对上"）。
                bin_names: bin_names_of(ctx, row, &package.name),
            });
        }
    }

    packages.sort_by(|a, b| (a.tool, a.source, &a.name).cmp(&(b.tool, b.source, &b.name)));

    Ok(GlobalsListing {
        roots: collected.roots.clone(),
        packages,
        notes: collected.notes.clone(),
    })
}

/// 一个包提供了哪些命令。
///
/// 前缀拿不到就是**空表**：判据是"前缀 + 包名 → 命令名"，两个输入缺一不可
/// （机器自己的 pip 在 `pip --version` 答不上来时就没有前缀）。
/// 空表**不等于**"它一个命令都没有" —— 调用方只在该表非空时才写 `binNames` 键，
/// 于是"拿不到"与"查过了、没有"在 JSON 里是两件不同的事（一个没键、一个是 `[]`）。
fn bin_names_of(ctx: &DetectContext<'_>, row: &CollectedRow, package: &str) -> Vec<String> {
    match &row.prefix {
        Some(prefix) => bin_names(
            ctx,
            row.tool,
            row.source,
            std::path::Path::new(prefix),
            &row.tool_version,
            package,
        ),
        None => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use tuoen_platform::fixture::{FixtureDir, FixturePath, FixtureProcess, MachineFixture};

    use super::*;
    use crate::detect::test_support::DetectFixture;

    const LOCAL: &str = r"C:\Users\dev\AppData\Local";
    const BASE: &str = r"C:\Users\dev\AppData\Local\tuoen\globals";
    const NPM_ROOT: &str = r"C:\Users\dev\AppData\Local\tuoen\globals\npm\v24.19.0";
    const PIP_ROOT: &str = r"C:\Users\dev\AppData\Local\tuoen\globals\pip";

    /// 一份固定装置的输出（`FixtureProcess`，决策 185 的 args 匹配）。
    fn probe(program: &str, args: &[&str], stdout: &str) -> FixtureProcess {
        FixtureProcess {
            program: program.to_owned(),
            args: args.iter().map(|arg| (*arg).to_owned()).collect(),
            stdout: stdout.to_owned(),
            stderr: String::new(),
            exit_code: Some(0),
            timed_out: false,
        }
    }

    /// 一台"两个工具都在、机器自己有 7 个包、tuoen 的根里有 3 个"的机器。
    ///
    /// 包数是照 #17 真机数字写的（npm 7 / pip 3），这样"只问机器会漏掉几个"
    /// 这条断言连着真实场景。
    fn machine() -> MachineFixture {
        MachineFixture {
            env: BTreeMap::from([
                (
                    "Path".to_owned(),
                    r"C:\tools;C:\Python312\Scripts".to_owned(),
                ),
                ("LOCALAPPDATA".to_owned(), LOCAL.to_owned()),
            ]),
            dirs: vec![
                FixtureDir::new(
                    r"C:\tools",
                    vec![
                        FixturePath::file("npm.cmd", 340),
                        FixturePath::file("node.exe", 84_043_264),
                    ],
                ),
                FixtureDir::new(
                    r"C:\Python312\Scripts",
                    vec![
                        FixturePath::file("pip.exe", 108_544),
                        FixturePath::file("pypinyin.exe", 108_544),
                    ],
                ),
                // 我们自己的两个根。
                FixtureDir::new(NPM_ROOT, vec![FixturePath::dir("node_modules")]),
                FixtureDir::new(PIP_ROOT, vec![FixturePath::dir("Python312")]),
                // npm 包的 `package.json`（bin 名唯一可靠的来源）。
                FixtureDir::new(
                    r"C:\Users\dev\AppData\Local\tuoen\globals\npm\v24.19.0\node_modules\pnpm",
                    vec![FixturePath::file_with_content(
                        "package.json",
                        r#"{"name":"pnpm","bin":{"pnpm":"bin/pnpm.mjs","pnpx":"bin/pnpx.mjs","pn":"bin/pnpm.mjs","pnx":"bin/pnpx.mjs"}}"#,
                    )],
                ),
                FixtureDir::new(
                    r"C:\Users\dev\AppData\Local\tuoen\globals\npm\v24.19.0\node_modules\corepack",
                    vec![FixturePath::file_with_content(
                        "package.json",
                        r#"{"name":"corepack"}"#,
                    )],
                ),
                FixtureDir::new(
                    r"C:\Users\dev\AppData\Local\tuoen\globals\pip\Python312\Scripts",
                    vec![
                        FixturePath::file("pip.exe", 108_544),
                        FixturePath::file("pypinyin.exe", 108_544),
                    ],
                ),
            ],
            processes: vec![
                probe("node.exe", &["-v"], "v24.19.0\n"),
                probe(
                    "cmd.exe",
                    &["/C", "npm.cmd", "config", "get", "prefix"],
                    "C:\\tools\\npm\n",
                ),
                // 机器自己的 7 个包（真机 #17 的数字）。
                probe(
                    "cmd.exe",
                    &[
                        "/C",
                        "npm.cmd",
                        "ls",
                        "-g",
                        "--json",
                        "--depth=0",
                        "--offline",
                    ],
                    r#"{"dependencies":{"@deepseek-ai/dsh":{"version":"0.1.0-rc.6"},"@openai/codex":{"version":"0.160.0"},"billion-context":{"version":"0.1.179"},"corepack":{"version":"0.35.0"},"npm":{"version":"11.17.0"},"pnpm":{"version":"11.21.0"},"tokentracker-cli":{"version":"0.87.3"}}}"#,
                ),
                // tuoen 的根里的 3 个包。
                probe(
                    "cmd.exe",
                    &[
                        "/C",
                        "npm.cmd",
                        "ls",
                        "-g",
                        "--json",
                        "--depth=0",
                        "--offline",
                        "--prefix",
                        NPM_ROOT,
                    ],
                    r#"{"dependencies":{"@deepseek-ai/dsh":{"version":"0.1.0-rc.6"},"pnpm":{"version":"11.21.0"},"corepack":{"version":"0.35.0"}}}"#,
                ),
                probe(
                    "pip.exe",
                    &["--version"],
                    "pip 25.0.1 from C:\\Python312\\Lib\\site-packages\\pip (python 3.12)\n",
                ),
                probe(
                    "pip.exe",
                    &["list", "--format=json", "--disable-pip-version-check"],
                    r#"[{"name":"pip","version":"25.0.1"},{"name":"pypdf","version":"6.19.0"},{"name":"pypinyin","version":"0.55.0"}]"#,
                ),
                probe(
                    "pip.exe",
                    &[
                        "list",
                        "--user",
                        "--format=json",
                        "--disable-pip-version-check",
                    ],
                    // tuoen 的 pip 根**存在但没有包**：这一份固定装置要的就是这个形状 ——
                    // "根在哪里、它是空的"必须说得出来（`roots` 里照样有一行）。
                    "[]",
                ),
            ],
            ..MachineFixture::default()
        }
    }

    fn listing() -> GlobalsListing {
        let fixture = DetectFixture::build(&machine());
        list_globals(&fixture.context()).expect("这台机器有 LOCALAPPDATA")
    }

    /// 两个来源都报：机器自己 10 个（npm 7 + pip 3，真机 #17 的数字）+ tuoen 3 个。
    ///
    /// **反例在最后两段**：只问机器来源的那一遍会少 3 行，而且这 3 行**按名字去重
    /// 也补不回来** —— `pnpm` / `corepack` / `@deepseek-ai/dsh` 两个来源都有，
    /// 合并成一行就等于丢掉了"这个包该装进哪个根"这个信息（而 `restore` 正是
    /// 靠 `source` 决定往哪儿装的）。
    #[test]
    fn both_sources_are_reported_and_the_machine_alone_would_miss_the_tuoen_rows() {
        let listing = listing();
        let count = |source: GlobalsSource| {
            listing
                .packages
                .iter()
                .filter(|row| row.source == source)
                .count()
        };

        assert_eq!(
            count(GlobalsSource::Machine),
            10,
            "机器自己的 npm 7 + pip 3"
        );
        assert_eq!(count(GlobalsSource::Tuoen), 3, "tuoen 的根里 3 个");
        assert_eq!(listing.packages.len(), 13, "两个来源都要报");
        assert_eq!(
            listing.package_count(GlobalsTool::Npm, GlobalsSource::Machine),
            7
        );
        assert_eq!(
            listing.package_count(GlobalsTool::Pip, GlobalsSource::Tuoen),
            0,
            "pip 的 tuoen 根是空的：它照旧没有一行"
        );

        // **反例一**：只问机器来源 → 少 3 行。
        let machine_only: Vec<&GlobalsPackageRow> = listing
            .packages
            .iter()
            .filter(|row| row.source == GlobalsSource::Machine)
            .collect();
        assert_eq!(
            listing.packages.len() - machine_only.len(),
            3,
            "只问机器来源就会漏掉 tuoen 的那 3 行"
        );

        // **反例二**：那 3 行不能靠"按包名去重"补回来 —— 名字两边都在。
        for name in ["pnpm", "corepack", "@deepseek-ai/dsh"] {
            assert!(
                listing
                    .packages
                    .iter()
                    .any(|row| row.name == name && row.source == GlobalsSource::Tuoen),
                "tuoen 那一行必须有：{name}"
            );
            assert!(
                machine_only.iter().any(|row| row.name == name),
                "同一个包名在机器那边也有，所以按名字合并会静默丢掉来源：{name}"
            );
        }
    }

    /// `roots`：两个根都在，**哪怕它是空的**；`exists` 说得出它今天在不在。
    #[test]
    fn both_roots_are_listed_even_when_one_is_empty() {
        let listing = listing();
        let npm = listing.root_of(GlobalsTool::Npm).expect("npm 的根");
        assert_eq!(npm.root, PathBuf::from(NPM_ROOT));
        assert_eq!(npm.source, GlobalsSource::Tuoen);
        assert!(npm.exists, "这份固定装置里它存在");
        let pip = listing.root_of(GlobalsTool::Pip).expect("pip 的根");
        assert_eq!(pip.root, PathBuf::from(PIP_ROOT));

        // 一个**空**的根照样在清单里（"根在哪里、它是空的"要说得出来）。
        let mut description = machine();
        // 把 tuoen 那两个根从固定装置里撤掉（目录不存在），包也撤掉。
        description.dirs.retain(|dir| !dir.path.starts_with(BASE));
        description.processes.retain(|process| {
            process.args.last().map(String::as_str) != Some(NPM_ROOT)
                && !process.args.contains(&"--user".to_owned())
        });
        let fixture = DetectFixture::build(&description);
        let listing = list_globals(&fixture.context()).expect("根算得出来");
        assert_eq!(listing.packages.len(), 10, "只剩机器自己的 10 个");
        assert_eq!(listing.roots.len(), 2, "两个根都在，且都是空的");
        assert!(
            listing.roots.iter().all(|row| !row.exists),
            "撤掉目录之后，它们说的是'不存在'而不是'有 0 个包'：{:?}",
            listing.roots
        );
        assert_eq!(
            listing.package_count(GlobalsTool::Npm, GlobalsSource::Tuoen),
            0
        );
    }

    /// `binNames`：npm 从 `package.json` 的 `bin` 来，pip 从 Scripts 的文件名来。
    #[test]
    fn bin_names_come_from_the_manifest_for_npm_and_from_the_scripts_for_pip() {
        let listing = listing();
        let find = |source: GlobalsSource, name: &str| {
            listing
                .packages
                .iter()
                .find(|row| row.source == source && row.name == name)
                .unwrap_or_else(|| panic!("没有 {source}/{name} 这一行"))
                .bin_names
                .clone()
        };

        assert_eq!(
            find(GlobalsSource::Tuoen, "pnpm"),
            ["pn", "pnpm", "pnpx", "pnx"],
            "一个包四个命令（实测形状，排序后）"
        );
        assert_eq!(
            find(GlobalsSource::Tuoen, "corepack"),
            Vec::<String>::new(),
            "没有 `bin` 字段 → 拿不到 → 空表（不是报一个错，也不是编一个名字）"
        );
        // pip：包名与 Scripts 里的文件名逐字对上才有。
        assert_eq!(find(GlobalsSource::Machine, "pip"), ["pip.exe"]);
        assert_eq!(find(GlobalsSource::Machine, "pypinyin"), ["pypinyin.exe"]);
        assert_eq!(
            find(GlobalsSource::Machine, "pypdf"),
            Vec::<String>::new(),
            "Scripts 里没有 pypdf.exe"
        );
    }

    /// 工具的 `PATH` 与 `LOCALAPPDATA` 缺席时，`roots` 会如实少掉，
    /// 而**原因**是一条 note（不是"零个包"）。
    #[test]
    fn a_missing_tool_or_an_unknown_runtime_version_is_a_note_not_a_zero() {
        // 没有 node → npm 的 tuoen 根算不出来；npm 自己那一行照旧（决策 175 的反面：
        // 这里是"运行时的版本判不出来"而不是"枚举不出来"）。
        let mut description = machine();
        description
            .processes
            .retain(|process| process.program != "node.exe");
        let fixture = DetectFixture::build(&description);
        let listing = list_globals(&fixture.context()).expect("根算得出来");
        assert_eq!(listing.root_of(GlobalsTool::Npm), None, "版本未知 → 没有根");
        assert!(
            listing.notes.contains(&GlobalsNote {
                tool: GlobalsTool::Npm,
                code: GlobalsNoteCode::RuntimeVersionUnknown,
            }),
            "必须说得出为什么：{:?}",
            listing.notes
        );
        assert!(listing.root_of(GlobalsTool::Pip).is_some(), "pip 不受影响");

        // 工具不在 PATH 上 → 一条 `tool-not-on-path`（决策 171 的那一条）。
        let mut description = machine();
        description
            .env
            .insert("Path".to_owned(), r"C:\Windows".to_owned());
        let fixture = DetectFixture::build(&description);
        let listing = list_globals(&fixture.context()).expect("根算得出来");
        assert!(listing.packages.is_empty());
        for tool in GlobalsTool::ALL {
            assert!(
                listing.notes.contains(&GlobalsNote {
                    tool,
                    code: GlobalsNoteCode::ToolNotOnPath,
                }),
                "{tool} 不在 PATH 上要说出来：{:?}",
                listing.notes
            );
        }
    }

    /// 没有 `LOCALAPPDATA` → **错误**，不是"一份只有机器那半边的表"。
    #[test]
    fn without_local_app_data_the_whole_listing_is_an_error() {
        let mut description = machine();
        description.env.remove("LOCALAPPDATA");
        let fixture = DetectFixture::build(&description);
        assert_eq!(
            list_globals(&fixture.context()),
            Err(GlobalsRootError::MissingLocalAppData)
        );
    }

    /// 行顺序：按（工具，来源，包名）排序 —— 顺序稳定才能比逐字节。
    #[test]
    fn the_rows_are_sorted_by_tool_source_and_name() {
        let listing = listing();
        let keys: Vec<(GlobalsTool, GlobalsSource, String)> = listing
            .packages
            .iter()
            .map(|row| (row.tool, row.source, row.name.clone()))
            .collect();
        let mut sorted = keys.clone();
        sorted.sort();
        assert_eq!(keys, sorted);
    }

    /// 两个来源的 slug 是契约（`globals.toml` 的 `source` 与 `--json` 的 `source`）。
    #[test]
    fn the_source_slugs_are_the_frozen_words() {
        assert_eq!(GlobalsSource::Machine.slug(), "machine");
        assert_eq!(GlobalsSource::Tuoen.slug(), "tuoen");
        for source in GlobalsSource::ALL {
            assert_eq!(GlobalsSource::from_slug(source.slug()), Some(source));
        }
        assert_eq!(GlobalsSource::from_slug("user"), None);
        // 顺序：机器自己的在前。
        assert!(GlobalsSource::Machine < GlobalsSource::Tuoen);
    }
}
