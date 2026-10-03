//! `tuoen globals list` 的编排。
//!
//! # 这条命令**只读**
//!
//! 它一次都不写磁盘，也不写任何用户配置（决策 27）。它跑的三类进程都是查询：
//! `node -v` / `npm ls -g` / `pip list`（机器那一份），以及带重定向的
//! `npm ls -g --prefix …` / `pip list --user`（我们那一份）。
//!
//! # 为什么六个依赖是真的、而不是写死的
//!
//! 与 `detect` 同一条理由：读机器的代码只有一处（[`tuoen_core::globals`]），
//! 而它拿到的是一组**注入的**适配器。真机跑真实现；固定装置跑假实现。
//! 于是"测试里绝不出现网络、绝不写真实的 `%LOCALAPPDATA%\tuoen\globals`"
//! 是架构，不是纪律。
//!
//! # 算不出根就是**错误**
//!
//! `LOCALAPPDATA` 不在进程环境里（决策 187 的那条理由：它本来就不在注册表里）时，
//! 这条命令一半的答案（`roots`）不存在。印一张只有机器那半边的表会被读成
//! "tuoen 什么都没管" —— 那是假话，所以退出码 1 加一个中文错误。
//! （`capture` 在同一个输入下照旧采机器那一份：快照少一半仍然是快照。）

use tuoen_core::{GlobalsListing, GlobalsRootError, list_globals};

use crate::detect_ctx::Backends;
use crate::envelope::Envelope;
use crate::exit;
use crate::globals::GlobalsListArgs;
use crate::globals_view::GlobalsListView;

/// `tuoen globals list`。
pub fn run_list(args: &GlobalsListArgs) -> i32 {
    match list() {
        Ok(listing) => {
            if args.json {
                crate::print_json(&Envelope::ok(
                    "globals.list",
                    &GlobalsListView::from(&listing),
                ));
            } else {
                crate::globals_view::print_human(&listing);
            }
            exit::SUCCESS
        }
        Err(error) => {
            if args.json {
                crate::print_json(&Envelope::err(
                    "globals.list",
                    error.code(),
                    error.message(),
                ));
            } else {
                eprintln!("tuoen: {}", error.message());
            }
            exit::RUNTIME_ERROR
        }
    }
}

/// 两件事：造上下文、枚举。
///
/// `probe_versions = true`：这条命令**要**版本 —— npm 的那个根就写在版本目录名里
/// （`…\npm\v24.19.0`），不问版本就报不出根。`capture --no-version` 那条路上的
/// 取舍完全不同（快照还要描述机器里别的东西），所以这里不跟着那个开关走。
pub fn list() -> Result<GlobalsListing, GlobalsError> {
    let backends = Backends::assemble();
    let ctx = backends.context(true);
    list_globals(&ctx).map_err(GlobalsError::from)
}

/// 这条命令能给出的两种失败。
#[derive(Debug)]
pub enum GlobalsError {
    /// 算不出根（进程环境里没有 `LOCALAPPDATA`，或者它不是一个绝对路径）。
    Root(GlobalsRootError),
}

impl From<GlobalsRootError> for GlobalsError {
    fn from(error: GlobalsRootError) -> Self {
        Self::Root(error)
    }
}

impl GlobalsError {
    /// `--json` 的 `error.code`（稳定 slug，**不本地化**）。
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::Root(_) => "unwired-root",
        }
    }

    /// 给人看的一句话（中文）。
    ///
    /// 前半句**原样来自** [`GlobalsRootError`]（`thiserror` 的那条消息）——
    /// "根算不出来的原因"只有一处实现，这里只追加"下一步做什么"。
    /// 一句只说"没有 `LOCALAPPDATA`"的错误会让人不知道该怎么办。
    #[must_use]
    pub fn message(&self) -> String {
        match self {
            Self::Root(error) => format!(
                "{error}\n  （tuoen 只从**进程环境**读 `LOCALAPPDATA`：注册表里没有这个变量，\
                 去那儿读会静默拿到 `None` —— 决策 187。在一个正常的用户会话里跑这条命令。）"
            ),
        }
    }
}
