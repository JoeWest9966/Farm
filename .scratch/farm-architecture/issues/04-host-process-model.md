# 04 - Farm 宿主进程模型与兄弟仓库集成方式

Type: grilling
Status: open
Blocked by: 05

## Question

Farm 宿主进程的运行时形态，以及它如何驱动/复用 pi 生态链（Sentry 部署守护、Kron 审计、nw/Warehouse 凭据）：
- Farm 是自己 spawn pi 子进程，还是委托 Sentry？agent 生命周期归谁管。
- Kron 是否直接作为审计后端（它已实现 record/read/verify/repair/archive）。
- Warehouse（nw）凭据的取用通路（编排器创建 agent 时要产出 cli 命令，需要 Warehouse 的 vps/github 密钥）。
- 插件进程（编排器等）的监督/重启策略（Symphony 的 OTP 分级重试可借鉴）。
- pi 版本偏斜（Sentry/Slaveholder 锁 0.84.3 vs Warehouse pin 1.0.2）如何裁决。
