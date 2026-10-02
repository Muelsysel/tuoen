//! `tuoen trust` 的参数与执行：信任清单的**唯一查看入口**。
//!
//! # 这个命令存在的原因
//!
//! 决策 15：`tuoen shell` 永远可用，而**自动切换**（`tuoen auto`）需要先信任一次。
//! 理由是安全：进入陌生仓库就自动改工具链版本，等于让一个 clone 下来的
//! `tuoen.toml` 决定你跑哪个"Node 版本"。
//!
//! 决策 16 决定了这份清单放在哪：**用户级中央清单**（`%APPDATA%\tuoen\trust.toml`），
//! 记绝对路径 + **首次信任时的指纹**。两个理由都不能省：
//!
//! * 信任标记若放在被信任的目录**内**，任何 clone 下来就自带信任标记，等于没有机制。
//! * 指纹是因为路径可能被替换（删掉目录再 clone 一个同名目录）——
//!   光记路径的话，新 clone 来的那个目录会**继承**旧目录的信任。
//!
//! # 三个动作，一个命令
//!
//! ```text
//! tuoen trust                  信任当前目录（算指纹、写清单）
//! tuoen trust --list           列出每一条 + 它**现在**还有不有效
//! tuoen trust --revoke <PATH>  摘掉一条
//! ```
//!
//! `--list` 与 `--revoke` 由 clap 声明为互斥：同时给两个会得到一个退出码 2 的
//! 用法错误，而不是"悄悄按其中一个执行"。理由与 `doctor` 拒掉 `--fix` 一样 ——
//! 一个能猜出用户意图的命令，在猜错的时候不会说自己猜错了。
//!
//! # `--list` 必须重算状态（决策 118）
//!
//! 一个把过期条目显示成"已信任"的清单是在**说谎**，而它恰好是用户唯一的查看入口。
//! 所以每一条的 `state` 都是**现场重算**的（`trusted` / `stale` / `missing-file`），
//! 不是写盘时记下来的。这一层不缓存、不猜 —— 判据在 core 的 `TrustState` 里。
//!
//! # 这一层写什么
//!
//! 只写 `%APPDATA%\tuoen\trust.toml`（由 core 的 `TrustFile::write` 做**临时文件 +
//! rename** 的原子替换，决策 118）。原子替换不是洁癖：这份文件被两个终端同时改的
//! 代价是**整份信任清单消失**（或半截 TOML）。

use std::path::Path;

use clap::Args;
use tuoen_core::pin::{PIN_FILE_NAME, TrustFile, fingerprint_of_file};

use crate::envelope::Envelope;
use crate::exit;
use crate::pin_view::{
    Failure, TrustAddView, TrustEntryView, TrustListView, TrustRevokeView, UNWIRED_ROOT,
    print_trust_add_human, print_trust_list_human, print_trust_revoke_human, report,
};

/// `tuoen trust` 的参数。
#[derive(Debug, Args)]
pub struct TrustArgs {
    /// 列出清单里的每一条，以及它**现在**还有不有效。
    #[arg(long, conflicts_with = "revoke")]
    pub list: bool,

    /// 摘掉一条（路径比较由 core 负责：绝对化 + 忽略大小写 + 忽略尾部分隔符）。
    #[arg(long, value_name = "PATH")]
    pub revoke: Option<std::path::PathBuf>,

    /// 输出稳定的 JSON（键与取值不本地化）。
    #[arg(long)]
    pub json: bool,
}

/// 跑 `tuoen trust`。返回退出码。
pub fn run(args: &TrustArgs) -> i32 {
    match dispatch(args) {
        Ok(code) => code,
        Err(failure) => report("trust", args.json, &failure, false),
    }
}

fn dispatch(args: &TrustArgs) -> Result<i32, Failure> {
    if args.list {
        return list(args);
    }
    if let Some(path) = &args.revoke {
        return revoke(args, path);
    }
    add(args)
}

// ─────────────────────────────────────────────────────────────────────────────
// 三个动作
// ─────────────────────────────────────────────────────────────────────────────

/// `tuoen trust`：信任当前目录。
///
/// 顺序是**先算指纹、再装载、最后写**：算指纹失败（没有 `tuoen.toml`）时
/// 一个字节都不该落盘 —— 一份"信任了一个没有 pin 的目录"的记录既没用又误导。
fn add(args: &TrustArgs) -> Result<i32, Failure> {
    let cwd = std::env::current_dir()
        .map_err(|error| Failure::io(None, format!("读不出当前目录：{error}")))?;
    let fingerprint = fingerprint_of_file(&cwd.join(PIN_FILE_NAME))?;

    let mut doc = load()?;
    let trust_file = doc.path().to_path_buf();
    doc.add(&cwd, &fingerprint, &tuoen_store::now_rfc3339());
    doc.write()?;

    let view = TrustAddView {
        path: cwd.display().to_string(),
        fingerprint,
        action: "trusted",
    };
    if args.json {
        crate::print_json(&Envelope::ok("trust", &view));
    } else {
        print_trust_add_human(&view, &trust_file);
    }
    Ok(exit::SUCCESS)
}

/// `tuoen trust --list`：列出每一条 + 它**现在**的状态。
fn list(args: &TrustArgs) -> Result<i32, Failure> {
    let doc = load()?;
    let trust_file = doc.path().to_path_buf();
    // **状态现场重算**（决策 118）：写盘时记下来的"已信任"会在
    // `tuoen.toml` 改动之后继续显示"已信任"，而那份清单的全部价值
    // 恰好在于它区分这两件事。
    let entries: Vec<TrustEntryView> = doc
        .entries()
        .iter()
        .map(|entry| TrustEntryView::new(entry, doc.state(Path::new(&entry.path))))
        .collect();

    let view = TrustListView {
        trust_file: trust_file.display().to_string(),
        entries,
    };
    if args.json {
        crate::print_json(&Envelope::ok("trust", &view));
    } else {
        print_trust_list_human(&view);
    }
    Ok(exit::SUCCESS)
}

/// `tuoen trust --revoke <PATH>`：摘掉一条。
///
/// # 没摘到就不写盘
///
/// 一次无谓的原子替换会改 mtime、会让 diff 工具看到一次变更，而这份文件被
/// 两个终端同时改的代价是整份清单消失（决策 118）。所以"清单里本来就没有这条"
/// 时**不写**，并且如实报 `absent` —— 把 no-op 印成成功就是"假装成功"。
fn revoke(args: &TrustArgs, path: &Path) -> Result<i32, Failure> {
    let mut doc = load()?;
    let trust_file = doc.path().to_path_buf();
    let revoked = doc.revoke(path);
    if revoked {
        doc.write()?;
    }

    let view = TrustRevokeView {
        path: path.display().to_string(),
        action: if revoked { "revoked" } else { "absent" },
    };
    if args.json {
        crate::print_json(&Envelope::ok("trust", &view));
    } else {
        print_trust_revoke_human(&view, &trust_file);
    }
    Ok(exit::SUCCESS)
}

// ─────────────────────────────────────────────────────────────────────────────
// 清单的装载（`shell_cmd` 的信任门也用这一份）
// ─────────────────────────────────────────────────────────────────────────────

/// 装载信任清单。**全族唯一一处**做这件事的地方。
///
/// `at_default_location()` 返回 `None` 意味着 `%APPDATA%` 读不到 ——
/// 那时清单会落到一个相对路径上，而相对路径的含义随 cwd 变。这正是本仓库
/// 最不能接受的那类错："一句看起来完全合理的错话，比一次崩溃更难被发现"
/// （`AGENTS.md`）。所以宁可失败。
///
/// `pub(crate)` 是给 `crate::shell_cmd` 的信任门用的：**同一份清单、同一套失败**，
/// 两处各写一遍的结果是某一天 `auto` 报的错误与 `trust --list` 报的不是同一件事。
pub fn load() -> Result<TrustFile, Failure> {
    let Some(handle) = TrustFile::at_default_location() else {
        return Err(unwired_root());
    };
    handle.load().map_err(Failure::from)
}

/// `%APPDATA%` 读不到时的失败。**一句话，一处定义。**
fn unwired_root() -> Failure {
    Failure::cli(
        UNWIRED_ROOT,
        "算不出信任清单的位置：`%APPDATA%` 读不到，\
         `%APPDATA%\\tuoen\\trust.toml` 因此退成了一个相对路径。\n\
         相对路径的含义随当前目录变，而信任清单**必须**在被信任的目录之外\
         （决策 16：否则任何 clone 下来就自带信任标记，等于没有机制）。\n\
         所以这里宁可失败：请先设好 `%APPDATA%` 再跑一次。",
    )
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::{TrustArgs, UNWIRED_ROOT};
    use crate::cli::{Cli, Command};

    #[test]
    fn a_bare_trust_means_trust_the_current_directory() {
        let cli = Cli::try_parse_from(["tuoen", "trust"]).expect("parse");
        match cli.command {
            Command::Trust(TrustArgs { list, revoke, json }) => {
                assert!(!list, "不带参数是**信任当前目录**，不是列出");
                assert!(revoke.is_none());
                assert!(!json, "默认是人类输出（中文优先）");
            }
            other => panic!("应当是 trust，实际：{other:?}"),
        }
    }

    #[test]
    fn list_and_revoke_are_mutually_exclusive() {
        // 同时给两个时必须是**用法错误**（退出码 2），而不是"悄悄按其中一个执行"：
        // 一个能猜出用户意图的命令，在猜错的时候不会说自己猜错了。
        let Err(error) = Cli::try_parse_from(["tuoen", "trust", "--list", "--revoke", r"C:\x"])
        else {
            panic!("`--list` 与 `--revoke` 必须互斥");
        };
        assert_eq!(error.exit_code(), 2, "{error}");
    }

    #[test]
    fn revoke_takes_a_path_and_list_takes_nothing() {
        let cli = Cli::try_parse_from(["tuoen", "trust", "--revoke", r"C:\Work\proj", "--json"])
            .expect("parse");
        match cli.command {
            Command::Trust(TrustArgs { revoke, json, .. }) => {
                assert_eq!(
                    revoke.as_deref(),
                    Some(std::path::Path::new(r"C:\Work\proj"))
                );
                assert!(json);
            }
            other => panic!("应当是 trust，实际：{other:?}"),
        }
        assert!(
            Cli::try_parse_from(["tuoen", "trust", "--list", "extra"]).is_err(),
            "`--list` 不收位置参数"
        );
    }

    #[test]
    fn the_unwired_root_code_is_lowercase_kebab_ascii() {
        assert_eq!(UNWIRED_ROOT, "unwired-root");
        assert!(
            UNWIRED_ROOT
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        );
    }
}
