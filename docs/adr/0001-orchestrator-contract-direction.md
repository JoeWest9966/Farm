# 0001 - 编排器契约：Farm 服务 API，编排器是客户端

日期: 2025-06（wayfinder 票 [#1](https://github.com/JoeWest9966/Farm/issues/1) 裁决）
状态: 已接受

## 背景

Farm 的锁定拓扑：Farm = 宿主 + 面板 + 插件骨架（无编排逻辑）；Orchestrator = 独立通用 AFK 编排器，以进程级 IPC 插件接入。悬而未决的是这份"编排器插件契约"的形状与边界——它决定了面板能不能统一观测、其他编排器实现能不能接入、以及非编程域 AFK（电商等）能不能复用同一套契约。

## 决策

契约是一组 **Farm 侧服务 API**（v1 五服务：**tracker / agent-runner / merge / workspace / events**），编排器进程跑自己的确定性循环、每 tick 调用服务。配套裁决：

1. **agent-runner 是 Farm 服务**，带本地（直接 spawn `pi --mode rpc`）与远程（架在 Crossline wire 之上）双后端；SuctionCup 不进契约（捕俘是另一条插件线）。
2. **完成信号 = 宿主 merge 服务产出的 merge commit**（agent 提议、宿主执行）；投影（关票/开 PR）也由宿主机械执行、由 workflow 配置开关。
3. **tracker 服务持有归一化 Issue 模型 + 原子 claim**；frontier/dispatchable 计算归编排器。
4. **契约本体零领域知识**：agent 行为住 pi skill（LLM 消费），调度策略住被编排仓库的 workflow 配置（确定性循环消费）。
5. **事件流是必需面**：`blocked`/`needs_attention` 为一等事件，面板只做投影层。
6. **工件纪律照抄 Crossline**：schema 单一源 + 生成文档 + 闭集错误码 + 一致性套件作为"符合契约"的唯一凭证。

## 考虑过的替代方案

- **Farm 跑循环骨架、插件只答决策**——被否：tick 循环本身就是编排 know-how，放进 Farm 等于 Farm 有了半个编排器，与"Farm 无编排逻辑"冲突；且通用 AFK 的循环形状未必相同。
- **编排器直接驱动 pi / Crossline 直接当 agent-runner**——被否：观测面分裂、每个编排器重写 pi 管理；Crossline 无派发语义且"后门是中继不是翻译器"。
- **agent 自报完成 / agent 执行投影（Symphony 式写回）**——被否：frontier 推进不应信任 LLM 自述。
- **tracker 抽象独立成仓 / 不要抽象（implement-spec 足矣）**——被否：前者多付治理成本（留"可整仓搬走"的后路即可），后者与会话内编排/确定性循环的分工矛盾。
- **域知识进 skill 或进代码**——被否：死循环读不了 markdown；进代码则换领域改代码。

## 后果

- Farm 的"厚"有明确上限：五服务 + 插件宿主，再多就是编排逻辑越界。
- 换 agent 运行时只改 Farm 一处；面板对任意契约符合的编排器获得统一观测。
- 需要为五服务维护 schema 与一致性套件（沿用 Crossline 已验证的纪律）。
- 传输层细节（stdio/socket、版本握手）归 Rust 技术选型票；存储形态归持久化票。
