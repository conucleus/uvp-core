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
- `HookStatusChanged(status=init)`：v0.10 合约不产出（Init 是隐含初值，无
  观察语义）；携带该状态的输入事件被裁剪——原生入口的输入契约因此不需要
  适配层预裁 init 观察。
- 语义重复的 `HookStatusChanged`（同 hook、同 status、同 dueAt 时刻）：
  投影重放/重排可能重复，重复被吸收，不产生 missing-observed 假阳性。
  dueAt 按时刻归一化比较（毫秒位数/时区偏移写法不是语义），不同渲染的
  同一时刻视为重复。

### 推导（derive，从链上事件补齐 oracle 状态）

合约出生路径共三种，全部以 `HookReady` 承载就绪断言，但只有一种需要推导：

- outside mint 出生（`triggerOrderFromOutsideFor`）与 dock 出生
  （`createDockedOrderFromModule`）：出生/entrance 事实 `_recordSignal`
  落在本订单，出生 hook 被编译器约束为单一正向 atom（ANCHOR 订阅 / D013），
  事实到达即由正常求值自然产生 `HookReady`，无需推导。
- order-link mint 出生（`triggerOrderFromSignalFromModule`）：出生事实留在
  origin 订单上，本订单不 `_recordSignal` 但 emit `HookReady`，求值路径无事实
  可依。oracle 据链上 `HookReady` 反推（推导门刻意 **mint-only**）：mint 标记
  的出生 hook 补 runtime ready/readyEmitted 并物化其阶段，同时把该观察记入
  observed（接受链上断言）。dock 标记的出生 hook **不接受**断言推导：dock
  出生事实恒先落本订单（`createDockedOrderFromModule` 内 `_recordSignal` →
  `SignalSubmitted` 先行），求值路径已可推导其 Ready，链上出现 oracle 未推导
  的 dock `HookReady` 只能是事实缺失的异常——接受断言会把异常吞成配对成功，
  该形态保持 missing-observed mismatch 暴露（冻结测试
  `dock_hook_ready_without_signal_stays_a_mismatch` 钉住）。重复的出生
  `HookReady` 在合约 `!readyEmitted` 门下不可达，第二次以 missing-observed
  暴露流异常。非 trigger hook 的无信号 `HookReady` 同样不推导，保持 mismatch
  暴露真实异常。plan 缺失 v2 结构字段（`orderTriggerKind`、`stageId` 等）在此
  响亮失败，不回退成"非 trigger"。

### 消费（consume，回填状态不进 expected）

- `StageMaterialized`：链上物化事实回填 `materializedStages`，后续依赖该
  阶段的 watcher 求值据此放行。
- `OrderMaterialized` / `OrderTriggered` / `OrderLinked`：仅存在性事件，
  无观察语义。

## 观察配对与比对契约

- 配对键：expected 与 observed 按 `(planId, orderId, hookId)` 分桶、桶内按
  到达序配对。全局下标配对会把不同 hook/订单间合法的事件流交错误配成
  semantic-mismatch——交错是流布局，不是语义分叉；每个事实键的观察序列
  只与该键自己的求值历史可比（与合约 `_evaluateAffectedHooks` 的 per-key
  hookIds 序一致）。
- `dueAt`：按时刻归一化比较（毫秒位数/时区偏移写法不是语义）；时刻不可
  解析或单侧缺失时不静默放行。

## poke 语义

对齐合约 `pokeTimer` 的两道门（`TimerNotWaiting` / `TimerNotDue` 在合约侧
是 revert）：oracle 对非 wait 或未到期的 `TimerPoked` 事件直接跳过，不产生
unexpected-observed 假阳性；到期后的 poke 照常重评。

## 指令集

冻结指令集 `SIGNAL` / `NOT` / `AND` / `OR` / `DELAY`：撮合扇入指令不在
指令集内，合约枚举与编码门同步收口。
`AND` / `OR` 的编码门要求 arity ≥ 2（k=1 观察入口是 cloud 运行时投递
形态，链上无对应物，编码层即拒绝）。携带指令集之外指令的 plan 在求值
期按 unsupported 指令响亮失败——官方编译器只产出冻结集，集外指令没有
合法生产者；回放不为其保留求值口径，整体以错误收场而非降级 mismatch。
