//! 传输层：把一个 URL 取成字节流。
//!
//! ## 为什么是 shell out 到系统 `curl.exe`
//!
//! 见 `docs/DESIGN.md` 决策 39。三条理由，按重要性排：
//!
//! 1. **Schannel 直接用 Windows 证书存储 → 企业 TLS 拦截的根证书自动被信任。**
//!    打包 rustls + webpki-roots 在内网里**必然连不上** —— 而"能在企业网络里用"
//!    是这个项目明确的目标场景（决策 34 提到企业内网镜像）。
//! 2. **Windows 10 1803+ 自带 `curl 8.21.0`**，Schannel 后端、HTTP/2、
//!    `--location` / `--connect-timeout` / `--proxy` 全都支持。
//! 3. **项目已经信任系统 `tar.exe` 做解压**（见 README 的"平台现实"），
//!    同一条理由：与其打包一个必然在某些机器上装错版本的东西，不如用系统那个
//!    已经跟着 OS 打过补丁的。
//!
//! 代价是诚实的：每次下载起一个进程（数十毫秒，相对网络传输可忽略），
//! 且我们把重定向/重试策略交给 curl 而不是自己控制。**但错误信息反而更好** ——
//! curl 的退出码（6 DNS / 7 连不上 / 28 超时 / 35 TLS / 56 收数据失败）
//! 比"连接失败"具体得多。
//!
//! ## 测试怎么不碰真实网络
//!
//! [`HttpTransport`] 建在 [`ProcessRunner`] 之上，所以：
//!
//! - **传输层的语义测试**用 [`FakeTransport`]（不碰任何东西）；
//! - **"curl 真的能取回字节"**这件事由 `tests/loopback.rs` 里的
//!   **环回 HTTP 服务器**（`127.0.0.1`）覆盖 —— 它走完整的 curl 路径
//!   （真实 HTTP 请求、真实 TLS 之外的每一层），但**不出本机**；
//! - 仍然不碰公网。
//!
//! [`ProcessRunner`]: tuoen_platform::ProcessRunner

use std::path::{Path, PathBuf};
use std::time::Duration;

use tuoen_platform::{ProcessRunner, SystemProcessRunner};

use crate::error::DownloadError;

/// 一次取字节的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchOutcome {
    /// 取回来的字节。
    pub bytes: Vec<u8>,
    /// 实际服务这些字节的 URL。
    ///
    /// **可能不同于请求的 URL**：curl 跟了重定向。把它记下来是因为
    /// "Node 的官方地址把你转到了别处"是一条用户需要知道的事实
    /// （而 `--location-trusted` 是另一回事，我们不跟跨主机凭据）。
    pub final_url: Option<String>,
    /// HTTP 状态码。
    ///
    /// **探测需要它，而不是需要一个假定值。** 一个不认区间请求的服务器
    /// 会把 `-r 0-0` 当成普通 GET，回 **200 + 整个文件** —— 那正是区间
    /// 请求存在要避免的事（Temurin JDK 约 190 MB）。如果探测把状态码
    /// 写死成 `206`，这种源看起来完全正常，而账单是用户的时间和流量。
    ///
    /// `None` 只在 curl 没吐出 `-w` 的产物时出现（那种情况下 `fetch`
    /// 早就因为退出码非零而返回 `Err` 了）。
    pub status: Option<u16>,
    /// 这次用了多久。
    pub elapsed: Duration,
}

/// 请求一个**字节区间**。
///
/// 存在的唯一理由是探测（[`crate::source::probe_sources`]）：问一个源
/// "你到得了吗"时，绝不能顺手把 190 MB 的 JDK 下下来。
///
/// 为什么不直接用 `HEAD`：本机实测有些镜像对 `HEAD` 回 405 而 `GET` 正常
/// （见 `scripts/probe-mirrors.ps1` 的注释），用 `HEAD` 会把能用的源淘汰掉。
/// `HEAD` 也不像 `GET` 那样会走缓存与 CDN 的真实路径。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ByteRange {
    /// 只要第一个字节（`-r 0-0`）。
    ///
    /// 一个字节足够回答"这个源到不到得了"：真正会失败的东西
    /// （DNS、TLS、403、404、超时）在第一个字节之前就已经失败了。
    FirstByte,
}

impl ByteRange {
    /// curl 的参数（`None` 表示不加）。
    #[must_use]
    const fn curl_args(self) -> [&'static str; 2] {
        match self {
            Self::FirstByte => ["-r", "0-0"],
        }
    }
}

/// 把一个 URL 取成字节。
pub trait Transport {
    /// 取 `url`。`timeout` 是**整个请求**的上限，不是每步的上限。
    ///
    /// # Errors
    ///
    /// 网络不可达、超时、非 2xx、TLS 失败都会返回 [`DownloadError`]。
    /// **实现必须在 `timeout` 之后的有界时间内返回**（同 [`ProcessRunner`] 的契约）。
    ///
    /// `range` 是 `Some` 时只取那段字节。实现**必须**真的把它变成一次
    /// 区间请求（而不是"取回来再切"）—— 否则探测会把整包下下来。
    fn fetch(
        &self,
        url: &str,
        range: Option<ByteRange>,
        timeout: Duration,
    ) -> Result<FetchOutcome, DownloadError>;
}

/// 真实实现：`curl.exe`。
pub struct HttpTransport {
    /// `curl.exe` 的路径（找不到时是 `None`，调用时才报错 —— 这样
    /// "curl 不存在"不会让整个程序起不来）。
    program: Option<PathBuf>,
    /// 跑进程的东西。**注入在这里，所以测试可以塞一个假的。**
    runner: Box<dyn ProcessRunner + Send + Sync>,
    /// 额外的 curl 参数（代理、`--cacert`、企业配置）。
    extra_args: Vec<String>,
}

impl std::fmt::Debug for HttpTransport {
    /// 手写 `Debug` 而不是 derive：`runner` 是 `dyn ProcessRunner`，
    /// 它不实现 `Debug`，而且**也不该实现** —— 打印一个跑进程的策略
    /// 除了说"这里有个东西"之外没有信息量。
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("HttpTransport")
            .field("program", &self.program)
            .field("extra_args", &self.extra_args)
            .field("runner", &"<注入的 ProcessRunner>")
            .finish()
    }
}

impl Default for HttpTransport {
    fn default() -> Self {
        Self::probe()
    }
}

impl HttpTransport {
    /// 找到系统 `curl.exe`。
    ///
    /// **先找 `%SystemRoot%\System32`，再退回 `PATH`。** 顺序有意义：
    /// `PATH` 上可能有一个用户装的、版本更老或被人替换过的 `curl.exe`，
    /// 而我们要的是**跟着 OS 打补丁的那个**。
    #[must_use]
    pub fn probe() -> Self {
        Self {
            program: locate_curl(),
            runner: Box::new(SystemProcessRunner),
            extra_args: Vec::new(),
        }
    }

    /// 换一个跑进程的实现（测试用）。
    #[must_use]
    pub fn with_runner(mut self, runner: Box<dyn ProcessRunner + Send + Sync>) -> Self {
        self.runner = runner;
        self
    }

    /// 指定 curl 的路径（测试用；也让用户能覆盖一个坏掉的系统 curl）。
    #[must_use]
    pub fn with_program(mut self, program: impl Into<PathBuf>) -> Self {
        self.program = Some(program.into());
        self
    }

    /// 加额外的 curl 参数。
    #[must_use]
    pub fn with_extra_args(mut self, args: impl IntoIterator<Item = String>) -> Self {
        self.extra_args = args.into_iter().collect();
        self
    }

    /// `curl.exe` 在哪（`None` 表示没找到）。
    #[must_use]
    pub fn program(&self) -> Option<&Path> {
        self.program.as_deref()
    }

    /// 构造 curl 的命令行。**抽出来单独测** —— 参数顺序错了 curl 会静默做错事。
    ///
    /// 参数逐条都有理由：
    ///
    /// - `-sS` —— 安静但**保留错误**。`-s` 单独用会让 404 变成"静默成功"。
    /// - `-L` —— 跟重定向（Node 与 Adoptium 都靠它）。
    /// - `--fail-with-body` —— 非 2xx 时**仍然写 body 但退出码非零**。
    ///   我们要的是"非 2xx 就算失败"，而 `--fail` 会让 curl 自己决定
    ///   什么时候算失败（它对某些 4xx 有例外）。
    /// - `--connect-timeout` —— **必须在"连不上"上快速失败**，否则一个
    ///   黑洞 IP 会让整个安装卡到超时上限。
    /// - `--max-time` —— 整个请求的上限。
    /// - `--retry 0` —— **不要 curl 自己重试**。重试策略在我们这一层
    ///   （换源比重试同一个源更可能成功，而且我们要记录每一次尝试）。
    /// - `-o -` —— 写到 stdout，我们直接读字节。
    /// - `-w` —— 把最终 URL 与耗时写到 stderr（见 [`Self::write_out`]）。
    /// - `-r 0-0` —— **只在探测时加**。见 [`ByteRange`]。
    #[must_use]
    pub fn curl_args(&self, url: &str, range: Option<ByteRange>, timeout: Duration) -> Vec<String> {
        let connect = timeout.as_secs().clamp(1, 30);
        let mut args = vec![
            "-sS".to_owned(),
            "-L".to_owned(),
            "--fail-with-body".to_owned(),
            "--retry".to_owned(),
            "0".to_owned(),
            "--connect-timeout".to_owned(),
            connect.to_string(),
            "--max-time".to_owned(),
            timeout.as_secs().max(1).to_string(),
        ];
        if let Some(range) = range {
            args.extend(range.curl_args().into_iter().map(str::to_owned));
        }
        args.extend([
            "-o".to_owned(),
            "-".to_owned(),
            "-w".to_owned(),
            Self::write_out().to_owned(),
        ]);
        args.extend(self.extra_args.iter().cloned());
        args.push(url.to_owned());
        args
    }

    /// `-w` 的格式串。
    ///
    /// **写在一个不容易被 URL 影响的位置。** `-w` 的内容被追加到响应体之后，
    /// 而响应体是任意字节 —— 所以解析时必须**从后往前找**那个哨兵
    /// （见 [`Self::split_write_out`]），不能靠"最后一行"。
    const fn write_out() -> &'static str {
        // 哨兵用两个 0x1f（单元分隔符）：它在文本里几乎不可能自然出现，
        // 也不会被 curl 自身的输出插进来。
        "\n\u{1f}\u{1f}%{http_code} %{url_effective} %{time_total}"
    }

    /// 把 curl 的 stdout 拆成"响应体"与"`-w` 写出来的那三个字段"。
    ///
    /// **从后往前找哨兵**，因为响应体是任意字节，可能包含任何东西
    /// （包括看起来像哨兵的行）。只有**最后**一个哨兵是 curl 写的。
    #[must_use]
    pub fn split_write_out(stdout: &[u8]) -> (Vec<u8>, Option<u16>, Option<String>, Option<f64>) {
        const SENTINEL: &[u8] = b"\n\x1f\x1f";
        let Some(position) = find_last(stdout, SENTINEL) else {
            // 没有哨兵：要么是 curl 的旧版本，要么是我们参数写错了。
            // **不要把整个 stdout 当 body** —— 那会把诊断信息当成制品。
            return (stdout.to_vec(), None, None, None);
        };
        let body = stdout[..position].to_vec();
        let tail = String::from_utf8_lossy(&stdout[position + SENTINEL.len()..]).into_owned();
        let mut parts = tail.split_whitespace();
        let status = parts.next().and_then(|text| text.parse::<u16>().ok());
        let final_url = parts.next().map(ToOwned::to_owned);
        let elapsed = parts.next().and_then(|text| text.parse::<f64>().ok());
        (body, status, final_url, elapsed)
    }
}

/// 在一段字节里找最后一次出现的 `needle`。
fn find_last(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    // 从后往前扫：只可能有一个哨兵，但**必须**取最后一个。
    (0..=haystack.len() - needle.len())
        .rev()
        .find(|start| &haystack[*start..*start + needle.len()] == needle)
}

/// 找系统 `curl.exe`。
///
/// **`%SystemRoot%\System32\curl.exe` 优先于 `PATH`。** `PATH` 上可能有
/// 用户装的、版本更老或被人替换过的 `curl.exe`。
#[must_use]
pub fn locate_curl() -> Option<PathBuf> {
    if let Some(root) = std::env::var_os("SystemRoot") {
        let candidate = Path::new(&root).join("System32").join("curl.exe");
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    // 退回 `PATH` 查找（不调外部命令，只看目录项）。
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join("curl.exe"))
        .find(|candidate| candidate.is_file())
}

impl Transport for HttpTransport {
    fn fetch(
        &self,
        url: &str,
        range: Option<ByteRange>,
        timeout: Duration,
    ) -> Result<FetchOutcome, DownloadError> {
        let Some(program) = &self.program else {
            return Err(DownloadError::Transport {
                url: url.to_owned(),
                code: -1,
                stderr: curl_missing_advice().to_owned(),
            });
        };

        let args = self.curl_args(url, range, timeout);
        let borrowed: Vec<&str> = args.iter().map(String::as_str).collect();
        // **自己掐表，不问 `ProcessOutcome`。** 那个结构体刻意不带耗时字段：
        // 它由 `ProcessRunner` 的实现填，而不同的实现（真进程、假进程）
        // 报出来的时间不可比。真正可比的只有"这一层观察到的时间"。
        let started = std::time::Instant::now();
        let outcome = self
            .runner
            .run(program, &borrowed, timeout + Duration::from_secs(5));
        let observed = started.elapsed();

        if !outcome.spawned {
            return Err(DownloadError::Transport {
                url: url.to_owned(),
                code: -1,
                stderr: outcome
                    .spawn_error
                    .unwrap_or_else(|| "curl 没能启动".to_owned()),
            });
        }
        if outcome.timed_out {
            return Err(DownloadError::Transport {
                url: url.to_owned(),
                code: 28,
                stderr: format!("超过 {} 秒仍未完成，已终止", timeout.as_secs()),
            });
        }

        // **用原始字节，不用 `outcome.stdout`。**
        //
        // `stdout` 是 lossy UTF-8 转换过的文本：一个压缩包里一定有非 UTF-8
        // 序列，它们会被换成 `U+FFFD`（三个字节换一个字符），于是**每一个
        // 制品都会"哈希不符"** —— 而那个症状看起来像上游被污染。
        // 这条路径由 `tests/loopback.rs` 上的环回服务器守着（它发任意字节）。
        let (body, status, final_url, elapsed) = Self::split_write_out(&outcome.stdout_bytes);

        // **先看 HTTP 码，再看退出码。**
        //
        // 理由是 `-w` 的产物与 curl 的退出码谁更具体：非 2xx（403 / 404）
        // 会同时给出"码 22"（`--fail-with-body` 的退出码）与"HTTP 404"。
        // 前者对所有 4xx/5xx 都是同一个数字，后者才是用户要的那个。
        //
        // 顺序反过来就看不见具体码了 —— 早先的实现就是这样，于是
        // **所有镜像的 404 都被报成"curl 退出码 22"**，`DownloadError::Http`
        // 那条分支永远走不到。
        //
        // 但"连不上"没有 HTTP 码（`-w` 只给出 `000`），所以那种情况
        // 仍然由退出码兜住 —— 两者都要，只是先后不同。
        if let Some(status) = status
            && status != 0
            && !(200..300).contains(&status)
        {
            return Err(DownloadError::Http {
                url: url.to_owned(),
                status,
            });
        }

        let exit_code = outcome.exit_code.unwrap_or(-1);
        if exit_code != 0 {
            return Err(DownloadError::Transport {
                url: url.to_owned(),
                code: exit_code,
                stderr: truncate(&outcome.stderr, 400),
            });
        }

        Ok(FetchOutcome {
            bytes: body,
            final_url,
            status,
            // `-w` 报的 `%{time_total}` 是**服务器视角**的传输时间，
            // 它不含进程启动与 Schannel 握手前的开销。两者取其一即可，
            // 但 `-w` 的值更能反映"这个源快不快"，所以优先它。
            elapsed: elapsed.map(Duration::from_secs_f64).unwrap_or(observed),
        })
    }
}

/// `curl.exe` 找不到时给用户的话。
///
/// **这条路径在 Windows 10 1803+ 上不该发生**，所以它一旦出现，说明用户
/// 在一个被裁剪过的镜像上（Windows Server Core、精简过的 LTSC 映像、
/// 或者有人从 `System32` 删了它）。给的可操作建议必须是真的可操作的。
#[must_use]
pub fn curl_missing_advice() -> &'static str {
    "找不到 curl.exe。Windows 10 1803+ 自带它；如果你在一个裁剪过的系统上，\
     装一个 curl 并把它放到 PATH 上，或者用 --offline 指定本地归档文件"
}

/// 截断一段文本，并标明被截断了。
fn truncate(text: &str, limit: usize) -> String {
    let trimmed = text.trim();
    if trimmed.chars().count() <= limit {
        return trimmed.to_owned();
    }
    let kept: String = trimmed.chars().take(limit).collect();
    format!("{kept}…（还有更多，已截断）")
}

// ─────────────────────────────────────────────────────────────────────────────
// 假传输层
// ─────────────────────────────────────────────────────────────────────────────

/// 测试用的假传输层：按 URL 前缀返回预置的字节或失败。
///
/// **它记下每一次尝试**，所以"镜像失败后回退到下一个源"可以被断言
/// （而不是靠猜）。
#[derive(Debug, Clone, Default)]
pub struct FakeTransport {
    routes: Vec<FakeRoute>,
    attempts: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
}

/// 一条假路由。
#[derive(Debug, Clone)]
pub struct FakeRoute {
    /// URL 前缀。
    pub prefix: String,
    /// 要么给字节，要么给一个失败。
    pub outcome: FakeOutcome,
}

/// 一条假路由的结果。
#[derive(Debug, Clone)]
pub enum FakeOutcome {
    /// 返回这些字节。
    Bytes(Vec<u8>),
    /// 返回**整个**正文并回 `200`，**无视区间请求**。
    ///
    /// 这是真实世界里会遇到的服务器（老式静态服务器、某些反代）。
    /// 它之所以要有自己的变体：探测打到这种源上会把整包下下来，
    /// 而"探测只取一个字节"的保证在这里破了 —— 只有能造出这种源，
    /// 才测得出我们发现它没有（见 `crate::source` 的探测报告）。
    IgnoresRange(Vec<u8>),
    /// 返回 HTTP 状态码。
    Status(u16),
    /// 传输层失败（模拟"连不上"）。
    Transport {
        /// 假的 curl 退出码。
        code: i32,
        /// 假的 stderr。
        stderr: String,
    },
}

impl FakeTransport {
    /// 空。
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// 加一条路由。
    #[must_use]
    pub fn route(mut self, prefix: &str, outcome: FakeOutcome) -> Self {
        self.routes.push(FakeRoute {
            prefix: prefix.to_owned(),
            outcome,
        });
        self
    }

    /// 加一条"返回这些字节"的路由。
    #[must_use]
    pub fn serves(self, prefix: &str, bytes: impl Into<Vec<u8>>) -> Self {
        self.route(prefix, FakeOutcome::Bytes(bytes.into()))
    }

    /// 加一条"返回这个 HTTP 码"的路由。
    #[must_use]
    pub fn fails_with_status(self, prefix: &str, status: u16) -> Self {
        self.route(prefix, FakeOutcome::Status(status))
    }

    /// 加一条"传输层失败"的路由。
    #[must_use]
    pub fn unreachable(self, prefix: &str) -> Self {
        self.route(
            prefix,
            FakeOutcome::Transport {
                code: 7,
                stderr: "Failed to connect to host".to_owned(),
            },
        )
    }

    /// 被尝试过的 URL，按顺序。**回退测试靠它。**
    #[must_use]
    pub fn attempts(&self) -> Vec<String> {
        self.attempts
            .lock()
            .map(|guard| guard.clone())
            .unwrap_or_default()
    }
}

impl Transport for FakeTransport {
    fn fetch(
        &self,
        url: &str,
        range: Option<ByteRange>,
        _timeout: Duration,
    ) -> Result<FetchOutcome, DownloadError> {
        // 记下请求的 URL**以及它是不是探测**，这样"探测没有下整包"
        // 可以被断言（而不是靠读代码相信）。
        if let Ok(mut guard) = self.attempts.lock() {
            guard.push(match range {
                Some(ByteRange::FirstByte) => format!("{url} #range"),
                None => url.to_owned(),
            });
        }
        // 最长前缀优先，这样 `https://a/b/` 能覆盖 `https://a/`。
        let matched = self
            .routes
            .iter()
            .filter(|route| url.starts_with(&route.prefix))
            .max_by_key(|route| route.prefix.len());

        match matched.map(|route| &route.outcome) {
            Some(FakeOutcome::Bytes(bytes)) => {
                // **假传输层也尊重区间。** 一个不尊重区间的假传输层会让
                // "探测不会下整包"这条断言变成谎言。
                let served = match range {
                    Some(ByteRange::FirstByte) => bytes.get(..1).unwrap_or_default().to_vec(),
                    None => bytes.clone(),
                };
                Ok(FetchOutcome {
                    bytes: served,
                    final_url: Some(url.to_owned()),
                    // 假传输层如实回 206：它**确实**尊重了区间请求
                    // （见上面那段）。返回 200 会让"探测有没有下整包"
                    // 这条断言失效。
                    status: Some(if range.is_some() { 206 } else { 200 }),
                    elapsed: Duration::from_millis(1),
                })
            }
            Some(FakeOutcome::IgnoresRange(bytes)) => Ok(FetchOutcome {
                // **无视区间**：整包发回去，状态码 200。
                bytes: bytes.clone(),
                final_url: Some(url.to_owned()),
                status: Some(200),
                elapsed: Duration::from_millis(1),
            }),
            Some(FakeOutcome::Status(status)) => Err(DownloadError::Http {
                url: url.to_owned(),
                status: *status,
            }),
            Some(FakeOutcome::Transport { code, stderr }) => Err(DownloadError::Transport {
                url: url.to_owned(),
                code: *code,
                stderr: stderr.clone(),
            }),
            // **没配路由 = 不可达**，不是"返回空"。
            // 返回空会让"忘了配路由"变成一个静默的成功下载。
            None => Err(DownloadError::Transport {
                url: url.to_owned(),
                code: 7,
                stderr: "假传输层没有为这个 URL 配置路由".to_owned(),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn curl_args_are_ordered_and_do_not_retry_behind_our_back() {
        let transport = HttpTransport::probe();
        let args = transport.curl_args("https://example.com/x.zip", None, Duration::from_secs(60));

        // URL 必须是**最后一个**参数，否则 curl 会把它当选项值。
        assert_eq!(
            args.last().map(String::as_str),
            Some("https://example.com/x.zip")
        );
        // **curl 不许自己重试**：重试策略在我们这一层（换源比重试更可能成功）。
        let retry = args.iter().position(|a| a == "--retry").expect("--retry");
        assert_eq!(args[retry + 1], "0");
        // `-sS`：安静但保留错误。`-s` 单独用会让 404 变成静默成功。
        assert!(args.contains(&"-sS".to_owned()));
        // 必须有连接超时，否则黑洞 IP 会卡到总超时。
        assert!(args.contains(&"--connect-timeout".to_owned()));
        assert!(args.contains(&"--max-time".to_owned()));
        // 非 2xx 算失败，且仍然带回 body。
        assert!(args.contains(&"--fail-with-body".to_owned()));
    }

    #[test]
    fn write_out_is_split_from_the_back_so_a_body_containing_the_sentinel_survives() {
        // **响应体是任意字节**，可能包含看起来像哨兵的东西。
        // 只有**最后**一个哨兵是 curl 写的，所以必须从后往前找。
        let mut stdout = b"PK\x03\x04 fake zip \n\x1f\x1f 999 garbage".to_vec();
        stdout.extend_from_slice(b" more body");
        stdout.extend_from_slice("\n\u{1f}\u{1f}200 https://cdn.example/node.zip 1.25".as_bytes());

        let (body, status, url, elapsed) = HttpTransport::split_write_out(&stdout);
        assert_eq!(status, Some(200));
        assert_eq!(url.as_deref(), Some("https://cdn.example/node.zip"));
        assert!((elapsed.unwrap() - 1.25).abs() < 1e-9);
        // body 必须**只**是哨兵之前的部分，并且那个"假哨兵"保留在 body 里。
        let text = String::from_utf8_lossy(&body);
        assert!(
            text.contains("999 garbage"),
            "假哨兵之后的字节不该被丢掉：{text:?}"
        );
        assert!(!text.contains("200 https"), "真哨兵必须被削掉");
    }

    #[test]
    fn a_response_without_the_sentinel_is_treated_as_data_not_as_a_success() {
        // 没有哨兵说明参数写错了或 curl 版本太老。此时 HTTP 码是 `None`，
        // 于是上层**不会**拿到一个"看起来成功"的结果（它会去查退出码）。
        let (body, status, url, elapsed) = HttpTransport::split_write_out(b"just bytes");
        assert_eq!(body, b"just bytes");
        assert_eq!(status, None);
        assert_eq!(url, None);
        assert_eq!(elapsed, None);
    }

    #[test]
    fn a_probe_asks_for_one_byte_and_a_download_does_not() {
        // **这条测试存在的原因是一次数百 MB 的浪费风险。**
        // 探测一个 Temurin 镜像时如果不发区间请求，就会把整个 JDK 下下来。
        let transport = HttpTransport::probe();

        let probe = transport.curl_args(
            "https://api.adoptium.net/v3/info/available_releases",
            Some(ByteRange::FirstByte),
            Duration::from_secs(15),
        );
        let range = probe.iter().position(|a| a == "-r").expect("探测必须带 -r");
        assert_eq!(probe[range + 1], "0-0", "只要第一个字节");
        assert_eq!(
            probe.last().map(String::as_str),
            Some("https://api.adoptium.net/v3/info/available_releases")
        );

        let download = transport.curl_args(
            "https://nodejs.org/dist/v24.19.0/node-v24.19.0-win-x64.zip",
            None,
            Duration::from_secs(60),
        );
        assert!(
            !download.contains(&"-r".to_owned()),
            "真下载绝不能带区间：{download:?}"
        );
    }

    #[test]
    fn the_fake_transport_honours_ranges_so_the_probe_claim_is_testable() {
        // 一个不尊重区间的假传输层会让"探测不会下整包"这条断言变成谎言。
        let transport = FakeTransport::new().serves("https://a/", b"0123456789".to_vec());

        let probed = transport
            .fetch(
                "https://a/x",
                Some(ByteRange::FirstByte),
                Duration::from_secs(1),
            )
            .expect("探测");
        assert_eq!(probed.bytes, b"0", "探测只该拿到一个字节");

        let whole = transport
            .fetch("https://a/x", None, Duration::from_secs(1))
            .expect("整取");
        assert_eq!(whole.bytes, b"0123456789");

        // 探测在尝试记录里被标出来，这样"没有探测"也能被断言。
        assert_eq!(
            transport.attempts(),
            vec!["https://a/x #range".to_owned(), "https://a/x".to_owned()]
        );
    }

    #[test]
    fn http_status_outside_2xx_is_a_failure_carrying_the_code() {
        let transport = FakeTransport::new().fails_with_status("https://a/", 403);
        let error = transport
            .fetch("https://a/x.zip", None, Duration::from_secs(1))
            .expect_err("403 必须失败");
        assert_eq!(error.kind(), crate::error::FailureKind::SourceUnavailable);
        assert!(
            error.to_string().contains("403"),
            "用户需要看到那个码：{error}"
        );
        assert!(error.should_try_next_source(), "403 应当触发换源");
    }

    #[test]
    fn a_missing_route_is_unreachable_rather_than_an_empty_download() {
        // 返回空会让"忘了配路由"变成一个静默的成功下载 —— 那是最糟的一类测试 bug。
        let transport = FakeTransport::new();
        let error = transport
            .fetch("https://nowhere/x.zip", None, Duration::from_secs(1))
            .expect_err("没配路由必须失败");
        assert_eq!(error.kind(), crate::error::FailureKind::SourceUnavailable);
    }

    #[test]
    fn fake_transport_records_every_attempt_in_order() {
        let transport = FakeTransport::new()
            .unreachable("https://first/")
            .fails_with_status("https://second/", 404)
            .serves("https://third/", b"ok".to_vec());

        assert!(
            transport
                .fetch("https://first/x", None, Duration::from_secs(1))
                .is_err()
        );
        assert!(
            transport
                .fetch("https://second/x", None, Duration::from_secs(1))
                .is_err()
        );
        assert!(
            transport
                .fetch("https://third/x", None, Duration::from_secs(1))
                .is_ok()
        );
        assert_eq!(
            transport.attempts(),
            vec![
                "https://first/x".to_owned(),
                "https://second/x".to_owned(),
                "https://third/x".to_owned(),
            ],
            "回退顺序必须可断言"
        );
    }

    #[test]
    fn the_longest_matching_route_wins() {
        // 前缀路由必须是"最长匹配"，否则一条宽泛的 `https://a/` 会盖掉
        // 后面配的 `https://a/broken/` —— 于是"某条路径失败"永远测不出来。
        let transport = FakeTransport::new()
            .serves("https://a/", b"generic".to_vec())
            .fails_with_status("https://a/broken/", 500);
        assert!(
            transport
                .fetch("https://a/ok", None, Duration::from_secs(1))
                .is_ok()
        );
        assert!(
            transport
                .fetch("https://a/broken/x", None, Duration::from_secs(1))
                .is_err()
        );
    }

    #[test]
    fn truncate_marks_that_it_truncated() {
        assert_eq!(truncate("short", 100), "short");
        let long = "x".repeat(50);
        let cut = truncate(&long, 10);
        assert!(cut.starts_with("xxxxxxxxxx"));
        assert!(cut.contains("已截断"), "截断必须说明：{cut}");
    }
}
