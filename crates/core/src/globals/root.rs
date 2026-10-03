//! `tuoen` 自己管的全局包根：`%LOCALAPPDATA%\tuoen\globals\`（决策 26 的落点）。
//!
//! # 这个模块只回答一个问题：根在哪
//!
//! 它**一个字节都不读磁盘、一个字节都不写磁盘**：`GlobalsRoot` 是一个**算出来的值**
//! （纯路径拼接），而"那个目录在不在、里面有什么"是采集器的事（`capture::collect::globals`
//! 与 [`super::listing`]）。这条分界不是洁癖：根的位置是**契约**（后面几张票的
//! `restore` / `shell` 都要用同一份），而"它今天在不在"是**机器状态**。
//!
//! # 形状是隔离的，不是"另一个全局目录"
//!
//! ```text
//! %LOCALAPPDATA%\tuoen\globals\
//! ├─ npm\
//! │  └─ v24.19.0\        ← `node -v` 的**原样**输出（带 `v`），就是那个运行时的标识
//! └─ pip\                ← `PYTHONUSERBASE` 指这一层；pip 自己会插一层 `Python312\`
//! ```
//!
//! **按运行时版本隔离**（决策 26）：换一个 Node 版本就换一个 prefix，于是
//! "切版本把 7 个全局包静默地变成 0 个"这件事不会发生 —— 那些包还在它们自己那个版本的
//! 目录里，只是当前这个版本看不见它们。
//!
//! # 重定向只走**进程环境**，绝不写用户的配置文件（决策 27）
//!
//! [`GlobalsRoot::redirect_env`] 给出的三个变量（`NPM_CONFIG_PREFIX` /
//! `PYTHONUSERBASE` / `PIP_USER=1`）**只用于我们自己启动的子进程**，绝不写进
//! `HKCU\Environment`（所以 [`GlobalsRoot::from_process_env`] 读的也是**进程环境**：
//! 决策 187 的同一条理由 —— 注册表里根本没有 `LOCALAPPDATA` 这个东西，
//! 它是登录时派生的）。写 `.npmrc` / `pip.ini` 同样禁止：那会覆盖用户自己的配置。
//!
//! # 判不出来时不猜（决策 172 的同一条规矩）
//!
//! npm 的版本目录名来自 `node -v`。答不上来时它是 [`UNKNOWN_VERSION`]，
//! 而 [`GlobalsRoot::npm_prefix`] 在那种情况下返回 `None` —— **不产生
//! `…\npm\unknown\` 这样一个"不知道是哪个运行时"的目录**：装到那里比不装更糟
//! （它会被所有答不上版本的机器共用一个名字）。

use std::ffi::OsStr;
use std::fmt;
use std::path::{Path, PathBuf};

/// npm 的**前缀**重定向变量。
///
/// 名字是 npm 自己定的契约（`npm_config_*` 的写法之一），不是我们发明的。
pub const NPM_PREFIX_VAR: &str = "NPM_CONFIG_PREFIX";

/// pip 的**用户 site** 根（`PYTHONUSERBASE`）。
pub const PYTHONUSERBASE_VAR: &str = "PYTHONUSERBASE";

/// 让 pip 走"用户安装"那一支（`pip install --user` 的等价环境）。
pub const PIP_USER_VAR: &str = "PIP_USER";

/// [`PIP_USER_VAR`] 的值。pip 认的是"这个变量**在**"而不是它的内容。
pub const PIP_USER_VALUE: &str = "1";

/// 判不出运行时版本时**不产生根**的那个 slug（决策 172 的同一个词）。
///
/// 它与 `globals.toml` 的 `tool_version = "unknown"` 是**同一个字符串**：
/// 一处定义，两处用（写两遍迟早会漂移，而漂移的表现是"根算出来了、行没写出来"）。
pub const UNKNOWN_VERSION: &str = "unknown";

/// `%LOCALAPPDATA%` 底下属于我们的那两段。
const TUOEN_DIR: &str = "tuoen";
/// 全局包根在 `<tuoen 家目录>` 下的那一段。
const GLOBALS_DIR: &str = "globals";
/// npm 的隔离层：`<base>\npm\<node -v 原样>`。
const NPM_DIR: &str = "npm";
/// pip 的 `PYTHONUSERBASE` 层：`<base>\pip`（`Python312\` 由 pip 自己插）。
const PIP_DIR: &str = "pip";

/// 这一票管的两个工具（决策 171 的工具表）。
///
/// 取值空间是**开放的**（`globals.toml` 的 `tool` 是自由字符串）：将来加 pnpm / yarn
/// 只多一个变体，不改任何已有形状。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum GlobalsTool {
    /// `npm`（它的运行时版本是 `node -v`）。
    Npm,
    /// `pip`（它的运行时版本是 `pip --version` 里那个 python 版本号）。
    Pip,
}

impl GlobalsTool {
    /// 全部工具，**顺序即行顺序**（`globals.toml` 里 npm 在 pip 之前，决策 174）。
    pub const ALL: [Self; 2] = [Self::Npm, Self::Pip];

    /// 稳定 slug（`globals.toml` 的 `tool`、`--json` 的 `tool`，**不本地化**）。
    #[must_use]
    pub const fn slug(self) -> &'static str {
        match self {
            Self::Npm => "npm",
            Self::Pip => "pip",
        }
    }

    /// 从 slug 解析。**不认识就是 `None`**（不是"默认成 npm"）。
    #[must_use]
    pub fn from_slug(slug: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|tool| tool.slug() == slug)
    }
}

impl fmt::Display for GlobalsTool {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.slug())
    }
}

/// tuoen 管理的全局包根。
///
/// 它是**算出来的**（[`Self::from_base`] / [`Self::from_local_app_data`] /
/// [`Self::from_process_env`] 三种来路），所以它可以在没有磁盘、没有注册表的
/// 情况下被构造与断言 —— 测试因此不需要碰真实的 `%LOCALAPPDATA%`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GlobalsRoot {
    base: PathBuf,
}

/// 算不出根的原因。
///
/// **它是错误而不是"一个空的根"**：`%LOCALAPPDATA%` 读不到时，
/// 相对路径 `.tuoen\globals` 的含义随当前目录变，而"根在哪"必须不随 cwd 变
/// （`trust` 的 `unwired-root` 是同一条理由）。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum GlobalsRootError {
    /// 进程环境里没有 `LOCALAPPDATA`（空串也算没有）。
    #[error(
        "进程环境里没有 `LOCALAPPDATA` —— tuoen 的全局包根在 \
         `%LOCALAPPDATA%\\tuoen\\globals`，所以算不出来。（注册表里没有这个变量：\
         它由登录时派生，见决策 187。）"
    )]
    MissingLocalAppData,
    /// `LOCALAPPDATA` 在，但它不是一个绝对路径。
    #[error(
        "`LOCALAPPDATA` 的值不是一个绝对路径（`{value}`）—— 用它拼出来的根会随当前目录变，\
         所以拒绝：宁可失败，也不要把包装进一个含义会漂移的目录里。"
    )]
    LocalAppDataNotAbsolute {
        /// 拿到的那个值（原样，不展开）。
        value: PathBuf,
    },
}

impl GlobalsRoot {
    /// 从**进程环境**的 `%LOCALAPPDATA%` 拼。
    ///
    /// # 为什么不读注册表（决策 187 的同一个理由）
    ///
    /// `LOCALAPPDATA` / `APPDATA` / `USERPROFILE` **不在**注册表里 ——
    /// 本机实测 `HKCU\Environment` 14 个值、机器级 19 个值，两个作用域都没有它们
    /// （它们是登录时由 `HOMEDRIVE`+`HOMEPATH`+用户名派生的）。一个"去注册表读
    /// `LOCALAPPDATA`"的实现会拿到 `None`，然后**静默地**把整件事变成"没有根"。
    ///
    /// # Errors
    ///
    /// 变量不存在、为空、或者不是绝对路径。
    pub fn from_process_env() -> Result<Self, GlobalsRootError> {
        Self::from_local_app_data(std::env::var_os("LOCALAPPDATA").as_deref())
    }

    /// 从"一个 `LOCALAPPDATA` 的值"拼（[`Self::from_process_env`] 的可测内核）。
    ///
    /// 单独留一个入口是因为"没有 `LOCALAPPDATA`"这条分支**必须能被测到**，
    /// 而测试不能去改自己的进程环境（那是进程级全局状态，测试是并行跑的）。
    ///
    /// # Errors
    ///
    /// `None` / 空串 → [`GlobalsRootError::MissingLocalAppData`]；
    /// 相对路径 → [`GlobalsRootError::LocalAppDataNotAbsolute`]。
    pub fn from_local_app_data(value: Option<&OsStr>) -> Result<Self, GlobalsRootError> {
        let Some(value) = value.filter(|value| !value.is_empty()) else {
            return Err(GlobalsRootError::MissingLocalAppData);
        };
        let path = Path::new(value);
        if !path.is_absolute() {
            return Err(GlobalsRootError::LocalAppDataNotAbsolute {
                value: path.to_path_buf(),
            });
        }
        Ok(Self::from_base(path.join(TUOEN_DIR).join(GLOBALS_DIR)))
    }

    /// 直接给一个已经拼好的根（测试与将来的调用方用；**不做任何检查**）。
    #[must_use]
    pub fn from_base(base: PathBuf) -> Self {
        Self { base }
    }

    /// 根本身：`<LOCALAPPDATA>\tuoen\globals`（不带尾部分隔符）。
    #[must_use]
    pub fn base(&self) -> &Path {
        &self.base
    }

    /// npm 的前缀：`<base>\npm\<tool_version>`。
    ///
    /// # `tool_version == "unknown"` → `None`（决策 26 的那一条）
    ///
    /// 版本目录名就是**那个运行时自己的标识**（`node -v` 的原样输出，带 `v`）：
    /// 答不上来时**不产生任何目录、不产生任何行、不产生任何计划** ——
    /// 所有答不上版本的机器共用一个 `…\npm\unknown\` 比不装更糟。
    ///
    /// **不归一化**：`v24.19.0` 原样当目录名；`24.19.0`（没有 `v`）也是一个合法的
    /// 目录名（我们不去补 `v`，也不去删它）。
    #[must_use]
    pub fn npm_prefix(&self, tool_version: &str) -> Option<PathBuf> {
        if tool_version.is_empty() || tool_version == UNKNOWN_VERSION {
            return None;
        }
        Some(self.base.join(NPM_DIR).join(tool_version))
    }

    /// pip 的 `PYTHONUSERBASE`：`<base>\pip`。
    ///
    /// **这一层不按版本分**：pip 自己会在下面插一层 `Python312\`
    /// （本机实测：`user-base = <root>`、`user-site = <root>\Python312\site-packages`、
    /// console script 落在 `<root>\Python312\Scripts\`），所以版本信息在那层里，
    /// 不需要我们再分一次 —— 也就没有"版本答不上来就不能用"这条限制。
    #[must_use]
    pub fn pip_userbase(&self) -> PathBuf {
        self.base.join(PIP_DIR)
    }

    /// 该工具该版本的重定向变量（名字 + 值），**只用于我们自己启动的子进程**。
    ///
    /// * npm → `[(NPM_CONFIG_PREFIX, <prefix>)]`；版本判不出来时是**空表**
    ///   （那个变量本来就没有正确的值可给，给一个 `…\unknown` 是在编）。
    /// * pip → `[(PYTHONUSERBASE, <userbase>), (PIP_USER, "1")]`。
    ///
    /// 顺序稳定（借用者可以直接断言整张表）。**它不写任何文件、不碰注册表**：
    /// 决策 27 —— 写 `.npmrc` 会覆盖用户自己的配置。
    #[must_use]
    pub fn redirect_env(&self, tool: GlobalsTool, tool_version: &str) -> Vec<(String, String)> {
        match tool {
            GlobalsTool::Npm => self
                .npm_prefix(tool_version)
                .map(|prefix| vec![(NPM_PREFIX_VAR.to_owned(), path_text(&prefix))])
                .unwrap_or_default(),
            GlobalsTool::Pip => vec![
                (
                    PYTHONUSERBASE_VAR.to_owned(),
                    path_text(&self.pip_userbase()),
                ),
                (PIP_USER_VAR.to_owned(), PIP_USER_VALUE.to_owned()),
            ],
        }
    }
}

/// 一条路径 → 环境变量里那个字符串。
///
/// `to_string_lossy` 而不是 `display().to_string()`：两者对"不是合法 Unicode 的
/// Windows 路径"都会替换，但 `to_string_lossy` 的名字直接说出了这件事。
/// 真机上拼进来的两段都是我们自己的常量，出问题的只可能是 `%LOCALAPPDATA%` 本身。
fn path_text(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOCAL: &str = r"C:\Users\dev\AppData\Local";

    fn root() -> GlobalsRoot {
        GlobalsRoot::from_local_app_data(Some(OsStr::new(LOCAL))).expect("有 LOCALAPPDATA")
    }

    /// 根 = `<LOCALAPPDATA>\tuoen\globals`，而且**全部来自那个输入**。
    #[test]
    fn the_root_is_local_app_data_plus_tuoen_plus_globals() {
        assert_eq!(
            root().base(),
            Path::new(LOCAL).join("tuoen").join("globals")
        );

        // 换一个输入就换一个根 —— 这里没有一个硬编码的用户名 / 盘符。
        let elsewhere =
            GlobalsRoot::from_local_app_data(Some(OsStr::new(r"D:\Other\Local"))).expect("有值");
        assert_eq!(
            elsewhere.base(),
            Path::new(r"D:\Other\Local").join("tuoen").join("globals")
        );
        assert_ne!(root().base(), elsewhere.base());

        // 用户名的**判据**：根里除输入之外的每一段都是我们自己的常量。
        let built = root();
        let suffix = built
            .base()
            .strip_prefix(LOCAL)
            .expect("根必须以输入为前缀");
        assert_eq!(suffix, Path::new("tuoen").join("globals"));
        // 而它绝不等于某个"用户目录下的全局包目录"（那是机器的，不是我们的）。
        assert_ne!(root().base(), Path::new(LOCAL).join("npm"));
    }

    /// 版本目录名 = `node -v` 的**原样**输出（带 `v`），一个字都不改写。
    #[test]
    fn the_npm_prefix_is_the_runtime_string_verbatim() {
        let base = root().base().to_path_buf();
        assert_eq!(
            root().npm_prefix("v24.19.0"),
            Some(base.join("npm").join("v24.19.0"))
        );
        // 没有 `v` 的值也是原样用（我们不补也不删）——"归一化"是**比较者**的事。
        assert_eq!(
            root().npm_prefix("24.19.0"),
            Some(base.join("npm").join("24.19.0"))
        );
    }

    /// 决策 26/172：版本判不出来时**没有根**（不是 `…\npm\unknown\`）。
    #[test]
    fn an_unknown_runtime_version_has_no_npm_root_at_all() {
        assert_eq!(root().npm_prefix(UNKNOWN_VERSION), None);
        assert_eq!(root().npm_prefix(""), None);
        // 一真一假里的"假"的反面：同一个输入换一个版本，根必须真的不同。
        assert_ne!(root().npm_prefix("v24.19.0"), root().npm_prefix("v22.0.0"));
    }

    /// pip 的 userbase 是 `<base>\pip`，与版本无关。
    #[test]
    fn the_pip_userbase_does_not_depend_on_the_version() {
        let base = root().base().to_path_buf();
        assert_eq!(root().pip_userbase(), base.join("pip"));
        // 与 npm 那一支**不同层**（同一个 base 下并列的两个目录）。
        assert_ne!(root().pip_userbase(), base.join("npm"));
    }

    /// 三个重定向变量：名字与值都是契约。
    #[test]
    fn the_redirect_environment_is_the_frozen_triple() {
        // 字面量断言：名字是 npm / pip 自己的契约，改一个字母就等于没设。
        assert_eq!(NPM_PREFIX_VAR, "NPM_CONFIG_PREFIX");
        assert_eq!(PYTHONUSERBASE_VAR, "PYTHONUSERBASE");
        assert_eq!(PIP_USER_VAR, "PIP_USER");
        assert_eq!(PIP_USER_VALUE, "1");

        let base = root().base().to_path_buf();
        assert_eq!(
            root().redirect_env(GlobalsTool::Npm, "v24.19.0"),
            vec![(
                "NPM_CONFIG_PREFIX".to_owned(),
                base.join("npm")
                    .join("v24.19.0")
                    .to_string_lossy()
                    .into_owned()
            )]
        );
        // npm 版本判不出来 → **空表**（不是"一个没有值的变量"）。
        assert_eq!(
            root().redirect_env(GlobalsTool::Npm, UNKNOWN_VERSION),
            vec![]
        );

        assert_eq!(
            root().redirect_env(GlobalsTool::Pip, "3.12"),
            vec![
                (
                    "PYTHONUSERBASE".to_owned(),
                    base.join("pip").to_string_lossy().into_owned()
                ),
                ("PIP_USER".to_owned(), "1".to_owned()),
            ]
        );
        // pip 的版本不影响它的重定向（`Python312\` 那一层由 pip 自己插）。
        assert_eq!(
            root().redirect_env(GlobalsTool::Pip, UNKNOWN_VERSION),
            root().redirect_env(GlobalsTool::Pip, "3.12")
        );
    }

    /// 没有 / 空的 `LOCALAPPDATA` 是一个**错误**，不是"一个空的根"。
    #[test]
    fn a_missing_local_app_data_is_an_error_and_never_a_relative_root() {
        assert_eq!(
            GlobalsRoot::from_local_app_data(None),
            Err(GlobalsRootError::MissingLocalAppData)
        );
        assert_eq!(
            GlobalsRoot::from_local_app_data(Some(OsStr::new(""))),
            Err(GlobalsRootError::MissingLocalAppData)
        );
        // 相对路径**拒绝**：根的含义随 cwd 变就等于没有根（`unwired-root` 同源）。
        match GlobalsRoot::from_local_app_data(Some(OsStr::new(r"AppData\Local"))) {
            Err(GlobalsRootError::LocalAppDataNotAbsolute { value }) => {
                assert_eq!(value, Path::new(r"AppData\Local"));
            }
            other => panic!("相对路径必须被拒绝，实际：{other:?}"),
        }
        // 错误消息里要有**变量名**（人说得出该去设什么）。
        let text = GlobalsRootError::MissingLocalAppData.to_string();
        assert!(text.contains("LOCALAPPDATA"), "{text}");
    }

    /// [`GlobalsRoot::from_process_env`] 读的是**进程环境**：它与
    /// `std::env::var_os("LOCALAPPDATA")` 是同一个答案（在任何一台机器上都成立，
    /// 所以这条断言不依赖开发机的状态）。
    #[test]
    fn from_process_env_agrees_with_the_process_environment() {
        let expected =
            GlobalsRoot::from_local_app_data(std::env::var_os("LOCALAPPDATA").as_deref());
        let actual = GlobalsRoot::from_process_env();
        assert_eq!(actual, expected);
        // 本仓库的进程环境里**有** `LOCALAPPDATA`（登录时派生的那一个）；
        // 没有的话上面那条等式仍然成立，但那时说明这台机器上整条路都走不通。
        if let Ok(root) = actual {
            assert!(root.base().is_absolute(), "{:?}", root.base());
        }
    }

    /// 工具 slug 是**往返**的，而且不认识就是不认识。
    #[test]
    fn the_tool_slugs_round_trip() {
        for tool in GlobalsTool::ALL {
            assert_eq!(GlobalsTool::from_slug(tool.slug()), Some(tool));
            assert_eq!(tool.to_string(), tool.slug());
        }
        assert_eq!(GlobalsTool::Npm.slug(), "npm");
        assert_eq!(GlobalsTool::Pip.slug(), "pip");
        assert_eq!(GlobalsTool::from_slug("nvm"), None);
        assert_eq!(GlobalsTool::from_slug("NPM"), None, "slug 是大小写敏感的");
        // 顺序即行顺序（决策 174：npm 在前）。
        assert!(GlobalsTool::Npm < GlobalsTool::Pip);
    }
}
