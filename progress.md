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
| process（spawn + 读循环） | pending | |
| 判据阶梯四梯 | pending | 读循环健康度前置 / 两口钟 / abort 探针 / fork 找回 |
| 完成双条件 | pending | `agent_settled` + 最后权威 assistant `stopReason==="stop"` |
| token 流式记账 | pending | 绝对累计、宁少报不虚报、`parentToolCallId` 嵌套用量 |
| disposition 回流 | pending | `prompt/steer/follow_up` 响应 disposition 进事件总线 |
| settle 期注入推迟 | pending | fake pi 模拟 `_deferredSettledActions` |
| attempts 持久化（migration v6） | pending | running/succeeded/failed/interrupted + token 快照 + accounting_truncated |
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

slice 2 起：process → ladder → accounting → store → service → credentials → 宿主路由 → 真 pi 冒烟。
