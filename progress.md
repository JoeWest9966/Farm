# progress — ticket #13 Tracker 服务与原子 claim

Branch `farm/t13` (base `implement/spec-10` @ 3b55b9a). Worktree `/var/opt/farm-wt/t13`.
CARGO_TARGET_DIR=/opt/Warehousetui/target (shared).

## Seams under test (agreed up front, per tdd skill)

| seam | 位置 | 测什么 | 不测什么 |
| --- | --- | --- | --- |
| S1 归一化视图 | `farm_core::tracker::TrackerService::issues` + `GET /v1/tracker/issues` | native blocking 边、状态/assignee 归一化 | provider 内部表示 |
| S2 原子 claim | `TrackerService::claim` + `POST /v1/tracker/claims` | CAS 只有一家赢、幂等、冲突闭集码 | SQL 细节 |
| S3 release | `TrackerService::release` | 幂等、release 后他人可 claim、非持有者释放即冲突 | — |
| S4 状态历史 | `TrackerService::history` + SQLite 落库 | 迁移落库、跨重启可查询、无变化不造历史 | 事件日志（#15 域） |
| S5 migration 组织 | `farm_core::migrations::MIGRATIONS` | 按五服务分域：v3 = tracker 域，SQL 住在服务模块 | 其他服务域表 |

## Slices (red → green)

- [ ] S1 归一化 Issue 模型 + native adapter + blocking 边（farm-core）
- [ ] S2 原子 claim CAS + 冲突/幂等（farm-core）
- [ ] S3 release 语义（farm-core）
- [ ] S4 状态历史落库 + 跨重启查询（farm-core）
- [ ] S5 farm-sdk 加法类型（blocking 边进 `IssueView`）+ 契约文档重生成
- [ ] S6 farm-host 两条占位路由换真 handler（HTTP 接缝）
- [ ] S7 并发 claim 一致性（HTTP 接缝）+ claim 事件投影到内存总线

## 约定（先例，供 #12/#14/#15/#16 兄弟票跟随）

- **按五服务分域的 migration**：每个服务域独占一个版本号；该域的 SQL 住在服务自己的
  `farm-core/src/<service>/migrations.rs`（`pub fn v<n>_<domain>(conn)`），
  `farm-core/src/migrations.rs` 只做**版本登记表**（`up: tracker::migrations::v3_...`），
  不放任何服务域 SQL。版本号在集成分支上协调分配（#11 = v1 host_meta；
  #13 = v3 tracker 票快照 + 状态历史；后续 #14/#15/... 依合并顺序取 v4/v5/...，
  注意 v2 仍空置，未被任何票占用）。
- **契约面加法**：`farm-sdk` 只加类型/字段（`#[serde(default)]`），不加错误码、不改路由路径。
  本票只往 `IssueView` 加 `blocking` / `blocked_by` 两个字段。
- release / 状态历史查询的**契约路由**本票不新增（任务指定只接通两条占位路由）；
  服务面 API 已就绪，路由由 #22/#23 加法追加。
