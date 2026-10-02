//! `tuoen lock` 的参数与执行：把**解析结果**写成一份可提交的锁文件。
//!
//! # 锁文件是什么、不是什么
//!
//! `tuoen.toml` 是**声明**（"我要 Node 24"），`tuoen.lock` 是**解析结果**
//! （"这台机器上 24 指的是 24.19.0，它在哪、从哪来、哈希是多少"）。
//! 两者进 git，而它们的关系只有一条判据（决策 116）：
//!
//! > 锁与声明**不一致** → `shell` / `auto` 拒绝启动，并让你跑 `tuoen lock`。
//!
//! 这条判据的价值全在"不一致"那一侧：它让"我改了 `tuoen.toml` 但忘了重新 lock"
//! 变成一句**当场就说出来**的话，而不是"某个人在别的机器上拿到了另一套工具链"。
//!
//! # 决策 115：锁里**没有**时间戳
//!
//! `to_toml()` 是纯函数：同一台机器状态解析两次**逐字节相同**。
//! 时间戳会让每次 `tuoen lock` 都产生一行 diff，而那一行**不携带任何信息** ——
//! 一个每天都要在 code review 里被忽略的 diff，等于训练人忽略这份文件。
//!
//! `manager` 与 `hash` 用 `skip_serializing_if`：TOML **没有** `null`
//! （与决策 87 同一条规矩），一个 `manager = ""` 会让"没有管理器"与
//! "管理器名叫空串"变成同一件事。
//!
//! # `--dry-run`
//!
//! 走**同一套**解析与组装代码，只是不落盘（决策 20：两套代码路径必然漂移，
//! **漂移的预览比没有预览更危险**）。所以 `written` 这个字段是**如实**的：
//! `--dry-run` 下恒为 `false`，而那**不是**"写失败了"，是"这一次的任务里没有写"。
//!
//! # 为什么 `lock` 每次都重新解析
//!
//! `shell` 有锁就用锁（那是它的快路径），而 `lock` 的**全部工作**就是重新解析 ——
//! 它是一条显式的"把我现在看到的东西记下来"的命令。这里没有缓存，
//! 也没有"锁没变就不写"：那会让"跑过 lock"与"锁被更新了"变成两件事。

use clap::Args;
use tuoen_core::pin::{LOCK_FILE_NAME, LockFile, PIN_FILE_NAME, PinFile};

use crate::envelope::Envelope;
use crate::exit;
use crate::pin_view::{Failure, LockView, print_lock_human, report};
use crate::shell_cmd;

/// `tuoen lock` 的参数。
#[derive(Debug, Args)]
pub struct LockArgs {
    /// 只打印解析结果，**不写 `tuoen.lock`**。
    #[arg(long)]
    pub dry_run: bool,

    /// 输出稳定的 JSON（键与取值不本地化）。
    ///
    /// **不像 `shell` / `auto` 那样要求 `--dry-run`**：`lock` 本来就不启动子进程，
    /// stdout 上没有第二个写者（决策 123 只对"会起交互式子 shell"的那两个命令生效）。
    #[arg(long)]
    pub json: bool,
}

/// 跑 `tuoen lock`。返回退出码。
pub fn run(args: &LockArgs) -> i32 {
    match dispatch(args) {
        Ok(code) => code,
        Err(failure) => report("lock", args.json, &failure, false),
    }
}

fn dispatch(args: &LockArgs) -> Result<i32, Failure> {
    let cwd = std::env::current_dir()
        .map_err(|error| Failure::io(None, format!("读不出当前目录：{error}")))?;

    let pin = PinFile::load(&cwd.join(PIN_FILE_NAME))?;
    // 走 `shell_cmd` 的那一个解析器 —— **本仓只有一处**做"解析 pin"这件事。
    // 另写一份的结果是 `tuoen lock` 写出来的锁与 `tuoen shell` 认的锁
    // 在某个边界条件下不是同一个东西，而那个边界条件只有用户会撞上。
    let tools = shell_cmd::resolve_now(&pin)?;

    let lock_path = cwd.join(LOCK_FILE_NAME);
    let view = if args.dry_run {
        LockView::new(&lock_path, &tools, false)
    } else {
        // **锁的形状只有一处定义**（core 的 `LockFile::from_resolved`）：
        // 命令行这边再拼一遍 `LockTool` 的结果是"`shell` 写的锁"与"`lock` 写的锁"
        // 在某个字段上不一样，而它们必须逐字节可比。
        LockFile::write(&lock_path, &LockFile::from_resolved(&tools))?;
        // **写完再读回来**：`to_toml()` 是纯函数，而"磁盘上现在是什么"是另一件事。
        // 报告里给的必须是后者 —— 否则一次写坏（磁盘满、权限、别的进程同时改）
        // 会静默消失在一句"我以为我写了什么"里。
        let lock = LockFile::load(&lock_path)?.ok_or_else(|| {
            Failure::io(
                Some(&lock_path),
                format!(
                    "刚写完 `{}` 却读不回来 —— 磁盘上的东西和刚才写下去的不是一回事。",
                    lock_path.display()
                ),
            )
        })?;
        LockView::from_lock_file(&lock_path, &lock, true)
    };

    if args.json {
        crate::print_json(&Envelope::ok("lock", &view));
    } else {
        print_lock_human(&view);
    }
    Ok(exit::SUCCESS)
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::LockArgs;
    use crate::cli::{Cli, Command};

    #[test]
    fn a_bare_lock_writes_and_a_dry_run_does_not() {
        let cli = Cli::try_parse_from(["tuoen", "lock"]).expect("parse");
        match cli.command {
            Command::Lock(LockArgs { dry_run, json }) => {
                assert!(!dry_run, "默认是**真的写** —— 这条命令的工作就是写锁");
                assert!(!json, "默认是人类输出（中文优先）");
            }
            other => panic!("应当是 lock，实际：{other:?}"),
        }
    }

    #[test]
    fn json_is_allowed_without_dry_run_because_lock_starts_no_shell() {
        // 决策 123 把 `--json` 与 `--dry-run` 绑在一起，理由**只对会起交互式
        // 子 shell 的那两个命令成立**（子进程会往同一个 stdout 上写东西）。
        // `lock` 不起任何子 shell，所以它照常支持 `--json`。
        let cli = Cli::try_parse_from(["tuoen", "lock", "--json"]).expect("parse");
        assert!(matches!(
            cli.command,
            Command::Lock(LockArgs { json: true, .. })
        ));
        let cli = Cli::try_parse_from(["tuoen", "lock", "--json", "--dry-run"]).expect("parse");
        assert!(matches!(
            cli.command,
            Command::Lock(LockArgs {
                json: true,
                dry_run: true
            })
        ));
    }

    #[test]
    fn the_lock_schema_version_comes_from_core_and_is_pinned() {
        // 改这个数字是破坏性变更（锁文件进 git），必须是有意为之。
        // **CLI 不自己定义一份**：两个来源的结果是某一天 CLI 写的锁带着
        // 一个 core 不认的版本号，而那条错误只有在用户那边才看得见。
        assert_eq!(tuoen_core::pin::LOCK_SCHEMA_VERSION, 1);
    }

    #[test]
    fn there_is_no_ignore_lock_switch_on_lock_either() {
        // `lock` 是 `lock-mismatch` 的**解法**，所以它没有"忽略锁"这回事。
        let Err(error) = Cli::try_parse_from(["tuoen", "lock", "--ignore-lock"]) else {
            panic!("`--ignore-lock` 必须被拒");
        };
        assert_eq!(error.exit_code(), 2, "{error}");
    }
}
