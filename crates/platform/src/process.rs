//! 版本探测需要的那种"跑一个进程并拿回它的输出"。
//!
//! **为什么必须有超时**（ticket #11 的硬要求）：一个挂住的工具不能挂住整个 `detect`。
//! 超时之后记录的是"**发现但版本未知**"，不是失败 —— 工具本身是存在的，
//! 我们只是没问到版本。把超时当失败会丢掉一条真实安装。
//!
//! **为什么必须同时读两路**（`docs/DESIGN.md` §2.4，本机实测）：
//! `git --version` 与 `java -version` 输出到 **stderr**，`node -v` 到 **stdout**。
//! 只读 stdout 会得到"git 没有版本"，只读 stderr 会得到"node 没有版本"。
//!
//! **测试实现**：[`crate::fixture::FakeProcessRunner`]。
//! 真实实现 [`SystemProcessRunner`] 的超时有独立用例：真的起一个不返回的进程。

use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::error::PlatformError;

/// 一次进程调用的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessOutcome {
    /// 进程真的起来了。`false` 表示路径不可执行（我们不会为它编一个版本）。
    pub spawned: bool,
    /// 到了超时还没退出，已被终止。
    pub timed_out: bool,
    /// 退出码。被信号终止或超时时为 `None`。
    pub exit_code: Option<i32>,
    /// 标准输出的**原始字节**。
    ///
    /// **二进制的东西必须从这里拿。** 用 [`Self::stdout`] 拿的是
    /// lossy 转换过的文本，任意非 UTF-8 序列会变成 `U+FFFD` —— 对
    /// "把 HTTP 响应体从 curl 的 stdout 里捞出来"这条路径是致命的
    /// （压缩包里一定有非 UTF-8 序列）。见 [`Capture::finish`]。
    pub stdout_bytes: Vec<u8>,
    /// 标准输出的文本形式（**有意的 lossy**：版本号是 ASCII，
    /// 而本地化输出在本机可能是 GBK；我们只从里面抠数字，
    /// 永远不匹配消息文本 —— 见 `AGENTS.md`）。
    pub stdout: String,
    /// 标准错误。**版本号经常在这里**。
    pub stderr: String,
    /// 没起来时的原因。仅用于人类可读的 `evidence`，**不参与任何判断**。
    pub spawn_error: Option<String>,
}

impl ProcessOutcome {
    /// 进程没起来。
    #[must_use]
    pub fn not_spawned(reason: impl Into<String>) -> Self {
        Self {
            spawned: false,
            timed_out: false,
            exit_code: None,
            stdout_bytes: Vec::new(),
            stdout: String::new(),
            stderr: String::new(),
            spawn_error: Some(reason.into()),
        }
    }

    /// stdout 与 stderr 合起来。**版本探测只看这个**，因为两路都可能承载版本号。
    #[must_use]
    pub fn combined(&self) -> String {
        let mut out = String::with_capacity(self.stdout.len() + self.stderr.len() + 1);
        out.push_str(&self.stdout);
        out.push('\n');
        out.push_str(&self.stderr);
        out
    }
}

/// 跑进程的抽象。
///
/// # 为什么有 [`ProcessRunner::run_env`] 这个入口（票据 #23）
///
/// 决策 27 只允许**通过进程环境变量**重定向包管理器（`NPM_CONFIG_PREFIX` /
/// `PYTHONUSERBASE` / `PIP_USER`）：绝不写 `.npmrc` / `pip.ini` / 注册表。
/// 于是"把几个变量塞进子进程的环境块"必须是这一层的能力。
///
/// **它刻意是必需方法，而不是"带默认实现的便利方法"**：一个有默认实现的版本
/// 只要忘记覆盖就会**静默忽略** `env`，而忽略的表现是"包被装进了机器自己的
/// prefix / 用户自己的 site-packages 里"—— 决策 26 里最贵的那类错（它不报错，
/// 直到换运行时版本的那一天）。必需方法把这件事变成编译期问题。
pub trait ProcessRunner {
    /// 跑 `program args`，最多等 `timeout`。**不改子进程的环境。**
    ///
    /// **实现必须保证**：无论子进程做什么，本调用都会在 `timeout` 之后的一个有界时间内返回。
    fn run(&self, program: &Path, args: &[&str], timeout: Duration) -> ProcessOutcome {
        self.run_env(program, args, &[], timeout)
    }

    /// 跑 `program args`，并把 `env` 里的变量**覆盖/新增**进子进程的环境块。
    ///
    /// 语义与 [`crate::spawn_inherit`] 的 `env` 参数一致，**两者必须一致**：
    ///
    /// * 作用在**继承来的**环境块上（**不** `env_clear`：`PATH`、`PATHEXT` 这些
    ///   子进程本来就要有，清掉它们等于让每个工具都找不到）；
    /// * 变量名大小写不敏感（Windows 的查找规则），值**原样**写进去 ——
    ///   不做任何 `%VAR%` 展开（展开是不可逆的信息损失，见 `AGENTS.md` 规矩二）。
    fn run_env(
        &self,
        program: &Path,
        args: &[&str],
        env: &[(String, String)],
        timeout: Duration,
    ) -> ProcessOutcome;
}

/// 真实实现：`std::process::Command` + 自建超时。
///
/// **不用 `wait-timeout` crate**：我们需要的不是"等一会儿再看"，而是
/// "超时就杀掉、然后把已经读到的输出带走"，这个组合它不覆盖。
///
/// **stdin 接 NUL 设备**：会停下来问问题的工具不能把 `detect` 挂住。
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemProcessRunner;

impl SystemProcessRunner {
    /// 读线程结束后，最多再等它这么久。
    ///
    /// **绝不无限等**：孙子进程可能继承并持有那根管道，`read_to_end` 因此永远不返回。
    /// 无限等就等于让 `detect` 挂住 —— 而这正是本票不许发生的事。
    /// 等不到就带走已经读到的部分（版本号几乎总在第一行）。
    const DRAIN_GRACE: Duration = Duration::from_millis(500);

    /// 轮询间隔。子进程退出的检测精度就靠它。
    const POLL: Duration = Duration::from_millis(10);
}

/// 一个读线程正在写进去的缓冲。
#[derive(Debug, Default)]
struct Capture {
    bytes: Mutex<Vec<u8>>,
    done: AtomicBool,
}

impl Capture {
    /// 另起一个线程把 `reader` 读干，**不 join**（理由见 `DRAIN_GRACE`）。
    fn spawn<R: Read + Send + 'static>(mut reader: R) -> Arc<Self> {
        let capture = Arc::new(Self::default());
        let sink = Arc::clone(&capture);
        std::thread::spawn(move || {
            let mut chunk = [0u8; 8192];
            while let Ok(n) = reader.read(&mut chunk) {
                if n == 0 {
                    break;
                }
                match sink.bytes.lock() {
                    Ok(mut buffer) => buffer.extend_from_slice(&chunk[..n]),
                    Err(_) => break,
                }
            }
            sink.done.store(true, Ordering::SeqCst);
        });
        capture
    }

    /// 取走已经读到的内容（原始字节）。
    ///
    /// **必须有这个原始版本。** 早先这里只返回 `String`，于是下载层拿到的是
    /// **经过 lossy UTF-8 转换**的字节：制品里任何一个非 UTF-8 序列都会被
    /// 换成 `U+FFFD`（`EF BF BD`）。一个压缩包 100% 含有这种序列，所以
    /// "把响应体从 stdout 里捞出来"这条路径在不修这里的前提下**永远算不出
    /// 正确的哈希** —— 而症状是"哈希不符"，看起来像上游被污染。
    fn finish(self: &Arc<Self>) -> Vec<u8> {
        let deadline = Instant::now() + SystemProcessRunner::DRAIN_GRACE;
        while !self.done.load(Ordering::SeqCst) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        self.bytes
            .lock()
            .map(|buffer| buffer.clone())
            .unwrap_or_default()
    }
}

impl ProcessRunner for SystemProcessRunner {
    fn run_env(
        &self,
        program: &Path,
        args: &[&str],
        env: &[(String, String)],
        timeout: Duration,
    ) -> ProcessOutcome {
        let mut command = Command::new(program);
        command
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        // 空 `env` 时这一圈什么都不做 —— 于是 `run` 的行为与它存在之前**逐字节相同**
        // （`Command` 默认继承父进程的环境块）。
        for (name, value) in env {
            command.env(name, value);
        }
        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(err) => return ProcessOutcome::not_spawned(err.to_string()),
        };

        let stdout = child.stdout.take().map(Capture::spawn);
        let stderr = child.stderr.take().map(Capture::spawn);

        let deadline = Instant::now() + timeout;
        let mut timed_out = false;
        let mut exit_code = None;
        loop {
            match child.try_wait() {
                Ok(Some(status)) => {
                    exit_code = status.code();
                    break;
                }
                Ok(None) => {
                    if Instant::now() >= deadline {
                        timed_out = true;
                        // 先杀再收尸：不 wait 会留下僵尸进程，而 detect 会跑很多次。
                        let _ = child.kill();
                        let _ = child.wait();
                        break;
                    }
                    std::thread::sleep(Self::POLL);
                }
                Err(_) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    break;
                }
            }
        }

        let stdout_bytes = stdout.map_or_else(Vec::new, |capture| capture.finish());
        let stderr_bytes = stderr.map_or_else(Vec::new, |capture| capture.finish());

        ProcessOutcome {
            spawned: true,
            timed_out,
            exit_code,
            // **stdout 保留原始字节**：下载层要的是字节，而 lossy 转换
            // 会破坏制品（见 `stdout_bytes` 的文档）。
            stdout: String::from_utf8_lossy(&stdout_bytes).into_owned(),
            stdout_bytes,
            stderr: String::from_utf8_lossy(&stderr_bytes).into_owned(),
            spawn_error: None,
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// 继承 stdio 的那一种进程调用
// ─────────────────────────────────────────────────────────────────────────────

/// 继承 stdio 跑一个子进程，等它结束，返回退出码。
///
/// # 它和 [`ProcessRunner::run`] 的区别（也是它存在的理由）
///
/// | | [`ProcessRunner::run`] | `spawn_inherit` |
/// |---|---|---|
/// | 子进程的 stdout / stderr | **捕获**成字符串与字节 | 原样继承我们的控制台 |
/// | 子进程的 stdin | 接 NUL 设备（问问题的工具不能挂住 `detect`） | 原样继承 |
/// | 超时 | **有**（到点就杀） | **没有** |
///
/// 交互式 shell 必须拿到**真正的控制台**：颜色、Ctrl-C、全屏 TUI（`vim`、`top`）
/// 全靠它。一旦把它的 stdout 接进一根管道，它就不再是一个交互式 shell 了 ——
/// `cmd.exe` 会关掉行编辑与回显，Ctrl-C 也送不到它手上。
///
/// 而"超时杀掉"对用户正在用的 shell 是**错的**：一个开了四小时的构建会话不是故障。
/// 把超时留在这一层之外，代价是调用方**不能**拿它跑自己控制不了的第三方程序 ——
/// 那是 [`ProcessRunner`] 的活。
///
/// # 一次调用只起一个子进程
///
/// 本函数**不 fork 自己、不重试、不 daemonize**（`AGENTS.md` 铁律 1：绝不允许任何
/// 程序启动它自己 —— 那条规矩是两次真实事故换来的）。
///
/// `cmd.exe` 自己去起的孙子进程由 `cmd.exe` 管；我们只等**直接子进程**退出，
/// 然后把它的退出码**原样**返回（与 `crates/shim` 的转发器同一个口径：
/// `DWORD` 按 `as i32` 保留全部 32 位，所以被 Ctrl-C 打断的 `0xC000013A`
/// 也能一位不差地传出去）。
///
/// # 命令行怎么拼
///
/// `program` 与 `args` 原样交给 [`std::process::Command`]：引号与转义**只由标准库**
/// 处理，调用方**拿不到**一个半成品命令行（`AGENTS.md` 铁律 2）。本函数因此
/// **不接受**一个命令行字符串 —— `cmd.exe /C "a && b"` 那种写法里，整条
/// `"a && b"` 是**一个** argv 元素，由 `cmd.exe` 自己去解析，我们不碰。
///
/// # 环境块
///
/// `env` 是覆盖/新增，`remove` 是删除，两者都作用在**继承来的环境块**上
/// （本函数**不** `env_clear`）。调用方要给 `PATH` 设值时，`Path` 与 `PATH`
/// **两个拼法都要设**：Windows 的环境变量查找不区分大小写，但 Rust 的
/// [`std::process::Command`] 是按 `OsString` 存的，不设的那一个会留着父进程的原值
/// （本仓库既有做法见 `scripts/acceptance-L1-12.ps1`）。
///
/// # 已知限制：Ctrl-C 会同时打到我们身上
///
/// 控制台里的 Ctrl-C 是发给**整个进程组**的，所以 `tuoen` 自己也会收到。本函数
/// 没有装控制台处理器（那是 `crates/shim` 的转发器做的事，见决策 56），
/// 于是最坏情况下 `tuoen` 先退出、子进程继续跑，退出码就传不回来了。
/// 这条限制**写在这里而不是被忘掉**：它不影响 `--exec` 那条非交互路径，
/// 也不影响"子进程正常退出"这条主路径。
///
/// # 非 Windows
///
/// 返回 [`PlatformError::Unsupported`]（`what = "spawn_inherit"`）。
/// 本项目的产品目标是 Windows，交互式 shell 的构造没有跨平台语义。
pub fn spawn_inherit(
    program: &Path,
    args: &[&str],
    raw_args: &[&str],
    env: &[(String, String)],
    remove: &[String],
    cwd: Option<&Path>,
) -> Result<i32, PlatformError> {
    #[cfg(not(windows))]
    {
        // 绑定一次，避免"未使用参数"的告警，同时让这段代码在任何平台上都编译得过。
        let _ = (program, args, raw_args, env, remove, cwd);
        Err(PlatformError::Unsupported {
            what: "spawn_inherit".to_owned(),
        })
    }

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;

        let mut command = Command::new(program);
        command.args(args);
        // `raw_args` 原样拼在最后，**不经过 `Command` 的转义**。
        //
        // 为什么必须有这一条：`cmd.exe /C` 的解析规则**不是** MSVCRT 的规则。
        // `Command::arg` 会把含引号的参数转义成 `\"`，而 `cmd.exe` 不认这种转义 ——
        // 它看到的是字面的反斜杠，于是 `--exec 'node -e "console.log(1)"'` 被拆坏
        // （真机实测：输出为空、退出码 0，**一句看起来完全合理的成功**）。
        // 交给 `cmd.exe` 自己的解析器，引号、`&`、`|`、`%VAR%` 展开才是它本来的语义。
        for raw in raw_args {
            command.raw_arg(raw);
        }
        if let Some(dir) = cwd {
            command.current_dir(dir);
        }
        for (name, value) in env {
            command.env(name, value);
        }
        for name in remove {
            command.env_remove(name);
        }
        // stdin / stdout / stderr 全部**不配置**：`Command` 的默认就是继承。
        // 这里刻意不写 `Stdio::inherit()` —— 写了会让人以为还有别的选项，
        // 而本函数存在的全部理由就是"这一条路径上没有别的选项"。

        let mut child = command
            .spawn()
            .map_err(|error| spawn_failure(program, &error))?;
        let status = child
            .wait()
            .map_err(|error| spawn_failure(program, &error))?;

        // **Windows 上 `ExitStatus::code()` 永远有值**：它就是 `GetExitCodeProcess`
        // 拿回来的那个 `DWORD`。所以这里不写 `unwrap_or(0)` —— 那会在一条
        // （在 Windows 上）不可达的分支上把"进程没有退出码"说成"成功"，
        // 而"一句看起来完全合理的错话"正是本仓库最不能出现的东西。
        status.code().ok_or_else(|| PlatformError::Unsupported {
            what: format!(
                "spawn_inherit：子进程 `{}` 结束时没有退出码",
                program.display()
            ),
        })
    }
}

/// 把 `Command` 的 `io::Error` 归一化成一个 [`PlatformError`]。
///
/// **分类只依据 Win32 码，不依据错误文本**（`error.rs` 的模块文档）：
/// 本机是 zh-CN，`ERROR_FILE_NOT_FOUND` 的消息是中文的，按文本分支换台机器就静默失效。
///
/// `raw_os_error()` 拿不到码时落进 [`PlatformError::Unsupported`] ——
/// `PlatformError` 只有四个变体，另外三个各需要一个**真实的** Win32 码
/// （`Win32 { code: 0 }` 会印出"Win32 0"，那是 `ERROR_SUCCESS`，一句假话）。
#[cfg(windows)]
fn spawn_failure(program: &Path, error: &std::io::Error) -> PlatformError {
    match error.raw_os_error() {
        // `raw_os_error()` 给的是 `i32`，而 Win32 码是 `u32` —— `as u32` **保留全部
        // 32 位**（`0xC000013A` 这类高位码在 `i32` 里是负数，转换必须无损）。
        Some(code) => PlatformError::from_win32(code as u32, program.display().to_string()),
        None => PlatformError::Unsupported {
            what: format!("spawn_inherit：{}：{error}", program.display()),
        },
    }
}

/// 版本探测用的默认超时。
///
/// 取值理由：本机实测 `java -version` / `node -v` / `git --version` 都在 300ms 内返回
/// （JVM 最慢）。3 秒对"慢机器上的 JVM 冷启动"足够宽松，而一个真的挂住的工具
/// （等输入的安装器、卡住的网络盘上的可执行文件）会在这之后就放弃，
/// 不会让整次 detect 变成分钟级。
pub const DEFAULT_PROBE_TIMEOUT: Duration = Duration::from_secs(3);

#[cfg(test)]
mod tests {
    use super::*;

    /// 一个**真的不会自己返回**的进程。
    ///
    /// 用 `ping 127.0.0.1`（回环，不走真实网络）：Windows 上它按秒计时、稳定长时间运行。
    /// `-n 6` 把"万一没杀掉"的最坏情况限制在约 5 秒。
    fn a_process_that_does_not_return() -> Option<std::path::PathBuf> {
        let root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".to_owned());
        let ping = std::path::Path::new(&root)
            .join("System32")
            .join("ping.exe");
        ping.exists().then_some(ping)
    }

    #[test]
    fn stdout_and_stderr_are_both_captured() {
        // `cmd /c "echo out& echo err 1>&2"` —— 一路到 stdout，一路到 stderr。
        let runner = SystemProcessRunner;
        let outcome = runner.run(
            Path::new(r"C:\Windows\System32\cmd.exe"),
            &["/d", "/c", "echo out& echo err 1>&2"],
            Duration::from_secs(10),
        );
        if !outcome.spawned {
            panic!("cmd.exe 必须能起来：{outcome:?}");
        }
        assert!(!outcome.timed_out);
        assert!(outcome.stdout.contains("out"), "{outcome:?}");
        assert!(outcome.stderr.contains("err"), "{outcome:?}");
        assert!(outcome.combined().contains("out") && outcome.combined().contains("err"));
        assert_eq!(outcome.exit_code, Some(0));
    }

    /// **二进制 stdout 必须原样保留。**
    ///
    /// 这条测试的存在理由是一次真实的失败：下载层把 curl 的 stdout 当作
    /// 文本取字节，于是 `0xFF` 变成 `U+FFFD`（`EF BF BD`）—— 三个字节
    /// 变成一个替换字符。压缩包里 100% 含有这种序列，所以症状是
    /// **每一个制品都"哈希不符"**，而排查方向会指向网络与镜像。
    ///
    /// 用 `cmd /c` 写出一个非 UTF-8 字节序列来复现：`0xFF 0xFE 0x00 0x80`。
    #[test]
    fn binary_stdout_bytes_survive_intact() {
        let runner = SystemProcessRunner;
        // PowerShell 会把字节写坏，所以用 cmd + `type` 一个临时文件。
        let temp = std::env::temp_dir().join(format!(
            "tuoen-binary-stdout-{}-{}.bin",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let payload: Vec<u8> = vec![0x00, 0x01, 0x7f, 0x80, 0xfe, 0xff, 0x00, 0xff];
        std::fs::write(&temp, &payload).expect("写二进制样本");

        let outcome = runner.run(
            Path::new(r"C:\Windows\System32\cmd.exe"),
            &["/d", "/c", "type", temp.to_str().expect("临时路径是 UTF-8")],
            Duration::from_secs(10),
        );
        let _ = std::fs::remove_file(&temp);

        assert!(outcome.spawned, "{outcome:?}");
        assert_eq!(
            outcome.stdout_bytes, payload,
            "原始字节必须一个不差地留下（`stdout` 的 lossy 文本不算）"
        );

        // 反面对照：文本形式**确实**被破坏了 —— 这就是为什么必须有
        // `stdout_bytes`，而不是"顺手用 stdout 也行"。
        assert_ne!(
            outcome.stdout.as_bytes(),
            payload.as_slice(),
            "文本形式必然做不到无损（否则这个字段是多余的）"
        );
        assert!(
            outcome.stdout.contains('\u{fffd}'),
            "非 UTF-8 字节应当变成替换字符：{:?}",
            outcome.stdout
        );
    }

    #[test]
    fn a_missing_program_is_reported_not_panicked() {
        let outcome = SystemProcessRunner.run(
            Path::new(r"C:\tuoen-does-not-exist-9f3a\nope.exe"),
            &["--version"],
            Duration::from_millis(200),
        );
        assert!(!outcome.spawned);
        assert!(outcome.spawn_error.is_some());
        assert_eq!(outcome.exit_code, None);
    }

    #[test]
    fn a_nonzero_exit_code_is_preserved() {
        let runner = SystemProcessRunner;
        let outcome = runner.run(
            Path::new(r"C:\Windows\System32\cmd.exe"),
            &["/d", "/c", "exit 3"],
            Duration::from_secs(10),
        );
        assert_eq!(outcome.exit_code, Some(3));
    }

    #[test]
    fn a_tool_that_never_returns_is_killed_and_reported_as_timed_out() {
        let Some(ping) = a_process_that_does_not_return() else {
            // 没有 ping.exe 的机器上不假装测过 —— 但这台机器上有。
            panic!("本机必须存在 System32\\ping.exe 才能验证超时");
        };
        let timeout = Duration::from_millis(700);
        let started = Instant::now();
        let outcome = SystemProcessRunner.run(&ping, &["-n", "6", "127.0.0.1"], timeout);
        let elapsed = started.elapsed();

        assert!(outcome.spawned, "{outcome:?}");
        assert!(outcome.timed_out, "必须在超时后放弃：{outcome:?}");
        assert_eq!(outcome.exit_code, None, "被终止的进程没有退出码");
        // **这是本票的要求：不挂住。** 超时 + 收尾必须有界。
        assert!(
            elapsed < timeout + Duration::from_secs(3),
            "超时后必须在有界时间内返回，实际耗时 {elapsed:?}"
        );
    }

    #[test]
    fn a_fast_tool_does_not_wait_for_the_full_timeout() {
        let started = Instant::now();
        let outcome = SystemProcessRunner.run(
            Path::new(r"C:\Windows\System32\cmd.exe"),
            &["/d", "/c", "ver"],
            Duration::from_secs(30),
        );
        assert!(!outcome.timed_out);
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "正常退出的进程不该等满超时"
        );
    }
}
