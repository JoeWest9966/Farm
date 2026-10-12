# mattpocock/skills 源码研读报告

> 目标：为 FARM（Rust 全栈、类似 OpenAI Symphony、带面板、可插拔插件）项目做前期调研。
> 重点：`implement-spec` skill（用户称为"AFK 的精髓"）的完整工作流、它如何驱动 agent AFK 执行 spec、以及它与"编排器"共振的含义。
> 仓库位置：`/tmp/farm-research/skills`（clone 自 https://github.com/mattpocock/skills ，当前 HEAD `49dd158`，包版本 `mattpocock-skills@1.3.1`）

---

## 1. 仓库总览与 skill 的结构

### 1.1 目录布局

```
skills/
├── engineering/      # 每天写代码用（promoted）
├── productivity/     # 非代码的工作流工具（promoted）
├── misc/             # 冻结、不维护、不进插件
├── in-progress/      # beta，公开但不进插件
└── deprecated/       # 不再使用
docs/<bucket>/<skill>.md   # 每个 promoted skill 一个人工文档页（aihero.dev 发布）
.agents/                   # 仓库自身约定（invocation、writing-docs、ADR）
.claude-plugin/plugin.json # Claude Code 插件清单，skills 数组 = 发行集合
CHANGELOG.md / .changeset/ # changesets 发布流程
GLOSSARY.md / SCOPE.md     # 领域词表 + 收录门槛
```

`CLAUDE.md`（即 `AGENTS.md`）明确了几条硬约定，对理解"插件化"很有价值：

> Skills are organized into bucket folders under `skills/`: `engineering/` … `misc/` … `in-progress/` … `deprecated/`
> Every skill in `engineering/` or `productivity/` (the **promoted** buckets) must have a reference in the top-level `README.md` and an entry in `.claude-plugin/plugin.json`'s `skills` array (the Claude Code plugin ships exactly the promoted set).

也就是说，**"插件"的本体就是一份清单（`plugin.json` 的 `skills` 数组）+ 一堆自包含目录**。每个 skill 目录里只有 `SKILL.md` 和 `agents/openai.yaml`（外加少量引用文档）。没有运行时、没有依赖注入、没有构建步骤。这是 FARM 设计"可插拔插件"时值得对照的最小形态。

### 1.2 一个 skill 的物理结构

以 `implement-spec` 为例，目录下只有两个文件：

```
skills/engineering/implement-spec/
├── SKILL.md
└── agents/
    └── openai.yaml
```

`SKILL.md` 的 frontmatter：

```yaml
---
name: implement-spec
description: "Implement the result of /to-spec and /to-tickets in code."
disable-model-invocation: true
---
```

`agents/openai.yaml`（Codex 的 UI 元数据与策略）：

```yaml
interface:
  display_name: "Implement Spec"
  short_description: "Implement the result of /to-spec and /to-tickets in code."
policy:
  allow_implicit_invocation: false
```

### 1.3 唯一的分类轴：user-invoked vs model-invoked

`.agents/invocation.md` 把这个轴讲得非常清楚（这是理解 AFK 流水线能否被"自动触发"的关键）：

> Every `SKILL.md` in this repo is a skill. The one axis that splits them is **invocation**, who can reach it:
> - **User-invoked**: reachable **only by the human typing its name**. Set `disable-model-invocation: true` in the frontmatter (Claude Code) and `policy.allow_implicit_invocation: false` in `agents/openai.yaml` (Codex).
> - **Model-invoked**: reachable by **model or user**. The default …
> A user-invoked skill may invoke model-invoked skills, but it can never reach another user-invoked skill.

`implement-spec` 是 **user-invoked**：只有人类打 `/implement-spec` 才能点火，别的 skill（包括别的人类之外的 agent）都不能调用它。这一点对"编排器如何与它共振"极为关键，见第 5 节。

### 1.4 skill 之间的依赖：只经 Skill tool，不做深链接

`.agents/invocation.md`：

> Dependencies are expressed as an explicit instruction to **call the Skill tool** with the named skill (`Call the Skill tool with "grilling"`), not deep `../other-skill/FILE.md` cross-references, and not a bare `/skill`-style mention left for the model to interpret. … Dropping the leading `/` … keeps this harness-neutral …
> The Skill tool takes one skill per call. A step that needs two skills is two calls …

这是 FARM 要做"skill/插件编排"时值得照抄的一条：**组件之间用"命令名 + 工具调用"解耦，用相对路径硬编码会破坏 harness 中立性**。

---

## 2. `implement-spec` 的完整工作流程（逐字）

### 2.1 SKILL.md 全文

`/tmp/farm-research/skills/skills/engineering/implement-spec/SKILL.md` 全文如下（这是最权威的第一手材料）：

```markdown
---
name: implement-spec
description: "Implement the result of /to-spec and /to-tickets in code."
disable-model-invocation: true
---

You have been provided a spec. This spec should have tickets associated with it, describing how to implement the spec.

The issue tracker should have been provided to you. If not, tell the user to run `/setup-matt-pocock-skills`.

The goal is the entire spec implemented on a single **integration branch**, with every ticket resolved the way the issue tracker closes work.

The tickets are not a list of steps. They are a **task graph** with blocking relationships between them. This means there is always a **frontier** of tickets which are ready to be grabbed.

Communication to and from subagents should be sparse. Communicate primarily through **context pointers**: to the spec, tickets, research notes, and previous commits. Don't duplicate information already available via pointers.

**Implementer subagents** should be run in the background where possible for maximum concurrency.

## Steps

1. Read the spec and tickets to understand the task graph.

2. (optional) Use an **exploration subagent** to conduct any exploration required by the tickets - relevant codebase files or external documentation. Ensure the exploration subagent can save files - it should save its markdown notes in a directory outside the repo, accessible by all future subagents. This lets **implementer subagents** focus on implementation rather than exploration.

3. Create the integration branch. If the issue tracker closes work through PRs, or the user asks for one, open a draft PR after the first merge in step 5 (a branch with no commits ahead of main can't open one), marked as closing the spec and tickets.

4. Use **implementer subagents** to implement each ticket, each in its own worktree on its own branch. Each implementer subagent:
   - confirms its worktree is based on the integration branch before starting, and resets onto it if not;
   - calls the Skill tool with `tdd` to build the ticket;
   - merges the integration branch tip into its own branch before reporting done

5. Once an **implementer subagent** completes, merge its work to the integration branch with a **merger subagent**.

6. If this changes the **frontier** of available tickets, kick off more **implementer subagents** to work on the new tickets. This allows for maximum concurrency.

7. Once all tickets are complete, call the Skill tool with `code-review` on the integration branch. Fix all issues raised by the code review in a single **implementer subagent**.

8. If a draft PR exists, mark it ready for review. Otherwise, resolve each ticket the way the issue tracker closes work, and report the integration branch.

9. Clean up all **implementer subagent** worktrees.
```

### 2.2 逐步拆解

| 步 | 动作 | 关键原语 |
| --- | --- | --- |
| 1 | 读 spec 与 tickets，理解 **task graph**（不是 step list） | task graph / blocking edges |
| 2 | （可选）**exploration subagent** 做代码库/外部文档探路，笔记写到 **repo 外**的目录，供后续所有 subagent 共享 | 共享探索笔记 |
| 3 | 建 **integration branch**；只有 tracker 走 PR 或用户要 PR 时，才在第一次 merge 之后开 draft PR | integration branch |
| 4 | 对每张 ticket 起 **implementer subagent**，各自一个 **worktree + branch**；三件事：确认 worktree 基于 integration branch（不是就 reset 上去）、调 Skill tool `tdd` 建票、报告完成前把 integration 分支 tip merge 进自己的分支 | worktree / tdd / fast-forward 预备 |
| 5 | implementer 完成后，用 **merger subagent** 把结果 merge 进 integration branch | merger subagent |
| 6 | merge 改变 **frontier** 时，立刻再起新的 implementer subagent → 最大并发 | frontier 驱动调度 |
| 7 | 全部 ticket 完成后，在 integration branch 上调 Skill tool `code-review`；把所有发现交给 **一个** implementer subagent 修 | code-review |
| 8 | 有 draft PR 就标 ready；否则按 tracker 的关闭方式 resolve 每张 ticket，并报告 integration branch | 结票 |
| 9 | 清理所有 implementer subagent 的 worktree | 清理 |

值得注意的几处设计意图：

- **票是图，不是队列。** skill 第二段直接定义了它和"逐票 implement"的本质区别：
  > The tickets are not a list of steps. They are a **task graph** with blocking relationships between them. This means there is always a **frontier** of tickets which are ready to be grabbed.
- **通信靠 context pointer，不靠粘贴摘要。** 对 subagent 的 prompt 要小，把编排者的 context window 留给"图"：
  > Communication to and from subagents should be sparse. Communicate primarily through **context pointers**: to the spec, tickets, research notes, and previous commits. Don't duplicate information already available via pointers.
- **merge 用专门的 merger subagent**，而不是让 implementer 自己合进 integration，也不是让编排者亲手冲突解决。
- **每个 implementer 完工前先把 integration tip merge 进自己的分支**，这样第 5 步合进 integration 时是 **fast-forward**，把"冲突"提前到 implementer 手上解决（引入 tdd 的上下文里），编排者拿到的永远是可 FF 的分支。
- **收尾只跑一次 code-review**，因为 code-review 是拿 diff 与"整份 spec"对照，中途跑会把未建的票当成缺失。

### 2.3 文档页给出的"何时用 / 前置条件"（第二手，但补齐了 SKILL.md 没说透的部分）

`docs/engineering/implement-spec.md` 补充了：

- 它读票为 task graph，blocking edges 决定"能开什么"，任一时刻 frontier 上的票**全部同时**开跑；"The graph's shape sets the pace, not the tickets' order on the tracker."
- 前置条件：
  > - **An issue tracker.** …
  > - **Tickets with blocking edges**, as to-tickets writes them. Without edges the graph is flat and every ticket starts at once.
  > - **A harness that runs subagents in the background and gives each one a git worktree.** The skill exists to run tickets at the same time, so on a harness that runs subagents one at a time, it is only a slower `implement`.
- 定位（main chain 的 build 步）：
  > `grill-with-docs → to-spec → to-tickets → implement-spec → retro`

### 2.4 上游/下游技能（决定它能"共振"的接口面）

- **`to-spec`**：产出 spec（Problem Statement / Solution / User Stories / Implementation Decisions / Testing Decisions / Out of Scope / Further Notes），发布到 issue tracker，并打 `ready-for-agent` 标签。**不做访谈**，只综合已有对话。
- **`to-tickets`**：把 spec 拆成 **tracer-bullet 垂直切片**，每张票声明自己的 **blocking edges**（本地 tracker 写 `Blocked by: NN` 行；真实 tracker 用 **原生阻塞关系**）。还规定了"宽重构"用 **expand–contract** 序列化。发布时每张票已带 `ready-for-agent`。
- **`tdd`**（model-invoked）：implementer 内部调它做 red→green 循环，测试必须落在**事先约定的 seam**上。
- **`code-review`**（model-invoked）：两轴（Standards + Spec）各起一个**并行 subagent**，分别报告、不互相污染，最后由调用方聚合。
- **`research`**（model-invoked）：后台 agent 读一手来源，产出一份带引用的 markdown 文件。

---

## 3. 它如何驱动 agent AFK 地执行 spec

先对齐术语。这个仓库里 **AFK** 的定义来自 wayfinder 与字典：

> Every ticket is either **HITL** (human in the loop, worked _with_ a human who speaks for themselves) or **AFK**, driven by the agent alone. (`wayfinder/SKILL.md`)
> `ready-for-agent`: fully specified, ready for an AFK agent (`triage/SKILL.md`)

`implement-spec` 之所以是"AFK 的精髓"，在于它把整条流水线做成了**agent 自己当调度器**的确定性循环。它满足 AFK 的三个条件：

1. **输入已经冻结**：spec + tickets + blocking edges 全部事先落到 issue tracker。agent 不需要再问用户任何设计问题（`to-spec` 明说 "Do NOT interview the user"）。用户的决策已经被压平成图上的边。
2. **进度可被机器读取与推进**：票的状态就是进度；blocking edges 就是依赖；frontier = 所有 blocker 已 close 的票 = "现在能跑什么"。
3. **反馈闭环内置**：每个 implementer 走 `tdd`（先红后绿），整条流水线末尾走 `code-review`。agent 不必靠人盯，靠红/绿与两轴评审自我纠偏。

它的 AFK 驱动机制，抽象出来是 4 个原语：

- **Task graph + frontier**：决定并发度与启动顺序。第 6 步是它的心跳——"If this changes the **frontier** of available tickets, kick off more **implementer subagents**"。这是一个事件循环：merge 完成 → 重算 frontier → 起新 worker → 等它完成 → 再 merge。
- **Worktree 隔离**：每个 worker 一个 git worktree + branch，避免多个 agent 共享 working dir / index / HEAD（`implement.md` 记录过"两个 session 撞 commit、stash 消失"的真实事故）。
- **Integration branch 收敛**：所有并行分支汇聚到一条分支；每个 implementer 先自行 merge integration tip，保证最终是 fast-forward。
- **Context pointer 通信**：subagent 的 prompt 只给指针（spec、票、探索笔记、commit），既省 orchestrator 的窗口，也省 token。

**已知的 AFK 边界与坑**（`docs/engineering/implement-spec.md` 的 Common questions，对 FARM 做同类系统是免费的需求清单）：

- code-review 只能在全票落地后跑；中途跑会把未建的票读成失败，触发"修-审"死循环（一例 5 张票的报告跑了约 4 小时）。
- 并行 implementer 会在**共享文件**上冲突，或给同一概念起不同名字（实例：web 和 mobile 两张票把同一字符串命名成 `blockedSince` 与 `blockedOn`）。要么加 blocking edge 串行化，要么在探索笔记里钉死命名。
- 在 GitHub 上，blocked-by 计数只在 blocker **close** 时下降，而票通常在 PR merge（即整轮结束时）才 close，所以 tracker 的图会**中途失效**；文档建议编排者自己记录"哪些票已经 merge 进 integration branch"，据此算 frontier。（→ FARM 的 tracker 必须支持"merge 即完成"的语义，否则 frontier 会卡死。）
- worktree 只含 git 追踪的内容，依赖被 gitignore 的 fixture/数据库/凭据的测试会**静默跳过并报绿**。这类票要在主 checkout 跑。
- 它给出"运行正常的信号"（It's working if）：
  > - Several implementers are running at once whenever the graph allows, not one after another.
  > - A ticket starts as soon as its last blocker lands on the integration branch, not when the whole run ends.
  > - Every ticket's trace shows `tdd` running, with a failing test before the code.
  > - Merges into the integration branch are fast-forwards, not conflict resolutions.
  > - The run ends on one branch with every ticket resolved, and a PR only if your tracker wanted one.

---

## 4. 与"编排器"共振的含义

用户说「`/skill:implement-spec` 是 AFK 的精髓，估计是编排器部分涉及到，毕竟它们是共振的」。这句话在源码里可以被精确解释为下面三层。

### 4.1 它们解决的是同一个问题：把"谁来调度"从人换成程序

`docs/engineering/implement.md` 里有一段用户原话，直接就是对编排器的需求陈述：

> users wanted "subagents implement the tickets" instead of having "to individually create new session and tell them to implement a ticket one by one, when a spec may contain over 5 tickets." With `implement` you are the dispatcher: one session per ticket, clearing in between, and keeping track yourself of which tickets are unblocked. `implement-spec` hands that job to one orchestrating session.

**编排器（Orchestrator）= "the dispatcher" 的角色。** `implement-spec` 就是把"人类 dispatcher"换成"一个 orchestrating session"。两者共用同一套概念：subagent、worktree、integration branch、frontier、ready-for-agent。这就是"共振"的第一层：**词汇表和状态机完全同构**。

### 4.2 共振面 = 可被外部程序读取/写入的那几个接口

`implement-spec` 本身是 user-invoked skill，跑在**一个 harness session 内**，由 **agent** 自己做编排决策。它的文档明确把它和"真正 AFK 的确定性循环"划界：

> `implement-spec` puts an agent in charge of orchestration inside one harness session, which needs no infrastructure and lets you watch and steer. For work that is truly [AFK], a deterministic loop (Sandcastle, a shell script, a CI job) is faster, cheaper, and more reliable, because no agent makes the orchestration decisions.

也就是说：**同样一条流水线，可以由 agent 当调度器（implement-spec），也可以由确定性程序当调度器（Sandcastle/编排器）**。这就是"共振"的第二层：**同一条流水线有"agent 调度"与"程序调度"两种实现，二者共享同一组接口**。要把 agent 版改造成程序版（即 FARM 的编排器），只需要抓住下面这组原语（也就是共振面）：

| 原语 | implement-spec 里的表现 | 编排器需要的对应能力 |
| --- | --- | --- |
| **Spec** | 引用 spec issue | 能读 spec/issue |
| **Tickets + blocking edges** | 直接读 issue tracker 的票与阻塞关系 | 能读写 tracker 的票与依赖边 |
| **Ticket 状态机** | `ready-for-agent`；关闭 = 完成 | 能打标、能 close、能按 close 算 frontier |
| **Frontier** | "blocker 全 close 的票" | 能查询"就绪票集合"并据此派发 |
| **Worker 隔离** | worktree + branch per implementer | 能为每个 job 起 worktree/分支 |
| **Worker 执行体** | implementer subagent（内部调 tdd） | 能起 agent 进程并喂 context pointer |
| **合并** | merger subagent 合进 integration branch | 能做 FF merge |
| **收尾评审** | code-review（两轴并行 subagent） | 能跑评审并把发现回灌 |
| **结果落地** | resolve 票 / draft PR | 能关票 / 开 PR |

换句话说：**`implement-spec` 是一份"编排协议"的人类可读参考实现**；FARM 的编排器是同一份协议的**程序化实现**。共振之所以成立，是因为两者都以 issue tracker 为共享状态（single source of truth）：agent 版把决策写回 tracker，程序版从 tracker 读决策。**tracker 就是它们之间的总线。**

### 4.3 对 FARM 有直接价值的几个尖锐结论

1. **`ready-for-agent` 标签就是"AFK 接活口"。** 文档 `to-spec.md` 记了一个真实的坑：
   > AFK agents that poll for `ready-for-agent` cannot see that difference. They will try to build the whole spec in one run instead of picking up the ticket slices. This is the most-reported problem with the skill.

   → 也就是说，**外部编排器是"轮询 `ready-for-agent` 来抢活"的**。FARM 如果要接 mattpocock 这套 skill，只要它能轮询这一标签、拿到票与阻塞边、起 worktree/subagent，就已经在共振了。
2. **`implement-spec` 不能被子 agent 调用（`disable-model-invocation: true`）。** 外部编排器不能"调 skill 名"来启动它，只能：(a) 把一个 **user 身份**的 `/implement-spec` 输入进一个 harness session；或 (b) 不调它，而是**自己实现它内部那 9 步**（因为那 9 步只是协议，不是代码）。这解释了为什么"编排器"和 `implement-spec` 是**并列的两个调度器**，而不是上下级。
3. **tracker 的 close 语义决定 frontier 是否会卡死。** GitHub 的"blocker close 才解锁"这一语义会让 `implement-spec` 中途停摆（见 3 节）。FARM 若自建 tracker/编排器，应把"**merge 进 integration branch 即完成**"作为完成信号，而不是"ticket close"。这是源码里明确写下的需求。
4. **并发不是无条件的。** 共享文件冲突是并行的真实代价；`to-tickets` 用 blocking edge 串行化只是**猜测**（"A blocking edge written from ticket text is a guess about which files each ticket will touch"）。FARM 若做面板展示任务图，最好把"共享文件/共享符号"作为可选的额外边来源。
5. **context pointer 是省 token 的关键，也是面板可以展示的东西。** 每次通信只传"spec / 票 / 探索笔记 / commit"的指针。FARM 的面板天然可以渲染这四类指针，而不必复制内容。

---

## 5. 关键原文引用索引

| 主题 | 文件 | 引用 |
| --- | --- | --- |
| AFK 定义 | `skills/engineering/wayfinder/SKILL.md:75` | "Every ticket is either **HITL** … or **AFK**, driven by the agent alone." |
| AFK 就绪标签 | `skills/engineering/triage/SKILL.md:35` | "`ready-for-agent`: fully specified, ready for an AFK agent" |
| 票是图不是列表 | `implement-spec/SKILL.md` | "The tickets are not a list of steps. They are a **task graph** … there is always a **frontier** …" |
| context pointer | `implement-spec/SKILL.md` | "Communication to and from subagents should be sparse. Communicate primarily through **context pointers** …" |
| 9 步流程 | `implement-spec/SKILL.md` | 见 2.1 全文 |
| 调度器对比 | `docs/engineering/implement-spec.md:62-64` | "`implement-spec` puts an agent in charge of orchestration inside one harness session … For work that is truly AFK, a deterministic loop (Sandcastle, a shell script, a CI job) is faster, cheaper, and more reliable …" |
| 编排者需求来源 | `docs/engineering/implement.md` | "you are the dispatcher: one session per ticket … `implement-spec` hands that job to one orchestrating session." |
| 轮询 ready-for-agent | `docs/engineering/to-spec.md:42` | "AFK agents that poll for `ready-for-agent` cannot see that difference. They will try to build the whole spec in one run instead of picking up the ticket slices." |
| user-invoked 约束 | `.agents/invocation.md` | "A user-invoked skill may invoke model-invoked skills, but it can never reach another user-invoked skill." |
| worktree 隔离动机 | `docs/engineering/implement.md` | "One field report describes a `git commit --amend` in one session landing on another session's commit …" |
| gutter/发散 | `docs/engineering/implement-spec.md` | "Two implementers running in parallel collided on the same file, or picked different names for the same thing." |

---

## 6. 研读结论（对 FARM 的一句话）

`implement-spec` 的价值不在代码，而在它**把一套 AFK 编排协议具象化了**：spec → tickets(task graph) → frontier 并发派发（worktree+subagent）→ integration branch 收敛 → code-review 收尾 → resolve。FARM 的编排器与它"共振"的含义是：**两者共享同一套领域词汇与同一份 issue tracker 状态机，只是一个由 agent 调度、一个由程序调度。** FARM 要接入的接口面就是第 4.2 节那张表，尤其要把"完成信号"定义在 merge 而非 ticket close 上，否则 frontier 会卡死。
