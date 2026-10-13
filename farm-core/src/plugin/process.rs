//! 插件子进程：spawn、stdin 持有、`hello` 握手行读取、有序停机。
//!
//! ADR-0004 决策 4：**插件是宿主子进程，宿主持有其 stdin**（关 stdin 即请求有序停机，
//! 父死子陪葬）。所以这里与 agent-runner 的 pi 句柄同构（`Stdio::piped` + 保存
//! `ChildStdin` + stdout 读循环 + `kill_on_drop`），但**没有**判据阶梯——pi 子进程不是插件，
//! 插件的生命周期归 [`super::PluginHost`] 的分级退避重试管。
//!
//! 宿主持有的两样东西：
//! - `stdin` 管道写端：宿主不写业务指令，只在停机时**关掉**它（= 请求有序停机）；
//! - `FARM_PLUGIN_HOST_PID` 环境变量：插件的第二道保险（父进程死了也要退，见示例插件）。
//!
//! 插件→宿主只说 JSONL（stdout）：`hello`（握手自描述）之后是可选的 `state`/`bye` 行。

use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use farm_sdk::handshake::{is_compatible, ClientKind, CONTRACT_VERSION};

use super::{DEFAULT_HANDSHAKE_TIMEOUT, PluginError};
use crate::error::CoreError;

/// 宿主注入给插件的环境变量：宿主自己的 pid（父死子陪葬的第二道保险）。
pub const HOST_PID_ENV: &str = "FARM_PLUGIN_HOST_PID";

/// 宿主注入给插件的环境变量：插件身份（与 spec 的 `plugin_id` 一致）。
pub const PLUGIN_ID_ENV: &str = "FARM_PLUGIN_ID";

/// 拉起一个插件的计划（测试把 `program` 指向替身进程）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginSpec {
    pub plugin_id: String,
    pub name: String,
    pub program: String,
    pub args: Vec<String>,
    /// 只进插件子进程的环境变量（如 `FARM_HOST` / `FARM_TOKEN` / 场景开关）。
    pub env: Vec<(String, String)>,
    /// 等 `hello` 握手行的时长。
    pub handshake_timeout: Duration,
    /// 关 stdin 之后等插件自己退出的宽限，超时才强杀。
    pub shutdown_grace: Duration,
}

impl PluginSpec {
    pub fn new(
        plugin_id: impl Into<String>,
        name: impl Into<String>,
        program: impl Into<String>,
    ) -> Self {
        Self {
            plugin_id: plugin_id.into(),
            name: name.into(),
            program: program.into(),
            args: Vec::new(),
            env: Vec::new(),
            handshake_timeout: DEFAULT_HANDSHAKE_TIMEOUT,
            shutdown_grace: crate::plugin::DEFAULT_SHUTDOWN_GRACE,
        }
    }

    pub fn with_args(mut self, args: Vec<String>) -> Self {
        self.args = args;
        self
    }

    /// 追加一对只进插件子进程的 env。
    pub fn with_env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.push((key.into(), value.into()));
        self
    }

    /// 覆写握手等待时长。
    pub fn with_handshake_timeout(mut self, timeout: Duration) -> Self {
        self.handshake_timeout = timeout;
        self
    }

    /// 覆写停机宽限。
    pub fn with_shutdown_grace(mut self, grace: Duration) -> Self {
        self.shutdown_grace = grace;
        self
    }

    /// 人读的一行命令（落 `plugin_state` 的 `program` 列，只作诊断）。
    pub fn describe(&self) -> String {
        if self.args.is_empty() {
            self.program.clone()
        } else {
            format!("{} {}", self.program, self.args.join(" "))
        }
    }

    /// 计划合法性（宿主绝不拿空 id / 空程序去 spawn）。
    pub(crate) fn validate(&self) -> Result<(), PluginError> {
        if self.plugin_id.trim().is_empty() {
            return Err(PluginError::InvalidSpec("plugin_id must not be empty".into()));
        }
        if self.program.trim().is_empty() {
            return Err(PluginError::InvalidSpec(format!(
                "plugin {} has no program to spawn",
                self.plugin_id
            )));
        }
        Ok(())
    }
}

/// 插件的握手自描述（stdout 第一行 `hello`）。
///
/// 插件经 farm-sdk 声明自己的契约版本与身份种类，宿主按
/// [`farm_sdk::handshake::is_compatible`] 判定——与编排器握手同一套规则
/// （ADR-0001：插件是契约客户端）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginHello {
    pub plugin: String,
    pub name: String,
    pub contract_version: String,
    pub kind: ClientKind,
}

impl PluginHello {
    /// 从一行 stdout JSONL 解出握手；形状不对 → `None`（调用方按握手失败处理）。
    pub fn parse(line: &[u8]) -> Option<Self> {
        let value: serde_json::Value = serde_json::from_slice(line).ok()?;
        if value.get("t").and_then(|t| t.as_str()) != Some("hello") {
            return None;
        }
        Some(Self {
            plugin: value.get("plugin")?.as_str()?.to_string(),
            name: value
                .get("name")
                .and_then(|n| n.as_str())
                .unwrap_or_default()
                .to_string(),
            contract_version: value.get("contract_version")?.as_str()?.to_string(),
            kind: match value.get("kind").and_then(|k| k.as_str()) {
                Some("orchestrator") => ClientKind::Orchestrator,
                Some("panel") => ClientKind::Panel,
                _ => ClientKind::Plugin,
            },
        })
    }

    /// 契约版本是否被本宿主兼容（同主版本内兼容）。
    pub fn is_compatible(&self) -> bool {
        is_compatible(&self.contract_version)
    }
}

/// 插件子进程句柄：child + 宿主持有的 stdin + stdout 行通道。
///
/// `child` / `stdin` / `lines` 是 `pub(crate)`：监督循环（[`super`]）要在同一个
/// `select!` 里同时等「子进程退出」与「插件说话」，两个字段必须能分开可变借用
/// （与 agent-runner 的 `PiProcessParts` 同一个理由）。
pub struct PluginProcess {
    pub(crate) child: Child,
    pub(crate) stdin: Option<ChildStdin>,
    pub(crate) lines: mpsc::UnboundedReceiver<Vec<u8>>,
    read_loop: JoinHandle<()>,
    spec: PluginSpec,
}

impl std::fmt::Debug for PluginProcess {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PluginProcess")
            .field("plugin_id", &self.spec.plugin_id)
            .field("pid", &self.pid())
            .finish_non_exhaustive()
    }
}

impl PluginProcess {
    /// spawn 插件子进程（stdin/stdout 管道，`kill_on_drop` 兜底防孤儿）。
    pub async fn spawn(spec: &PluginSpec) -> Result<Self, PluginError> {
        spec.validate()?;
        let mut command = Command::new(&spec.program);
        command.args(&spec.args);
        // 宿主持有的 stdin + 读得着的 stdout；**stderr 丢进 null**（code review P2-11）：
        // 原先 `inherit()` 让插件直接写宿主的 stderr，64KB 管道一写满，多话的插件就
        // 卡在自己的 stderr 上（表现为插件「卡住」/握手超时），而宿主的日志也被插件
        // 的噪音污染。插件自己的日志归插件自己管（写文件/自己的通道），宿主只收
        // stdout 上那条 JSONL 契约面——与 agent-runner 的 `PiProcess` 同一纪律。
        command
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true);
        // 插件侧契约：告诉它自己是谁、父进程是谁（父死子陪葬）。
        command.env(PLUGIN_ID_ENV, &spec.plugin_id);
        command.env(HOST_PID_ENV, std::process::id().to_string());
        for (key, value) in &spec.env {
            command.env(key, value);
        }

        let mut child = command.spawn().map_err(|source| PluginError::Spawn {
            plugin_id: spec.plugin_id.clone(),
            program: spec.describe(),
            source: CoreError::Io(source),
        })?;
        let stdin = child.stdin.take();
        let stdout = child.stdout.take().ok_or_else(|| PluginError::Spawn {
            plugin_id: spec.plugin_id.clone(),
            program: spec.describe(),
            source: CoreError::Io(std::io::Error::other("plugin child has no stdout pipe")),
        })?;

        // stdout 读循环：按行切（JSONL），送进无界通道。
        // 通道断开（EOF / 插件退出）即「插件不说话了」，由监督循环解释。
        let (tx, rx) = mpsc::unbounded_channel::<Vec<u8>>();
        let read_loop = tokio::spawn(async move {
            let mut reader = BufReader::new(stdout);
            let mut line = Vec::new();
            loop {
                line.clear();
                match reader.read_until(b'\n', &mut line).await {
                    Ok(0) => break, // EOF：插件关了 stdout（通常是退出前）
                    Ok(_) => {
                        let trimmed = trim_line(&line);
                        if trimmed.is_empty() {
                            continue;
                        }
                        if tx.send(trimmed).is_err() {
                            break; // 监督者没了：读循环没有存在意义
                        }
                    }
                    Err(_) => break,
                }
            }
        });

        Ok(Self {
            child,
            stdin,
            lines: rx,
            read_loop,
            spec: spec.clone(),
        })
    }

    pub fn spec(&self) -> &PluginSpec {
        &self.spec
    }

    pub fn pid(&self) -> Option<u32> {
        self.child.id()
    }

    /// 等 `hello` 握手行（超时 / 提前退出 / 形状不对都算握手失败）。
    ///
    /// 期间插件说的别的行只记录不打断——握手是第一跳，不是唯一一跳。
    pub async fn handshake(&mut self) -> Result<super::PluginHello, PluginError> {
        let timeout = self.spec.handshake_timeout;
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let line = tokio::time::timeout_at(deadline, self.lines.recv())
                .await
                .map_err(|_| PluginError::Handshake {
                    plugin_id: self.spec.plugin_id.clone(),
                    timeout_ms: timeout.as_millis() as u64,
                    reason: "no hello line before the deadline".into(),
                })?;
            let Some(line) = line else {
                return Err(self.exited_before_handshake());
            };
            match super::PluginHello::parse(&line) {
                Some(hello) if hello.is_compatible() => return Ok(hello),
                Some(hello) => {
                    return Err(PluginError::HandshakeRejected {
                        plugin_id: self.spec.plugin_id.clone(),
                        reason: format!(
                            "plugin speaks contract {} but this host serves {CONTRACT_VERSION}",
                            hello.contract_version
                        ),
                    })
                }
                None => continue, // 不是 hello：继续等
            }
        }
    }

    /// 读循环结束了还没等到 hello：区分「退出」与「还在跑但不说话」。
    fn exited_before_handshake(&mut self) -> PluginError {
        // `try_wait` 不阻塞：已经退出的插件能给出退出状态。
        match self.child.try_wait() {
            Ok(Some(status)) => PluginError::ExitedBeforeHandshake {
                plugin_id: self.spec.plugin_id.clone(),
                status: status.to_string(),
            },
            _ => PluginError::Handshake {
                plugin_id: self.spec.plugin_id.clone(),
                timeout_ms: self.spec.handshake_timeout.as_millis() as u64,
                reason: "plugin closed its stdout before the handshake".into(),
            },
        }
    }

    /// 收下一行插件输出（`None` = 读循环结束）。监督循环的 select 分支。
    pub async fn next_line(&mut self) -> Option<Vec<u8>> {
        self.lines.recv().await
    }

    /// 等子进程退出。监督循环的 select 分支。
    pub async fn wait(&mut self) -> Result<Option<std::process::ExitStatus>, CoreError> {
        self.child.wait().await.map(Some).map_err(CoreError::Io)
    }

    /// **有序停机**：关 stdin（请求有序停机）→ 宽限等待 → 强杀。
    ///
    /// ADR-0004 决策 4：关 stdin 即请求有序停机。插件见到 EOF 自己收尾退出；
    /// 宽限内不退出的才强杀（父死子陪葬的硬边界）。
    pub async fn shutdown(mut self, grace: Duration) -> Result<Option<std::process::ExitStatus>, CoreError> {
        self.close_stdin().await;
        match tokio::time::timeout(grace, self.child.wait()).await {
            Ok(status) => status.map(Some).map_err(CoreError::Io),
            Err(_) => {
                let _ = self.child.kill().await;
                self.child.wait().await.map(Some).map_err(CoreError::Io)
            }
        }
    }

    /// 只关 stdin（不等待）：请求有序停机的最小动作。
    pub async fn close_stdin(&mut self) {
        if let Some(mut stdin) = self.stdin.take() {
            let _ = stdin.flush().await;
            drop(stdin); // EOF 送到插件侧
        }
    }

    /// 强杀（不等宽限；停机路径的兜底）。
    pub async fn kill(&mut self) {
        let _ = self.child.kill().await;
    }
}

impl Drop for PluginProcess {
    fn drop(&mut self) {
        self.read_loop.abort();
    }
}

/// 去掉行尾的 `\r\n` / `\n` 与首尾空白。
fn trim_line(line: &[u8]) -> Vec<u8> {
    let text = String::from_utf8_lossy(line);
    text.trim().as_bytes().to_vec()
}
