//! 测试夹具：一个极简 fake HTTP 服务器（真实 TCP，非 mock）。
//!
//! notifier 插件同时是**宿主客户端**（读事件总线快照 + 读通道配置）与**推送客户端**
//! （POST 到端点）。这个 fake 用真实 TCP 同时扮好这三个角色：按 method + path 路由，
//! 记录每个请求（含到达时刻），让测试在真实 HTTP 接缝上断言——不 mock 插件的任何内部协作者。

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;

/// 被记录的一个请求（method + path + headers + body + 到达时刻）。
#[derive(Clone, Debug)]
pub struct RecordedRequest {
    pub method: String,
    pub path: String,
    pub headers: Vec<(String, String)>,
    pub body: String,
    pub received_at: Instant,
}

/// 路由函数：给定请求，回 `(状态码, body)`。
type Handler = Box<dyn Fn(&RecordedRequest) -> (u16, String) + Send + Sync + 'static>;

/// 一个 fake HTTP 服务器（`Drop` 时中止 accept 循环）。
pub struct FakeHttp {
    pub addr: SocketAddr,
    requests: Arc<Mutex<Vec<RecordedRequest>>>,
    accept_loop: JoinHandle<()>,
}

impl FakeHttp {
    /// 在回环临时端口上起一个 fake 服务器，用 `handler` 路由。
    pub async fn start(handler: impl Fn(&RecordedRequest) -> (u16, String) + Send + Sync + 'static) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind fake http");
        let addr = listener.local_addr().expect("local addr");
        let requests = Arc::new(Mutex::new(Vec::new()));
        let handler: Handler = Box::new(handler);
        let handler = Arc::new(handler);

        let recorded = requests.clone();
        let accept_loop = tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else { break };
                let recorded = recorded.clone();
                let handler = handler.clone();
                tokio::spawn(async move {
                    if let Ok((mut stream, request)) = read_request(stream).await {
                        recorded.lock().expect("lock").push(request.clone());
                        let (status, body) = (handler)(&request);
                        let _ = respond(&mut stream, status, &body).await;
                    }
                });
            }
        });

        Self {
            addr,
            requests,
            accept_loop,
        }
    }

    /// 全部被记录的请求。
    pub fn all(&self) -> Vec<RecordedRequest> {
        self.requests.lock().expect("lock").clone()
    }

    /// 被记录的 POST（投递到端点的那批）。
    pub fn posts(&self) -> Vec<RecordedRequest> {
        self.all().into_iter().filter(|r| r.method == "POST").collect()
    }

    /// 被记录的、路径以 `prefix` 开头的 GET。
    pub fn gets(&self, prefix: &str) -> Vec<RecordedRequest> {
        self.all()
            .into_iter()
            .filter(|r| r.method == "GET" && r.path.starts_with(prefix))
            .collect()
    }
}

impl Drop for FakeHttp {
    fn drop(&mut self) {
        self.accept_loop.abort();
    }
}

/// 读一个 HTTP 请求：读到头终止符，按 Content-Length 读满 body。
async fn read_request(mut stream: TcpStream) -> std::io::Result<(TcpStream, RecordedRequest)> {
    let received_at = Instant::now();
    let mut buf: Vec<u8> = Vec::new();
    let header_end = loop {
        if let Some(pos) = find(&buf, b"\r\n\r\n") {
            break pos + 4;
        }
        let mut chunk = [0u8; 4096];
        let n = stream.read(&mut chunk).await?;
        if n == 0 {
            return Err(std::io::Error::other("connection closed before headers"));
        }
        buf.extend_from_slice(&chunk[..n]);
    };

    let header_text = String::from_utf8_lossy(&buf[..header_end]).into_owned();
    let content_length = header_text
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().ok())
                .flatten()
        })
        .unwrap_or(0);
    while buf.len() < header_end + content_length {
        let mut chunk = [0u8; 4096];
        let n = stream.read(&mut chunk).await?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
    }

    let body = String::from_utf8_lossy(&buf[header_end..header_end + content_length]).into_owned();
    let mut lines = header_text.lines();
    let request_line = lines.next().unwrap_or("");
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("").to_string();
    let path = parts.next().unwrap_or("").to_string();
    let headers = lines
        .filter_map(|line| {
            let (name, value) = line.split_once(':')?;
            Some((name.trim().to_string(), value.trim().to_string()))
        })
        .collect();

    Ok((
        stream,
        RecordedRequest {
            method,
            path,
            headers,
            body,
            received_at,
        },
    ))
}

/// 回一个 HTTP 响应（`Connection: close`，与插件的极简客户端对齐）。
async fn respond(stream: &mut TcpStream, status: u16, body: &str) -> std::io::Result<()> {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        404 => "Not Found",
        500 => "Internal Server Error",
        _ => "OK",
    };
    let response = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(response.as_bytes()).await?;
    stream.flush().await?;
    Ok(())
}

/// 在 `haystack` 里找 `needle` 首次出现的下标。
fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}
