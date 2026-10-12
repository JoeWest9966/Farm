# 源码深读报告：Orchestrator 与 Nous-Crossline

范围：`/tmp/farm-research/Orchestrator`（JoeWest9966/Orchestrator）、`/tmp/farm-research/Nous-Crossline`（JoeWest9966/Nous-Crossline），均为 `git clone --depth 1` 到本地后逐文件阅读。
目的：为 Farm（Rust 全栈 AFK + 面板 + 可插拔插件）判断这两个仓库的定位匹配度与可插拔可行性。
阅读方式：全量文件树 + 全部文档（README/AGENTS/CONTEXT/ADR/issues）、全部生产代码（daemon/CLI/protocol/ledger-relay/conformance）、Rust 侧（Cargo workspace / xtask / generated.rs / 测试）。实跑了测试（见 §5）。

---

## 0. 结论速览

| 仓库 | 实质内容 | 与"Agent 与 management 之间的协调者"匹配度 | 作为 Farm 可插拔插件的可行性 |
|---|---|---|---|
| **Orchestrator** | **只有 2 个 markdown 文件，27 行，零代码**。一张 mermaid 草图 + 5 行解释。是一份**意图声明**，不是组件。 | 定位陈述与你的目标**逐字吻合**，但**零实现**：没有接口、没有契约、没有边界定义。 | **不适用**。它不是"可接入的组件"，而是**Farm 编排器本体的一份古典草图**——应当被 Farm 实现，而不是被 Farm 编辑 |
| **Nous-Crossline** | TS monorepo（5 个包 + spike + Rust workspace）+ 14 篇 ADR + 43 张票的 wayfinder 地图。约 **10.7k 行 ts/rs**（自述生产代码 3,989 行、零生产依赖）。Rust 重写**刚起步（1/10 片，只有类型生成+黄金语料回环）**。 | 它是"协调者的**肢体**/南向接口"——看（事件流）+ 控（prompt/abort/审批）已经完整可跑；**北向（management=github）完全为零**。 | **推荐形态：Rust 核心 crate 进程内嵌入 + wire 协议跨机**。可行性高且有明确路径，但被三个硬约束与三处缺口限制（§4.3）。 |

一句话：**Crossline 是已经写好的"手和眼"，Orchestrator 是还没写的"脑"。Farm 要不要写这个脑，取决于 Farm 与"编排器"是否本来就是同一个东西。**

---

## 1. Orchestrator 仓库

### 1.1 内容清单（全量）

```
1-Orchestrator草图.md    22 行
2-Orchestrator解释.md     5 行
```

没有 `Cargo.toml`、没有 `package.json`、没有 `src/`、没有 `LICENSE`、没有 CI。仓库的**全部**内容如下。

`1-Orchestrator草图.md` 的全部实质内容是一张 mermaid 图：

```
Top[顶层架构]:  M[management] <--> O[Orchestrator] <--> A[Agent]
M --> GH[github]
M --> FUT[未来还会增加其他 比如: Gogs]
O --> PL[插件]
O --> API[API]
```

`2-Orchestrator解释.md` 全文 5 条：

1. 本项目就是类似 openai/symphony 里面的 Orchestrator 组件，专门负责 Agent 和 management 之间的协调者
2. 不同的是本项目可以直接用命令来创建启动 agent（工具我做了 Nous-Warehouse，我们将会把他开发为 Orchestrator 的可插拔插件）
3. API/CLI 指的是本项目未来也会被开发为插件，因为真正的面板要用此项目实现 Nous-Farm（我还没开发）。FARM 就是我最终的 AFK 架构，决定是前后端都是 rust
4. Agent 的实现是本项目触发
5. management 目前仅作 github，未来会增加别的

### 1.2 功能 / 架构 / 代码质量 / 完成度

- **功能**：无。图上四个方框（management、Orchestrator、Agent、插件、API）没有任何一个被定义过语义。
- **架构**：星型中继——`management ↔ Orchestrator ↔ Agent`，Orchestrator 两侧各挂插件与 API。**这正是 Crossline 的 ADR-0003 明确否决的"控制器拓扑"**（见 §4.3 冲突点）。
- **技术栈**：无（只有中文 markdown）。
- **完成度**：0%。它不是"早期项目"，而是"命名过的想法"。

### 1.3 它与 Crossline 的关系（同一作者的地图自己说了）

Crossline 地图（`.scratch/crossline-v1/map.md`）与 `AGENTS.md` 都明确写着：

- "**编排器（未来）：大脑。Crossline 是编排器的肢体：感官（看）+ 手脚（控）。**"（AGENTS.md）
- "**编排器本体**与**完整后门**：指令→图纸→N 个容器的决策调度" 列在地图的 **Not yet specified**（雾）里
- "**编排器与工厂的完整实现**：本图目的地止于「肢体」；**编排器是 v1 之后的独立 effort**" 列在 **Out of scope**
- 地图里另有一句关键裁决：面对"编排器上台后要不要长成中央基站"，答案是 **ADR-0003 信任侧联邦：不长成中央式**。

所以三者的层级关系是：**Crossline（肢体，已实现）→ Orchestrator/Farm 编排器（脑，未实现）→ Farm 面板（脑的操作界面，未实现）**。Orchestrator 仓库**不是** Crossline 的插件，而是 Crossline 的**上位消费者**。

### 1.4 结论

- 与"Agent 与 management 之间的协调者"的**定位匹配度：100%（写在纸面上）**；**实现匹配度：0%**。
- 作为 Farm 插件的可行性：**不适用**——没有可插的对象。它真正的价值是一份"我为什么要做 Farm"的意向书。
- **它反而暴露了 Farm 最需要先定、现在完全没定的四件事**（这些是 Farm wayfinder 该开的票，不是本文档能替你决定的）：
  1. **management 抽象**：github 之外还有 Gogs，那么 management 的接口是什么？（PR？issue？branch？webhook？轮询？）
  2. **Agent 生命周期**：谁拥有容器、谁拥有"图纸"（provision）、失败重试与回收归谁。
  3. **插件契约**：插件是进程内 Rust trait、独立进程、还是 wire 协议上的对等体？（Orchestrator 草图对"插件"与"API"各画了一个方框，没有区分二者）
  4. **面板与 API 的关系**：面板是 API 的消费者（必然）还是与 API 同进程（Rust 全栈下可能）。

---

## 2. Nous-Crossline 仓库

### 2.1 它是什么

README 第一行自述："**编排器的肢体**：一颗**芯片**（Pi 扩展，装进每个 agent 进程）+ 每台服务器一个**后门**（daemon）+ 一个**遥控器**（CLI）。芯片只出站拨号——不开入站端口、host 不装 agent 侧东西；遥控器经 Tailscale 连后门：看见 agent 的对话、给它发消息、打断它、审批它的工具调用。"

领域词汇（`CONTEXT.md`，中文专名，Farm 侧如果复用应照抄以免词汇漂移）：

| 术语 | 含义 |
|---|---|
| 线/航线 | 整套系统：后门、遥控器、芯片、图纸 + 两条链路 |
| 后门 | 每台服务器常驻 daemon（本库）。芯片拨它，遥控器经 Tailscale 连它 |
| 遥控器 | 操作者手里的 CLI（本库），只与后门对话 |
| 芯片 | 装进 agent（Pi）进程的扩展，随 Pi 版本走（**姊妹库** `Pi-Crossline-Extension`） |
| 图纸 | 一条命令在容器里拉起植入芯片的 agent 的 provision 脚本 |
| 链路 A / B | 遥控器↔后门（TCP+token）/ 芯片↔后门（UDS 挂载即凭据） |
| 靶子 | 一次通信指向的 agent = `server_id / container_name / session_id` 三键 |
| 舰队 | 所有服务器/容器/会话的整体视野 |
| 账本 / 中继 | 只记元数据、绝不记对话内容的耐久记录 / 后门侧送它进账本的部件 |

### 2.2 技术栈与代码规模（实测）

| 路径 | 行数 | 角色 |
|---|---|---|
| `packages/protocol/lib/wire.ts` + `wire.generated.ts` + `version.ts` | ~ | 协议唯一源：信封编解、帧守卫、闭合词汇表、版本三轴 |
| `packages/backdoor/lib/daemon.ts` | **1011** | 后门本体（**被 ADR-0007 判为"神对象，必须拆开而不是翻译"**） |
| `packages/backdoor/lib/registry.ts` / `cursor.ts` / `channel.ts` | 249 / 105 / 70 | 注册表 + 活性 / 补帧游标 / NDJSON 帧 |
| `packages/backdoor/tests/daemon.test.ts` | 944 | wire 层测试（57 用例，地图记载） |
| `packages/cli/lib/*.ts` | ~450 | 遥控器：args/run/render/client/errors/target |
| `packages/conformance/` | 1140 | 一致性套件（场景层 40 + 黄金帧 2），**Rust 迁移的准入门槛** |
| `packages/ledger-relay/` | 1132 | 账本中继（可选、注入式） |
| `spike/` | ~ | e2e harness（真 Pi + 真芯片 + 真后门 + 真 CLI，24 断言） |
| `crates/channel/src/generated.rs` | 1174 | **Rust 侧由 schema 生成**的类型（23 defs） |
| `crates/xtask/src/main.rs` | 63 | 生成器（typify） |
| `crates/channel/tests/golden_roundtrip.rs` | 61 | Rust 侧唯一测试：黄金语料全量反序列化+回环 |
| 合计（ts+rs） | **10,751** | |

技术栈：**TypeScript（ESM，`node --experimental-strip-types`，Node 22，零运行时依赖）+ JSON Schema + typify（Rust 侧生成）+ Rust（cargo workspace，toolchain 钉在 `rust-toolchain.toml` = 1.99.0）**。
**没有** HTTP 服务、没有 REST/gRPC/WebSocket、没有数据库、没有 web 前端、没有 CI 工作流（`.github/` 不存在；所有门只在 `npm run check` 里手动跑）。

### 2.3 架构

**两个链路，一张信封**（NDJSON，LF 唯一分隔符，一行一个 JSON）：

```
遥控器(CLI) ══Tailscale TCP + daemon token══ 后门(daemon) ══容器内 UDS(主) / TCP+token(降级)══ 芯片 ⇆ Pi
                    链路 A（控，跨机）                      链路 B（拨，机内，芯片出站）
```

**三个平面**：身份平面（`hello/register/ping/bye/fleet.*`，后门所有）、命令平面（`session.*`，芯片是唯一 schema 裁决者，后门是中继而非翻译器）、事件平面（芯片唯一出声路径）。

**深模块纪律（机器把守）**：`packages/README.md` 定义"包=深模块，公开面=包根入口点，子目录皆私有"，并由 `.dependency-cruiser.cjs` 四条 `error` 级规则强制执行（入口点边界、跨包只走入口点、测试只走入口点、无环）。这是**"可插拔/可替换实现"的既有文化证据**：`packages/example/` 就是一份"复制即用、可删"的模板包。

**两库分工**：本库（后门+CLI+协议，同一个 semver，git tag 发布，逐台 pull+重启，无自动升级）；姊妹库 `Pi-Crossline-Extension`（芯片，版本 = `<所随 Pi 版本>-crossline.<n>`，Blueprint 烤镜像）。**跨仓契约的最终守卫是 `packages/conformance` 那份"同一份断言能跑在两个实现上"的套件**（ADR-0010），不是任何一侧的类型系统——因为芯片永远留在 TS。

### 2.4 对外接口（这是 Farm 最需要的部分）

#### (a) wire 协议（唯一对外契约，也是唯一的扩展面）

- **信封**：`{"id":"…","method":"…","params":{…}}` / `{"id":"…","ok":true,"result":{…}}` / `{"id":"…","ok":false,"error":{"code":"…","message":"…"}}` / `{"event":"…","target":{…},"seq":N,"data":{…}}`。
- **七条铁律**（`packages/protocol/README.md` §1）：`id` 必须字符串；**一个 id 只有一个 response，永不追加**（ack=接受≠送达≠完成）；snake_case 上线（entries payload 原样 camelCase 透传是唯一例外）；`seq` 是缺口探测器不是连续性保证；`target` 骑在每个事件上；请求行上限 1 MiB；后门是中继不是翻译器。
- **闭合词汇表**（由 `wire.schema.json` 生成文档，改 schema 才能改，且 `check:docs` 门禁止文档与代码分歧）：
  - **12 个方法**：`hello` `register` `ping` `bye` `fleet.snapshot` `fleet.subscribe` `session.subscribe` `session.snapshot` `session.entries` `session.prompt` `session.abort` `session.tool_decision`
  - **11 个事件**：`agent_start/end`、`message_start/update/end`、`turn_start/end`、`tool_call`、`resync_required`、`session_switched`、`ui_request_expired`
  - **12 个错误码**：`invalid_request` `unknown_method` `unauthorized` `not_found` `gone` `target_offline` `busy` `timeout` `version_unsupported` `unsupported` `cursor_invalid` `internal`（**闭合集，无 catch-all**）
- **版本三轴 + 窗口**：协议号（独立小整数，只对破坏性变更 bump，v1 预计恒 1）、本库 semver、芯片库版本；每条连接首帧必带 `protocol`+peer 串，两侧窗口 `[MIN, CURRENT]`，窗外拒旧且**拒旧要 Loud**（人话点名双方版本与修法）。peer 串一律由 `package.json` 生成，禁止手写。
- **线参数**：UDS `/run/crossline/backdoor.sock`（0666，挂载即凭证）、TCP 9735（Tailscale 上，token 鉴权）、state `/var/lib/crossline`、心跳 10s×3 miss→offline、控制类 deadline 5s / `session.entries` 60s、芯片分块 512 KiB、ring buffer 512 条/2 MiB/60s、审批 60s 超时默认**放过**。
- **补帧**：唯一原语 `session.entries`；**游标由后门持有，客户端永远不能指定**（只能 `reset:true`）；`cursor_invalid` 由后门自消化；陈旧回复按 `session_id` 校验整条丢弃答 `gone`。

#### (b) CLI（遥控器）——今天唯一的人机面

六个动词：`list` / `view <target>` / `send <target> <msg> [--queue-as steer|followUp]` / `abort <target>` / `approve <target>` / `deny <target> [--reason]`。
契约：退出码 **0 成功 / 1 线上错误 / 2 用法错误**；每个错误码带**一行可行动 hint**；`--json` 出机器可读；`--daemon`/`--token` 可省走环境变量 `CROSSLINE_DAEMON_URL`/`CROSSLINE_DAEMON_TOKEN`。**面板要复用其渲染与错误契约，这六个动词 + hint 表就是规格**（`packages/cli/README.md`）。

#### (c) 库入口点（进程内嵌入面）

- 后门：`startBackdoor(options: BackdoorOptions): Promise<Backdoor>`（`packages/backdoor/index.ts`），返回 `{udsPath, tcpPort, stop}`。
- `BackdoorOptions` 的可注入旋钮：`token`、`udsPath`、`tcp`、`fallbackToken`、`stateDir`、`pingIntervalMs`/`pingMisses`、`entriesDeadlineMs`/`controlDeadlineMs`、`cursorTtlMs`、`now()`、**`log()`**、以及 **`ledgerDelivery?: LedgerDeliveryBridge`**。
- **`LedgerDeliveryBridge` 是这个代码库里唯一一个"插件式"注入点**（`{start?(hooks), stop?(), health()}`），`ledger-relay` 就是通过它挂进去的：daemon 给它两个钩子（`liveSessions()`、`relayEntries(target, since)`），它自己负责发现会话树、只取元数据、spool、投递。**Farm 的"可插拔插件"若要在 Crossline 侧落地，最自然的先例就是把这个 bridge 泛化**（而不是新造一个插件系统）。
- 遥控器：`runCli(argv, io): Promise<number>`（`packages/cli/index.ts`），`CliIo` 可注入（测试/面板复用渲染）。
- 协议：`packages/protocol/index.ts` + **`wire-peer.ts`（唯一的"原始对端"入口点，一致性套件与替身共用）**。
- 一致性套件：`ImplementationAdapter { name, start(), stop() }`（`packages/conformance/lib/suite.ts`）——**"换实现只换这一个文件"**，这正是 Farm 日后对接 Rust 实现的正式接缝。

### 2.5 代码质量与完成度

**做得好的（可核查证据）**：

- 契约有唯一可 review 的源（`wire.schema.json`，552 行），两侧类型 + 替身 + 文档词汇表全部生成；`npm run check` 五道门：`typecheck` + `depcruise 边界` + `check:generated` + `check:generated-rs` + `check:docs`（后两道是"重生成必须留净树"）。
- 一致性套件分层且**known-gap 是绿的**（"看得见的缺口是计划，看不见的缺口是事故"），黄金帧层从 daemon 录制并钉字节 + MANIFEST 记 commit/protocol 版本。
- 日志红线：只记元数据，命令文本/对话原文/token 永不进日志（代码里多处注释与实现一致）。
- 安全红线成文且有实现：芯片不碰 provider 密钥、只出站、命令白名单、审批门是红线 3 的唯一破例（只读 input、绝不改写、永不用 terminate、60s 有界默认放过）。
- 决策全部落成 **14 篇 ADR**（0001–0014），每篇有 Considered Options 与 Consequences；地图 43 张票有 Resolution 记录（含失败与残留），可追溯性极高。

**弱点与已知缺口（作者自己记的 + 我核对源码确认的）**：

| 缺口 | 位置/证据 | 影响 |
|---|---|---|
| `daemon.ts` 1011 行神对象 | ADR-0007 判死，待 Rust 拆 | 嵌入/可测性差；Farm 若嵌 TS 版本会背这个包袱 |
| **链路 B 的 UDS 是 0666 且注册不校验 token** | 票 34 resolution 原文；`daemon.ts` `chmodSync(udsPath, 0o666)` 确认 | 服务器上任何进程都能注册假芯片，从而收到遥控器转发的 prompt 内容并回传伪造快照 |
| **daemon token 是全舰队共用的配置常量** | ADR-0002 审计（`daemon.ts` token 单值） | 一台机失守 = 全舰队遥控权；per-server/per-container 派生在票 32（blocked by 31） |
| 审批门 **fail-open**（60s 超时/掉线/异常一律放过） | 票 12/19 + 芯片侧红线 | 规模上"门故障 = 静默失守"，票 23 明确留到编排器时代再定策略 |
| CLI 一遥一后门，**无 fleet 联邦实现** | README/地图；`list` 是单后门的 `fleet.snapshot` | Farm 面板要跨机视野必须自己联邦（协议已定，代码没有） |
| `spike/backdoor.ts` 曾是生产入口且**零监督** | ADR-0014；票 32 | 三套 env 混乱之源；32 票未发车 |
| 两个仓库**都没有 LICENSE 文件** | 文件树全量确认 | 若 Farm 要链进 crate/复用代码，授权状态需要先澄清 |
| 无 CI | 无 `.github/` | 五道门靠人手动跑 |

**我实跑的测试结果（重要）**：

- `node --experimental-strip-types --test packages/*/tests/*.test.ts`：第一跑 **77 tests / 76 pass / 1 fail**；第二跑同命令出现 **3 个 `not ok`（#19 `the daemon-held cursor survives a daemon restart`、#20 `events are forwarded with per-subscription seq…`、#32 `a forwarded tool_call derives the session's approval_pending`）**。
- 这是**已知偶发**：地图的"flaky session-switch 根因"票把根因归为**测试替身不忠实**（FakeChip 从不发心跳，测试的离线预算 20ms×2 比真实场景紧百倍），并在生产代码零改动的前提下靠替身补心跳"修好"过；我在干净 clone 上复现出新的偶发红，说明**该抖动并未真正消失，只是换了一组用例**。
- `cargo test`：**绿**（1 passed，`every_pinned_line_deserializes_and_round_trips`，即 Rust 侧能吃掉黄金语料全部 28 行）。

### 2.6 Rust 重写现状（对 Farm 而言最关键的一节）

**为什么重写（ADR-0001，明确"不是性能工程"）**：本库仅 3,989 行生产代码、零生产依赖、只用 6 个 Node 内建模块，**没有规模问题可解**。要的是四样 TS 给不了的东西：

1. **契约即代码**（题 30/33 已交付：schema 单一源 + 两侧生成 + 一致性套件）
2. **核心即可嵌入**（那 3,989 行变成库，daemon 与 CLI 是薄壳；明确点名"可被嵌进测试、CLI、**以及未来的舰队控制器**"——**这句就是为 Farm 写的**）
3. **状态即一等公民**（但范围按 ADR-0006 精确划定）
4. **单一静态工件**（一个文件，无 Node、无 npm、无 `--experimental-strip-types` 版本地板）

**已定的形状**：核心 crate 按领域切 `channel / conn / registry / cursor / router / subscription / approval`（ADR-0007，神对象拆开而非翻译）；**wire 冻结**（peer 串、npm 名、协议字节一律不变，语言变化对线上不可见）；耐久只 spool + 游标、**不引入 redb/SQLite**（ADR-0006）；隔离靠**精确禁止**——seccomp 禁对新 socket 的 `connect()`、Landlock 限 state dir 与 socket 目录，不做全局白名单（ADR-0012）；三套 env 合一到 `CROSSLINE_*` + sd_notify + 结构化 tracing（ADR-0013）；**信任侧联邦 + 静态名册**（ADR-0003/0011）；后门发布地址、蓝图注入（ADR-0004）。

**落地进度（实测）**：

- **已有**：`rust-toolchain.toml`（1.99.0 stable，`cargo --version` 确认）、根 Cargo workspace（members = channel + xtask，version 0.1.0，edition 2021，`Cargo.lock` 入库）、`crates/xtask`（typify 生成器，读 `wire.schema.json` 写 `crates/channel/src/generated.rs`，1174 行 23 defs）、Rust 侧 1 个测试（黄金语料回环，绿）、`npm run check` 已接 `check:generated-rs` 门。
- **没有**：**Rust daemon 与 Rust CLI 一行都还没写**。执行拆解为 10 个纵向片（票 34–43），当前前沿 = 票 34「身份平面端到端」（`Status: ready-for-agent`、`Assignee:` 空），35–43 全部 blocked。
- **迁移裁决（已定，无需 Farm 重新决定）**：**原地换**——同 socket 路径 / 同 TCP 端口 / 同 token，芯片与遥控器无感；蓝绿被否决（目录绑定下换路径=改图纸挂载=重 provision）；**daemon 优先、ncctl 随后**；回滚 = 停 Rust 起 TS（自愈双向，票 20 已证芯片能自动重注册 + 补帧不重不漏）。

**对 Farm 的含义**：**Farm 今天只能对接到 TS 版后门**（协议字节冻结，所以这条路是安全的、且日后原地换 Rust 对 Farm 也应当无感——但 Farm 会成为新的"线上对端"，因此 Farm 的客户端必须自我遵守同一套窗口/错误码/hint 契约，否则"无感迁移"这一性质会在 Farm 侧被打破）。

---

## 3. 与"Agent 与 management 之间的协调者"这一定位的匹配度

| 定位要素 | Nous-Crossline 现状 | Orchestrator 现状 | 差距 |
|---|---|---|---|
| 与 **Agent** 的通道 | **已成**：芯片经挂载 UDS 注册/心跳、事件流实时转发、命令白名单（prompt/abort/tool_decision）、断线退避重连 + 补帧游标、审批门（含 deny 原因逐字进模型上下文） | 0 | 无实质差距，可直接复用 |
| 与 **management**（github）的通道 | **零**：本库只有 host 侧 ledger 中继；github/PR/branch/webhook 一个字都没有 | 图上两个方框（`M --> GH`） | **最大缺口**。symphony 式编排的核心（issue/PR 驱动）在这里没有任何设计 |
| **创建/启动 Agent** | 有"图纸"（`provision.sh` + Dockerfile），但**住在姊妹芯片库**，是 shell + docker，不是 API/Rust | 说要用 Nous-Warehouse 产出 CLI 命令 | 图纸不属本库 → Farm 想"用命令创建启动 agent"需要跨到第三个仓库，且当前是脚本不是接口 |
| **插件 / API** | **没有插件系统**。有四个接缝：wire 协议、`startBackdoor(options)` 的 `LedgerDeliveryBridge`、包入口点（深模块）、(计划中) 领域 crate 边界 | 草图里"插件"与"API"各一方框 | "可插拔"在 Crossline 侧**没有现成定义**，需要 Farm 侧先定义契约再反向要求 Crossline 暴露面 |
| **面板** | 地图归雾："遥控器富 UI（web/移动端）——**Collie 的接班人形态**"；协议已备 `session.subscribe`/`fleet.subscribe` 长连订阅，但 CLI 只用一次性连接 | 说面板由 Farm 实现 | **Farm 的面板正是 Crossline 已经等着的那个消费者**：订阅长连 + 渲染 + 审批推送 |
| 调度/决策/"指令→图纸→N 个容器" | 地图 **Not yet specified**（雾） | 一句"management ↔ Orchestrator ↔ Agent" | 全空白 |

**定位结论**：

- 把"协调者"拆成南向（对 Agent）与北向（对 management）两半：**Crossline 独占南向且已完成度很高；北向为零；调度逻辑为零。**
- **Orchestrator 仓库目前不是"协调者组件"，而是一张图**。它提供的**全部价值是明确了 Farm 的位置**：Farm = 脑 + 面板；Crossline = 肢体。任何把 Orchestrator 当"可接入依赖"的计划都应先放弃。
- ⚠️ **一个必须先解决的架构冲突**：Orchestrator 草图是**中央控制器拓扑**（management ↔ Orchestrator ↔ Agent，两侧挂插件/API）；而 Crossline 的 ADR-0003 已**明确否决中央控制器**并给出理由（"它会把智能和凭证搬到被暴露的那一侧"），ADR-0002 更要求**后门永不出站**（seccomp 禁 `connect()` 强置）、ADR-0011 要求**名册是操作者机器上的静态配置**。两者不可能同时照字面实现：**Farm 若要 Crossline 当肢体，Farm 自己就必须扮演"信任侧联邦者"**——持每台后门各自的 token、逐台出站拨号、绝不让任何服务器侧组件拨回 Farm。这是 Farm 的架构决定，不是我能在报告里替你决定的；但它**直接否掉了"Farm 是一个中央服务器 + 各机 agent 回调"的最省事形态**。

---

## 4. 作为 Farm 可插拔插件的可行性评估

### 4.1 Orchestrator

**不适用**：无可插对象（0 行代码、无产物、无接口）。建议在 Farm 的地图里把它**作为"目的地本身"的原料**（它就是 Farm 编排器的一份草图），而不是作为依赖。

### 4.2 Nous-Crossline：可行性 = 高，但形态必须选对

| 维度 | 评估 |
|---|---|
| 语言 | 本库 Rust 化已在路上（目标形状 = 核心 crate + 两个薄壳），**与 Farm 的 Rust 全栈同构**；芯片永远 TS（Pi 是 TS 的），是唯一不可 Rust 化的一半 |
| 接口清晰度 | **极高**：wire 协议是唯一跨机契约，闭合词汇表 + 生成物 + 一致性套件；库侧只有 `startBackdoor` 与 `runCli` 两个入口点。这正是"可插拔"友好的形状 |
| 进程模型 | 推荐**进程内嵌入**（核心 crate 作 Farm 依赖，`daemon` 是 Farm 侧薄壳），跨机走 wire。SQL/incompat: ADR-0001 第 2 条已为此铺好 |
| 依赖/部署 | 目标"单一静态工件、无 Node、无 C 依赖"；今天 TS 实现需 Node 22 + `--experimental-strip-types` |
| 安全 | 强项：不出站（内核强置）、挂载即凭证、元数据日志、闭集错误码。**弱项：全舰队共用 token、UDS 0666 假芯片可注册、审批 fail-open** —— 单人自用舰队可接受，**编排器规模下会被放大成"静默失守"** |
| 测试/可验证性 | 强：一致性套件是"给重写发通行证"的机构；黄金字节钉；known-gap 机制。**弱：我的两跑出现偶发红**（#19/#20/#32），CI 缺位 |
| 可嵌入性 | 计划中（ADR-0007 拆解），今天 1011 行神对象未拆 |
| 维护活性 | 高：ADR 14 篇、地图 43 票、有明确前沿（票 34）与已定的迁移裁决 |
| 授权 | **两仓库均无 LICENSE**——需要在复用前澄清 |

**推荐的插件形态（三选一，按推荐排序）**：

1. **进程内 crate 依赖（推荐）**：等 Rust 片落地后，Farm 的编排器/面板后端直接 `cargo add` Crossline 核心 crate，把 `startBackdoor` 装进 Farm 进程（或 Farm 的 per-server agent），CLI 的六动词渲染与 hint 表被面板复用。理由：ADR-0001 明写"核心可被嵌进……未来的舰队控制器"，这正是为 Farm 准备的形状；且 Rust 全栈下这是唯一不引入额外进程/协议翻译的路径。
2. **wire 协议对等体（今天就能走）**：Farm 作为链路 A 的客户端，实现 12 方法/11 事件/12 错误码 + 窗口握手，直接对接 TS daemon；日后原地换 Rust 无感。代价：Farm 必须自己联邦 N 台后门（无跨机实现可借），且必须自己做长连订阅（`session.subscribe`/`fleet.subscribe` 契约在，代码不在 CLI）。
3. **进程外子进程 + 现有 CLI（最省事，不推荐长期）**：Farm 调 `ncctl` 命令解析 stdout。违反"面板要有结构"的目标，且把核心变成文本接口。

**必须在 Farm 侧先定的三件事（越权不决，仅列出）**：

- **联邦者身份**：Farm 面板后端 = 信任侧联邦者（持 N 个 token、逐台拨号、静态名册），还是引入一个农场内服务？ADR-0003/0011 只允许前者（后者=被否决的控制器拓扑）。
- **插件契约落点**：是把 Crossline 的 `LedgerDeliveryBridge` 式桥接泛化成 Farm 的插件接口，还是在 Farm 侧另立插件宿主（trait/动态库/子进程）再把 Crossline 当第一个插件。
- **协议扩展政策**：Farm 若要在线上加"编排"命令（建 agent / 跑图纸 / 批量注入），必须走 schema **加法**（新方法/新事件 → 不 bump 协议号，旧端 → `unknown_method`）并同步 2 个仓库 + 一致性套件；**禁止走旁路 API**，否则冻结字节的迁移保证失效。

### 4.3 三处缺口在 Farm 规模下会放大（风险清单）

1. **审批门 fail-open**：60s 超时/掉线/异常一律放过。AFK 场景下这是"门故障=静默失守"，票 23 已把它标为编排器时代的必答项。
2. **凭据模型**：全舰队共用 daemon token + UDS 0666 无 token 注册 → 单台机失守即全舰队遥控权。per-server/per-container 派生在票 32，而 32 blocked by 31（Rust）。
3. **监督与配置债**：`spike/backdoor.ts` 曾是生产入口且零监督、三套 env 混乱（票 32）；systemd 单元存在但从未被安装且自身三处路径是坏的（票 31 resolution 记载）。

---

## 5. 验证方式（本报告的实测证据）

| 命令 | 结果 |
|---|---|
| `git clone --depth 1`（两仓库） | 成功；Crossline 155 个文件（不含 .git） |
| `find`/`wc -l` 全量文件树与行数 | Orchestrator = 2 文件 27 行；Crossline ts+rs 合计 10,751 行 |
| `node --experimental-strip-types --test packages/*/tests/*.test.ts`（Crossline，共两跑） | 第一跑 77 tests / 76 pass / **1 fail**；第二跑出现 **3 个 not ok**（#19/#20/#32）→ 偶发红，见 §2.5 |
| `cargo test`（Crossline) | **passed**：1 test，`every_pinned_line_deserializes_and_round_trips` |
| `cargo --version` | cargo 1.99.0（5f94df48 2026-08-27），与 `rust-toolchain.toml` 一致 |
| 全文阅读 | Crossline：README / CONTEXT / AGENTS / packages README / protocol README / 14 篇 ADR / map + 票 31,34,35,37,42,43 / daemon.ts(1011) / registry / cursor / channel / protocol / version / bin / CLI 全部 lib / wire-peer / suite.ts / ts-adapter.ts / ledger-relay index / package.json / dependency-cruiser；Orchestrator：全部 2 文件 |

---

## 6. 给 Farm wayfinder 的输入（不作为决定，只作为事实与候选问题）

1. **"编排器"在 Farm 里是不是就是 Farm 本身？** 若是，Orchestrator 仓库应被当作设计原料而非依赖，且必须与 ADR-0003 的"禁止中央控制器"做一次正面裁决（草图的星型拓扑 vs 后门永不出站的强置）。
2. **北向（management）接口完全没有设计**——github 之外还有 Gogs 的意图意味着需要一个 management 抽象；这是 Farm 编排器的第一性问题，Crossline 侧帮不上。
3. **"图纸/工厂"在第三个仓库（芯片库）里，且是 shell + docker**——Farm 若要以 Rust 全栈实现"用命令创建启动 agent"，需要决定：把图纸 API 化（谁拥有？），还是 Farm 直接编排 docker。
4. **面板的第一版可以只做 Crossline 已有的六动词 + 长连订阅**（协议已备、CLI 已定渲染与 hint 契约），这可能是最短的可见成果路径。
5. **两个仓库都缺 LICENSE**；Crossline 无 CI；这些是"能不能合法/可靠地依赖"的前置小事，但会卡住决策。
