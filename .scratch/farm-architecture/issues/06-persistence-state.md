# 06 - 持久化与状态层

Type: grilling
Status: open

## Question

Symphony 规格故意只用内存态（重启即失），Farm 必须自定持久化与状态层：
- 状态存哪里：嵌入式 SQLite / sled / 文件？哪些状态需要持久（任务图、agent 会话转写、token 记账、审计）。
- 与 Kron（append-only 审计）的分工：审计归 Kron，运行态归 Farm 内嵌存储？
- 崩溃恢复语义：宿主重启后，运行中的 agent/插件如何对账（Symphony 的 tick 对账循环可借鉴）。
