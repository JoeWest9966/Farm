# 研究：Sentry 闸门在 pi 1.0.2 上的重验

> 来源任务：Farm [ADR-0004](../adr/0004-farm-host-process-model.md) 裁决第 5 条的前置任务（wayfinder 票 #4）——「Sentry 在 1.0.2 上的重验是 spec 的前置任务」。
> 重验对象：`/tmp/farm-research/Nous-Sentry`（`extension/index.js` + `src/`，46 JS + 23 Go 测试全绿的实现版）。
> 对照物：pi **0.84.3**（`/usr/lib/node_modules/@earendil-works/pi-coding-agent`，本机 `pi --version` = 0.84.3）与 pi **1.0.2**（Warehouse runtime pin：`/opt/nwt-realrun/nw-home/runtimes/node-22.19.0+pi-1.0.2/pi`，Node 22.19.0）。
> 实验脚本与原始输出：`/tmp/sentry-reval/`（`e2e-rpc.mjs`、`throw-rpc.mjs`、`fail-modes.mjs`、`out-102.json`、`out-0843.json`、`failmodes.json`）。本机无模型凭据，端到端用 pi-ai 的 **faux provider** 脚本化响应驱动真实 `pi --mode rpc` 进程。

---

## 0. 结论（先给答案）

**Sentry 在 pi 1.0.2 上可直接用，无需改造、无需降级 pi。三项随版本可能漂移的语义全部未漂移。**

| 重验项 | 结论 | 证据强度 |
|---|---|---|
| ① `gate_failed` 触发语义 | **不漂移**。1.0.2 上 hook 抛异常 = 工具不执行（fail-closed），与 0.84.3 逐字同构；Sentry 自身失败路径（手册缺失 / 落账失败 / 连续硬上限）实测 fail-closed 且达 3 次上报 `gate_failed` escalation | 源码逐字对照 + 端到端实测（两版本） |
| ② `stopReason` 白名单 | **不漂移**。两版本同一 union、逐字一致，仍是七值 | 类型定义对照 + 端到端实测观测 |
| ③ `agent_settled` 终态锚点 | **不漂移**。仍在 `_runAgentPrompt` 的 `finally` 发出，语义（无重试/无 compaction 续跑/无排队续跑）不变 | 源码对照 + 端到端实测（两版本各 1 次） |

**同时纠正 ADR-0004 重读清单里的一条错误前提**：清单写「Sentry 的 `gate_failed` 触发差异（0.84.3 fail-safe vs durable fail-closed）」。直接实验表明 **0.84.3 在 hook 抛异常时同样拦下工具**（不是放行）——差别不在「放行/拦下」，而在「静默与否」：两者都零 `extension_error` 遥测，且异常文本会泄进模型对话。durable 地基未装，未验。

需要进 spec 的动作（详见 §4）：① 重读清单那条改写；② 补 1.0.2 的接口面增量（`clear_queue`、`disposition`、`parentToolCallId` 嵌套调用也过闸）；③ 闸门依赖解析（Sentry `manual.js` 硬编码 0.84.3 安装路径取 `yaml`，在「只有 1.0.2 的主机」上会把闸门打成全拦）。

---

## 1. 方法与材料

- **端到端**：脚本化 faux provider（`pi-ai/providers/faux.ts`，0.84.3 与 1.0.2 都带）产出「一次 bash 工具调用 → 最终文本」，真实 `pi --mode rpc` 子进程；Sentry 真实扩展经 `-e` 加载，真实闸门处理 `tool_call`，落账打到假 `ledger` 子进程（记录 stdin NDJSON）。
- **异常路径**：把闸门换成「`tool_call` 处理器直接抛异常」的扩展，看工具是否执行、模型看到什么、有无遥测。
- **闸门自身失败**：直接调 Sentry 的 `tool_call` 处理器（stub `pi.on`），注入落账失败 / 手册缺失，验证 fail-closed 与硬上限上报。
- **判据**：bash 工具若真执行，会在临时目录留下 marker 文件——`markerExists` 是工具是否执行的机械事实（不采信任何自述）。
- **未做的**：真实 provider（无凭据）、durable 地基（未安装）、pi 1.0.0（未安装，无法复现旧事故）。

---

## 2. 逐项对照

### 2.1 `gate_failed` 触发语义

**A. pi 层：hook 失败时工具执行与否（端到端实测）**

| 场景 | 0.84.3 | 1.0.2 | 模型看到的工具结果 | `extension_error` 遥测 |
|---|---|---|---|---|
| 闸门返回 `{block:true, reason}` | 工具不执行（`markerExists:false`） | 工具不执行（`markerExists:false`） | `isError:true` + `[SENTRY HOLD] … held by gate=r0 …` | 无 |
| 闸门处理器**抛异常** | 工具不执行（`markerExists:false`） | 工具不执行（`markerExists:false`） | `isError:true` + 异常文本 `gate-bug-throws`（泄进对话） | **无** |
| 扩展**加载**阶段抛异常 | pi 启动即硬错：`Error: Failed to load extension "…": ext-load-boom` + 提示 `pi -ne` | 同上，逐字一致 | 进程根本没起来 | stderr 硬错 |

实测原始片段（1.0.2，抛异常路径）：

```json
{ "markerExists": false,
  "toolExecutionEnd": [{ "toolName": "bash", "isError": true,
      "result": "{\"content\":[{\"type\":\"text\",\"text\":\"gate-bug-throws\"}],\"details\":{}}" }],
  "extensionErrors": [], "agentSettled": 1 }
```

源码依据（两版本同构）：

- 扩展 hook 包装（异常 re-throw，注释即 `blocking execution`）：0.84.3 `core/agent-session.js:222/239`；1.0.2 `core/agent-session.js:314/332`。
- `emitToolCall` **无 try/catch**（异常直穿）：0.84.3 `core/extensions/runner.js:701`；1.0.2 `core/extensions/runner.js:957`。
- 异常在工具管线内被翻译成错误结果（工具不执行）：`prepareToolCall` 的 `catch` → `{kind:"immediate", result: createErrorToolResult(...), isError:true}`，0.84.3 `pi-agent-core/agent-loop.js:393`、1.0.2 `agent-loop.js:481`。1.0.2 的 `agent-loop.d.ts` 把这条写进了契约文档：*“Never rejects for tool failures: unknown tools, validation errors, blocked calls, and thrown errors come back as `isError: true`.”*

**B. Sentry 层：闸门自身失败的 fail-closed 与 `gate_failed` 上报（实测）**

| 场景 | 实测结果 |
|---|---|
| 手册规则 `action: approve` | 不 block、不落账（`ledgerWrites: 0`） |
| 手册规则 `action: reject` | `block:true` + 落 approval 终态 `decision: rejected`/`actor: policy`/`reason_code: policy_denied`，`gate_id: r0` |
| 手册规则 `action: ask` | `block:true` + 落 approval `pending` |
| 落账子进程失败 | `block:true`，reason 退化为能力式文案：`the gate could not evaluate this call safely (bash). Re-issue later.`（**fail-closed**） |
| 连续 3 次落账失败 | 3 次全部 `block:true`；第 3 次追加 escalation：`{"event":"escalation","actor":"sentinel","client":"sentry-gate","reason":"gate_failed","unit":null,"result_class":null,"exit_status":null,"tool_call_id":"tc-12"}`（**进程内形态**，systemd 三位置 null，与 spec §8 一致） |
| 手册文件缺失 | 每次调用 `block:true`（`the gate could not load its manual…`）；达硬上限同样上报 `gate_failed` |

端到端确认（1.0.2）：闸门在真实 RPC 会话里拦下 bash，落账 NDJSON 与 spec §10 形状一致：

```json
{"event":"approval","ts":"2026-10-12T05:15:43.468Z","run_id":"01a12e89-bf33-…","subject_kind":"tool_call",
 "subject_id":"tool:1791782142888:6dkz51tipeo","gate_id":"r0","actor":"policy","decision":"rejected","reason_code":"policy_denied"}
```

reason 红线同时复验通过：长命令被截断为 100 字（`containsFullCommand:false`）、不含规则散文（`containsRuleProse:false`）。

### 2.2 `stopReason` 白名单

**逐字一致**（两版本同一文件、同一行内容）：

```
0.84.3  pi-ai/dist/types.d.ts:287
1.0.2   pi-ai/dist/types.d.ts:313
export type StopReason = "pending" | "stop" | "length" | "toolUse" | "error" | "aborted" | "deferred";
```

- 仍是 **七值**，无新增、无删除；`AssistantMessage.stopReason` 就是它（另有可选 `rawStopReason`）。
- 端到端实测在 1.0.2 上观测到的取值：`toolUse`（发起工具调用那轮）与 `stop`（最终轮）——即判据阶梯第二条件 `stopReason === "stop"` 在 1.0.2 上语义不变。
- 1.0.2 新增 `ClassifierStopReason = "stop" | "error" | "aborted"`（`models.classify()` 的结果用），**不在** `AgentMessage` 上，与 Farm 读的字段无关。
- 另注（两版本一致）：`agent_end` 事件的 `willRetry` 由 `stopReason !== "stop"` 派生，实测拦截后完整轮次 `willRetry:false`。

### 2.3 `agent_settled` 终态锚点

- **发射点不变**：`_emitAgentSettled()`（0.84.3 `agent-session.js:330`；1.0.2 `agent-session.js:671`），由 `_runAgentPrompt` 的 `finally` 调用（0.84.3 `:758`；1.0.2 `:1376`）——即**只要 run 走完就一定发**（含 abort/取消）。
- **语义不变**：发在「后处理循环」之后——无自动重试、无 compaction 续跑、无排队续跑、`before_settle` 边界不再要求续跑；类型注释照旧：*“Fired after an agent run has fully settled and no automatic retry, compaction, or queued continuation will run.”*
- **RPC 模式用法不变**：两版本 `modes/rpc/rpc-mode.js:267` 都是 `if (event.type === "agent_settled") checkShutdownRequested()`。
- **实测**：两版本端到端各发出 **1 次** `agent_settled`，且 `agent_end` 的最终消息 `stopReason: "stop"` —— 判据阶梯的「双条件」在 1.0.2 上仍可满足。
- **1.0.2 新增的周边机制**（不影响锚点本身，但影响宿主实现）：
  - `agent_before_settle` 扩展边界 + `_runBeforeSettleBoundary()`：settle **之前**可再插一轮（`{entries:[…], continue:true}`）。
  - `_isEmittingAgentSettled` 重入闸 + `_deferredSettledActions`：在 settle 发射期间发起的 `prompt`/记忆注入被**推迟**到 settle 之后执行（0.84.3 无此机制）。
  - 0.84.3 的 `isIdle` 依赖 `_isAgentRunActive`；1.0.2 变为 `!_isAgentRunActive && !isCompacting`。

---

## 3. 1.0.2 相对 0.84.3 的接口面增量（对 Farm 契约有用）

| 面 | 0.84.3 | 1.0.2 | 对 Farm 的意义 |
|---|---|---|---|
| RPC 命令 | 34 | 34 + `clear_queue`（清空 steering/followUp 队列） | 面板/HITL 多一个现成原语 |
| RPC 响应 | `prompt`/`steer`/`follow_up` 仅 `success` | 三者新增 `data.disposition`（如 `prompt` → `"started"`） | Farm 可判「注入被接受/排队/丢弃」 |
| `tool_call` 事件 | 无 `parentToolCallId` | 新增 `parentToolCallId`；`ctx.executeTool()` 嵌套调用（0.84.3 完全不存在）也过 `tool_call`/`tool_result` hook | **闸门覆盖面自动扩大**（嵌套调用也受审）；Farm 记账须把嵌套用量一并计入 |
| 扩展事件/边界 | `ExtensionRunner.emit()` 接受 `turn_end` | `emit()` **不再接受** `turn_end`；新增 `agent_before_settle`、`context_with_system`、`provider_stream_event` | Farm 若做扩展，别绑 `turn_end` 边界 |
| `agent_end` messages | 不含 system 消息 | 含一条 `role:"system"` 消息（canonical session context） | Farm 统计消息数/记账时注意多一条 |
| 内置扩展命名 | `<inline:…>`/`<builtin:…>` | `builtin:<name>`；`--no-extensions` 也关内置（`-ne -e builtin:mcp` 单开） | 诊断/日志解析注意 |
| 扩展加载失败 | 硬错退出 | 硬错退出（逐字一致） | 无静默放行面 |

RPC 协议其余部分（帧定界、命令/事件集）**未变**：`modes/rpc/rpc-mode.js` 的事件/命令字符串集合在两版本完全一致，`rpc-types.d.ts` 的差异仅上面两行（`clear_queue` + `disposition`）。

---

## 4. 给 Farm spec 的建议

1. **基准维持 1.0.2，不降级。** ADR-0004 第 5 条成立；Sentry 在 1.0.2 上无需改码即可用（本次实测：加载、手册、闸门、落账、拦截、上报全通）。
2. **改写「换 pi 版本必重读清单」第 1 条。** 现文案「`gate_failed` 触发差异（0.84.3 fail-safe vs durable fail-closed）」与实测不符。建议替换为两条真事实：① hook 抛异常在 0.84.3/1.0.2 上**都是拦下**（fail-closed），但**都无遥测**且异常文本进对话——「静默」与「拦下」在外部不可区分，这正是哨兵自证活的理由；② 与 durable 地基的差异仍未验证（durable 未安装），保留为「舰队迁 durable 时重读」。
3. **把 pi 1.0.2 的接口增量写进契约冒烟清单**：`clear_queue`、`prompt/steer/follow_up` 的 `disposition`、`parentToolCallId`（嵌套调用也过闸）、`agent_before_settle`。
4. **闸门依赖解析必须进 spec（本次新发现，部署阻断级）**：Sentry `src/manual.js` 用 `createRequire("/usr/lib/node_modules/@earendil-works/pi-coding-agent/dist/index.js")` 取 `yaml`——**硬编码 0.84.3 安装路径**。实测该 base 不存在时 `require('yaml')` 直接 `MODULE_NOT_FOUND`；两个安装里 `yaml` 都在。在「只有 Warehouse runtime 的 pi 1.0.2」主机上，这会让闸门手册加载失败 → 按 fail-closed 全拦（安全但不可用）。spec 应要求闸门相对**自身/当前 pi 安装**解析依赖（或内联/自带依赖），不得硬编码版本路径。
5. **Sentry 的 fail-closed + 硬上限保留为一线防线**；pi 1.0.2 实测不静默（抛异常=拦、加载失败=硬错），所以 Farm 侧**不需要**额外自证机制作为默认项；若 AFK 期要对「闸门到底挂上没挂上」做正面证明，用 ADR-0007 已有的 `gate_id` 可证伪锚（spec §8 已定），别新造事件类型。
6. **完成信号不受影响**：Farm 的完成信号是宿主 merge 的 merge commit（ADR-0001）；`agent_settled + stopReason==="stop"` 只作为 attempt 终态（判据阶梯），该双条件在 1.0.2 上实测成立。
7. **HITL 通道**：`steer`/`follow_up`/`abort` 在 1.0.2 上仍在；新增的 `disposition` 建议纳入 agent-runner 的事件回流（判定注入结果），比只看 `success:true` 更准。
8. **settle 期间的行为差异入档**：1.0.2 在 settle 发射期间发起的 prompt 会被推迟（`_deferredSettledActions`）。Farm 的面板若允许「agent 收尾瞬间插话」，需按「推迟到 settle 后执行」建模，而非「立即注入」。

---

## 5. 未实证 / 风险（诚实标注）

- **pi 1.0.0 的「静默放行」事故未复现**：本机无 1.0.0 安装，只能在 0.84.3/1.0.2 上验证两条异常路径都拦下。1.0.1/1.0.2 CHANGELOG 未见对应修复条目（仅见「扩展注册不合法时改为加载失败而非崩溃」等相邻修复），**根因未定**，不能断言「1.0.2 已修复该事故」。
- **durable 地基未验**：`@earendil-works/pi-durable` 未安装，spec 里「durable fail-closed」的比较仍是纸面。
- **faux provider 非真实 provider**：端到端未走真实模型流式路径；`stopReason` 的真实取值分布（`length`/`error`/`aborted`/`deferred`）未实测，只证类型不变 + `stop`/`toolUse` 可用。
- **Sentry 自带 46 JS 测试未跑**：其 `parseYaml` 依赖 0.84.3 安装路径（见建议 4），跑测不构成 1.0.2 证据；建议 Farm spec 把「闸门依赖解析」列为契约冒烟的前置。
- **未验多会话并发、会话恢复/分叉在 1.0.2 的行为**（本报告只覆盖闸门与终态语义）。
- 1.0.2 的 `disposition` 字段值域未穷举（实测见 `"started"`）。

---

## 附：复现命令

```bash
N=/opt/nwt-realrun/nw-home/runtimes/node-22.19.0+pi-1.0.2/bin/node
B=/opt/nwt-realrun/nw-home/runtimes/node-22.19.0+pi-1.0.2/pi
A=/usr/lib/node_modules/@earendil-works/pi-coding-agent

# 端到端：真实 RPC + 真实 Sentry 闸门（1.0.2 / 0.84.3）
cd /tmp/sentry-reval
PI_ROOT=$B PI_BIN=$B/dist/bundle/cli.js PI_TAG=pi102  $N e2e-rpc.mjs
PI_ROOT=$A PI_BIN=$A/dist/bundle/cli.js PI_TAG=pi0843 $N e2e-rpc.mjs

# 异常路径：hook 抛异常时工具是否执行
PI_ROOT=$B PI_BIN=$B/dist/bundle/cli.js PI_TAG=pi102  $N throw-rpc.mjs
PI_ROOT=$A PI_BIN=$A/dist/bundle/cli.js PI_TAG=pi0843 $N throw-rpc.mjs

# 闸门自身失败：fail-closed / 硬上限 / gate_failed 上报 / reason 红线
$N fail-modes.mjs

# 扩展加载失败是否静默（两版本一致：硬错）
# 见 /tmp/sentry-reval/loadext.mjs
```
