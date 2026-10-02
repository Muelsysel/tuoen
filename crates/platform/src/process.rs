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
pub trait ProcessRunner {
    /// 跑 `program args`，最多等 `timeout`。
    ///
    /// **实现必须保证**：无论子进程做什么，本调用都会在 `timeout` 之后的一个有界时间内返回。
    fn run(&self, program: &Path, args: &[&str], timeout: Duration) -> ProcessOutcome;
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
    fn run(&self, program: &Path, args: &[&str], timeout: Duration) -> ProcessOutcome {
        let mut command = Command::new(program);
        command
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
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
