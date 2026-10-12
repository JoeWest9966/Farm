# progress.md — ticket #16 Agent-runner 本地后端与判据阶梯

worktree `/var/opt/farm-wt/t16`，branch `farm/t16`（base `implement/spec-10` tip 3b55b9a）。
TDD 红→绿，竖切片，每片一次 checkpoint commit。

## 接缝清单（seams）

| 接缝 | 状态 | 说明 |
| --- | --- | --- |
| framing（帧定界） | done | `agent_runner::rpc::{drain_frames, FrameReader}`：只按 `\n`，禁智能 line reader |
| fake pi 替身 | done | `src/bin/fake-pi.rs`：说文档化 1.0.2 wire protocol，场景经子进程 env 注入 |
| 版本探测 | done（替身侧） | `get_state` 的 `version`；缺字段/版本不符只记录偏差 |
| clear_queue 往返 | done（替身侧） | 宿主侧 API 待 service 片 |
| env 只进 pi 子进程 | done（替身侧） | 宿主侧 `credentials::EnvPolicy` 待 credentials 片 |
| process（spawn + 读循环） | done | `PiProcess::{spawn,send,close_stdin,kill,wait}` + `probe_version`/`round_trip`；读循环 = `read_until(b'\n')`；三段式收尾 |
| 判据阶梯四梯 | done | 前置 = 帧数度量；两口钟 + abort 探针在 `run_ladder`；fork 找回归属 `supervise` 外层循环 |
| 完成双条件 | done | `JudgementLadder::is_settled`：`agent_settled` 且最后权威 assistant `stopReason==="stop"` | `agent_settled` + 最后权威 assistant `stopReason==="stop"` |
| token 流式记账 | done | `ladder::EventSignals` + `accounting::TokenAccounting` 接进读循环；settle 时 `get_session_stats` 对账 | 绝对累计、宁少报不虚报、`parentToolCallId` 嵌套用量 |
| disposition 回流 | done | 监督循环收到 pi 响应时发 `Steer` 事件（channel/message/disposition/accepted）| `prompt/steer/follow_up` 响应 disposition 进事件总线 |
| settle 期注入推迟 | done | `deferred` disposition 回流 + 替身 `_deferredSettledActions` 推迟执行有日志为证 | fake pi 模拟 `_deferredSettledActions` |
| attempts 持久化（migration v6） | done | `agent_runner::migrations::v6_agent_runner_attempts` + `store::AttemptStore` | running/succeeded/failed/interrupted + token 快照 + accounting_truncated |
| 宿主三条路由 | pending | /v1/agent-runner/{dispatches,steer,abort} |
| 真 pi 冒烟 | pending | 记录版本偏差，不断言失败 |

## 已完成（done）

- **slice 1** `feat(agent-runner): JSONL 帧定界 + fake pi RPC 替身`
  - `farm-core/src/agent_runner/rpc.rs`：`drain_frames`（纯函数，只按 `\n`，剥 `\r`，跳空行）、
    `FrameReader`（`AsyncBufRead::read_until(b'\n')`）、`parse_frame`、命令/响应形状。
  - `farm-sdk` 加法类型：`Disposition`（开放字符串 + 已知值判据）、`SteerResponse`、`AbortResponse`。
  - `farm-core/src/bin/fake-pi.rs`：替身进程；场景 `normal/silence/total-clock/abort-probe/
    fork-recovery/token-streaming/no-stop-reason/deferred-settle/reject-steer/separators/quiet`；
    `agent_end` 多一条 system 消息（放最后）；tool 用量带 `parentToolCallId`；
    settle 期注入进 `_deferredSettledActions` 并推迟到 settle 后执行；`FAKE_PI_LOG` 记录命令与 env。
  - 测试：`tests/agent_runner_fake_pi.rs` 6 条 + `rpc.rs` 单测 6 条，全绿。

## 环境备注

- `CARGO_INCREMENTAL=0`：多 worktree 共用 `/opt/Warehousetui/target` 时，共享 incremental 目录
  会造成假「unresolved import」编译错误；本票所有 cargo 命令都带这个环境变量。
- `pi` 0.84.3（/usr/bin/pi）；规格钉 1.0.2 → 一切按文档化 1.0.2 实现，真 pi 只做探测+记录。

## 剩余（remaining）

## 已完成（追加）

- **slice 1（process）** `feat(agent-runner): pi 子进程 spawn + 版本探测 + 读循环`
  - `process.rs` 接入 `mod.rs`（此前是未编译的孤儿文件）；`ChildStdout` 不是 `AsyncBufRead`
    → 包 `tokio::io::BufReader`，读循环只按 `\n` 取帧（`rpc::FrameReader`）。
  - `probe_version` 收敛到通用 `round_trip`（写命令 → 等配对响应 → **错过的帧原样回放**，
    不丢 `agent_start`/`message_end`）。缺版本/版本不符/探测无答 → 记偏差，不断失败。
  - `credentials::EnvPolicy::apply` 改泛型在 `EnvTarget` 上（`std`/`tokio` 两种 Command 都能接；
    原签名收 `tokio::process::Command` 编不过，这是本片接上的第一个洞）。
  - 测试：`tests/agent_runner_process.rs` 7 条，全绿。

- **slice 2（判据阶梯内核）** `feat(agent-runner): 判据阶梯——两口钟 + abort 探针独立死线`
  - `ladder.rs`：纯判决内核（不碰 I/O），`JudgementLadder::{note_frame,note_authoritative_message,
    note_settled,is_settled,check,next_wait,request_abort}`。
  - 读循环健康度前置 = 帧数度量（`frames_seen()==0` = 启动即坏）；静默钟看「离上一帧多久」，
    总钟看「attempt 活了多久」（流式场景静默钟不到点、总钟照样到）；abort 探针 30s
    **独立死线**，来帧不重置。
  - `observe_event` 一帧过筛：`agent_settled` / `message_end`（权威）/ `agent_end.messages[]`
    （**system 消息滤掉**）/ `tool_execution_end`（`parentToolCallId` 嵌套用量）/
    `compaction_end`；`message_update` 的累积快照**不入账**。
  - 测试：`ladder.rs` 内 15 条单测，全绿。

- **slice 3（attempts 持久化 v6）** `feat(agent-runner): attempts 表（migration v6）+ 宿主私有存储`
  - `migrations.rs`：`v6_agent_runner_attempts`，SQL 住在服务域（per-service pattern，同 #13）。
    `state` 闭集 `running/succeeded/failed/interrupted` 由 `CHECK` 在存储层把门。
  - `store.rs`：`AttemptStore`（upsert/find/list/list_for_ticket/has_running_for_ticket）+
    `mark_running_as_interrupted`（崩溃恢复：running → interrupted + accounting_truncated=1，
    最后快照原样保留；终态 attempt 不动；幂等）。
  - 契约态 ↔ 持久态映射：`Settled→succeeded`、`Aborted→failed`（原因进 `note`）。
  - 测试：migrations 2 条 + store 7 条，全绿。

- **slice 4（service + 判据阶梯驱动）** `feat(agent-runner): 监督循环——判据阶梯四梯 + 记账 + disposition 回流`
  - `service.rs`：`AgentRunner` 句柄（挂在 AppState 上）+ 每 attempt 一个监督任务。
    `dispatch/steer/follow_up/abort/clear_queue/attempt/attempts_for_ticket/recover_interrupted`。
  - 事件回流一律走 `AppState::publish`（持久化路径：先落事件日志再广播）；句柄不反向持有
    AppState（构造期循环），发布面由调用方以 `&AppState` 传入，监督任务持一份克隆。
  - 判据阶梯四梯：读循环健康度前置（`frames_seen()==0` = 启动即坏，不进 fork）→ 静默钟
    （发 abort）→ 总钟（强杀，不等回应）→ abort 探针（独立死线，强杀）→ fork 找回
    （重 spawn `--session` + fork + 重发 prompt；**阶梯不重建**，总钟预算跨 fork 有效）。
  - 收尾三段式：关 stdin → 宽限等待 → 强杀。settle 时 `get_session_stats` 权威对账。
  - `RunnerError` 投影到契约闭集（not_found / conflict / service_unavailable / invalid_request）。
  - 测试：`tests/agent_runner_dispatch.rs` 17 条端到端（真实 fake pi 子进程），全绿。
  - 踩过的坑（记下来免得再犯）：`mpsc::UnboundedSender::send().is_err()` 与 `if !build(...)`
    组合把成功当失败；`parentToolCallId` 住在 **usage 对象里**而非事件顶层。

## 剩余（remaining）

宿主三条路由 → 真 pi 冒烟 → progress/验收。
