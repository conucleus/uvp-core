# uvp-replay

链上事件流回放 oracle：给定按 `(blockNumber, logIndex)` 排序的合约事件
（`PlanRegistered` / `OrderRegistered` / `SignalSubmitted` / `TimerPoked` /
`StageMaterialized` / `HookReady` / `HookStatusChanged` 等），用 Rust 权威语义
重演求值，产出 `expected`（链上声明的观察）/ `observed`（oracle 自推导的
观察）/ `mismatches` 三列表并逐位比对。

## 状态词表

hook 运行态状态名与云侧 hook_state 语义层、合约 `HookStatus` 枚举统一：

| oracle 序列化值 | 合约枚举 | 含义 |
|---|---|---|
| `init` | `Init` | 尚无事实到达 |
| `wait` | `Wait` | 等待延时到期（携带 `dueAt`） |
| `ready` | `Ready` | 就绪（就绪观察以 `HookReady` 事件表达） |
| `cxl` | `Cancelled` | 负向条件成立，终态 |

## 事件裁剪与推导规则

真实事件流与 oracle 模型存在两类系统性分叉，吸收口是
`absorb_chain_observation`（本文件与代码注释同步维护）：

### 裁剪（trim，不进入 expected）

- `HookStatusChanged(status=ready)`：合约对 →Ready 先 emit 状态变更再 emit
  `HookReady`；oracle 只以 `HookReady` 观察就绪，ready 状态变更被裁剪。
- 逐字重复的 `HookStatusChanged`（同 hook、同 status、同 dueAt）：投影
  重放/重排可能产生重复，重复被吸收，不产生 missing-observed 假阳性。

### 推导（derive，从链上事件补齐 oracle 状态）

合约出生路径共三种，全部以 `HookReady` 承载就绪断言，但只有一种需要推导：

- outside mint 出生（`triggerOrderFromOutsideFor`）与 dock 出生
  （`createDockedOrderFromModule`）：出生/entrance 事实 `_recordSignal`
  落在本订单，出生 hook 被编译器约束为单一正向 atom（ANCHOR 订阅 / D013），
  事实到达即由正常求值自然产生 `HookReady`，无需推导。
- order-link mint 出生（`triggerOrderFromSignalFromModule`）：出生事实留在
  origin 订单上，本订单不 `_recordSignal` 但 emit `HookReady`，求值路径无事实
  可依。oracle 据链上 `HookReady` 反推：order-trigger hook 补 runtime
  ready/readyEmitted 并物化其阶段，同时把该观察记入 observed（接受链上
  断言；重复的出生 `HookReady` 在合约 `!readyEmitted` 门下不可达，第二次
  以 missing-observed 暴露流异常）。非 trigger hook 的无信号 `HookReady`
  不推导，保持 mismatch 暴露真实异常。

### 消费（consume，回填状态不进 expected）

- `StageMaterialized`：链上物化事实回填 `materializedStages`，后续依赖该
  阶段的 watcher 求值据此放行。
- `OrderMaterialized` / `OrderTriggered` / `OrderLinked`：仅存在性事件，
  无观察语义。

## poke 语义

对齐合约 `pokeTimer` 的两道门（`TimerNotWaiting` / `TimerNotDue` 在合约侧
是 revert）：oracle 对非 wait 或未到期的 `TimerPoked` 事件直接跳过，不产生
unexpected-observed 假阳性；到期后的 poke 照常重评。

## 指令集

冻结指令集 `SIGNAL` / `NOT` / `AND` / `OR` / `DELAY` / `MERGE`。`MERGE`
（撮合扇入，合约 semantic 0.6）按合约 `_mergeValue` 逐字求值：任一在场分支
即就绪、锚点取在场分支最早到达、操作数限定裸 `SIGNAL` 引用、编码层 arity
k≥2（k=1 观察入口是 cloud 运行时投递形态，链上无对应物）。权威 DSL
（uvp.semantic.v1）已退役 MERGE 表达式语法，官方编译器不产出该指令；
合约仍接受手工 plan 的 `op=Merge`，oracle 必须同口径求值。
