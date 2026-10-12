//! pi 子进程：spawn、stdin 写入、stdout 读循环、启动版本探测。
//!
//! ADR-0004 决策 1：agent-runner **自 spawn、自监督**。pi 子进程**不是插件**——它不進
//! 插件重试策略（决策 4 末句），生命周期完全由本模块 + [`crate::agent_runner::ladder`]
//! 的判据阶梯持有。
//!
//! 收尾三段式（研究票 §6 的未决项）：关 stdin → 宽限等待 → 强杀。

use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use tokio::io::AsyncBufReadExt;
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use super::accounting::Usage;
use super::credentials::EnvPolicy;
use super::rpc::{CommandKind, Frame, FrameReader, RpcCommand};
use crate::error::CoreError;
use crate::now_rfc3339;

/// spawn 计划：程序、参数、环境、cwd（测试把 `program` 指向 fake pi）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PiSpawnPlan {
    pub program: String,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    pub env_policy: EnvPolicy,
    pub cwd: Option<PathBuf>,
}

impl PiSpawnPlan {
    pub fn new(program: impl Into<String>, args: Vec<String>) -> Self {
        Self {
            program: program.into(),
            args,
            env: Vec::new(),
            env_policy: EnvPolicy::default(),
            cwd: None,
        }
    }

    /// 追加一对只进 pi 子进程的 env（凭据就走这里）。
    pub fn with_env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.push((key.into(), value.into()));
        self
    }

    pub fn describe(&self) -> String {
        format!("{} {}", self.program, self.args.join(" "))
    }
}

/// 启动版本探测结果：**记录偏差，不硬失败**（ADR-0004 决策 5 的偏斜容忍）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PiVersionProbe {
    /// 规格钉的版本（`1.0.2`）。
    pub expected: String,
    /// pi 自报版本（`get_state` 的 `version`/`piVersion`/`rpcVersion`/`protocolVersion`）。
    pub reported: Option<String>,
    pub session_id: Option<String>,
    /// 偏差清单（人读；进 attempt 记录与事件载荷）。
    pub deviations: Vec<String>,
    pub probed_at: String,
}

impl PiVersionProbe {
    pub fn new(expected: impl Into<String>) -> Self {
        Self {
            expected: expected.into(),
            reported: None,
            session_id: None,
            deviations: Vec::new(),
            probed_at: now_rfc3339(),
        }
    }

    pub fn note_deviation(&mut self, deviation: impl Into<String>) {
        self.deviations.push(deviation.into());
    }

    /// 版本是否与规格钉一致（不一致只记录，不影响 attempt 生死）。
    pub fn is_aligned(&self) -> bool {
        self.reported.as_deref() == Some(self.expected.as_str())
    }

    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "expected": self.expected,
            "reported": self.reported,
            "sessionId": self.session_id,
            "aligned": self.is_aligned(),
            "deviations": self.deviations,
            "probedAt": self.probed_at,
        })
    }
}

/// pi 子进程句柄：child + stdin + 读循环出来的帧通道。
pub struct PiProcess {
    child: Child,
    stdin: Option<ChildStdin>,
    frames: mpsc::UnboundedReceiver<Vec<u8>>,
    read_loop: JoinHandle<()>,
    plan: PiSpawnPlan,
}

/// 拆开后的部件（监督循环里 `select!` 不能同时可变借用同一个结构体）。
pub struct PiProcessParts {
    pub child: Child,
    pub stdin: Option<ChildStdin>,
    pub frames: mpsc::UnboundedReceiver<Vec<u8>>,
    pub read_loop: JoinHandle<()>,
    pub plan: PiSpawnPlan,
}

impl PiProcess {
    /// spawn `pi --mode rpc`（`plan.program` 可指向 fake pi）。
    pub async fn spawn(plan: PiSpawnPlan) -> Result<Self, CoreError> {
        let mut command = Command::new(&plan.program);
        command.args(&plan.args);
        if let Some(cwd) = &plan.cwd {
            command.current_dir(cwd);
        }
        // 凭据 env 只进子进程（#17 断言面）。
        plan.env_policy.apply(&mut command);
        for (key, value) in &plan.env {
            command.env(key, value);
        }
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);

        let mut child = command.spawn().map_err(|source| CoreError::Io(source))?;
        let stdin = child.stdin.take();
        let stdout = child.stdout.take().ok_or_else(|| {
            CoreError::Io(std::io::Error::other("pi child has no stdout pipe"))
        })?;

        // 读循环：read_until(b'\n') 取帧，送进无界通道。
        // 通道断开（EOF）即「读循环健康度」信号，由判据阶梯解释。
        let (tx, rx) = mpsc::unbounded_channel::<Vec<u8>>();
        let read_loop = tokio::spawn(async move {
            let mut reader = FrameReader::new(stdout);
            loop {
                match reader.next_frame().await {
                    Ok(Some(frame)) => {
                        if tx.send(frame).is_err() {
                            break; // 监督者没了：读循环没有存在意义
                        }
                    }
                    Ok(None) => break, // EOF
                    Err(_) => break,   // IO 错误同样交给判据阶梯
                }
            }
        });

        Ok(Self {
            child,
            stdin,
            frames: rx,
            read_loop,
            plan,
        })
    }

    pub fn plan(&self) -> &PiSpawnPlan {
        &self.plan
    }

    pub fn pid(&self) -> Option<u32> {
        self.child.id()
    }

    pub fn into_parts(self) -> PiProcessParts {
        PiProcessParts {
            child: self.child,
            stdin: self.stdin,
            frames: self.frames,
            read_loop: self.read_loop,
            plan: self.plan,
        }
    }

    /// 写一条命令（JSONL，帧尾只补 `\n`）。
    pub async fn send(&mut self, command: &RpcCommand) -> Result<(), CoreError> {
        self.send_kind(&command.id, command.kind.clone()).await
    }

    pub async fn send_kind(
        &mut self,
        id: &str,
        kind: CommandKind,
    ) -> Result<(), CoreError> {
        let line = serde_json::to_vec(&RpcCommand {
            id: id.to_string(),
            kind,
        })
        .map_err(|e| CoreError::Config(format!("rpc command must serialize: {e}")))?;
        let stdin = self.stdin.as_mut().ok_or_else(|| {
            CoreError::Io(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "pi stdin is closed",
            ))
        })?;
        tokio::io::AsyncWriteExt::write_all(stdin, &line)
            .await
            .map_err(|source| CoreError::Io(source))?;
        tokio::io::AsyncWriteExt::write_all(stdin, b"\n")
            .await
            .map_err(|source| CoreError::Io(source))?;
        tokio::io::AsyncWriteExt::flush(stdin)
            .await
            .map_err(|source| CoreError::Io(source))?;
        Ok(())
    }

    /// 关 stdin（请求有序停机）。
    pub async fn close_stdin(&mut self) {
        if let Some(mut stdin) = self.stdin.take() {
            use tokio::io::AsyncWriteExt;
            let _ = stdin.flush().await;
            drop(stdin);
        }
    }

    pub async fn kill(&mut self) {
        let _ = self.child.kill().await;
    }

    pub async fn wait(&mut self) -> Result<Option<std::process::ExitStatus>, CoreError> {
        self.child.wait().await.map(Some).map_err(CoreError::Io)
    }
}

/// 启动探测：`get_state`，拿版本与会话 id。
///
/// 0.84.3 的 `get_state` 不带版本字段 → 记偏差；探测超时也记偏差。**都不让 spawn 失败**。
pub async fn probe_version(
    parts: &mut PiProcessParts,
    expected: &str,
    timeout: Duration,
) -> (PiVersionProbe, Vec<Vec<u8>>) {
    let mut probe = PiVersionProbe::new(expected);
    let request_id = format!("probe-{}", std::process::id());
    let command = RpcCommand {
        id: request_id.clone(),
        kind: CommandKind::GetState,
    };
    let line = match serde_json::to_vec(&command) {
        Ok(line) => line,
        Err(e) => {
            probe.note_deviation(format!("get_state probe could not serialize: {e}"));
            return (probe, Vec::new());
        }
    };
    let sent = match parts.stdin.as_mut() {
        Some(stdin) => {
            use tokio::io::AsyncWriteExt;
            stdin
                .write_all(&line)
                .await
                .and_then(|_| stdin.write_all(b"\n"))
                .await
                .and_then(|_| stdin.flush())
                .await
                .is_ok()
        }
        None => false,
    };
    if !sent {
        probe.note_deviation("get_state probe could not be written to pi stdin");
        return (probe, Vec::new());
    }

    let deadline = tokio::time::Instant::now() + timeout;
    let mut leftovers: Vec<Vec<u8>> = Vec::new();
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            probe.note_deviation(format!(
                "get_state probe timed out after {}ms",
                timeout.as_millis()
            ));
            return (probe, leftovers);
        }
        match tokio::time::timeout(remaining, parts.frames.recv()).await {
            Ok(Some(frame)) => match super::rpc::parse_frame(&frame) {
                Frame::Response(response) if response.command == "get_state" => {
                    if let Some(data) = &response.data {
                        probe.reported = data
                            .get("version")
                            .or_else(|| data.get("piVersion"))
                            .or_else(|| data.get("rpcVersion"))
                            .or_else(|| data.get("protocolVersion"))
                            .and_then(|v| v.as_str())
                            .map(str::to_string);
                        probe.session_id = data
                            .get("sessionId")
                            .and_then(|v| v.as_str())
                            .map(str::to_string);
                    }
                    match &probe.reported {
                        Some(reported) if reported == expected => {}
                        Some(reported) => probe.note_deviation(format!(
                            "pi reports version {reported}; farm pins the documented {expected} wire protocol — deviations are recorded, not enforced"
                        )),
                        None => probe.note_deviation(format!(
                            "pi did not report a version in get_state; farm pins the documented {expected} wire protocol — deviations are recorded, not enforced"
                        )),
                    }
                    return (probe, leftovers);
                }
                _ => leftovers.push(frame),
            },
            Ok(None) => {
                probe.note_deviation("pi stdout closed before the get_state probe answered");
                return (probe, leftovers);
            }
            Err(_) => {
                probe.note_deviation(format!(
                    "get_state probe timed out after {}ms",
                    timeout.as_millis()
                ));
                return (probe, leftovers);
            }
        }
    }
}

/// 从 pi 的 usage JSON 里取一个数（探测/诊断用）。
pub fn usage_total(value: &serde_json::Value) -> Option<u64> {
    Usage::from_pi_json(value).map(|usage| usage.total())
}

/// 让读循环任务结束（不阻塞监督者）。
pub fn abort_read_loop(read_loop: JoinHandle<()>) {
    read_loop.abort();
}
