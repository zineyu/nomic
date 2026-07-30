//! 测试用迷你 HTTP 服务器：每个连接消费脚本中的一条响应。
//!
//! workspace 的 tokio 未启用 `net` feature，这里用阻塞
//! [`std::net::TcpListener`] 加独立线程实现；响应带 `connection: close`，
//! 保证 reqwest 每次尝试都建立新连接（即脚本的一条响应对应一次请求）。

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::Duration;

/// 启动脚本化服务器，返回 `(base_url, 请求计数)`。
///
/// 脚本用尽后服务器线程退出、监听端口关闭，后续连接即被拒绝
/// （对客户端表现为连接错误）。
pub fn start(responses: Vec<String>) -> (String, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind test server");
    let addr = listener.local_addr().expect("local addr");
    let count = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&count);
    thread::spawn(move || {
        let mut responses = responses.into_iter();
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { break };
            counter.fetch_add(1, Ordering::SeqCst);
            let Some(response) = responses.next() else {
                break;
            };
            read_request(&mut stream);
            stream
                .write_all(response.as_bytes())
                .expect("write response");
        }
    });
    (format!("http://{addr}"), count)
}

/// 构造一条纯文本错误响应。
pub fn http_error(status: u16, reason: &str, body: &str) -> String {
    format!(
        "HTTP/1.1 {status} {reason}\r\ncontent-type: text/plain\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    )
}

/// 构造一条 SSE 成功响应。
pub fn sse(body: &str) -> String {
    format!(
        "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    )
}

/// 读完一个完整请求（请求头 + `Content-Length` 指示的请求体）再响应，
/// 避免客户端还在发 body 时连接被关闭导致 RST。
fn read_request(stream: &mut TcpStream) {
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("set read timeout");
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    let header_end = loop {
        if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break pos + 4;
        }
        let n = stream.read(&mut chunk).expect("read request headers");
        assert!(n > 0, "connection closed before full headers");
        buf.extend_from_slice(&chunk[..n]);
    };
    let headers = String::from_utf8_lossy(&buf[..header_end]);
    let content_length = headers
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.trim()
                .eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().ok().unwrap_or(0))
        })
        .unwrap_or(0);
    while buf.len() < header_end + content_length {
        let n = stream.read(&mut chunk).expect("read request body");
        assert!(n > 0, "connection closed before full body");
        buf.extend_from_slice(&chunk[..n]);
    }
}
