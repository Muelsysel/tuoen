//! 环回验收：**真的用 `curl.exe` 发一次 HTTP 请求**，只是服务器在本机。
//!
//! ## 这个文件要证明什么
//!
//! 单元测试用 [`tuoen_download::transport::FakeTransport`] 覆盖了"下载层的语义"
//! （换源、哈希、缓存、报告），但那些测试**一行 curl 都没跑过**。
//! 于是下面这些东西一直是"读代码相信它对"的状态：
//!
//! - `curl.exe` 真的存在，而且真的能被我们这样调起来；
//! - `-sS -L --fail-with-body -o - -w <哨兵>...` 这串参数在真实 curl 上
//!   的行为与 [`tuoen_download::transport::HttpTransport::split_write_out`]
//!   的假设一致（尤其是**哨兵确实在响应体之后**、且响应体是任意字节）；
//! - 非 2xx 真的会变成 [`DownloadError::Http`]，而不是静默成功；
//! - 连不上（端口没人听）真的会变成 [`DownloadError::Transport`]；
//! - **区间请求真的只取一个字节**（探测靠它，不然会把 190 MB 的 JDK 拖下来）。
//!
//! ## 为什么这不算"测试访问真实网络"
//!
//! 服务器是**本文件自己起的一个 `TcpListener`，绑在 `127.0.0.1`**，
//! 每个用例跑完就关。数据不出本机，也不依赖任何外部服务。
//! `127.0.0.1` 是唯一被允许的连接目标。
//!
//! 这也是唯一能碰到真实的 curl **退出码**与**真实 HTTP 语义**的办法 ——
//! 用 `FakeTransport` 测这些等于用被测量自己证明自己。

use std::io::{BufRead as _, BufReader, Write as _};
use std::net::{TcpListener, TcpStream};
use std::time::Duration;

use tuoen_download::error::DownloadError;
use tuoen_download::transport::{ByteRange, HttpTransport, Transport};

/// 一个一次性的环回 HTTP 服务器，服务**一个**固定响应。
struct LoopbackServer {
    port: u16,
    /// 收到的请求行，用来断言"确实被请求过"。
    requests: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
}

/// 服务器怎么回话。
#[derive(Clone)]
enum Reply {
    /// `200 OK` + 这些字节。
    Body(Vec<u8>),
    /// 这个状态码 + 一句短文本。
    Status(u16),
}

impl LoopbackServer {
    /// 起一个服务器，只回一次（再来的连接照样接，但只回同一个响应）。
    ///
    /// 绑 `127.0.0.1:0` 让内核挑一个空闲端口 —— 硬编码端口的测试在
    /// 并行跑的时候会互相抢，而那种失败看起来像"网络问题"。
    fn start(reply: Reply) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("绑环回端口");
        let port = listener.local_addr().expect("拿本地地址").port();
        let requests = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let log = std::sync::Arc::clone(&requests);

        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { break };
                let reply = reply.clone();
                let log = std::sync::Arc::clone(&log);
                std::thread::spawn(move || {
                    serve(stream, reply, log);
                });
            }
        });

        Self { port, requests }
    }

    /// 这个服务器的 `http://` 基地址。
    fn base(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    /// 收到过的请求行。
    fn requests(&self) -> Vec<String> {
        self.requests
            .lock()
            .map(|guard| guard.clone())
            .unwrap_or_default()
    }
}

/// 处理一次连接：读请求头，然后按 `reply` 回话。
fn serve(mut stream: TcpStream, reply: Reply, log: std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));

    // 读请求行与请求头。
    //
    // **必须把这些头读出来**，尤其是 `Range`：不读的话就没法判断
    // "探测到底有没有发区间请求"，而那条断言正是这个文件的重点。
    let mut request_line = String::new();
    let mut range_header: Option<String> = None;
    {
        // 用 `&mut stream` 而不是 `try_clone`：clone 出来的 `BufReader`
        // 一旦被丢弃，它**已经读进缓冲区**的那些字节就一起没了 ——
        // 而当客户端只发了一个请求时，丢掉的就是整个请求的剩余部分。
        let mut reader = BufReader::new(&mut stream);
        if reader.read_line(&mut request_line).is_err() {
            return;
        }
        loop {
            let mut line = String::new();
            match reader.read_line(&mut line) {
                Ok(0) => break,
                Ok(_) => {
                    if line == "\r\n" || line == "\n" {
                        break;
                    }
                    if let Some(value) = line.to_ascii_lowercase().strip_prefix("range:") {
                        range_header = Some(value.trim().to_owned());
                    }
                }
                Err(_) => break,
            }
        }
    }
    if let Ok(mut guard) = log.lock() {
        guard.push(format!(
            "{} Range={}",
            request_line.trim_end(),
            range_header.as_deref().unwrap_or("<none>")
        ));
    }

    let (mut status_line, mut body): (String, Vec<u8>) = match reply {
        Reply::Body(bytes) => ("200 OK".to_owned(), bytes),
        Reply::Status(code) => {
            let reason = match code {
                403 => "Forbidden",
                404 => "Not Found",
                500 => "Internal Server Error",
                _ => "Error",
            };
            (format!("{code} {reason}"), Vec::new())
        }
    };

    // **尊重 `Range`。** 一个不尊重区间的服务器会让"探测只取一个字节"
    // 这条断言变成在测服务器（而且它会失败）—— 而它本该测的是
    // "我们的 `-r 0-0` 有没有真的被 curl 发出去"。
    //
    // 只支持 `bytes=<start>-<end>` 这一种形式 —— 我们只会发 `0-0`。
    if let Some(rest) = range_header
        .as_deref()
        .and_then(|range| range.strip_prefix("bytes="))
    {
        let (start, end) = rest.split_once('-').unwrap_or((rest, ""));
        let start: usize = start.trim().parse().unwrap_or(0);
        let end: usize = end.trim().parse().unwrap_or(start);
        let end = end.min(body.len().saturating_sub(1));
        if start <= end && start < body.len() {
            body = body[start..=end].to_vec();
            status_line = "206 Partial Content".to_owned();
        } else {
            body = Vec::new();
            status_line = "416 Range Not Satisfiable".to_owned();
        }
    }

    let response = format!(
        "HTTP/1.1 {status_line}\r\nContent-Length: {}\r\nContent-Type: application/octet-stream\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(response.as_bytes());
    let _ = stream.write_all(&body);
    let _ = stream.flush();
}

/// 跳过没装 curl 的机器（Windows 10 1803+ 都有）。
///
/// **不是静默跳过**：打印一行说明。一个"永远不跑"的测试比没有测试更糟，
/// 因为它会让人以为这里被覆盖了。
fn transport_or_skip() -> Option<HttpTransport> {
    let transport = HttpTransport::probe();
    if transport.program().is_none() {
        eprintln!("SKIP: 这台机器上没有 curl.exe，环回验收无法进行");
        return None;
    }
    Some(transport)
}

#[test]
fn curl_really_fetches_bytes_from_a_loopback_server() {
    let Some(transport) = transport_or_skip() else {
        return;
    };

    // 内容刻意包含**看起来像哨兵的东西**与任意字节：`-w` 的输出被追加在
    // 响应体之后，所以解析必须从后往前找哨兵。如果 curl 或我们的解析
    // 在这上面搞错了，这个用例会直接暴露出来。
    let payload = b"hello tuoen \n\x1f\x1f not-the-real-sentinel 999 \x00\xff bytes".to_vec();
    let server = LoopbackServer::start(Reply::Body(payload.clone()));

    let fetched = transport
        .fetch(
            &format!("{}/x.bin", server.base()),
            None,
            Duration::from_secs(20),
        )
        .expect("环回请求应当成功");

    assert_eq!(fetched.bytes, payload, "取回的字节必须与发出去的一模一样");
    assert!(
        !server.requests().is_empty(),
        "服务器必须真的收到过请求（否则上面那句是自证）"
    );
    let request = &server.requests()[0];
    assert!(
        request.starts_with("GET /x.bin"),
        "请求行应当是 GET：{request}"
    );
}

#[test]
fn a_probe_range_request_asks_the_server_for_one_byte() {
    // **这条测试的动机是一次数百 MB 的浪费风险。** 探测一个 Temurin 镜像时
    // 如果不发区间请求，就会把整个 JDK 下下来。
    //
    // 在真实 curl 上验，而不是在 `curl_args` 上验：参数对不对只有服务器
    // 看到的 `Range` 头能回答。
    let Some(transport) = transport_or_skip() else {
        return;
    };

    let payload: Vec<u8> = (0u8..=255).collect();
    let server = LoopbackServer::start(Reply::Body(payload));
    let url = format!("{}/big.zip", server.base());

    let probed = transport
        .fetch(&url, Some(ByteRange::FirstByte), Duration::from_secs(20))
        .expect("区间请求应当成功");
    assert_eq!(probed.bytes.len(), 1, "探测只该拿到一个字节");
    assert_eq!(probed.bytes[0], 0, "而且应当是第一个字节");

    // 服务器看到的是一个带 Range 的 GET。**这是唯一能证明"我们那对参数
    // 真的变成了区间请求"的证据** —— 光看自己的 `curl_args` 只能证明
    // 我们打算这么做。
    let requests = server.requests();
    assert!(!requests.is_empty(), "服务器必须真的收到过请求");
    assert!(
        requests[0].contains("Range=bytes=0-0"),
        "服务器看到的请求必须带 `Range: bytes=0-0`，实际是：{}",
        requests[0]
    );
}

#[test]
fn a_404_becomes_an_http_error_not_a_silent_success() {
    let Some(transport) = transport_or_skip() else {
        return;
    };

    let server = LoopbackServer::start(Reply::Status(404));
    let error = transport
        .fetch(
            &format!("{}/missing.zip", server.base()),
            None,
            Duration::from_secs(20),
        )
        .expect_err("404 必须失败");

    match error {
        DownloadError::Http { status, .. } => assert_eq!(status, 404),
        other => panic!("应当是 Http(404)，实际是 {other:?}"),
    }
    // 403 也要能被区分出来（国内镜像实测大多回 403）。
    assert_eq!(
        error_kind_name(&error),
        "source-unavailable",
        "404 属于'这个源不可用'，应当触发换源"
    );
}

/// 拿一个失败分类的稳定字符串（不本地化）。
fn error_kind_name(error: &DownloadError) -> &'static str {
    error.kind().as_str()
}

#[test]
fn nothing_listening_is_reported_as_unreachable_not_as_a_bad_response() {
    let Some(transport) = transport_or_skip() else {
        return;
    };

    // 绑一个端口然后立刻放掉：几乎可以肯定没人听它。
    let port = {
        let listener = TcpListener::bind("127.0.0.1:0").expect("占一个端口");
        let port = listener.local_addr().expect("地址").port();
        drop(listener);
        port
    };

    let error = transport
        .fetch(
            &format!("http://127.0.0.1:{port}/x.zip"),
            None,
            Duration::from_secs(10),
        )
        .expect_err("没人听就必须失败");

    // **关键区分**：这必须是 Transport（curl 退出码非零），
    // 而不是 Http(000) —— 后者会让用户以为"服务器返回了 000"。
    assert!(
        matches!(error, DownloadError::Transport { .. }),
        "连不上应当是 Transport，实际是 {error:?}"
    );
    assert_eq!(error_kind_name(&error), "source-unavailable");
    assert!(
        error.should_try_next_source(),
        "连不上必须触发换源（这正是国内镜像挂掉时的路径）"
    );
}
