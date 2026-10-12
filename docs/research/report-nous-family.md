# Nous 家族五仓深读报告（Nous-Warehouse / Nous-Kron / Nous-Sentry / Nous-Slaveholder / Nous-Whitehelmet）

> 调研方式：`git clone --depth 1` 五个仓库到 `/tmp/farm-research/`，逐文件读源码与设计文档；实际构建并跑测试（Go/Node 均可用）；真机跑 `nw` CLI 对真数据仓 `/opt/Data-001` 取真实 JSON 输出。
> 报告口径：**只写读到的与实测到的**；未实测处明确标注「未实证」。行号引用以本次 clone 的树为准（五仓均为浅克隆，各 1 个 commit，故不提供 commit 历史分析）。

---

## 0. 一句话结论

这五个仓库是同一作者（JoeWest9966）为「**AFK 无人值守跑 agent**」建的**基座组件族**：
**仓库（Warehouse）= 装配派**、**账本（Kron）= 记账**、**哨兵（Sentry）= 站岗放行**、
**监工（Slaveholder）= 把它拉起来盯着**、**纠察（Whitehelmet）= 干完活审一遍**。

对 Farm 的意义分三档：

| 档 | 仓 | 对 Farm 的含义 |
|---|---|---|
| **可直接复用（现在是可运行的二进制/扩展，有测试与真机实证）** | Kron（`ledger` 二进制）、Sentry（pi 扩展 + CLI + handler）、Warehouse（`nw` 二进制） | Farm 的**记账 / 工具调用审批门 / 装机与插件装配**三件事今天就有实现可用（子进程方式），契约已字段级冻结 |
| **规格级复用（设计已到可直接落码，但无实现）** | Whitehelmet（纠察）1 篇交接件 + 释义表 | 判决三分类 + 隔离 judge 会话 + 闸门 JSON 的**设计**可直接照做 |
| **领域模型级复用（源码级实证，但无可运行产物）** | Slaveholder（监工）6 篇 ADR + CONTEXT | 「怎么判断一个跑着的 agent 完成/卡住/挂起」是**pi RPC 源码级实证**的判据学，Farm 编排层直接吃这份结论，省掉整轮逆向 |

**分布特征**：五仓**零 Rust**（Warehouse / Kron / Sentry handler = Go；Sentry 闸门与 CLI = Node JS；装配命令与 CI = POSIX sh）。因此 Farm（Rust）与它们的关系只能是「**子进程 + JSON 契约**」或「**读契约自己用 Rust 重写**」，不存在 Rust 链接式复用。
**另一条硬事实**：五仓**都没有服务端**（全仓 grep `net/http` / `ListenAndServe` / `listen(` 零命中），全部是 CLI + stdout JSON envelope；**人机面板被全部五仓明确划到 scope 之外**，作为「JSON 契约的消费者」预留（Warehouse 的 map.md Out of scope 原文：「前端面板 / TUI / Agent 界面（本期只做 headless CLI；JSON 契约已为上层预留）」，且 `面板` 一词在 Warehouse 的 Go+MD 里出现 **69 处**，全部是「面板按 id 渲染 / 面板只写一套渲染逻辑」这类**为面板准备的纪律**）。

---

## 1. 速览表

| 仓 | 一句话定位 | 语言 / 体量 | 完成度（实测） |
|---|---|---|---|
| **Nous-Warehouse** | 装配引擎 `nw`：配方 + 组件 → 锁定集 → 一条装配命令，把 pi 运行时装成一个「派」 | Go，源码 ~29.3k 行（含测试），依赖仅 stdlib + `yaml.v3` | **高**。`go build ./...` 通过；`go test ./...` **全绿 16 个有测试的包**（344 个 Go 测试函数）；27 项 validate 冻结检查、`up` 七步幂等、`doctor` 七面只读复核、`ls` 五域、`source`/`vps`/`key`/`subscription`/`tag` 五组命令全部落地；已真机首跑成功（`/opt/Data-001/docs/real-first-run.md`：干净容器 rc 0 ×2、清单逐字节一致、doctor 0）|
| **Nous-Kron** | 账本 `ledger`：AFK 期一切动作的封闭 schema 事件账，append-only + 哈希链 | Go，6.3k 行（含测试） | **高**。`go test ./...` 全绿（66 个测试函数）；五条命令 `record/read/verify/repair/archive` **全部实现**（`internal/ledger/ledger.go:41` 的 `scaffold()` 已成死码）；哈希链、`FS_APPEND_FL` 内核级只追加、段清单 manifest、幂等、有界游标读面全部有测试 |
| **Nous-Sentry** | 哨兵：无人值守期的 `tool_call` 审批闸门（放/拦/问）＋ 失败上报 | Node JS（扩展 + CLI）+ Go（handler），5.3k 行 | **高**。`npm test` **96 JS 测试全绿**、`handler/ go test` **23 Go 测试全绿**；三层执勤手册、先落 pending 再 block、reason 白名单、fail-closed 硬上限、热重载、systemd `OnFailure=` handler + fsync spool + 退避重放全部落地并有真机部署演示记录 |
| **Nous-Slaveholder** | 监工：把装好的派拉起来干活、盯着、挂了抽活、干完回收 | **纯文档**（CONTEXT.md + 6 篇 ADR + agents 三件套），**零代码** | **规格完成、实现为零**。ADR 0001–0006 已接受；判据学（完成双条件、两口钟、三层状态列、挂起不可逆）全部有 pi 0.84.3 **源码行号级**依据 |
| **Nous-Whitehelmet** | 纠察：AFK 期间自动审稿人，跑确定性检查 + 隔离 judge 会话，出三分类判决 | **纯文档**（README 1 行 + CONTEXT.md 释义表 + 1 篇 handoff），**零代码、零 ADR** | **只有交接件**。判据、硬边界、调研事实写得细，但设计未开图（该仓的 wayfinder 图还没画） |

---

## 2. Nous-Warehouse —— 装配引擎（`nw`）

### 2.1 功能定位

作者自己的词：**「装配」**——把「这个派要哪些组件、什么版本、什么模型配置」（**配方 recipe.yaml**）解析成
**锁定集**（精确版本 + sha256），再烤成**一条 POSIX sh 装配命令**；目标机（只保证有 `gh`）执行该命令，
就把 pi 运行时 + profile + 组件铺成一个可跑的**派**（`$NW_HOME/profiles/<名>/`，用 `PI_CODING_AGENT_DIR` 指定）。

一句话：**它是「环境即产物」的收敛式安装器，也是插件（skill/extension）的分发与装配系统。**

### 2.2 架构（读源码得到的形状）

- **分层**：`internal/{model,validate,bake,up,doctor,ls,manifest,source,vps,key,subscription,tag,entry,envelope,exec}`。
- **三类文档**（`internal/model/model.go`）：`Recipe`（配方，`apiVersion: nw/v1`）、`Component`（组件清单 `component.yaml`）、`Lock`（锁定集 `nw/lock.v1`，内嵌在装配命令的**引号 heredoc** 里 → 锁定集里一个变量都不能留，`validate lock.literal` 盯着这条）。
- **装配七步**（`internal/up/`，README 有表）：0 前置 → 1 引导运行期资产（sha256 校验 + profile 级并发锁）→ 2 chezmoi 铺骨架（三个路径显式钉进 `$NW_HOME/state`）→ 3 落组件（按名稳定序、shax256 不符即 3、不在锁里的旧组件**移入 `nw/pruned/<ts>/` 而不是删**）→ 4 **键级 merge** 配置态（只写声明键，用户其余键保留；`auth.json` 永不写）→ 5 写 `.nw-manifest`（**唯一提交点，永远最后**）→ 6 `nw doctor` 独立进程自检。
- **幂等**：同一 `lockId` 连跑两次，清单**逐字节一致**（`installedAt` 只在 lockId 变化时刷新；`env` 只记 `present` 布尔，永不记值）。
- **两仓取货**（ADR 0001，2026-10-07）：运行期资产从**引擎仓** release 取、bundle 从**数据仓**取，两个字面量都写进命令、都可由 env 覆盖 → 数据（组件、配方、catalog、vps、keys、subscriptions、tags）**不在引擎仓**，在独立私库 `JoeWest9966/Data-001`（本机 `/opt/Data-001` 就是它）。
- **`nw validate` 27 项冻结检查**（`internal/validate/validate.go:190-216` 常量表）：`lock.literal` / `recipe.schema` / `recipe.profile_default` / `recipe.runtime_pins` / `recipe.components_exist` / `recipe.external_refs_form` / `recipe.version_match` / `recipe.external_reserved_fields` / `component.deps_resolve` / `recipe.params_match` / `recipe.env_registered` / `secrets.no_literals` / `component.packages_pinned` / `env.usedby_consistent` / `component.landing` / `lock.matches_recipe` / `lock.external_fields` / `lock.external_matches` / `skeleton.tmpl_suffix` / `skeleton.no_config_state` / `component.vendored_declared` / `component.no_symlinks` / `catalog.{schema,records,refs_resolve,extension_entries}` / `command.no_bashism`。
- **bake 不联网**：资产 sha256 只能来自真文件（`--assets-dir`），「绝不编造 sha256」；但**外部源会联网**（bake 现拉外部仓默认分支 HEAD，`git clone --depth 1` + `rev-parse`，快照与 sha256 打进锁定集）。

### 2.3 外部源（= 它的「插件系统」，对 Farm 最有用的一节）

`docs/external-sources.md` + `internal/source/`：

- **本体不住在数据仓**：一条**编目记录** = 一个 GitHub 仓库（名字 + 仓库链接 + 扫出来的 `entries[{name,path}]`），**不含版本**（`ref`/`@tag` 见了就退回——明令「别把本地组件的 version 心智模型缝到外部源上」）。
- **一仓可多条目**：注册时**扫全入账不挑**，装配时在配方里点 `<仓库>` 或 `<仓库>/<条目>`。
- **注册五条验收 + 第六条（仅 extension）**：入口结构可识别 / 每个 skill 目录有 `SKILL.md` frontmatter（`name` + `description`）/ 名字在数据仓内唯一 / `.bak-*`、`.github/`、`README.md` 等脏文件忽略 / 扫完列清单**确认才落账**（`--yes`）；extension 额外要求子树自带 `package.json` 且 `pi.extensions` 非空。
- **落地约定（pi 的发现机制不对称，这条对 Farm 极关键）**：
  - 外部 **skill** → `<profile>/skills/<仓库>/<条目>/`（pi **递归自动发现**，**不登记**）；
  - 外部 **extension** → `<profile>/nw/extensions/<仓库>/<条目>/`（pi **只扫一层**，**必须登记**进 `settings.json` 的 `packages`，字符串相对路径 `./nw/extensions/<仓库>/<条目>`）。
- **vendored**：扩展的 npm 依赖在 bake 的临时 clone 里 `npm ci --omit=dev` 装好、随资产下发 → **目标机零 npm**（`nw` 永不跑 `npm install`）。
- **编目纪律**：机器写、确定性排序（tags 按 name、refs 按 kind+id）、`tmp+rename` 原子落盘、**永不手改**；`recheck` 只报告漂移**永不写账**；写入口只有 `register`。
- **凭据纪律（D6）**：`secrets.no_literals` 检查；配方/锁定集/bundle/装配命令四处**绝不落**订阅链接与机密。**两道书面口子**（要看清这是风险模式，不是无懈可击）：`keys.json`（github key）与 `subscriptions.json`（订阅 URL）**存本体明文**、住数据仓（ADR 0003 / 0004）。
- **`nw tag`**：条目级扁平标签，独立成账（`tags.json`），核心是**反查**（`nw tag ls pi`）；**绝不进配方/锁定集/bundle/装配命令**（烤进锁定集 = 每次改标签重烤一次派）。

### 2.4 对外接口（实测过的形状）

stdout **只有一份 JSON envelope**，人话/进度走 stderr，`--json` 是显式 no-op（**明确拒绝人类可读 stdout 模式**）：

```json
{"schema":"nw/ls.v1","nw":"0.1.0","ok":true,"errors":[],"warnings":[],
 "data":{"domain":"all","recipes":[...],"components":[...],"sources":[],"profiles":[],"locks":[]}}
```

命令面（`internal/cli/cli.go:43-70` usage 逐字）：`validate` / `recipe new|bake` / `up` / `doctor` / `ls [recipes|components|sources|profiles|locks]` / `source register|recheck` / `vps register|ls|rm` / `key register|ls|get|rm` / `subscription register|ls|get|rm` / `tag add|ls|rm`。
**退出码表**（零新码，冻结）：通用 `0 成功 / 2 环境缺失 / 3 输入无效或 fail 项`；`bake 0/2/3/4`；`up 0/2/3/4/5`（5 = 同 profile 已有 up 在跑）；`doctor 0/10 降级/11 漂移/12 清单损坏`；各 `register/ls/get/rm` 0/3/4。
**契约纪律**：「字段名即契约」——`errors[]` 恒为数组、数组恒不为 null（理由一律写成「面板只写一套渲染逻辑」）、已发布字段名一个字符不许改、`ok == (exitCode == 0)`。

**我实测的两个真实输出**：

```bash
$ cd /opt/Data-001 && /tmp/nw-test ls --repo .          # rc 0，五域全在（profiles/locks 空数组）
$ /tmp/nw-test validate --recipe recipes/research-desk/recipe.yaml --repo .
# ok true，summary {total:27, pass:12, warn:0, fail:0, skip:15}，rc 0
```

### 2.5 完成度与已知缺口

- 真机首跑报告（`/opt/Data-001/docs/real-first-run.md`）记录 **11 条真实世界的坑**并已回填代码，其中对 Farm 有直接参考价值的：pi 的 `models.json` 是**键控对象不是数组**；真实 `pi auth check` **必须带 `--provider`**；pi **只认 `PI_CODING_AGENT_DIR` 环境变量**（无等价 flag）；chezmoi `--version` 输出带尾逗号（假漂移）；bundle 体积 89.4MB（node 121MB / pi node_modules 141MB）。
- 未决（作者自标）：老 glibc（CentOS 7/8、Ubuntu ≤20.04）未实测；供应链只有 sha256（chezmoi 侧有 sigstore）；幂等的前提是「发布后 bundle 与资产不可改写」。
- 与 Farm 相关的显式边界：`architecture` 上**没有服务端、没有面板、没有 HTTP**；`ls` 是**廉价预览**（明确「不重算收敛指纹，那是 doctor 的活」）。

---

## 3. Nous-Kron —— 账本（`ledger`）

### 3.1 功能定位

**AFK 期发生的一切记成一笔笔可翻的账**：谁在什么时候对哪个 agent 下了什么命令、哪个审批被放行/拦下、哪次跑烧了多少 token、最后交付了什么。作者自定位：**「唯一一个『现成方案与已定红线正面冲突』因而值得自建的基座组件」**（Langfuse/Phoenix 默认记 prompt/response，而红线禁入正文）。

### 3.2 架构（读源码）

- **存储**：`ledger/YYYY.jsonl`（真源，`O_APPEND`）、`ledger/YYYY.manifest`（段冻结清单）、`ledger/archive/YYYY.jsonl`、`cache/agg/`（**明文侧车聚合缓存，永不进真源**）。
- **只追加的四层强制**（ADR 0011）：① 一切写入经 `ledger record`；② 通道强制 —— `FS_APPEND_FL`（`ioctl FS_IOC_SETFLAGS`，`internal/ledger/appendflag.go`）拿不到 `CAP_LINUX_IMMUTABLE` 就**如实降级**为 `ChannelConstructive`（verify 会标注，不假装强制）；③ 逐笔内联哈希链 `hash = SHA256(len4(载荷) ‖ prev ‖ 载荷)`（4 字节大端长度前缀防拼接歧义，**裸 SHA-256 不做 MAC**）；④ 段清单对账。
- **并发**：跨进程 `flock(LOCK_EX)` 包住「刷新→分配 seq→算链→追加」，进程内再叠 mutex；`Open` 全量重放恢复状态（键表 + 链头）。
- **幂等**：账本侧自然键（如 escalation 的 `(event, run_id, unit, result_class, exit_status, ts)`）；同键同摘要 → `{ok:true, seq:原 cursor, duplicate:true}`；同键异摘要 → `INPUT_INVALID(details.field=idempotency_key)`，**不回显载荷**。
- **半行策略**：真源尾部半行拒绝继续追加（`ErrPartialTail`），要求先 `ledger repair`；`repair` 留证 `.partial` + integrity 事件。
- **封闭 schema**（`internal/schema/schema.go`）：全封闭 struct 树、**无 `any`/`map[string]any`**、`DisallowUnknownFields` 拒未知字段；写入方**不得**填 `cursor/prev/hash`（账本自填）。
- **10 类事件**：`token` / `approval` / `escalation` / `read` / `run_boundary` / `session_boundary` / `context_transition` / `tool_outcome` / `destruction` / `integrity`。
- **红线「禁入」**：消息正文、文件内容、system prompt/context、任何 provider key、任何 `Authorization` 头 —— **不是「先记后脱敏」，是「禁入」**（作者专门指出「脱敏」语义相反、本域禁用）。`details` 只放字段名/路径/校验和/present 布尔。

### 3.3 关掉的两条最重要契约（Sentry 与 Farm 都在消费）

**审批状态机（ADR 0007）**：`pending`（唯一非终态）→ `approved / rejected / expired / cancelled` 四终态；**超时/无应答一律 `expired`（fail-closed，无自动放行态）**；一 `subject_id` 至多一条终态；**已终态不可补批**（延期只能靠模型重试 = 全新 `subject_id`）；迁移**全由外部裁决方驱动**（`actor ∈ {sentinel, human, policy, timer}` 闭集）；`pending` 可缺 `reason_code`、终态**必填**（`enumApprovalReason` 9 值闭集）。
**escalation（ADR 0008）**：10 位封闭集，`reason ∈ {gate_blocked, ask_failed, ask_expired, gate_failed}`；`actor` 取**写入方闭集** `{sentinel, whitehelmet, slaveholder, crossline, human}`；systemd 三位（`unit`/`result_class`/`exit_status`）非 systemd 写入方置 null。

### 3.4 对外接口

CLI 唯一机器接口（`cmd/ledger/main.go`）：`ledger record|read|verify|repair|archive`；`LEDGER_ROOT` 环境变量给根（缺省 `.`）；stdout 单份 envelope（**契约直接从 Warehouse 移植**：六字段顶层 + 五字段错误 + `ok == (exitCode==0)`）；码表 `OK/0`、`INPUT_INVALID/3`、`VERIFY_FAILED/3`、`MANIFEST_BROKEN/12`、`DEGRADED/10`。
**`record`**：stdin **NDJSON** 流；容错两段式 —— 值级/类型级/枚举/必填/未知字段错误**逐项隔离**（该笔不入、其余照落、整体 rc 3）；语法坏行**污染整条流**（本次调用 0 笔落盘、坏行原文不落盘不回显）；`data.results[]` 与非空输入行一一对应。
**`read`**（ADR 0010，对面板设计最相关）：`--since <seq>` **必填**（`--since 0` 才是显式从头，不静默全扫）、`--purpose` **必填**（闭集 `review/verify/export/probe`）、排他下界、有界分页（默认 1000/上限 10000）、可按 agent/time/command/approval 切片、`--agg`（侧车缓存，头部带 `truth_fingerprint`）、`--orphan-pending`。**每次读调用都会同步落一条 `read` 事件**（这是设计，不是副作用 —— 谁翻过账也记账；fold 与缓存指纹口径**排除 read 事件**，否则永不命中）。
**v1 只承诺「回看」，不做「重演」**（ADR 0003）：编排状态一律读时纯函数投影、绝不落盘成快照；无存时快照事件；无上下文级重演。
`verify` 只读打开（链坏时也能翻账），如实例化 `ChainIssue{segment,line,seq,reason,checksum,manifest}`。

### 3.5 完成度与风险

五条命令 + 哈希链 + 幂等 + 归档 + 作废全部有实现与测试（`go test ./...` 全绿）。
**已知弱项（作者自标）**：真源只增不减 ⇒ 敏感数据「合法删除」无路（只能追加式作废）；归档需特权 ⇒ AFK 无人持权时段只增；去重无 TTL ⇒ 缓存删后长期退化为全扫 fold（吃性能不吃正确性）；read 策略衰减只兑现到查询默认窗。
**对 Farm 的运营注意**：面板若高频 `ledger read`，**每次都会往账里写一条 read 事件**（seq 增长 + 体积增长）——这是「审计优先」的取舍，不是 bug，但 Farm 面板的轮询策略必须据此设计（要么低频 + `--agg`，要么另建视图层）。

---

## 4. Nous-Sentry —— 哨兵（审批闸门 + 上报）

### 4.1 功能定位

**AFK 期间的站岗者**：对每次 pi `tool_call` 放 / 拦 / 问口令 / 上报；它是 Kron 审批状态机的**挂起方**（ADR 0007 点名）。交付边界明确：**不做**富 UI / 手机应答（归 Reins）/ 预算闸（作者自有）/ 凭据签发（Warehouse）/ 执行编排（成品项目）。

### 4.2 架构（三件器，全部有实现）

**① 闸门扩展（`extension/index.js` + `src/`，pi 0.84.3 的第二扩展）**
- 挂 `tool_call` hook；`block: true` ⇒ 工具 `execute` **从不调用**，模型收到 `isError:true` + **裸 `reason`**（0.84.3 无 `<harness>` 包装）。
- **铁律：自己失败 = fail-closed + 硬上限（3）+ trace 不可协商**。出处是一次真实事故（Pi 1.0.0 hook 静默放行且零遥测，外部不可区分「静默」与「通过」）。手册加载失败 → 拦下；落 pending 失败 → 拦下（**不能假装挂起成功**）；未预期异常 → 拦下；连续 3 次失败 → 进程内上报 `gate_failed`。
- **三层执勤手册（YAML，`manual/sentry.manual.yaml` 有生产样本）**：`preauth_rules[]`（放/问/拦，命中规则号 = `gate_id`）+ `preauth_window`（墙钟窗口，支持跨零点）+ `ask_timeout_minutes`（N，默认 10，**只对 ask 生效**）。作者特意点出原型的一处**静默失效**并修掉：窗口**只放宽显式标 `preapprove` 的规则**，`ask`（含 `default_action`）**永不因窗口自动放行**（否则 `rm -rf /etc/passwd` 会被兜底 ask 后自动放行）。`default_action` 独立字段、只在不命中任何规则时生效。路径匹配从**命令正文抽路径**喂（bash 调用没有 `path` 字段），**抽取失败一律退回「问」**。
- **先落 pending 再挂起**（ADR-0007 b）：拦（reject）也留痕（写 approval 终态 `rejected`）。
- **reason 措辞红线（上限 L2）**：只允许 `tool_call_id` / `tool_name` / `input_summary≤100` / `gate_id` / 已知终态（`no_responder`①/`timeout_expired`）+ 重试指引；**禁策略散文**；5 条禁入（手册散文 / 超 100 字正文 / 裁决理由 / 账本内部字段 / 凭据类）用正则**构造后扫描，命中即抛**（`src/reason.js` 的 `FORBIDDEN_PATTERNS`）——宁可不构造，也不让越界文案进会话树。作者点明理由比「模型会不会被骗」更硬：reason 是 tool result ⇒ **随 session JSONL 永久留存**，且模型每重试一次就再写一份，**是乘数账**。
- **手册热重载**：默认开（`SENTRY_MANUAL_RELOAD=0` 关），`fs.watchFile` 轮询 5s；语义 **fail-closed 的反面**：新手册坏 → **保留旧手册继续用**（一次坏编辑不该把 AFK 期打成裸奔）。

**② CLI（`bin/sentry.js` + `src/cli-*`）**
`pending` / `history` / `show` / `approve <subject_id>` / `reject <subject_id>` / `version`，全支持 `--json`；**调 `ledger read/record` 子进程，不 fork Kron 内部包**；pending/history **双源合并读**（approval + escalation，每行带 `source`）；`--show-rule-hint` 纯本地把 `gate_id` 翻成手册规则（缓解「pending 无命令正文」的反查负担，读不到就降级不报错）；`--note` 自由文本**只写哨兵本地文件、绝不进账**、重启即失；**已终态拒补批**（非零退出 + 提示「让 agent 重试」）；ANSI **只在真 TTY**。退出码 `0 成功 / 1 运行错误 / 2 参数错`。

**③ 上报 handler（`handler/`，Go stdlib-only + `deploy/` systemd）**
- 触发面：`OnFailure=` **只挂哨兵自己的 unit**（agent 崩溃交给 Crossline `run_boundary`，不重复上报）；必须写 `[Unit]`（写 `[Service]` 静默忽略）；⚠️ 坑：`OnFailure=template@实例` **静默失效**。
- `run_id` 靠 **B3 反查**：闸门在 `agent_start` 把 `unit→run` 写进 spool 目录的 `runmap.jsonl`；handler 从 `systemctl show <unit> -p Result` 反查 result_class / exit_status（`$SERVICE_RESULT` 等 unset）。
- **失败降级 = 持久 spool（write-side outbox）**：先写 spool（**fsync 文件 + 目录**）→ 调 `ledger record` → **得 ok 才删该行**；崩溃最坏是重复投递、被幂等吞掉、**永不丢**；**`ts` 首次落 spool 时冻结，重试永不重盖**（幂等键成分）；指数退避封顶 5 分钟、`drain` 定时器每分钟扫、跨重启无限重试。
- 跨模块纪律：**不 import Kron 的 `internal/`**（跨模块 internal 例外未开口），退路 A3 = 本地对齐形状 + **构建期对照测试防漂移**（`handler/drift_test.go`）。

### 4.3 完成度、未决与对 Farm 的价值

96 JS + 23 Go 测试全绿；真机部署演示（systemd 257.13）有记录。
**未决（作者明写在 `.scratch/sentry/issues/13、14`，均 open）**：pi 主线出现 `@earendil-works/pi-durable`（Experimental，自带 `beforeTool` 拦卡 + 持久状态 + durable task）—— 哨兵是否迁移地基未裁；当前实现建在 **0.84.3**（fail-safe：hook 抛异常不 fail-closed）而 durable 是 fail-closed，**换地基必须重读 `gate_failed` 触发差异**。
**明确不做（撞到就拒绝）**：面板/富 UI/手机应答（Reins）、预算闸、凭据签发（Warehouse）、执行编排（成品项目）、Crossline 芯片闸本体、durable 地基（舰队迁移时重估）。

**对 Farm 的直接价值（最高）**：如果 Farm 驱动的 agent 运行时是 pi，**哨兵扩展今天就能装**（它就是 pi 扩展，落点 `<agent-dir>/extensions/` 或 Warehouse 的 `nw/extensions/<仓库>/<条目>/` + 登记 `settings.packages`），配上 Kron 二进制即得到一套**有测试的无人值守审批门 + 可审计账**。若不驱动 pi，则这套「三层手册 + 先落 pending 再 block + fail-closed 硬上限 + reason 白名单 + spool 幂等重放」是 Farm 自建 gate 的**现成规格**。

---

## 5. Nous-Slaveholder —— 监工（零代码，判据学）

### 5.1 功能定位

**「把装好的派拉起来干活，盯着，挂了抽活，干完回收。」** 与告警工具的根本分野：告警告诉你「它挂了」，监工**把它抽起来继续干活**。分界句（作者原话）：**「成品项目决定该干什么；监工只管让它一直干下去。」**

### 5.2 它到底产出了什么（6 篇 ADR 的实质）

因为零代码，它的价值全在**判据学**（且全部带 `@earendil-works/pi-coding-agent@0.84.3` 的**行号级源码依据**）：

| ADR | 结论（对 Farm 直接可用） |
|---|---|
| 0001 词汇 | **「派」一个词 + 限定词**（Warehouse 派 = `$NW_HOME/profiles/<名>/` 宿主目录、无芯片、清不掉；蓝图派 = 容器 + 命名卷、带芯片、能清）；「组件」三义正名（基座组件 / Warehouse 组件 / 蓝图构件）；**「台账」vs「账」是类型差别（状态 vs 事件）**；抽活三手段冻结名：**重跑同一任务 / 恢复会话续跑 / 换派重来**；「重启」不在词汇表（pi 的 32 条 RPC 里没有 `restart`） |
| 0002 蛛网边界 | 蛛网（Crossline）**只答「它死了」**（心跳 30s + 审批门）；**不另造第二个活性真相源**；两个死亡视角不对称（流断 = 进程层面死；报 offline = 芯片连接层面死），**都收、别互当中继**；审批门在芯片侧是 **60s fail-open**（网络一抖门自动漏 60 秒） |
| 0003 判据面分层 | **实时判据面 = 监工自己 spawn 的派的 stdout JSONL 流**；活性面 = 蛛网心跳 + 子进程退出；事后重构面 = 落盘（台账 + 派自带会话 JSONL）；**蛛网 `session.entries` 不作取证面**；**流断 ⇒ 派有序停机（监工一死派陪葬）**——「关闭子进程 stdin 即请求有序停机」；**客户端不读 stdout 会把派 stall（背压）** ⇒ **读循环健康度是卡死判据的前置** |
| 0004 完成双条件 | **真完成 = `agent_settled` 且最后一条权威 assistant 消息 `stopReason === "stop"`**。理由（源码级）：`agent_settled` 在 `_runAgentPrompt` 的 `finally` 发，**正常收工和被打断都发** ⇒ 只监听它会把自己 abort 掉的失败轮记成成功；`stopReason` 白名单（实测**七值**：`stop`/`length`/`toolUse`/`error`/`aborted`/`pending`/`deferred`）**只 `stop` 算完成**；`deferred` 是占位消息不是终态；**时刻双记**（产物时刻 = 载荷内时间戳 / 观测时刻 = 帧到达时刻）；**完成判定只产事实不触发动作** |
| 0005 卡住判据 | **两口钟，不是一个超时**：静默钟（`now - last_progress_at > T`）看有没有消息；总钟（`now - 本级进入时刻 > C_rung`）看有没有完；**任一到期才算卡住**。五级次序：① 读循环健康度（独立 1s 定时器，连丢 5 tick ⇒ `judgement_face_unavailable`，**停判不抽活**）→ ② 子进程 EOF/退出（走降级路径，**不进「疑似卡住」格**）→ ③ 心跳 offline（只记账不作判据）→ ④ 在跑吗 → ⑤ 静默钟到期。**事件分两张表**：刷新时间轴的一批 vs **只豁免**一批（`auto_retry_start` 只豁免——否则永久重试环会永远显得活着；死线读载荷 `delayMs` 不猜）。阈值：`T_model = httpIdleTimeoutMs + 60s`（默认 360s）、`T_tool = 600s`、思考档位 ≥ high ⇒ ×2；`C_rung = k × T_entry`（`k_①=10, k_②=30`）；`T_readloop = 5s`。**「abort 叫不动」是硬信号**（`abort` 阻塞在 `waitForIdle()`，跑不到 idle 则永不应答 ⇒ 30s 独立死线 ⇒ `abort_unresponsive` 直接升 ③）。③ 前加一道**免费只读的盘上复核**（盘比流新 ⇒ 取消本次升级，把 `last_progress_at` 推到**盘上的真值**）。误抽兜底：只有 ③ 找得回来（`--fork <旧会话绝对路径>`）；①② 不检测不撤销。**`crashed.reason` 封闭枚举**（`silence_exhausted` / `total_clock_exhausted` / `abort_unresponsive` / `pre_nuke_check_failed` / `pre_nuke_check_unavailable` / `judgement_face_unavailable` / `stream_closed` / `ladder_exhausted`）。⚠️ 作者自标**最弱一环**：默认阈值压在「合法长任务通常 < 1h」这个**未实测**假设上，第一个派上线必须复测 |
| 0006 状态集 | **不是一个状态机，是三个各有真相源的列**（投影里三列，**永不做成一个 `status`**，因为三者会不一致，而不一致本身是最该记的事实）：活性层 `liveness{alive,dead}` / 进展层 `phase{starting,running,idle}` / 判据层 `judgement{ok,stuck,judgement_face_unavailable}`；`settled`/`dead`/`reaped`/`crashed`/`streaming` **都不是状态**（`settled` 是转移边 + `result` 派生字段；`reaped`/`crashed` 是**终态事件**，查「哪些派已收」读事件不读状态列）；**`starting` 有上界且零新常数**（`last_progress_at` 起点定在 `spawn` 返回，静默钟自然覆盖；钟在 `starting` 响 ⇒ **跳过 ①②直接上 abort 探针**，因为连一次 RPC 往返都答不上来的派不可能收下一条 prompt）；**「挂起」不成态**（实测：挂起**以在跑为前提**，且**永久**——`_isAgentRunActive` 全文件只有三个写点，唯一置假点就是它自己要等的那件事 ⇒ 挂起的会话永远不可能再 idle、永远收不了工、`agent_settled` 永不再发；`fork`/`clone`/`switch_session`/`new_session`/`abort` **五条命令共有同一机制**）；**spawn 没成就「不是派」**（不开派行，台账里「有几个派」必须可数）；**v1 零周期轮询**（唯一保留节拍 `fleet.snapshot` 10s 且不写状态） |

另外它暴露了一条**与 Farm 直接相关的结构事实**：**Warehouse 派没有芯片 ⇒ 活性层只剩一个写入者、且没有审批门**（没有芯片就没有 `approval_pending` 可轮）⇒ 「卡死判定没有第二见证者，误抽风险全压在自己两口钟上」。作者只提需求不决策（「把芯片登记成 Warehouse 外部源、落点 `<agent-dir>/nw/extensions/…`、须登记 `settings.packages`」）。

### 5.3 完成度

`CONTEXT.md`（含 8 个新词 + 限定词 + 分界句 + **禁词表**：`supervisor`/`ledger`/`agent`/`reap`/`abort`/`restart`… 不得指代表中词）+ ADR 0001–0006 全部「已接受」；**零源码、零测试、零 CLI**。地图（GitHub issue #1）13 张票，其中 CLI 接口面（#12）未落地。⚠️ 作者自标**信源已腐坏**：蛛网两仓源树不在本机，只能对部署副本复核、**未能重锁源仓 commit**。

---

## 6. Nous-Whitehelmet —— 纠察（只有交接件）

### 6.1 功能定位（作者原话）

> 它实际上是一个**自动审稿人**。agent 干完活，它先把确定性的东西跑一遍（类型、测试、lint），再开一个**干净会话**让另一个 agent 审 diff，最后给一份判决：哪几处能要、哪几处必须打回、哪几处要你亲自看。它审的是「**这次改了什么**」，不是「这个仓库好不好」。它的核心资产是**评审者与干活者的上下文隔离**——让写代码的 agent 自审基本没用。

### 6.2 已定内容与硬边界

- **地盘**：一份**判决**（三分类 + 理由）+ 一个**隔离 judge 会话** + 一个**闸门协议**（稳定 JSON，pass/fail/要人看，供编排方消费）。
- **硬边界（已裁）**：不取件/不搬产物（文件一律推仓库）、不拦卡/不叫口令（**那是哨兵的活**，但「要人看」这一类要与哨兵接缝）、不装配、不做预算闸、**不假设产物是代码 diff**（对账表、排版稿也得能判）。
- **它必须独立的第二条硬证据**：芯片红线禁 `registerTool` / `tool_call` / 不 exec / 零文件写，而这四样恰是「跑检查」要的 ⇒ **纠察不能做进芯片**。
- **调研事实（可省 Farm 一轮调研）**：增量边界 —— `vercel-labs/openreview`（★1696）已实现「沙箱 + 跑测试 + fresh agent 审 diff」整条，纠察的增量只有**闸门化**（pass/fail JSON 给编排方）+ **行业无关泛化** + **不绑 GitHub**；确定性检查**已商品化**（semgrep ★16.8k / sonarqube / codeql），**别造**；agent 评审已拥挤（CodeRabbit / Greptile / Qodo / Copilot）但**全绑 git PR、不懂「任务」**；自偏好研究：「Extreme Self-Preference」证实大规模自偏好，**nuance 是在确定性验证器判定的前提下自偏好会消失** ⇒ **能挂确定性验证器的地方就挂**；「LLMs Cannot Self-Correct Reasoning Yet」⇒ 外部反馈必需；**六条要处理的失败模式**（flaky check / green-but-wrong / 自审偏袒 / 上下文污染 / judge 盲目放行 / 成本）+ 应对（重跑 N 次 / check AND review 双闸且 review 有独立否决权 / 分级审）。
- **建议开图先 grill 的 5 个问题**（原文）：三件（确定性检查引擎 / 隔离 judge 会话管理 / 判决闸门协议）各自独立还是合一？闸门 JSON 的确切形状？「要人看」怎么送到人手里（与哨兵的接缝）？非代码行业的断言从哪来？是否复用 Warehouse 的 `check.cjs` 引擎（会不会耦死）？
- **残留不确定**：judge 用同模型不同上下文 / 不同模型 / 多评？**判决的有效期**（代码再改了要不要重审）？

### 6.3 完成度

README 只有一行标题；`CONTEXT.md` 6 个词 + 1 篇交接件（65 行）。**没有 ADR、没有图、没有代码、没有测试、没有 CLI**。位置：母图已把它判为四个原子核之一，但**它自己的 wayfinder 图还没开**。

---

## 7. 相互关系（读源码 + 母图交叉核对）

**族谱**：这些仓出自作者的一张母图 **AFK 基座组件地图（`/opt/find-AFK-idea/.scratch/afk-base/map.md`）**。
母图的既有三基座 = **画板/编排器 Composer**、**蛛网 Crossline + 芯片**、**仓库 Warehouse**；
母图的人工裁决（2026-10-02）新增**四个原子核** = **哨兵 Sentry**、**纠察 Whitehelmet**、**奴隶主·监工 Slaveholder**、**账本 Chronicle/Kron**（判据：「一句话说完的工作 + 一块自己的地盘 + 人能拿走的成品 + 别人可单独用它」）。否掉的候选包括 Ferry / Quarry / Seal / Meter / Conductor / Mneme 等——**这批 CLI 是为「成品项目」服务的**（成品项目才是指挥）。

**具体接缝（按源码/文档可验证）**：

```
                    ┌──────────────────────── 母图 afk-base（裁决层） ────────────────────────┐
                    │  既有三基座：Composer  ·  Crossline+芯片  ·  Warehouse                │
                    │  四原子核  ：Sentry    ·  Whitehelmet     ·  Slaveholder  ·  Kron     │
                    └───────────────────────────────────────────────────────────────────────┘

Warehouse ──装配──▶ 派（$NW_HOME/profiles/<名>/，PI_CODING_AGENT_DIR 指过去）
    │  外部源机制可分发：外部 skill → skills/<仓>/<条目>/（自动发现）
    │                    外部 extension → nw/extensions/<仓>/<条目>/ + settings.packages 登记
    ▼
Sentry（pi 扩展，挂 tool_call） ──调子进程──▶ Kron `ledger record`（approval pending/终态、escalation）
    ▲                                          ▲
    │ 上报接缝（ladder_exhausted）                │ 写入方闭集：sentinel / whitehelmet / slaveholder / crossline / human
    │                                          │
Slaveholder（监工）──spawn 派 + 读 stdout JSONL──▶ 判据（完成/卡住/挂起）→ 抽活阶梯
    │  活性与审批门：Crossline 只答「它死了」        │
    ▼                                          │
Whitehelmet（纠察）──干完活审本次改动──▶ 判决 JSON（pass/fail/要人看）──▶ 编排方
    │  「要人看」→ 接哨兵；确定性检查对象 = 产物（Warehouse 的 27 项 validate 对象 = 声明，共用引擎、对象不混）
```

**读出来的五条结构事实**：

1. **Warehouse 是上游供给方**：它把派装出来；Slaveholder 明确说「把芯片登记成 Warehouse 外部源，落点 `<agent-dir>/nw/extensions/…`，须登记 `settings.packages`」——即**它的外部源机制就是给别家的扩展（含 Sentry）做分发的通道**。
2. **Kron 是唯一的共享真源**：Sentry（最大写入方）、Slaveholder（抽活留痕）、Whitehelmet（判决留档）、Crossline（会话树事件）都往同一份账写，写入方是**闭集**（`enumActorWriter`）。Sentry 的上报 handler 与 CLI **不 fork Kron 内部包**，只调 `ledger` 子进程。
3. **Sentry 与 Slaveholder 的分界是「谁对它死/活下结论」**：蛛网只答活性；派干活的信号来自**监工自己看着的那个派**；Sentry 只管**动作要不要放行**与**上报**。三者都不许替别人当中继。
4. **两块「确定性地跑检查」的引擎可能共用但对象不同**：Warehouse 的 27 项 validate = 「**声明物**是否自洽」（配方/清单/锁定集）；Whitehelmet = 「**产物**对不对」。母图调研把它判为「共用引擎、对象不混」。
5. **版本偏斜是真实存在的**：Sentry / Slaveholder 的地基是 **pi 0.84.3**（Slaveholder 的实证与白名单全按 0.84.3 核字）；Warehouse 的转正配方把 pi **pin 到 1.0.2**；Crossline 芯片的 Dockerfile 还钉 **0.84.3**；pi durable 是 Experimental 待裁。**Farm 必须自己选一个 pi 版本并明确「换版本要重读哪几条」**（Sentry 的 `gate_failed` 触发、Slaveholder 的 `stopReason` 白名单与事件流字段）。

**对外部仓库的邻接（本次未深读，仅记名，避免越界断言）**：`/opt` 下还有 `Nous-Composer`、`Nous-Crossline`、`Nous-Writing`、`Nous-Slaver`、`SuctionCup`、`Warehousetui`（**Cargo.toml ⇒ Rust 项目**，疑为 Warehouse 的 TUI/面板）、`reins`（手机应答 UI，Sentry 明确把手机应答让给它）、`nous-farm`、`Data-001`（Warehouse 的数据仓）等。**其中 `Warehousetui` 是 Rust（`Cargo.toml`）**，与 Farm 的面板方向可能直接相关，但**不在本次五个仓的范围内，我未读其源码**。另：本次一并 clone 的 `Nous-Crossline` 树里出现 `crates/`（Rust 源码）—— 同样**不在本报告范围内，未读**。

---

## 8. 对 Farm 的直接价值 vs 只是灵感

> 前提：Farm = **Rust 全栈**（前端 + 后端 + 可插拔插件）、AFK 式多 agent 平台、**要有面板**、**不百分百模仿 symphony**。以下判定只看「能不能直接吃到东西」。

### 8.1 直接价值：契约与数据模型（可以直接照抄进 Rust）

| 来源 | 是什么 | 为什么对 Farm 直接可用 |
|---|---|---|
| Warehouse `envelope` | 六字段顶层 + 五字段错误 + `ok == (exitCode==0)` + 字符串码↔数字码一一对应 | **面板/前端消费的最小稳定面**。Farm 若调 `nw`，前端只写一套渲染逻辑；若自建，这套 envelope 形状已被验证（Kron 直接移植了它） |
| Warehouse 退出码表 | `0/2/3/4/5` + doctor `10/11/12` | Farm 的「装/查/漂移」三态判定可直接沿用；「warn 不改退出码」这条取舍省掉 CI 与 UI 的分叉 |
| Warehouse `ls` 五域 + `profiles[].state` 四态 | `recipes/components/sources/profiles/locks`，`state ∈ {converged, degraded, drift, broken}` 与 doctor 的 0/10/11/12 一一对应 | **这是为一个尚不存在的面板准备的只读模型**——正是 Farm 面板需要的第一屏。且「数组恒为数组、域不存在 = 空数组」这一条能省掉前端大量空值分支 |
| Warehouse catalog 模型 | 一条记录 = 一个仓库 + `entries[]`；**不含版本**；机器写、确定性排序、永不手改；`recheck` 只报不写 | **Farm 的可插拔插件系统可以直接用这条模型**：插件本体住各自仓库、平台只存编目、装配时点条目、市场/面板按 tag 反查（`tag add/ls/rm` 是现成的「按标签反查插件」） |
| Warehouse 落地约定 | skill 递归自动发现 / extension 只扫一层且**必须登记** | 这是 **pi 的真实行为**（源码级与文档级）；Farm 只要托管 pi profile，就必须遵守这条不对称，否则「装了但不加载」 |
| Kron 10 类事件 + 封闭 schema | 全封闭 struct、拒未知字段、无 `any`、写入方不得填账本自留字段 | **Farm 的事件账可直接按这套 schema 落 Rust enum**（serde + `deny_unknown_fields`），省掉一整轮 schema 设计 |
| Kron 审批状态机 + escalation | `pending → 4 终态`、fail-closed 无自动放行、单挂起、已终态不可补批、`reason_code` 9 值闭集、escalation 10 位闭集 | **Farm 的审批/放行语义今天就能定死**；「延期 = 模型重试（新 subject_id）」这条尤其反直觉、自己设计几乎一定做错 |
| Kron 读面纪律 | `--since` 必填、`--purpose` 必填、排他游标、有界分页、每次读落一条 read 事件、`cache/agg` 侧车 | **Farm 面板的读接口直接照抄这套纪律**；同时提醒：每次读都写账 ⇒ 面板轮询策略要单独设计 |
| Kron 只追加强制 | `O_APPEND` + `FS_APPEND_FL` + 逐笔哈希链 + 段 manifest + 半行留证 + 幂等自然键 | Farm 若要「不可篡改的 AFK 账」，这套四层强制是**有测试的实现蓝本**（含降级要如实标注 ChannelMode 这种诚实纪律） |

### 8.2 直接价值：可运行的东西（子进程调用即得）

| 来源 | 用法 | 代价/注意 |
|---|---|---|
| **Kron `ledger` 二进制** | Farm 作为「AFK 审计账」直接用：`ledger record`（NDJSON stdin）/ `read` / `verify` / `repair` / `archive` | Go 二进制 + `LEDGER_ROOT`；**每次读都写账**；归档需 `CAP_LINUX_IMMUTABLE`；真源只增不减 |
| **Sentry pi 扩展 + CLI + handler** | 若 Farm 的 agent 运行时是 pi：装上即得**审批闸门**（三层手册 + fail-closed）+ `sentry pending/approve/reject` 人裁决入口 + systemd 上报 | 地基是 pi 0.84.3；Python/Node 环境；CLI 依赖 `ledger` 二进制；「手机应答」它明确不做（归 Reins）⇒ **Farm 面板的「等待人裁决」列表正好补这个空位** |
| **Warehouse `nw` 二进制** | 若 Farm 要「装一个 agent 运行环境」：`nw validate`（27 项）/ `recipe bake`（出锁定集+装配命令）/ `up`（七步幂等）/ `doctor`（0/10/11/12）/ `ls`（面板读模型） | 目标机要 `gh`；私库分发；bundle 体积 MB 级（89MB 实测）；数据仓与引擎仓两仓分离 |

### 8.3 直接价值：判据学（无需代码，直接改进 Farm 的编排层）

来自 **Slaveholder**（全部有 pi 0.84.3 源码行号依据，等于替 Farm 做完了逆向）：

1. **完成判定必须双条件**（`agent_settled` **且** `stopReason === "stop"`）—— 只说 `agent_settled` 会把失败轮记成成功。
2. **卡住用两口钟**（静默钟 + 总钟），不是单一超时；**「只豁免不刷新」的事件表**（重试/压缩不得刷新时间轴）。
3. **状态是三列不是一列**（活性 / 进展 / 判据），且**「三者不一致」本身要作为事实记录**。
4. **读循环健康度是前置**（客户端不读 stdout 会背压 stall 子进程 ⇒ 假死与「安静地干长活」同形）。
5. **子进程 stdin 是关机信道**：关 stdin = 请求有序停机 ⇒ **父死子陪葬**；`docker exec -i` 出的是新进程。
6. **abort 会把自己卡住** ⇒ 它反而是**最好的卡死探针**（独立 30s 死线）。
7. **挂起不可逆**（五条命令共有 `await session.abort()` 机制）⇒ 必须有「此会话永不能再 idle」的不可逆结论留痕。
8. **`--fork <旧会话绝对路径>` 是误杀后的唯一找回手段**；会话 JSONL 是跨重启的取证面（自带 ISO 时间戳）。
9. **`starting` 期没有内建死线**（两端都会无限等）⇒ 起点必须定在 `spawn` 返回。

来自 **Sentry**：

10. **工具调用审批门的完整规格**（三层字段分离、窗口只放宽显式 preapprove、`default_action` 独立、路径抽取失败退回「问」、reason 上限 L2 与 5 条禁入、先落 pending 再 block、fail-closed + 硬上限 3）。
11. **reason 是乘数账**（进会话树、每次重试再写一份、永久留存）——这条对 Farm 的 prompt/日志设计是硬约束。

来自 **Whitehelmet**：

12. **判决三分类 + 隔离 judge 会话 + 闸门 JSON** 这套接口设计；**六条失败模式与对应策略**；**增量边界**（别造 semgrep、别绑 GitHub PR、行业无关）；「**能挂确定性验证器的地方就挂**」（自偏好会消失）。

### 8.4 只是灵感 / 不建议照搬

| 来源 | 为什么不建议直接采纳 |
|---|---|
| **Whitehelmet 全仓** | 只有交接件 + 释义表：无 ADR、无实现；三分类判决、隔离 judge、闸门 JSON 都还是**待定形状**（连「三件是否各自独立」都是它自己列的第一个待 grill 问题） |
| **Slaveholder 的具体数值** | `T_model=360s`、`T_tool=600s`、`k=10/30`（阶梯全程最坏 ~4h）、5s 读循环——作者自标**零运行时实测**，压在「合法长任务通常 < 1h」这个未实测假设上。**Farm 只能借结构，不能借数值** |
| **Kron 的保留/归档/作废纵深** | ADR 0012 的七步序归档、`CAP_LINUX_IMMUTABLE`、RFC 3161 时间戳令牌、去重键寿命=真源寿命——对 Farm v1 过重；先吃「事件 schema + 状态机 + 只追加 + 幂等」四样即可 |
| **Warehouse 的 D6「机密永不落盘」+ 两道明文口子** | `keys.json` / `subscriptions.json` 明文存本体住私库，是作者**书面立例**的例外，靠「私库必须 private + `.gitignore` 不许漏排」兜底 —— **是风险模式，不是可复制的安全设计**。Farm 若要存凭据，别照抄 |
| **Warehouse 的分发体重与两仓结构** | 89MB bundle / 每平台一份资产 / 引擎仓+数据仓两仓取货 / 目标机必须 `gh`：这是「目标机零决策」换来的代价。Farm 的插件分发若想轻，需重新取舍 |
| **母图的出界裁决** | Ferry（文件推仓库）/ Quarry（Docker 够了）/ Seal（Warehouse 已覆盖）/ Meter（用户自有方案）/ Conductor（成品项目就是指挥）/ Mneme（不够格）等 —— 这是**作者为他的族谱做的裁决**，Farm 可以参考其理由，但**不能当自己的决策**（Farm 的目的地不同） |
| **Kron 的「v1 只回看、不重演」** | 理由（LLM 重跑必然不同、正文禁入导致 prompt 还原不出）对 Farm 成立，但 Farm 若有面板「时间线/重放」需求，需自己裁一次 |

### 8.5 给 Farm 的三条最硬结论（从源码得出，不是意见）

1. **面板这件事在这五个仓里是空位，且空位是刻意留的**。五仓零 HTTP/零服务端；Warehouse 用 40+ 处注释为「面板」准备了渲染纪律（id 稳定、数组恒为数组、字段名即契约），却明确把面板写进 Out of scope。**Farm 的面板因此不重复任何人，但也拿不到现成后端**——只有 CLI + JSON。
2. **如果 Farm 的 agent 运行时是 pi，那么「审批门 + 审计账 + 装机」三件今天就有可运行实现**（Sentry 扩展 / Kron 二进制 / Warehouse `nw`），用法是**子进程 + JSON**，不是链接。如果 Farm 不用 pi，则这三份**规格**仍然完整可用，代价是 Rust 重写。
3. **版本偏斜是必须先裁的前置**：Sentry 与 Slaveholder 的证据全建在 **pi 0.84.3**，Warehouse 的转正配方 pin **1.0.2**，durable 是 Experimental 且**换地基会改变 fail-closed 语义**（0.84.3 fail-safe vs durable fail-closed）。Farm 必须先钉一个 pi 版本，并明确「换版本要重读哪几条证据」。

---

## 9. 实测证据清单（可复核）

| # | 命令 | 结果 |
|---|---|---|
| 1 | `cd Nous-Warehouse && go build ./...` | rc 0 |
| 2 | `cd Nous-Warehouse && go test ./...` | **17 包全绿**（bake 42.4s / validate 2.6s / cli 1.8s / …）；344 个 `func Test` |
| 3 | `cd Nous-Kron && go build ./... && go test ./...` | rc 0；`internal/ledger` + `internal/schema` **全绿**；66 个 `func Test` |
| 4 | `cd Nous-Sentry && npm test` | **96 tests / 12 suites / pass 96 / fail 0**（含并发压测：并发 314ms vs 串行 515ms，24 笔，幂等不双挂起） |
| 5 | `cd Nous-Sentry/handler && go test ./...` | **ok nous-sentry/handler** |
| 6 | `go build -o /tmp/nw-test ./cmd/nw && /tmp/nw-test --version` | `0.1.0`（裸版本号走 **stderr**，stdout 保持空，符合契约） |
| 7 | `cd /opt/Data-001 && /tmp/nw-test ls --repo .` | rc 0，真 JSON：`{"schema":"nw/ls.v1","nw":"0.1.0","ok":true,...,"data":{"domain":"all","recipes":[research-desk],...}}` |
| 8 | `cd /opt/Data-001 && /tmp/nw-test validate --recipe recipes/research-desk/recipe.yaml --repo .` | rc 0，`summary {total:27, pass:12, warn:0, fail:0, skip:15}`，27 项 id 逐条打印核对通过 |
| 9 | `grep -rn 'net/http\|ListenAndServe\|listen('` 三仓 | **零命中**（无服务端） |
| 10 | `find` 五仓文件清单（排除 `.git`） | Warehouse 127 / Kron 43 / Sentry 63 / Slaveholder 13 / Whitehelmet 7；**五仓 `.rs` 文件数均为 0** |

**读过的关键文件**（节选）：Warehouse `internal/model/*.go`、`internal/cli/cli.go`、`internal/ls/ls.go`、`internal/validate/validate.go`(id 表)、`internal/bake/{bake,command}.go`、`internal/up/components.go`(七步与 merge)、`docs/external-sources.md`、`docs/adr/README.md`、`.scratch/nous-warehouse/map.md`；Kron `internal/schema/schema.go`、`internal/ledger/{ledger,store,commands,read,verify,manifest,appendflag}.go`、`cmd/ledger/main.go`、ADR 0003/0012；Sentry `extension/index.js`、`src/{manual,reason,ledger,cli}.js`、`handler/*.go`、`deploy/*.service`、`docs/spec.md`、`docs/impl-decisions.md`、`docs/grill/06-*.md`、`.scratch/sentry/issues/13,14`；Slaveholder `CONTEXT.md` + ADR 0001–0006；Whitehelmet `CONTEXT.md` + `docs/handoff-whitehelmet.md`；语境 `Data-001/docs/real-first-run.md`、`find-AFK-idea/.scratch/afk-base/{map.md,research/09-*.md}`。

---

## 10. 未实证 / 残留风险（诚实清单）

1. **本报告五仓均为浅克隆（1 commit）**，无历史演化视角；Slaveholder 与 Sentry 的「地图/票」状态取自仓内 `.scratch` 副本，**GitHub 上的实际开放票未核对**（`gh` 需要认证，本轮未做）。
2. **Sentry 的闸门只在本机测试环境跑通**（`npm test`）；真机 systemd 部署是作者记录（`docs/impl-decisions.md` §17，j1900 / systemd 257.13），本轮未复现。
3. **Warehouse 的端到端装配未复跑**（需要 `gh` 私库凭据 + release 资产）；我复跑的是 `build/test/ls/validate`，真机实证引用作者报告。
4. **Kron 的 `FS_APPEND_FL` 通道强制未实测**（需要 `CAP_LINUX_IMMUTABLE` 与支持的文件系统）；我复跑的是测试套件（其中 verify 会如实报 `kernel`/`constructive` 两态）。
5. **Slaveholder 的阈值与白名单**建立在 pi 0.84.3 上，且作者自标「信源已腐坏（蛛网源仓不在本机）」——**它的 ADR 0002/0006 关于蛛网的结论需重核**。
6. **Whitehelmet 无任何已裁设计**；其交接件里的「调研事实」为第二手转述（我未复核 openreview / 自偏好论文原文）。
7. **`/opt` 下的相邻仓**（特别是 **Rust 的 `Warehousetui`**、`Nous-Composer`、`Nous-Crossline`、`reins`、`nous-farm`）**不在本次任务范围，未读源码**；报告中凡涉及它们处均只作邻接记录与母图引用。
