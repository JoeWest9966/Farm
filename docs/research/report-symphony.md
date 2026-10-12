# OpenAI Symphony 源码深度技术报告

> 面向 Farm（Rust 全栈 + 面板 + 可插拔插件的 AFK 编排平台）的对照阅读笔记。
> 阅读对象：`openai/symphony`，`main` @ `be10a1b79df723d6d7612b5651c8522704dafb2e`（2026-09-15T22:12:07Z；仓库 `language=Elixir`，`size≈30MB`）。获取方式：`codeload.github.com` tarball（`git clone --depth 1` 在该环境下反复失败，见 §15）。
> 阅读方式：完整读 `SPEC.md`（2312 行）+ `elixir/` 参考实现全部核心模块（`lib/` 13,070 行 / 51 文件，`test/` 15,275 行）+ 配置/文档/构建脚本。
> 本报告所有论断都带 `文件:行号` 证据，未读到的部分会显式标注。

---

## 0. 一句话结论

Symphony 不是"一个 agent 框架"，而是一个**长驻守护进程规格**：它把"项目工作"抽象成 tracker 上的 issue，把 issue 抽象成"一个隔离 workspace + 一串 Codex turn"，并用**单点权威内存状态机**管理声明（claim）、并发、重试、对账与清理。它最值钱的资产不是 Elixir 代码，而是 **`SPEC.md` —— 一份语言无关、带可移植性不变量和测试矩阵的移植契约**；官方 README 明确把"让任意语言的 agent 照着 SPEC 重写一遍"列为**首选运行方式（Option 1）**：`README.md:21-30`。

---

## 1. 仓库形态：SPEC 优先，参考实现次之

| 交付物 | 位置 | 体量 | 定位 |
|---|---|---|---|
| 规格 | `SPEC.md` | 2312 行，18 节 + 1 附录 | **主交付物**，RFC 2119 规范性语言，语言无关 |
| 参考实现 | `elixir/` | `lib` 13,070 行 / `test` 15,275 行（合计 28,345 行） | 可运行的 Elixir/OTP 版本，作者自称"低调度工程预览、仅评估用"：`elixir/README.md:6-8` |
| agent 技能 | `.codex/skills/{commit,push,pull,land,linear,release,debug}` | 8 个 | 由被编排的 agent 在工作区内使用，属于"harness 契约"的一部分 |
| 冒烟证据 | `docs/symphony-smoke-*.md` | 2 个短文件 | 自举验证：用 Symphony 处理 Jira issue 生成文档 |

关键含义：**Symphony 把"实现"和"规格"分离，并把规格写成移植契约**。这就是为什么把它移植到 Rust 是"官方认可路径"，而不是"重写别人的产品"。这也意味着：Farm 可以直接把 `SPEC.md` 当需求基线，用它的 §17 测试矩阵当验收清单（详见第 14 节）。

`elixir/AGENTS.md` 还规定了实现与规格的关系：实现可以是规格的**超集**，但不得与规格冲突；若行为语义变化，需在同一次变更里更新规格（`elixir/AGENTS.md:14-18`）。这条规矩本身就是"规格即契约"的体现，值得 Farm 照抄。

---

## 2. 核心架构

### 2.1 组件与职责（`SPEC.md:73-116`）

1. **Workflow Loader** —— 读 `WORKFLOW.md`，切 YAML front matter 与 prompt body，返回 `{config, prompt_template}`。
2. **Config Layer** —— 带类型的 getter、默认值、`$VAR` 间接寻址、启动/每次 tick 的 preflight 校验。
3. **Issue Tracker Adapter** —— 只读内核：按状态取候选、按 ID 刷新快照、启动时取终态做清理；把 provider payload 归一化成稳定的 `Issue`；**可选**暴露 provider 原生 agent 工具。
4. **Orchestrator** —— 拥有 poll tick、内存运行时状态、派发/重试/停止/释放决策、会话指标与重试队列。
5. **Workspace Manager** —— issue → 工作区路径映射、目录创建/复用、生命周期 hook、终态清理。
6. **Agent Runner** —— 建工作区、按模板渲染 prompt、拉起 coding-agent app-server 客户端、把 agent 事件流回编排器。
7. **Status Surface（可选）** —— 人类可读仪表盘（终端 TUI 或 Web）。
8. **Logging** —— 结构化日志，稳定 `key=value`。

### 2.2 六层抽象（`SPEC.md:118-141`）

`Policy（仓库内的 prompt） → Configuration（类型化配置） → Coordination（编排器） → Execution（workspace + 子进程） → Integration（tracker adapter） → Observability（日志 + 可选状态面）`。

这六层划分是"可移植性"的关键：规格要求把**策略放在 repo 里**（prompt 与运行参数随代码一起版本化，`SPEC.md:26-31`），把**provider 语义关进 adapter**，让编排器完全不感知 provider（`SPEC.md:1259-1264`：编排器 MUST NOT 检查 provider payload、不得假设 `issue.id` 就是 ticket ID、不得对 provider 的 blocker/board/transition/comment 语义分支）。

### 2.3 领域模型（`SPEC.md:152-317`）

- **Issue**：`id`（不透明派发身份，可能是 board entry / project item ID）、`native_ref`（非机密的 provider 标识，仅供 provider 原生工具与 prompt 使用）、`identifier`（人类可读 key，用于命名工作区，必须在 scope 内唯一）、`title/description/priority/state/branch_name/url/assignee_id/labels/blocked_by`、**`dispatchable`（adapter 推导的显式可派发标志）**、时间戳。
- **Workflow Definition**：`config`（front matter 根对象）+ `prompt_template`（去首尾空白后的 Markdown body）。
- **Service Config**：类型化的运行值。
- **Workspace**：`path`、`workspace_key`（防碰撞的清洗后 identifier）、`created_now`（用于门控 `after_create`）。
- **Run Attempt**：`issue_id / identifier / attempt / workspace_path / started_at / status / error`。
- **Live Session**：`session_id = "<thread_id>-<turn_id>"`、app_server pid、最近事件/时间戳/摘要、三类 token 计数（当前 + 上次上报）、`turn_count`。
- **Retry Entry**：`attempt`（1 基）、`due_at_ms`（单调时钟）、定时器句柄、`error`。
- **Orchestrator Runtime State**（唯一权威，`SPEC.md:280-293`）：`poll_interval_ms`、`max_concurrent_agents`、`running`、`claimed`、`retry_attempts`、`completed`（仅记账，不参与派发门控）、`codex_totals`、`codex_rate_limits`。

**归一化规则（`SPEC.md:295-317`）值得逐条照抄到 Rust 里**：

- `workspace_key`：把 `[A-Za-z0-9._-]` 之外字符替换为 `_`；**若替换确实改变了 identifier，则追加原 identifier 的稳定哈希后缀（≥64 bit 熵）**，保证"不同 identifier 清洗后同名"仍不碰撞。
- 状态比较：trim + lowercase 后再比。
- `session_id = <thread_id>-<turn_id>`。

---

## 3. 进程 / agent 模型（Elixir 实现细节）

### 3.1 监督树：一个进程 = 一个"权威 + 一群 worker"

```
Application (SymphonyElixir.Application)
└── Supervisor :one_for_one
    ├── Phoenix.PubSub                        # 面板广播
    ├── WorkflowStore (GenServer)             # WORKFLOW.md 缓存 + 1s 轮询热重载
    ├── AgentRuntimeSupervisor :one_for_all
    │   ├── Task.Supervisor                   # 每个 issue 一个 Task
    │   └── Orchestrator (GenServer)          # 单点权威状态机
    ├── HttpServer (可选, Phoenix Endpoint)   # --port / server.port 时启动
    └── StatusDashboard (GenServer)           # 终端 TUI
```

证据：`lib/symphony_elixir.ex:27-46`（children 列表）、`lib/symphony_elixir/agent_runtime_supervisor.ex:19-37`（`:one_for_all` 把权威与 worker 池绑在一起）。`:one_for_all` 的取舍很明确：**编排器挂了，其 worker 任务也必须一起重启**，避免孤儿 worker 继续改工作区。

- **单点权威**：`Orchestrator` 是唯一改调度状态的进程（`use GenServer`，`orchestrator.ex:5`；规格对应 `SPEC.md:637`"编排器是唯一会变更调度状态的组件"）。所有 worker 结果通过消息回报，转成显式状态转移（`SPEC.md:637-639`）。
- **worker = Task**：`Task.Supervisor` 里起一个 Task，编排器持有其 pid + monitor ref，用 `{:DOWN, ref, ...}` 感知退出（`orchestrator.ex:12` 的 State 里 `running` 存 `worker_handle/monitor_handle`，`orchestrator.ex:128-148` 处理 DOWN）。
- **agent = 子进程（Port）**：Codex 以 **app-server 模式**（JSON-RPC 2.0 over stdio）由 `bash -lc <codex.command>` 拉起，cwd = 工作区（`app_server.ex:192-214`），stderr 与协议流合并后按行解析（`:stderr_to_stdout`，`line: 1_048_576`）。
- **远端 worker（可选扩展）**：`worker.ssh_hosts` 让 orchestrator 留在中心、把 agent 通过 SSH stdio 跑在别的机器上（`app_server.ex:216-219` + `ssh.ex`；规格 Appendix A，`SPEC.md:2252-2313`）。**一个 worker 生命周期绝不跨机器**（`agent_runner.ex:22` 的注释明写："The orchestrator owns host retries so one worker lifetime never hops machines"）。

### 3.2 一次 worker 内部的"多 turn"结构

`AgentRunner.run/3`（`agent_runner.ex:22-36`）→ `run_on_worker_host/4`（`:38`）：

1. `Workspace.create_for_issue`（可能远端）→ 上报 `{:worker_runtime_info, issue_id, %{worker_host, workspace_path}}`；
2. `Workspace.run_before_run_hook`（失败即中止本次 attempt）；
3. `AppServer.start_session`（`app_server.ex:37-66`）：校验 cwd 在工作区内 → 建 Port → `initialize` → `thread/start`（带 `approvalPolicy/sandbox/cwd/dynamicTools`，`app_server.ex:314-330`）；
4. **turn 循环**（`agent_runner.ex:101-140`）：`run_turn` 完成 → 立刻 `continue_with_issue?` 刷新 issue 状态 → 若仍在 active 且 routable 且 `turn < max_turns`，**在同一 thread 上再开一个 turn**（第 1 turn 发完整 prompt，后续只发"continuation guidance"，`agent_runner.ex:142-154`）；
5. `after` 块必跑 `after_run` hook、`stop_session`。

**这个"worker 内循环 + 每 turn 结束后重查 tracker"的设计是 Symphony 长任务的核心**：它把"一个 issue 需要多轮才能干完"这件事压进了一次 worker 生命周期，而不是靠 orchestrator 反复重启 agent（后者会丢 thread 上下文）。

### 3.3 协议交互与"永不停滞"策略

- `handle_incoming/6`（`app_server.ex:403-462`）分派上游消息：`turn/completed` → 成功；`turn/failed` / `turn/cancelled` → 失败；其它带 `method` 的走 `handle_turn_method/8`。
- `handle_turn_method` 先试 **approval 自动批准**（`auto_approve_requests = approval_policy == "never"`，`app_server.ex:55`），再判断"需要人类输入"（`needs_input?/2`，`app_server.ex:1035+`，覆盖 `mcpServer/elicitation/request` 与一堆 `turn/input_required|needs_input|request_input|approval_required` 变体 + `requiresInput/input_required/type == "input_required"` 等字段探测）。
- 分叉结果：`:input_required` / `:approval_required` → emit 事件并以 `{:error, ...}` 结束该 turn；`:approved` → 继续收流；`:unhandled` → 若"不支持的工具调用"，返回结构化失败并**继续会话**（规格 `SPEC.md:1083-1088`：不得让会话卡死）。
- 规格对此的强制性要求是：**approval / user-input-required MUST NOT 让 run 无限期停住**（`SPEC.md:1069-1073`），实现必须文档化自己的姿态（自动批准 / 上报运维 / 失败）。
- 超时三件套（`SPEC.md:1142-1156`）：`read_timeout_ms`（启动与同步请求）、`turn_timeout_ms`（turn 流"静默"上限，**每次有输出就重置，因此不是总时长上限**）、`stall_timeout_ms`（编排器按事件不活跃判定）。

---

## 4. 消息流：一次长任务的完整生命周期

```
tick (poll_interval_ms)
 ├─ 1. refresh_runtime_config        # 让热重载的配置生效（orchestrator.ex:1632）
 ├─ 2. reconcile_running_issues      # 对账：先 stall，再按 ID 刷新 tracker 状态（orchestrator.ex:310）
 ├─ 3. validate_dispatch_config      # preflight：失败则本 tick 只对账不派发（SPEC.md:580-604）
 ├─ 4. tracker.fetch_issues_by_states(active_states)
 ├─ 5. sort_for_dispatch             # priority(1..4) → created_at 旧优先 → identifier 字典序（orchestrator.ex:796-814）
 └─ 6. dispatch while slots remain   # should_dispatch + 每状态并发上限（orchestrator.ex:816-851）

dispatch_issue
 └─ Task(run_agent_attempt)
     ├─ Workspace.create_for_issue（新建则跑 after_create）
     ├─ before_run hook
     ├─ AppServer.start_session  ──initialize──> ──thread/start(dynamicTools)──>
     └─ loop turn 1..max_turns
          ├─ turn/start(prompt)
          ├─ 上游事件流 → on_message → send(orchestrator, {:codex_worker_update, id, msg})
          │    → 编排器更新 running entry（session_id/turn_count/token/rate-limit/最后事件）
          ├─ turn/completed → tracker.fetch_issues_by_ids([id]) 复查
          └─ 仍 active 且 routable 且未到 max_turns → 下一 turn；否则跳出
     └─ after_run hook + stop_session

worker DOWN
 ├─ reason == :normal  → 记 completed（仅记账）→ 计划 **1s 固定** continuation retry（orchestrator.ex:208-224, 1232-1246）
 └─ reason != :normal  → 指数退避重试：delay = min(10_000 * 2^(attempt-1), max_retry_backoff_ms)

retry timer fired
 └─ 按 ID 刷新 issue → 不存在则释放 claim；终态则清工作区+释放；
    仍 active 且 routable 则看槽位派发，否则以 "no available orchestrator slots" 重新入队
```

关键细节与证据：

- **`claimed` 与 `running` 双检查**：派发前必须同时确认"未被声明、未在运行"，这是防重复派发的核心不变量（`SPEC.md:729-731`；实现 `orchestrator.ex:816-851` 的 `should_dispatch_issue?`）。
- **重试前重新校验（stale revalidation）**：worker 退出/重试触发时都会 `revalidate_issue_for_dispatch`，避免"排队的旧快照"被派发（`orchestrator.ex:1006-1024`）。
- **对账 A：stall 检测**：用"最后事件时间（无事件则 started_at）"算 `elapsed_ms`，超 `stall_timeout_ms` 就杀掉并重试；`<= 0` 关闭该功能（`orchestrator.ex:581-652`，规格 `SPEC.md:819-840`）。
- **对账 B：tracker 状态刷新**：终态 → 杀 worker **并清工作区**；仍 active 且 routable → 更新内存快照；active 但不 routable / 既非 active 也非终态 → 杀 worker **不清工作区**；刷新失败 → 保持运行、下 tick 再试（`orchestrator.ex:410-500`）。
- **启动清理**：启动时查终态 issue，删对应工作区目录，失败只告警（`orchestrator.ex:1160-1176`，规格 `SPEC.md:841-850`）。
- **申报与释放**：`release_issue_claim`（`orchestrator.ex:1223`）在不满足条件时释放 claim，而不是留一个"死声明"。
- **Elixir 实现独有：`blocked` 状态**。若 Codex 报"需要运维输入/审批/MCP elicitation"，实现**保留 claim**并把该 issue 记为 `blocked`，同时暴露到内存状态、JSON API 与面板；重启即清空（`elixir/README.md:29-33`；`orchestrator.ex:234-241, 747-780, 335-360`）。规格本身没有 `blocked` 字段（只在 §10.5 要求"不得无限停滞"），这是**参考实现相对规格的超集**——对 Farm 很有参考价值（"卡住"必须是一等状态，而不是静默挂起）。
- **重启恢复**：调度状态**故意只在内存**：重启不恢复重试定时器与运行会话，靠"启动清理 + 重新轮询 + 工作区复用"恢复（`SPEC.md:1690-1705`）。规格还留了 TODO：把重试队列与会话元数据持久化（`SPEC.md:2233-2244`）。

---

## 5. Workspace 与安全不变量

- 布局：`<workspace.root>/<workspace_key>`，**跨 run 复用，成功不自动删除**（`SPEC.md:853-867`；`agent_runner.ex` 注释同）。
- 钩子四件套（`SPEC.md:903-927`）：`after_create`（仅新建时，失败=工作区创建失败）、`before_run`（每次 attempt 前，失败=本次 attempt 失败）、`after_run`（无论成败都跑，失败仅记日志）、`before_remove`（删除前，失败仅记日志）；统一 `hooks.timeout_ms`（默认 60s）。
- **三条安全不变量（规格自称"最重要的可移植性约束"，`SPEC.md:928-948`）**：
  1. agent 只在 per-issue 工作区里跑：启动前断言 `cwd == workspace_path`；
  2. 工作区路径必须位于 `workspace.root` 之下（两侧都绝对化后做前缀目录比较）；
  3. 工作区目录名必须清洗 + 哈希防碰撞。
- 实现比规格更严：`validate_workspace_cwd` 会做 **canonicalize（解析软链接）**后比较，分别报 `:symlink_escape` / `:outside_workspace_root` / `:path_unreadable`（`app_server.ex:150-176`）；远端路径则拒绝空串、换行、NUL，报 `:empty_remote_workspace` / `:invalid_remote_workspace`（`app_server.ex:178-190`）。
- 机密处理（`SPEC.md:1748-1759`）：支持 `$VAR` 间接寻址；不打印机密；**adapter 必须声明要剔除的环境变量名**，本地启动时既用 Port 的 `env: [{NAME, false}]` 又在 shell 前置 `unset NAME`（`app_server.ex:221-248`）；provider 工具在 host 侧用 adapter 凭据执行，子进程只拿到工具结果，拿不到原始 token。

---

## 6. Tracker 适配器契约（这是最"可插拔"的地方，与 Farm 的插件化最相关）

### 6.1 只读内核：两个必需操作（`SPEC.md:1186-1229`）

- `fetch_issues_by_states(state_names)`：按 scope 与状态取候选；**空列表必须不发请求直接返回空**；候选轮询要包含 `dispatchable=false` 的 issue（最终过滤权在编排器）。
- `fetch_issues_by_ids(issue_ids)`：按不透明派发 ID 取**完整归一化快照**（不只 state，因为 label/指派/routability 都可能变）；**ID 刷新遇到畸形记录必须失败，不得静默省略**（省略=不可见，语义不同）；状态列表读取则可跳过畸形记录但要记日志。

规格刻意**不**给 tracker 加通用 CRUD：`SPEC.md:1181-1184` 明说"不要为了把各 provider 做得一样而加通用 comment/state/attachment CRUD，那会丢掉 provider 语义，编排器也不需要"。写入由**被编排的 agent 通过 provider 原生工具**完成（`SPEC.md:1310-1322`）。

### 6.2 适配器必须公布的"profile"（文档化义务，`SPEC.md:1230-1258`）

`tracker.kind` 取值、`tracker.provider` 的键/默认值/机密键与校验错误、scope 选择与分页、`id`/`native_ref` 映射、各字段归一化规则、原生工具名与 schema/是否可变/权限/错误语义，以及**"语言原生错误形式 → 可移植错误分类"的映射表**。"编排器只依赖成功/失败"。

### 6.3 已实现的适配器与原生工具（`tracker.ex:15-23` + 各 `agent_tool.ex`）

| `kind` | 原生工具 | 备注 |
|---|---|---|
| `linear` | `linear_graphql`（原样 GraphQL 查询/变更） | 默认 endpoint/states 硬编码在 schema（`config/schema.ex:12-16`） |
| `github` | `github_api` | |
| `gitlab` | `gitlab_api` | |
| `jira` | `jira_rest` | |
| `asana` | `asana_api` | |
| `memory` | — | 测试用内存 tracker |

工具广告绑定**会话快照**：`Tracker.bind_agent_tools()` 在 `thread/start` 前捕获 adapter + 有效 tracker 设置 + 工具 spec + 需剔除的机密环境变量名，热重载只影响未来会话，绝不让在途会话"广告一个 provider、执行另一个"（`tracker.ex:44-70`，规格 `SPEC.md:1096-1114`）。

**对 Farm 的启示**：Symphony 的"插件"目前是**编译期注册表**（`@adapters` 映射 + behaviour + `Code.ensure_loaded?/function_exported?` 探测，`tracker.ex:103-118`）。它证明了"只读内核 + 可选原生工具 + 会话快照绑定"这套边界足够跑通 5 家 tracker；但它**没有**动态加载、版本协商、权限清单或进程隔离——那正是 Farm 的插件系统需要额外补的（见第 12.3 节）。

---

## 7. 配置契约：`WORKFLOW.md`（仓库内契约）

- 路径优先级：显式 CLI 参数 > 当前工作目录的 `WORKFLOW.md`（`SPEC.md:320-331`；`cli.ex:44-72`，无参数时 `Path.expand("WORKFLOW.md")`）。
- 格式：`---` 包裹的 YAML front matter + Markdown prompt body；无 front matter 则整个文件是 prompt；front matter 必须解码为 map（`workflow.ex:71-105`）。
- 顶层键：`tracker / polling / workspace / hooks / agent / codex`，**未知键必须忽略以保前向兼容**，扩展可自带顶层键（`SPEC.md:355-374`）。
- **动态热重载是 REQUIRED**（`SPEC.md:560-579`）：检测变化、重读、重应用，不重启；能改的包括轮询节奏、并发、active/terminal 状态、codex 设置、workspace 根与 hooks、未来 run 的 prompt；**非法重载不得崩溃**，保持 last known good 并报可见错误。实现用 `WorkflowStore` 每 1s 轮询 + `{mtime, size, phash2(content)}` 指纹比对，失败保留旧配置（`workflow_store.ex:13, 116-180`）。
- **派发前 preflight**（`SPEC.md:580-604`）：启动时校验失败=启动失败；每 tick 再校验，失败则跳过本 tick 派发但继续对账。
- 环境变量**不全局覆盖 YAML**，只在值显式写 `$VAR_NAME`、或 adapter 文档化的 fallback 时参与（`SPEC.md:535-559`）。
- 默认值（规格 §6.4 速查表，`SPEC.md:605-633`）：

| 键 | 默认 |
|---|---|
| `tracker.kind` | 必填 |
| `tracker.provider` / `required_labels` | `{}` / `[]` |
| `polling.interval_ms` | `30000` |
| `workspace.root` | `<system-temp>/symphony_workspaces` |
| `hooks.{after_create,before_run,after_run,before_remove}` | 无 |
| `hooks.timeout_ms` | `60000` |
| `agent.max_concurrent_agents` | `10` |
| `agent.max_turns` | `20` |
| `agent.max_retry_backoff_ms` | `300000` |
| `agent.max_concurrent_agents_by_state` | `{}` |
| `codex.command` | `codex app-server` |
| `codex.approval_policy` / `thread_sandbox` / `turn_sandbox_policy` | 实现自定 |
| `codex.turn_timeout_ms` | `3600000` |
| `codex.read_timeout_ms` | `5000` |
| `codex.stall_timeout_ms` | `300000`（`<=0` 关闭） |

- 实现额外引入的扩展键（规格允许、但**规格未定义**，Farm 若照抄需自己定契约）：`server.host`（默认 `127.0.0.1`，`config/schema.ex:273-290`）、`observability.{dashboard_enabled, refresh_ms, render_interval_ms}`（`config/schema.ex:252-271`）。注意规格 §18.2 还把"把可观测性设置放进 workflow front matter"列为**未完成 TODO**（`SPEC.md:2238-2239`）——参考实现已经先行落地，属于"实现领先于规格"。

- Prompt 渲染（`SPEC.md:493-517`）：严格模板（Liquid 语义即可），**未知变量/未知过滤器必须渲染失败**；变量只有 `issue`（含 labels/blockers）与 `attempt`（首次为 null，之后为 1 基整数）；body 为空则可用最小默认 prompt；文件读取/YAML 错误不得静默回落到默认 prompt。

---

## 8. 可观测性 / API / 面板设计

### 8.1 运行时快照（REQUIRED 的同步接口形态，`SPEC.md:1390-1410`）

`running[]`（含 `turn_count`）、`retrying[]`、`codex_totals{input,output,total,seconds_running}`、`rate_limits`；建议错误模式 `timeout` / `unavailable`。实现里 `Orchestrator.snapshot/2` 是 `GenServer.call`（默认 15s 超时），面板与 API 都走它（`orchestrator.ex:1392-1406, 1409-1486`），超时/不可用被 `Presenter` 显式映射成带 `error.code` 的 payload（`presenter.ex:12-36`）。

### 8.2 HTTP 扩展（可选，`SPEC.md:1459-1633`）

- 启用条件：CLI `--port` 或 front matter `server.port`；**默认绑 loopback**；监听设置变更允许要求重启。
- 端点：
  - `GET /`：人类可读面板（服务端渲染或前端应用二者皆可）。
  - `GET /api/v1/state`：概要（counts / running / retrying / codex_totals / rate_limits）。
  - `GET /api/v1/:issue_identifier`：单 issue 调试详情（workspace 路径与 host、attempts、session、tokens、最近事件、`last_error`）；未知 → `404 {error:{code,message}}`。
  - `POST /api/v1/refresh`：请求一次立刻的 poll+reconcile，`202 {queued, coalesced, requested_at, operations}`，允许合并重复请求。
- 设计注记：端点除运维触发外应只读；未支持方法返回 `405`；错误统一 `{"error":{"code","message"}}`；**面板若为前端应用，必须消费这套 API 而不是复制状态逻辑**（`SPEC.md:1620-1633`）。

实现结构（可直接对照 Farm 的后端/前端切分）：

| 层 | 文件 | 作用 |
|---|---|---|
| Router | `symphony_elixir_web/router.ex` | `/`(LiveView) + `/api/v1/*` + 静态资源 + 405/404 兜底 |
| API Controller | `observability_api_controller.ex` | 薄壳，全部委托 Presenter |
| **Presenter** | `presenter.ex`（242 行） | 唯一的投影层：snapshot → JSON（含 issue 详情、workspace 路径、事件摘要） |
| LiveView | `dashboard_live.ex`（446 行） | 1s 运行时 tick + PubSub 推送刷新；内嵌 CSS |
| PubSub | `observability_pubsub.ex` | 单 topic `observability:dashboard`，编排器每次状态变化后广播（`orchestrator.ex:1177-1180`） |
| TUI | `status_dashboard.ex`（**1941 行**） | 终端仪表盘：TPS 图、token 汇总、运行/重试/阻塞分区 |
| 资产 | `static_assets.ex` + `priv/static/dashboard.css` | 把 JS/CSS 内嵌进 release，避免外部依赖 |

### 8.3 Token 记账（容易被忽略但很关键，`SPEC.md:1419-1449` + `elixir/docs/token_accounting.md`）

- 只用**绝对累计**（`thread/tokenUsage/updated`、`total_token_usage`），**忽略 delta**（`last_token_usage`）；用"相对上次上报的差值"累计，避免重复计数。
- 运行时长按快照时点计算（累计已结束会话 + 活跃会话 elapsed），不要求后台定时累加。
- 实现里有一段相当细致的 payload 形状嗅探（`orchestrator.ex:1650-1890`），这是"跟一个仍在演进的第三方协议对接"的真实成本，Farm 应把它设计成**可测试的纯函数 + fixture 回放**，而不是散落在状态机里。

---

## 9. 依赖与运行形态

`elixir/mix.exs`：`bandit`(HTTP) / `phoenix` + `phoenix_live_view`(面板) / `req`(HTTP 客户端) / `jason`(JSON) / `yaml_elixir`(front matter) / `solid`(Liquid 模板) / `ecto`(配置 schema 校验，不落库) / `burrito`(打包单文件可执行) / `floki`+`lazy_html`(测试) / `credo`+`dialyxir`(静态检查)。

- **运行时外部依赖**：一个 tracker API、本地文件系统、可选 git、**Codex 可执行文件（app-server 模式）**、宿主机认证。
- **发布形态**：escript + Burrito 单文件（macOS/Linux × arm64/x86_64 四目标），CI 里 4 平台冒烟后更新 `nightly` 预发布（`mix.exs:78-112`、`.github/workflows/burrito-*.yml`）。
- **质量门**：`make all` = fmt-check + `specs.check` + credo --strict + **覆盖率阈值 100** + dialyzer（`Makefile:36-45`、`mix.exs:11-17`）。注意 100% 覆盖率是通过一长串 `ignore_modules` 达成的（把 CLI/Orchestrator/StatusDashboard/Web 全部排除），**这是"门很响、但实际豁免了很多"的典型**——Farm 若照抄这个门，要先决定豁免清单。
- **CLI 姿态**：必须显式传 `--i-understand-that-this-will-be-running-without-the-usual-guardrails` 才肯启动，否则打红框警告并退出（`cli.ex:96-140`）。这是把"高信任、无沙箱护栏"这一安全前提**做成不可跳过的仪式**，是很值得学的产品动作。

---

## 10. 它到底如何组织"AFK 多 agent 长任务"（核心问题）

把 Symphony 的 AFK 能力拆成可复用的机制，共 9 条：

1. **工作源是 tracker，不是聊天**。服务只读 tracker，把"要继续干哪件事"外包给人类改 issue 状态——这是"人在管理工作、不管理 agent"的实现基础（`README.md:7-10`）。
2. **确定性循环 + 单点权威**。AFK 的可靠性来自"没有 agent 做编排决策"：tick→对账→校验→取候选→排序→派发全是确定性代码；LLM 只出现在工作区内的 Codex 会话里（`SPEC.md:16-45, 733-850`）。
3. **有界并发 + 每状态配额**（`max_concurrent_agents` / `_by_state`），且重试在无槽位时以显式错误 `no available orchestrator slots` 重新入队，而不是丢弃或超发（`SPEC.md:777-789, 810-818`）。
4. **声明（claim）防重复 + 对账防陈旧**：`claimed`/`running` 双检查 + 每次 tick 的 tracker 状态刷新 + 派发前 revalidation（`SPEC.md:725-731, 819-840`）。
5. **故障分级重试**：正常退出 → 1s continuation（"再看一眼还活跃不"）；异常退出 → `min(10s * 2^(n-1), 5min)` 指数退避；上限可配（`SPEC.md:790-818`）。
6. **卡死检测**：按"最后事件时间"判 stall，而不是总时长，避免长任务被误杀（`SPEC.md:819-829`）。
7. **一个 worker 内的多 turn 长跑**：同 thread 连续 turn（首轮全量 prompt、续轮只给 continuation guidance），`max_turns` 封顶；这是"长任务"不丢上下文的关键（`SPEC.md:666-691`；`agent_runner.ex:101-154`）。
8. **工作区跨 run 持久复用**，终态才清理；agent 永不碰仓库源目录（§5）。
9. **写回由 agent 负责**：状态流转、评论、PR 由 agent 用 provider 原生工具做；"成功的 run 可以停在 `Human Review` 这种交接态，而不必是 `Done`"（`SPEC.md:16-45, 1310-1322`）。

### 与 mattpocock/skills（Farm 指定的"AFK 精髓"）的关系

`/implement-spec`（`docs/engineering/implement-spec.md`）是**会话内**（一个 harness session 内部）的编排：把 spec 的 tickets 读成任务图，按"frontier 全并行"派 implementer subagent，每个在自己 git worktree 里跑 `tdd`，逐个 merge 到 integration branch，最后跑一次 `code-review`。它明确说：**"真正 AFK 的工作，用确定性循环（Sandcastle / shell / CI）更快、更便宜、更可靠，因为没有 agent 做编排决策"**（同文件 "Does this replace Sandcastle or an AFK script?" 一节）。

这两者恰好是同一件事的两个高度：

| 维度 | `implement-spec` | Symphony |
|---|---|---|
| 编排者 | 一个 agent session | 确定性守护进程 |
| 任务图来源 | tracker 上的 blocked-by | tracker 状态 + label + dispatchable |
| 隔离 | git worktree / ticket | 每 issue 工作区目录 + 独立子进程 |
| 并发 | 一次 frontier 全上 | `max_concurrent_agents` + 每状态配额 + 退避 |
| 上下文传递 | context pointer（spec/ticket/notes/commit），不粘贴摘要 | prompt 模板渲染 issue + 同 thread continuation |
| 收尾 | 一次 code-review + 一个 fix subagent | 停在交接态，交给人；review 由 prompt/skill 定义 |
| 已知坑 | 并行 collision、blocked 边陈旧、worktree 缺 gitignore 文件 | provider 语义漂移、agent 卡在等输入、token 记账 |

**结论**：Symphony 是"AFK 的机器层"，`implement-spec` 是"AFK 的会话层"。Farm 要做的正是把后者那套工作流（spec→tickets→frontier→worktree→tdd→review→交接）**沉到前者那种确定性循环里**，并加上 Symphony 刻意不做的东西：面板、Rust 全栈、可插拔插件。

---

## 11. 值得借鉴（建议清单）

**A. 直接当基线照抄的（规格级）**

1. `SPEC.md` 的六层划分与"策略在 repo 内（WORKFLOW.md 式契约）"：Farm 的 prompt/工作流应当随项目版本化，并可热重载、非法重载保 last-known-good。
2. `Issue` 归一化模型与 `dispatchable` 显式化——**把 provider 语义关进 adapter，让调度器零 provider 分支**。
3. `workspace_key` 的"清洗 + ≥64bit 哈希后缀"防碰撞规则，与三条工作区安全不变量；Rust 里对应 `std::fs::canonicalize` + `Path::starts_with` 断言。
4. 双检查 claim + 每 tick 对账 + 派发前 revalidation。
5. 重试分级（1s continuation / 10s×2^n，带上限）与"无槽位→显式 requeue 原因"。
6. stall 判定用"事件静默"而非总时长；`turn_timeout_ms` 每次输出重置。
7. "一台 worker 生命周期不跨主机"（若 Farm 有远端执行需求）。
8. 面板/API 只做**投影层**（Presenter），且前端只消费 API 不复制状态；快照有显式超时/不可用语义。
9. 机密不进子进程环境（声明式 `secret_environment_names` + `unset`/`env=false` 双保险）；provider 工具在 host 侧执行。
10. CLI 的 guardrail 确认仪式——把"无沙箱、高信任"写进启动前置条件。

**B. 值得学但规格没写的（实现级超集）**

11. **`blocked` 是一等状态**：agent 要人工输入/审批时保留 claim 并显式暴露（API+面板），而不是静默挂起；重启清空。这直接回答了 Farm"面板上要看什么"的问题。
12. 事件流上要**同时**维护 `last_event` / `last_message` / `last_event_at` / `turn_count`，并做 humanize 摘要（纯观测用途，不得影响控制逻辑，`SPEC.md:1450-1458`）。
13. `test/symphony_elixir/*` 的组织方式：把编排器行为拆成 `core_test`(2364 行) / `orchestrator_status_test`(1803) / `workspace_and_config_test`(1671) / `app_server_test`(1631) 四个大文件 + 每 provider 一个 adapter 测试 + 一个 `extensions_test`（HTTP/面板/workflow store 的扩展面）。**"快照测试 + evidence 文档"**（`test/fixtures/status_dashboard_snapshots/*.{snapshot.txt,evidence.md}`）是面板类功能的低成本回归手段，Rust 侧可用 insta 等价实现。
14. 用 `Task.Supervisor` + `:one_for_all` 把"权威 + worker 池"绑成一个生命周期单元——Rust 对应 `tokio::task::JoinSet` + 一个"编排器 drop 即取消全部 worker"的 handle 结构。

---

## 12. 哪些不适合（或必须偏离）一个 Rust 全栈 + 面板 + 插件的项目

1. **规格明确"非目标"的部分不足以支撑 Farm 的目标**：`SPEC.md:60-69` 把"富 Web UI / 多租户控制平面"、"通用工作流引擎/分布式调度器"列为 non-goals，并强调"不规定具体面板或终端 UI"。Farm 要面板、要插件化，**这两条正是超集，需要自己立项**。
2. **调度状态完全在内存、无持久化**：规格把它当特性（`SPEC.md:1690-1705`），但同时留了"持久化重试队列与会话元数据"的 TODO。Farm 若要做"重启后面板仍能看到历史/审计/长任务断点"，必须自己设计持久化层——这是最大的架构增量，不能用 Symphony 现成方案覆盖。
3. **插件模型是编译期注册表，没有动态加载/隔离/权限**：`@adapters` 映射 + behaviour + `function_exported?` 探测（`tracker.ex:15-23, 103-118`）。Farm 的"可插拔插件"（吸盘/编排器/Warehouse/Crossline 那套）需要：动态加载、能力清单、权限与凭据边界、版本/协议协商、崩溃隔离。Symphony 只提供了"进程内 adapter"这一档，**不能照搬**。
4. **OTP 特异性**：GenServer 单点权威、PubSub、Port、Ecto changeset 做配置校验、LiveView 做面板。Rust 侧需要等价替换：`tokio` actor/`mpsc` + 单一 owner task；`tokio::sync::broadcast` 或 SSE/WebSocket 替代 PubSub；`tokio::process::Command` 替代 Port（**注意 line-based 分帧要自己实现**）；`serde` + 校验库替代 Ecto changeset；前端要么 SSR+SSE，要么 WASM（Leptos/Yew/Dioxus）——**规格对前端形态完全中立，所以这是纯自由决策点，不是迁移障碍**。
5. **100% 覆盖率门 + 大量豁免模块**：混着"可读性差的大文件"（`orchestrator.ex` 1990 行、`status_dashboard.ex` 1941 行）。Farm 若用 Rust，建议按 SPEC 的层切 crate，而不是学它把状态机堆进一个 2000 行文件。
6. **规格与实现的具体偏差（照抄前必须选一边）**：
   - **行分帧上限不一致**：规格建议 10MB（`SPEC.md:979-981`），实现用 1MiB（`app_server.ex:12`）。
   - **`server.host`、`observability.*` 是实现扩展键**，规格未定义（规格 §18.2 甚至把 observability 配置列为 TODO）。Farm 要么把它们升格为规格，要么另立契约。
   - **`blocked` 状态是实现超集**，规格无此模型。
   - **Ecto 的 snake_case/camelCase 归一化**（`config/schema.ex:466-473` 的 `normalize_keys`）决定了配置键的宽容度；Rust 用 serde 时要显式决定是否 `alias`。
7. **对 Codex app-server 的强耦合**：协议形状、方法名、错误分类都以"目标 Codex app-server 版本"为准（`SPEC.md:950-965`，且实现里到处是"字段名多形态嗅探"）。Farm 若要支持多 agent 后端（Codex/Claude/其它），**必须把这一层抽象成 Farm 自己的 agent-driver 插件契约**，而不是复制 Codex 形状——这也是 Farm 的"编排器插件"最该吃掉的一块。
8. **SSH 远端 worker 是一整块可用但粗糙的扩展**（`SPEC.md:2252-2313`）：工作区是 host-local 的，跨主机只能冷启动；主机失效/饱和必须**减少容量而不是回落到本地或重复执行**。Farm 若要做"吸盘"式的任意服务器部署，这里的坑清单（环境漂移、工作区本地性、路径与 shell 转义、启动失败 vs 工作区内失败的区分、清理归属）是现成的事故预防清单。

---

## 13. 关键默认值 / 不变量速查（实现者清单）

| 项 | 值 / 规则 | 证据 |
|---|---|---|
| poll 间隔 | 30s（默认） | `SPEC.md:402-408` |
| 并发 | 全局 10；可按状态覆盖（状态名 trim+lowercase 后查表） | `SPEC.md:444-462` |
| max_turns | 20 | `SPEC.md:448-450` |
| 退避 | 正常退出 1s 固定；异常 `min(10s·2^(n-1), 300s)` | `SPEC.md:798-805`；`orchestrator.ex:13-14, 1232-1246` |
| stall | 300s 无事件（`<=0` 关闭） | `SPEC.md:485-491` |
| turn 静默超时 | 3600s，每次输出重置 | `SPEC.md:481-484` |
| 读超时 | 5s | `SPEC.md:486-487` |
| hook 超时 | 60s | `SPEC.md:438-443` |
| 排序 | priority 1..4 → created_at 旧优先 → identifier 字典序 | `SPEC.md:767-776` |
| 工作区 | 跨 run 复用；成功不删；终态删 | `SPEC.md:860-867, 841-850` |
| 派发资格 | 有 id/identifier/title/state ∧ active∧¬terminal ∧ dispatchable ∧ 必含 label ∧ ¬running ∧ ¬claimed ∧ 有槽位 | `SPEC.md:754-766` |
| session_id | `<thread_id>-<turn_id>` | `SPEC.md:312-317` |
| 扩展键 | 未知顶层键必须忽略 | `SPEC.md:368-374` |
| 日志必需字段 | `issue_id` + `issue_identifier`；会话日志加 `session_id` | `SPEC.md:1361-1378` |
| 错误分类 | tracker: `unsupported_tracker_kind / invalid_tracker_config / missing_tracker_secret / tracker_request / tracker_status / tracker_response / tracker_pagination / tracker_rate_limited`；codex: `codex_not_found / invalid_workspace_cwd / response_timeout / turn_timeout / port_exit / response_error / turn_failed / turn_cancelled / turn_input_required` | `SPEC.md:1286-1309, 1142-1162` |

---

## 14. 可直接复用的"验收矩阵"

`SPEC.md` §17（测试与验证矩阵，`SPEC.md:2049-2203`）分三档：**Core Conformance**（所有合规实现必测）、**Extension Conformance**（只测你选择实现的可选特性）、**Real Integration Profile**（生产前建议跑、凭据缺失时允许跳过但必须报"跳过"而不是"通过"）。§18 是同档的 Definition of Done 清单（`SPEC.md:2204-2251`）。

对 Farm 的价值：**这是一份现成的、按"层"组织的验收清单**（配置解析 / 工作区与安全 / tracker adapter / 调度对账重试 / agent 客户端 / 可观测性 / CLI 生命周期），其中每一条都写成了可断言的句子（例如"空 `fetch_issues_by_states([])` 不得发起 provider 请求"、"清洗后同名的不同 identifier 必须得到不同 workspace key"、"`dispatchable=false` 不得被派发"）。Farm 的 Rust 实现可以直接把它转成集成测试清单，并在其上追加自己那三块规格里没有的部分（面板、插件、持久化）。

---

## 15. 未覆盖 / 存疑（供后续核实）

- 本报告未逐行审读：`status_dashboard.ex`(1941 行) 的渲染细节、`dashboard_live.ex` 的模板与 CSS、各 provider `client.ex` 的 HTTP 细节（linear 657 / jira 532 / gitlab 419 / asana 416 / github 397 行）、`lib/mix/tasks/pr_body.check.ex` 与 `specs_check.ex`（自定义 lint 规则：要求公开函数有 `@spec`、要求 PR body/spec 一致性）。
- 源码获取：`git clone --depth 1 https://github.com/openai/symphony` 在本环境反复失败（`invalid index-pack output` / 磁盘 tmpfs 剩余空间紧张），改用 `https://codeload.github.com/openai/symphony/tar.gz/refs/heads/main` 解包到 `/tmp/farm-research/symphony`，因此目录内无 `.git`；commit 身份由 GitHub API 单独核对（见文首）。
- 未运行任何测试/构建（环境无 Elixir/mise；且此仓库只是外部参考），因此"实现是否真的通过 100% 覆盖率门"未经实测，仅从 `mix.exs` 与 `Makefile` 读取。
- `SPEC.md` 称 "Status: Draft v1"，且 README 标注为低调度工程预览：**规格本身是移动目标**，Farm 若以其为基线，应在自己的仓库里冻结一份并记录偏离（正如 `elixir/AGENTS.md:14-18` 要求实现方做的那样）。
