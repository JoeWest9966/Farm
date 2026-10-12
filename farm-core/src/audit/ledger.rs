//! `ledger record` 子进程客户端（ADR-0004 决策 2：Farm 子进程调 `ledger record/read`）。
//!
//! - 程序路径**可注入**（测试传 fake `ledger` 替身；生产用 PATH 上的 `ledger`）；
//! - stdin 收 NDJSON 流（一行一笔），stdout 是单份 envelope（六字段顶层 + 五字段错误，
//!   `ok == (exitCode == 0)`，码表 `OK/0`、`INPUT_INVALID/3`、`DEGRADED/10`）；
//! - **有界**：写超时/卡死都由 [`LedgerWriter::timeout`] 硬杀——审计是旁路，
//!   绝不允许一个挂住的 `ledger` 把宿主发布路径拖死（ADR-0005 §3：Kron 写失败不阻塞）；
//! - envelope 解析**宽松**（未知字段忽略）：Kron 扩字段不该让 Farm 误判成写失败；
//!   判真伪以退出码 + `ok` 为准。

use std::io::{Read, Write};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use serde::Deserialize;

use crate::audit::schema::KronRecord;
use crate::audit::{DEFAULT_LEDGER_TIMEOUT, LedgerProgram};
use crate::error::CoreError;

/// `ledger record` 一次调用的结果。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecordResult {
    /// 账本认下这批（`ok == (exitCode == 0)`）。
    pub ok: bool,
    /// 退出码（Kron 码表：`OK/0`、`INPUT_INVALID/3`、`MANIFEST_BROKEN/12`、`DEGRADED/10`）。
    pub exit_code: i64,
    /// 本批里被幂等自然键吞掉的笔数（同键同摘要 → `duplicate`）。
    pub duplicates: usize,
    /// 失败时的错误消息（envelope 的 `error.message`）。
    pub message: Option<String>,
}

/// `ledger` CLI 的 envelope 形状（六字段顶层 + 五字段错误；宽松解析）。
#[derive(Clone, Debug, Default, Deserialize)]
pub struct LedgerEnvelope {
    #[serde(default)]
    pub ok: bool,
    #[serde(default)]
    pub exit_code: i64,
    #[serde(default)]
    pub data: Option<EnvelopeData>,
    #[serde(default)]
    pub error: Option<EnvelopeError>,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct EnvelopeData {
    #[serde(default)]
    pub results: Vec<EnvelopeResult>,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct EnvelopeResult {
    #[serde(default)]
    pub seq: Option<i64>,
    #[serde(default)]
    pub duplicate: bool,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct EnvelopeError {
    #[serde(default)]
    pub code: Option<String>,
    #[serde(default)]
    pub message: Option<String>,
}

/// `ledger` 子进程客户端（句柄可廉价复制）。
#[derive(Clone, Debug)]
pub struct LedgerWriter {
    program: LedgerProgram,
    timeout: Duration,
    enabled: bool,
}

impl LedgerWriter {
    /// 禁用的客户端：不起子进程（宿主没配 `ledger` 时的默认态）。
    pub fn disabled() -> Self {
        Self {
            program: LedgerProgram::new("ledger"),
            timeout: DEFAULT_LEDGER_TIMEOUT,
            enabled: false,
        }
    }

    /// 指向一个 `ledger` 程序，`record` 子命令 + 默认超时。
    pub fn new(program: impl Into<std::path::PathBuf>) -> Self {
        Self {
            program: LedgerProgram::new(program),
            timeout: DEFAULT_LEDGER_TIMEOUT,
            enabled: true,
        }
    }

    /// 覆写子命令参数（默认 `["record"]`）。
    pub fn with_args(mut self, args: Vec<String>) -> Self {
        self.program = LedgerProgram {
            path: self.program.path().to_path_buf(),
            args,
        };
        self
    }

    /// 覆写硬超时。
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// `ledger` 程序路径（诊断/测试可读）。
    pub fn program(&self) -> String {
        self.program.path().display().to_string()
    }

    pub fn timeout(&self) -> Duration {
        self.timeout
    }

    /// 实际会执行的 argv（除程序名外的参数；调用形状的可断言面）。
    pub fn args(&self) -> &[String] {
        self.program.args()
    }

    /// 调 `ledger record`：一批 Kron 记录一次子进程，stdin 走 NDJSON 流。
    ///
    /// 空批 = 一次调用都不发（`ok` 且零笔）。失败形状只有两种：子进程起不来/卡死/非零
    /// 退出（[`CoreError::Config`] / [`CoreError::Io`]）与账本拒收（`ok == false`）。
    pub fn record(&self, records: &[KronRecord]) -> Result<RecordResult, CoreError> {
        if !self.enabled {
            return Err(CoreError::Config(
                "ledger writer is disabled (no `ledger` program configured)".into(),
            ));
        }
        if records.is_empty() {
            return Ok(RecordResult {
                ok: true,
                exit_code: 0,
                duplicates: 0,
                message: None,
            });
        }

        let payload: Vec<String> = records.iter().map(|r| r.to_ndjson()).collect();
        let mut command = Command::new(self.program.path());
        command
            .args(self.program.args())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());

        let mut child = child_spawn(&mut command, self.program.path())?;
        // 管道在主线程先取走：写 stdin / 读 stdout 各占一个线程，`child` 留在主线程
        // 轮询退出——超时就地硬杀（旁路不许挂住主流程）。
        let stdin = child.stdin.take();
        let stdout = child.stdout.take();

        let writer = thread::spawn(move || -> std::io::Result<()> {
            if let Some(mut stdin) = stdin {
                for line in &payload {
                    stdin.write_all(line.as_bytes())?;
                    stdin.write_all(b"\n")?;
                }
                stdin.flush()?;
            } // drop 即关管道：告知 EOF
            Ok(())
        });
        let reader = thread::spawn(move || -> std::io::Result<Vec<u8>> {
            let mut buf = Vec::new();
            if let Some(mut stdout) = stdout {
                stdout.read_to_end(&mut buf)?;
            }
            Ok(buf)
        });

        // 主线程持句柄轮询退出：超时就地硬杀（旁路不许挂住主流程）。
        let deadline = Instant::now() + self.timeout;
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) => {}
                Err(e) => return Err(CoreError::Io(e)),
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                let _ = writer.join();
                let _ = reader.join();
                return Err(CoreError::Config(format!(
                    "`{} record` timed out after {:?} and was killed",
                    self.program.path().display(),
                    self.timeout
                )));
            }
            thread::sleep(Duration::from_millis(10));
        };

        let _ = writer.join().map_err(|_| {
            CoreError::Io(std::io::Error::other("ledger stdin writer thread panicked"))
        })?;
        let stdout = reader
            .join()
            .map_err(|_| CoreError::Io(std::io::Error::other("ledger stdout reader panicked")))??;

        Ok(interpret(status.code(), &stdout))
    }
}

/// spawn 一个子进程，把「程序不存在/不可执行」翻译成宿主错误形状。
fn child_spawn(command: &mut Command, program: &std::path::Path) -> Result<Child, CoreError> {
    command.spawn().map_err(|e| {
        CoreError::Config(format!(
            "cannot spawn `{}` (audit sidecar is a subprocess; is it installed?): {e}",
            program.display()
        ))
    })
}

/// 退出码 + stdout envelope → [`RecordResult`]。
///
/// `ok == (exitCode == 0)`（Kron 契约）；stdout 解析不了时以退出码为准，
/// 不把「解析失败」误判成「账本拒收」之外的第三种状态。
fn interpret(exit_code: Option<i32>, stdout: &[u8]) -> RecordResult {
    let envelope: Option<LedgerEnvelope> = serde_json::from_slice(stdout).ok();
    let exit_code = i64::from(exit_code.unwrap_or(-1));
    let ok = exit_code == 0 && envelope.as_ref().is_none_or(|e| e.ok);
    let duplicates = envelope
        .as_ref()
        .and_then(|e| e.data.as_ref())
        .map(|d| d.results.iter().filter(|r| r.duplicate).count())
        .unwrap_or(0);
    let message = envelope
        .as_ref()
        .and_then(|e| e.error.as_ref())
        .and_then(|e| e.message.clone());
    RecordResult {
        ok,
        exit_code,
        duplicates,
        message,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use farm_sdk::events::{EventEnvelope, EventKind, NewEvent};

    fn dispatch_record(id: &str) -> KronRecord {
        KronRecord::from_event(&EventEnvelope::from_new(
            NewEvent::new(EventKind::Dispatch, "agent-runner").with_ticket("20"),
            id,
            "2026-01-01T00:00:00Z",
        ))
        .expect("dispatch maps")
    }

    #[test]
    fn an_empty_batch_never_spawns_a_subprocess() {
        let writer = LedgerWriter::new("/definitely/not/installed/ledger");
        let result = writer.record(&[]).expect("empty batch is a no-op");
        assert!(result.ok);
        assert_eq!(result.duplicates, 0);
    }

    #[test]
    fn a_missing_program_is_an_error_not_a_panic() {
        let writer = LedgerWriter::new("/definitely/not/installed/ledger");
        let err = writer
            .record(&[dispatch_record("evt-1")])
            .expect_err("spawn failure must surface");
        assert!(err.to_string().contains("ledger"), "{err}");
    }

    #[test]
    fn the_default_args_are_the_record_subcommand() {
        let writer = LedgerWriter::new("ledger");
        assert_eq!(writer.args(), ["record"]);
        assert_eq!(writer.timeout(), DEFAULT_LEDGER_TIMEOUT);
        assert!(writer.is_enabled());
        assert!(!LedgerWriter::disabled().is_enabled());
    }

    #[test]
    fn a_nonzero_exit_without_an_envelope_is_still_a_failure() {
        let result = interpret(Some(3), b"not json at all");
        assert!(!result.ok, "退出码非零 = 账本没认下");
        assert_eq!(result.exit_code, 3);
        assert_eq!(result.duplicates, 0);
    }

    #[test]
    fn an_ok_envelope_reports_its_duplicates() {
        let stdout = br#"{"ok":true,"exitCode":0,"data":{"results":[{"seq":7,"duplicate":true}]},"error":null}"#;
        let result = interpret(Some(0), stdout);
        assert!(result.ok);
        assert_eq!(result.duplicates, 1);
    }

    #[test]
    fn an_error_envelope_carries_its_message() {
        let stdout = br#"{"ok":false,"exitCode":3,"error":{"code":"INPUT_INVALID","message":"unknown field"}}"#;
        let result = interpret(Some(3), stdout);
        assert!(!result.ok);
        assert_eq!(result.message.as_deref(), Some("unknown field"));
    }

    #[test]
    fn a_signal_killed_child_is_a_failure() {
        let result = interpret(None, b"");
        assert!(!result.ok, "被信号杀掉的子进程不算入账");
    }
}
