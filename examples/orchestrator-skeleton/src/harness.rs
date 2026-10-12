//! 演练夹具（ticket #26）：在库里定位并 spawn **真实 farm-host 二进制**与 fake 替身，
//! 供整链 tracer（二进制与测试共用）驱动「一次跑通的整链 + 崩溃恢复演练」。
//!
//! 与 tests/ 夹具同源，但住在 lib 里：二进制 `orchestrator-skeleton chain` 与集成测试
//! 都复用它。定位优先级 `*_BIN` 环境变量 > `CARGO_TARGET_DIR/debug/` > manifest 相对路径。

use std::net::{SocketAddr, TcpListener};
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener as TokioListener, TcpStream};

/// 定位 farm-host 二进制。
pub fn host_bin() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("FARM_HOST_BIN") {
        return Some(PathBuf::from(p));
    }
    target_debug_bin("farm-host")
}

/// 定位一个 farm-core fake 替身（fake-pi / fake-nw / fake-ledger）。
pub fn fake_bin(name: &str) -> Option<PathBuf> {
    if let Ok(dir) = std::env::var("CARGO_TARGET_DIR") {
        let cand = PathBuf::from(&dir).join("debug").join(name);
        if cand.exists() {
            return Some(cand);
        }
    }
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let cand = manifest.join("../../target/debug").join(name);
    cand.exists().then_some(cand)
}

/// 定位 http-notifier 插件二进制（examples/http-notifier，独立 workspace 单独编）。
pub fn notifier_bin() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("FARM_NOTIFIER_BIN") {
        return Some(PathBuf::from(p));
    }
    if let Ok(dir) = std::env::var("CARGO_TARGET_DIR") {
        for cand in [
            PathBuf::from(&dir).join("debug/http-notifier"),
            PathBuf::from(&dir).join("examples/http-notifier/debug/http-notifier"),
        ] {
            if cand.exists() {
                return Some(cand);
            }
        }
    }
    None
}

fn target_debug_bin(name: &str) -> Option<PathBuf> {
    if let Ok(td) = std::env::var("CARGO_TARGET_DIR") {
        let cand = PathBuf::from(&td).join("debug").join(name);
        if cand.exists() {
            return Some(cand);
        }
    }
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let cand = manifest.join("../../target/debug").join(name);
    cand.exists().then_some(cand)
}

/// 取一个临时回环端口（预绑 :0 拿 OS 分配后放开，交给宿主 bind）。
pub fn ephemeral_addr() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral");
    let addr = listener.local_addr().expect("local addr");
    drop(listener);
    addr
}

async fn wait_listening(addr: SocketAddr, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if TcpStream::connect(addr).await.is_ok() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    false
}

/// 一个被测的真实 farm-host 进程（Drop 收尾）。**DB 路径由调用方给**——崩溃恢复演练
/// 要求杀宿主后重启用的是**同一个库**，故库里数据（attempt / 事件 / 映射）跨重启共享。
pub struct FarmHost {
    child: Child,
    pub addr: SocketAddr,
    pub op_token: String,
    pub admin_token: Option<String>,
    pub db_path: PathBuf,
}

impl FarmHost {
    /// spawn 宿主并绑到指定地址、用指定 DB（整链演练先定住 addr + 共享库）。
    pub async fn spawn_at(addr: SocketAddr, db_path: &Path, extra_env: &[(String, String)], with_admin: bool) -> Self {
        let bin = host_bin().unwrap_or_else(|| panic!("farm-host binary not found; build it first (cargo build -p farm-host)"));
        let op_token = "chain-op-token".to_string();
        let admin_token = with_admin.then(|| "chain-admin-token".to_string());

        let mut command = Command::new(&bin);
        command
            .env("FARM_BIND", addr.to_string())
            .env("FARM_TOKEN", &op_token)
            .env("FARM_DB", db_path)
            .env("RUST_LOG", "info");
        if let Some(admin) = &admin_token {
            command.env("FARM_ADMIN_TOKEN", admin);
        }
        for (key, value) in extra_env {
            command.env(key, value);
        }

        let child = command.spawn().unwrap_or_else(|e| panic!("spawn {}: {e}", bin.display()));
        let host = Self { child, addr, op_token, admin_token, db_path: db_path.to_path_buf() };
        assert!(wait_listening(addr, Duration::from_secs(30)).await, "farm-host did not listen on {addr}");
        host
    }

    pub fn client(&self) -> crate::client::ContractClient {
        crate::client::ContractClient::new(self.addr, self.op_token.clone(), self.admin_token.clone())
    }

    /// SIGKILL：模拟宿主崩溃（非有序停机）——崩溃恢复演练用。
    pub fn kill(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for FarmHost {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// 一个 fake HTTP 端点（真实 TCP）：记录每个到达的请求，供断言「通知外发到达」。
pub struct FakeEndpoint {
    pub addr: SocketAddr,
    requests: std::sync::Arc<std::sync::Mutex<Vec<RecordedRequest>>>,
    accept_loop: tokio::task::JoinHandle<()>,
}

#[derive(Clone, Debug)]
pub struct RecordedRequest {
    pub method: String,
    pub path: String,
    pub body: String,
}

impl FakeEndpoint {
    pub async fn start() -> Self {
        let listener = TokioListener::bind("127.0.0.1:0").await.expect("bind fake endpoint");
        let addr = listener.local_addr().expect("local addr");
        let requests = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let recorded = requests.clone();
        let accept_loop = tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else { break };
                let recorded = recorded.clone();
                tokio::spawn(async move {
                    if let Ok(req) = read_request(stream).await {
                        recorded.lock().expect("lock").push(req);
                    }
                });
            }
        });
        Self { addr, requests, accept_loop }
    }

    pub fn posts(&self) -> Vec<RecordedRequest> {
        self.requests.lock().expect("lock").clone()
    }
}

impl Drop for FakeEndpoint {
    fn drop(&mut self) {
        self.accept_loop.abort();
    }
}

async fn read_request(mut stream: TcpStream) -> std::io::Result<RecordedRequest> {
    let mut buf: Vec<u8> = Vec::new();
    let header_end = loop {
        if let Some(pos) = find_subslice(&buf, b"\r\n\r\n") {
            break pos + 4;
        }
        let mut chunk = [0u8; 4096];
        let n = stream.read(&mut chunk).await?;
        if n == 0 {
            return Err(std::io::Error::other("closed before headers"));
        }
        buf.extend_from_slice(&chunk[..n]);
    };
    let header_text = String::from_utf8_lossy(&buf[..header_end]).into_owned();
    let content_length = header_text.lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        name.eq_ignore_ascii_case("content-length").then(|| value.trim().parse::<usize>().ok()).flatten()
    }).unwrap_or(0);
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
    // 回一个 200，让插件的投递判成功（否则插件侧重试）。
    let response = "HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok";
    let _ = stream.write_all(response.as_bytes()).await;
    let _ = stream.flush().await;
    Ok(RecordedRequest { method, path, body })
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}
