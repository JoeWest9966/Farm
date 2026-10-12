# 研究：pi 作为被驱动 agent 运行时的可编程面

> 票：[05 - pi 作为被驱动 agent 运行时的可编程面](../issues/05-pi-runtime-interface.md)
> 一手资料：`/usr/lib/node_modules/@earendil-works/pi-coding-agent/`（本机安装版 **0.84.3**）
> 阅读范围：`README.md`、`docs/sdk.md`、`docs/rpc.md`、`docs/json.md`、`docs/sessions.md`、`examples/sdk/*`、`package.json`、`dist/modes/rpc/` 类型定义

## 结论（先给答案）

**pi 已经内置了 Symphony 那个模式本身。** Symphony 用 Elixir `Port` 起一个 Codex `app-server` 子进程、走 JSON-RPC 双向通信；pi 的对应物是：

**`pi --mode rpc` —— 子进程 + stdin/stdout 双向 JSONL（不是 JSON-RPC 2.0，是行分隔 JSON 命令/响应/事件流）。**

命令行客户端就是它的"app-server"，Rust 宿主把 pi 当黑盒驱动即可，**无需内嵌 Node、无需理解 pi 内部**。另外 pi 还提供三条备选通路（`--mode json` 单向事件流、`--print` 单发、SDK 在进程内），但它们都要求 Rust 宿主直接吃 Node 生态，只有 RPC 模式是语言无关的。所以：

- **Rust 宿主的正解 = RPC 模式**（进程隔离 + 语言无关，与 Symphony 的架构选择同构）。
- **SDK 模式不作为 Farm 的集成面**（它是给 Node/TS 客户端用的；Rust 侧用不到，也无 FFI 桥）。
- **pi 是"每个 agent 一个子进程、一个会话文件"的模型**，`SessionManager` 与 RPC 的 `new_session / switch_session / fork / clone` 覆盖了并发与恢复；但**子进程的生命周期、崩溃对账、并发上限由 Farm 宿主自己管**——pi 不做这些（与 Symphony 的"编排器自己管候选/重试"一致）。

---

## 1. headless / SDK：能否编程式驱动？

**能，且是官方一等公民。** pi 有四种运行模式（`docs/sdk.md` 的 "Run Modes"）：

| 模式 | 入口 | 通信 | 适用 |
|------|------|------|------|
| `InteractiveMode` | SDK 类 | 进程内 | TUI |
| `runPrintMode` | SDK 函数 / `pi -p "..."` | 单发文本 | 一次性跑 |
| `runRpcMode` | SDK 函数 / `pi --mode rpc` | **stdin/stdout JSONL 双向** | **外部程序集成（本票答案）** |
| JSON 事件流 | `pi --mode json "..."` | stdout JSONL 单向 | 集成到别的工具、只看 |

`docs/rpc.md` 开篇原文：

> RPC mode enables headless operation of the coding agent via a JSON protocol over stdin/stdout. This is useful for embedding the agent in other applications, IDEs, or custom UIs.

启动：

```bash
pi --mode rpc [--provider <name>] [--model <pattern>] [--name <name>] [--no-session] [--session-dir <path>]
```

**协议形状（Rust 侧要实现的全部）**：

- 宿主 → pi：stdin 上每行一个 JSON 命令对象，如 `{"id":"req-1","type":"prompt","message":"..."}`。
- pi → 宿主：stdout 上每行一个 JSON 响应（`{"type":"response","command":"prompt","success":true}`）或事件（`{"type":"agent_start"}` …）。
- `id` 字段可选，用于请求/响应配对。

**帧定界是关键坑**（`docs/rpc.md` "Framing"）：**只按 `\n` 切分**，允许输入带尾随 `\r` 时剥掉；**不能用会把 Unicode 行分隔符 `U+2028`/`U+2029` 当换行的通用 line reader**（Node 的 `readline` 就因此不合规）。Rust 侧用 `BufRead::read_until(b'\n')` 或手写缓冲即可，别用某些"智能按行"封装。

**命令清单**（进程控制与查询，非流式驱动的那半）。已对照 `dist/modes/rpc/rpc-types.d.ts` 的 `RpcCommand` 联合类型核实，共 34 个（含 `response` 这一响应类型），与文档完全一致：

`prompt`、`steer`、`follow_up`、`abort`、`new_session`、`get_state`、`get_messages`、`set_model`、`cycle_model`、`get_available_models`、`set_thinking_level`、`cycle_thinking_level`、`get_available_thinking_levels`、`set_steering_mode`、`set_follow_up_mode`、`compact`、`set_auto_compaction`、`set_auto_retry`、`abort_retry`、`bash`、`abort_bash`、`get_session_stats`、`export_html`、`switch_session`、`fork`、`clone`、`get_fork_messages`、`get_entries`、`get_tree`、`get_last_assistant_text`、`set_session_name`、`get_commands`。

**恰好就是 Symphony 的 JSON-RPC 方法表所扮演的角色**（派发 prompt、查询状态、中断、切换会话）。逐条对照 Symphony 设计：`prompt`≈派发任务，`abort`≈中止 worker，`get_session_stats`≈token 记账，`get_entries/get_tree`≈转写/历史，`switch_session/fork/clone`≈恢复与分叉。

---

## 2. 事件流、转写、token 用量可否导出？

**全部可导出。**

### 事件流（stdout JSONL）

事件类型表（`docs/rpc.md` "Events"）共 22 种，分四组：

- **生命周期**：`agent_start`、`agent_end`（含 `messages`、`willRetry`）、`agent_settled`（**真正停稳**：无重试、无 compaction 重试、无排队续跑——这是宿主判断"这个 agent 跑完了"的最可靠信号）。
- **轮次/消息**：`turn_start`、`turn_end`（含 `message` + `toolResults`）、`message_start`、`message_update`（delta 流：`text_delta`/`thinking_delta`/`toolcall_delta`，带累积 `usage`）、`message_end`（**权威消息**）。
- **工具**：`tool_execution_start`、`tool_execution_update`（`partialResult` 是**累积**结果，客户端直接替换显示即可）、`tool_execution_end`（`isError`）。
- **系统**：`queue_update`、`compaction_start/end`、`auto_retry_start/end`、`summarization_retry_*`、`extension_error`、`bash_execution_update`。

> Symphony 的 `CodexStreamEvent` → 状态时间线映射，在 pi 上就是这张表。`agent_settled` 尤其值得当作"任务终态"锚点。

`docs/json.md` 补充了 `--mode json` 的线格式：**首行是 session header**（`{"type":"session","version":3,"id":...,"cwd":...}`），随后是事件；且 `message_update` 是 **delta-only**（为线性流大小省掉了累积快照），要用 `contentIndex`+`delta` 自行拼，`message_end` 才是权威。

### 转写（transcript）

Session 存成 **JSONL 文件**，默认 `~/.pi/agent/sessions/`，按工作目录组织（`docs/sessions.md`）。文件是**树结构**：每个 entry 有 `id`/`parentId`，当前位置是 active leaf，支持原地分支。**这是 append-only、id 稳定的树**，所以——

- `get_entries`（RPC）**支持游标**：传 `since=<上次看到的 entry id>` 只取之后的，**跨客户端重启依然有效**（`docs/rpc.md` 原文：an entry id works as a durable cursor）。含压缩前历史与被放弃的分支。
- `get_tree` 取整棵树（含 `label`）。
- `export_html` 导出 HTML 单文件。

**对 Farm 的意义**：pi 的 session JSONL = 权威转写 + 可增量拉取的游标（Rust 面板做实时/回放都够用），粒度细到 entry id，稳定且跨重启。这正补上 Symphony 规格里"只留内存态、重启即失"的空白的一半（另一半是 Farm 自己的持久化层）。

### token 用量与成本

- **权威聚合**：`get_session_stats` → `tokens{input,output,cacheRead,cacheWrite,total}`、`cost`（金额）、`contextUsage{tokens,contextWindow,percent}`，并含 `userMessages/assistantMessages/toolCalls/toolResults/totalMessages` 计数。计费口径覆盖 assistant 消息、**工具上报的嵌套用量**（`ToolResultMessage.usage`）、以及 compaction/branch-summary 生成。
- **细粒度**：`message_update` 顶层 `usage`（逐流累积）、`AssistantMessage.usage`、`ToolResultMessage.usage`、`compaction_end.result.usage`。
- `contextUsage.tokens/percent` 在压缩刚完成后可能为 `null`，直到下一次 post-compaction assistant 响应提供有效值。

**对 Farm 的意义**：Symphony 那种"绝对累计 token"记账所需的输入 pi 已经给全，且**连金额和上下文窗口占比都算了**，Farm 只需按 session/agent/任务聚合。

---

## 3. 多会话并发、恢复 / steer

### 并发

pi 的模型是**一个进程一个活动会话**（`AgentSession` 是单会话；`AgentSessionRuntime` 管"替换当前会话"）。所以并发 = **宿主起多个 pi 子进程**（每个带自己的 cwd/`--session-dir`）。

- 一个进程内**没有** API 同时跑多个 `AgentSession` 并各自 `prompt`；但可以在单进程内**顺序**替换会话（`new_session`/`switch_session`/`fork`/`clone`）。
- **Rust 侧并发策略**：每个 agent 一个 `pi --mode rpc` 子进程，宿主持有 `ChildHandle + stdin/stdout`，各自独立事件循环。这与 Symphony "每个 agent 一个 Codex 子进程、由编排器监督"完全同构。

### 恢复

- 启动即续：`pi -c`（继续最近）、`pi --session <path|id>`（指定）、`pi --fork <path|id>`（分叉）。
- 运行中切换：RPC `switch_session`（含 `session_before_switch` 扩展钩子，可被取消）、`new_session`（可带 `parentSession` 跟踪）、`fork`（从某 user message 分叉，返回被分叉的文本）、`clone`（复制当前 active 分支）。
- 存在 `get_fork_messages` 供列出可分叉点。
- **注意**：会话替换后，**事件订阅归属于旧的 `AgentSession`，必须重新订阅**（`docs/sdk.md` 原文）。Rust 侧对应：`switch_session`/`new_session`/`fork`/`clone` 之后，事件流仍是同一 stdout，但宿主视角要重置"当前会话 id"（新会话的 `sessionId` 变了）。

### steer（运行中注入指导）

pi 有**两级**注入，语义清晰（RPC 与 SDK 一致）：

- **`steer`**：在当前 assistant 轮次执行完其工具调用之后、下一次 LLM 调用之前送达。用于"改道"。
- **`follow_up`**：等 agent **完全停下**才送达。用于"顺便也做 X"。
- 也可用 `prompt` + `streamingBehavior: "steer" | "followUp"` 达成同样效果（`prompt` 在流式中**不带** `streamingBehavior` 会**报错**）。
- 送达节奏可调：`set_steering_mode` / `set_follow_up_mode`（`"all"` 全送 / `"one-at-a-time"` 每轮一条，默认 one-at-a-time）。
- `queue_update` 事件暴露待发队列（`steering`/`followUp` 数组）。
- **扩展命令**（`/mycommand`）例外：即使在流式中也**立即执行**；skill 命令（`/skill:name`）与 prompt 模板（`/template`）在发送/排队前**展开**。

**对 Farm 的意义**：面板的"HITL 介入"就有了现成语义——想插话改方向用 `steer`，想追加任务用 `follow_up`，想硬停用 `abort`。**且 `/skill:name` 会被展开**，这意味着编排器可以**注入 `/skill:implement-spec`**（正是 skills 报告里的关键共振点：pi 会展开 skill 命令，Farm 不必程序化复刻那套 9 步协议）。

### 中断

`abort`（中止当前操作）、`abort_retry`（取消重试延迟）、`abort_bash`（中止运行中的 bash）。另有 `set_auto_retry` / `set_auto_compaction` 开关。

---

## 4. 对票中核心问题的直答

> **Symphony 用 Port+JSON-RPC 驱动 Codex app-server 的模式，在 pi 上的对应物是什么（子进程 + stdout JSONL？SDK 库内嵌？HTTP？）**

**对应物 = 子进程 + stdin/stdout 双向 JSONL，命令名就是 `pi --mode rpc`。**

- **是子进程**：宿主 `spawn("pi", ["--mode", "rpc", ...])`，拿 stdin/stdout 管子。与 Symphony 的 `Port.open({:spawn_executable, ...})` 一一对应。
- **是 stdout JSONL（严格 LF 定界）**，不是 JSON-RPC 2.0（无 `jsonrpc`/`method` 字段，是 `{"type":..., "command":..., "id":...}` 的自定义行协议），**也不是 HTTP/WebSocket/SSE**——pi **没有** HTTP 服务端。已核实：`dist/modes/rpc/rpc-mode.js` 里没有任何 http/listen 痕迹，而 `dist/server/` 目录只有一个 `create-harness.js`（测试用 harness），不是网络服务。RPC 文档明示 stdin/stdout 才是集成面。
- **不是 SDK 库内嵌**：SDK 是 Node/TS 的进程内 API，Rust 用不上，也不建议为它做 FFI/桥接。

**因此 Symphony 的架构可以在 Rust 里逐条落地**：编排器（Farm 的 Orchestrator 插件）为每个 agent `spawn` 一个 `pi --mode rpc` 子进程，按行读写 JSONL，用 `agent_settled` 判终态、`get_session_stats` 记账、`get_entries` 增量拉转写、`steer`/`follow_up`/`abort` 做 HITL 与调度干预。**Symphony 需要自己实现的 Codex 客户端层，pi 直接白送。**

---

## 5. 对 Farm 架构的连带影响（供后续票参考）

1. **进程级 IPC 插件决定被 pi 印证**：pi 自己就用"子进程 + JSONL"作为官方集成面，与 Farm 已锁定的"进程级 IPC 插件"方向同构。Farm 的编排器插件 ↔ pi 之间可复用同一套"行分隔 JSON + 双向流"的心智模型。
2. **agent 生命周期归宿主**：pi 不做并发上限、不做崩溃对账、不做重试编排（只有自身网络级 auto-retry）。这些都归 Farm 宿主的票 04（进程模型）。
3. **转写/审计可二选一或并存**：pi 的 session JSONL 已是权威转写；Kron 若作审计后端，则由 Farm 决定"镜像 pi 的 entry 流进 Kron"还是"Kron 只记编排决策"。见票 06。
4. **HITL 通道现成**：面板的介入按钮直接映射 `steer`/`follow_up`/`abort`，无需发明协议。见票 02。
5. **版本偏斜要注意**：本机是 **0.84.3**；兄弟仓库里 Warehouse pin 了 1.0.2，Sentry/Slaveholder 锁 0.84.3。RPC 协议在不同版本间可能有差异，Farm 应在启动时用 `get_state`/版本探测做兼容断言（票 04 的裁决项）。

---

## 6. 未决 / 风险

- **RPC 模式下 agent 子进程的终止语义**未在文档中明确（是否随 stdin 关闭而退出、SIGTERM 行为），Farm 宿主需自行设计"关闭 stdin + 超时 + 强杀"的三段式收尾。建议列入票 04。
- **扩展 UI 子协议**（`extension_ui_request`/`extension_ui_response`）在 RPC 模式可用：若 Farm 加载的 pi 扩展要弹确认框，宿主必须应答，否则带 timeout 的对话框会超时、不带的会**永久阻塞**。这是 Farm 面板要处理的真实协议分支（对"危险命令确认"这类 HITL 尤其相关）。
- **`--mode json` 与 `--mode rpc` 的关系**：前者单向、无命令输入；若 Farm 只要观测不要干预可用，但既然要 steer/abort，**统一走 RPC**。
- 本报告基于**文档 + 类型定义 + 示例**，未实际启动 `pi --mode rpc` 做端到端验证（研究票范围为只读）。建议把"起一个 pi RPC 子进程跑通 prompt→事件→stats"列为实现期的冒烟测试。
