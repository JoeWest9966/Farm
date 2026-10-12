//! plugin-skeleton：第一个**插件**契约客户端（ticket #19）。
//!
//! GLOSSARY「插件骨架」：插件是宿主子进程，以**契约客户端**身份操作——与编排器、面板
//! **同一条路由、同一个原子 claim**（ADR-0004 决策 4：宿主不为插件开私有接口）。本骨架
//! 证明这一点：连上宿主、`POST /handshake`、再 `POST /v1/tracker/claims` 认领一张票
//! ——用的正是编排器/面板那条契约路由，没有任何私有后门。
//!
//! 宿主用 stdin 持有本进程：宿主关 stdin 即请求有序停机（本骨架读到 EOF 即退）。
//! 只 depend `farm-sdk`（纯契约类型，与面板/编排器同源）。

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use std::net::SocketAddr;

use farm_sdk::handshake::HandshakeResponse;
use tokio::net::TcpStream;

/// 极简 HTTP/1.1 请求（`Connection: close` + 读到 EOF）。
async fn http(
    addr: SocketAddr,
    method: &str,
    path: &str,
    token: &str,
    body: Option<&str>,
) -> std::io::Result<(u16, String)> {
    let mut stream = TcpStream::connect(addr).await?;
    let body = body.unwrap_or("");
    let mut req = format!("{method} {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n");
    if !token.is_empty() {
        req.push_str(&format!("X-Farm-Token: {token}\r\n"));
    }
    if !body.is_empty() {
        req.push_str("Content-Type: application/json\r\n");
    }
    req.push_str(&format!("Content-Length: {}\r\n\r\n{body}", body.len()));
    stream.write_all(req.as_bytes()).await?;
    stream.flush().await?;
    let mut buf = Vec::new();
    stream.read_to_end(&mut buf).await?;
    let text = String::from_utf8_lossy(&buf).into_owned();
    let status = text
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|s| s.parse::<u16>().ok())
        .unwrap_or(0);
    let payload = text.split_once("\r\n\r\n").map(|(_, b)| b.to_string()).unwrap_or_default();
    Ok((status, payload))
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let host = std::env::var("FARM_HOST").unwrap_or_else(|_| "127.0.0.1:3721".to_string());
    let addr: SocketAddr = host.parse().expect("FARM_HOST must be host:port");
    let token = std::env::var("FARM_TOKEN").unwrap_or_default();
    let assignee = std::env::var("FARM_PLUGIN_ASSIGNEE").unwrap_or_else(|_| "plugin-skeleton".to_string());

    // 1) 握手：以插件客户端身份自描述（kind = plugin）。
    let hs = format!(
        r#"{{"contract_version":"1.0.0","client":{{"name":"plugin-skeleton","version":"0.1.0","kind":"plugin"}}}}"#
    );
    match http(addr, "POST", "/handshake", &token, Some(&hs)).await {
        Ok((200, body)) => match serde_json::from_str::<HandshakeResponse>(&body) {
            Ok(resp) => println!("[handshake] host {} · contract {} · {} services", resp.host_version, resp.contract_version, resp.services.len()),
            Err(e) => eprintln!("[handshake] unparseable response: {e}"),
        },
        Ok((status, body)) => eprintln!("[handshake] HTTP {status}: {body}"),
        Err(e) => eprintln!("[handshake] connect failed: {e}"),
    }

    // 2) 经契约 claim 一张票（与编排器/面板同一条 POST /v1/tracker/claims，同原子 claim）。
    let issue = std::env::var("FARM_PLUGIN_ISSUE").unwrap_or_else(|_| "demo".to_string());
    let claim = format!(r#"{{"issue_id":"{issue}","assignee":"{assignee}"}}"#);
    match http(addr, "POST", "/v1/tracker/claims", &token, Some(&claim)).await {
        Ok((status, body)) => println!("[claim] POST /v1/tracker/claims → HTTP {status}: {body}"),
        Err(e) => eprintln!("[claim] failed: {e}"),
    }
    println!("plugin-skeleton done (contract client, no private backdoor). Refs #19");
}
