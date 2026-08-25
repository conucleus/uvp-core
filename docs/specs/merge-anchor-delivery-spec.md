# MERGE@ / ANCHOR@ 投递语义规格（normative）

- 状态：**权威**。与实现冲突时，先修本文档或先报冲突，不得静默偏离。
- 版本基线：uvp-semantic/0.5；本文锚定的行为自 0.3 引入、0.4 固化根形态校验。
- 适用轨道：cloud（已完整实现）；EVM 已实现 MERGE 同单跨源扇入（semantic 0.6），ANCHOR 未实现（见 §8）。
- 代码锚点以 `crates/uvp-hook-dsl/src/lib.rs` 与 cloud `pkg/statemachine` 当前 HEAD 为准。

## 1. 定位

`MERGE@(...)`（撮合扇入）与 `ANCHOR@(task.stage.signal)`（收购回流）是**事件驱动的投递原语**，
不是布尔表达式节点：

- 它们**不参与信号时间表求值**：求值器对二者恒返回 `NeedsMore`（reason 见 §4），永不 Ready。
- "就绪"的唯一含义是：一个携带溯源的贡献事件到达，且通过投递门（§5）。
- 判定权（配对、建单、聚合计数）全部归阶段静态执行器；引擎只做逐事件转交。

## 2. 语法与解析层契约

| 形态 | 合法写法 | 约束 |
| --- | --- | --- |
| 撮合 | `::MERGE@(a::t.s.x[, b::t2.s2.y ...])` | 空标头强制；每路目标必须是裸 `source::task.stage.signal` 四段引用；目标互异；**k≥1**（解析层下限，测试 `merge_entry_with_single_upstream_is_observation_entry` 钉住） |
| 回流 | `::ANCHOR@(task.stage.signal)` | 空标头强制；目标是**不带 source 名空间**的裸三段式（子订单事件来自任意秩序，血缘过滤归状态机）；不可与 `&` `\|` `~` 延时混用 |

- 三种跨源入口（含 `OUTSIDE@`）都必须是完整 hook 条件：不得嵌入布尔树/延时，不得带标头
  （负例测试钉住 `wholesaler::MERGE@(...)`、嵌套组合等拒绝形态）。
- 解析产物 dependencies：merge 展开各路 positive 依赖；anchor 为 `{kind:"positive", source:"", signalName}`——
  空 source 是回流通配符，编译期不可枚举。

## 3. Cloud AST 线格式与根形态守卫

顶层字段：`mode ∈ {normal, outside_spawn, merge, anchor}`；merge 另有 `mergeTargets[]`
（每项 `{source, signalName}`）；anchor 另有 `anchorTarget.signal`；outside_spawn 必带 `upstreamSource`。

解码层根形态守卫（`expr_from_cloud_value` + mode 匹配，手写毒 AST 唯一防线）：

- `mode=merge` ⇔ root 是 merge 节点；`mode=anchor` ⇔ root 是 anchor 节点；
- `mode=outside_spawn` ⇔ root 是**纯 signal 节点**（委托关系由顶层 source+upstreamSource 表达；
  要求 external 根会拒绝编译器自身的合法产物——0.4 已修）；
- `mode=normal` 禁止内部嵌套跨源节点（`validate_external_position` 只允许根级）；
- 仅 anchor 模式允许顶层 `source` 为空。

## 4. 求值器契约

对 `Expr::Merge` / `Expr::Anchor`，`eval_expr` 恒返回：

```
state = NeedsMore, anchors = [], ready_at = None,
reason = "merge hooks are delivered per contributing event by the state machine"
       | "anchor hooks are delivered per child-order event by the state machine"
```

**k=1 merge 表达式在求值层非法**：解码层对 merge root 强制 `targets.len() >= 2`
（错误串 `"compiled merge AST node requires at least two targets"`）。这不是矛盾而是分层：

> k=1 是合法的**运行时投递形态**（跨订单观察入口），但作为汇合**表达式**没有求值意义。
> 生产投递路径必须在进入求值器**之前**按 mode 短路（§5.3），永远不把 merge/anchor AST
> 送进 `EvalCompiledHook`。任何通用求值工具（回放、审计）遇到这两个 mode 必须同样短路，
> 而不是报错。

## 5. 投递契约（状态机侧，cloud 已实现）

### 5.1 受影响钩子的发现
`loadAffectedHooks` 按 `(signal_name, source_zhixu_id)` 反查 `hook_dependency`，
并额外匹配 `source_zhixu_id = ''` ——这是 anchor 通配依赖的唯一消费点：
任意秩序发出的同名信号都可能命中回流钩子，由血缘门二次过滤。

### 5.2 血缘门（anchor 专有，构成性过滤）
`orderHasAnchorLineage`：到达订单必须能沿 `rel_order_order` 反查到父母边，
且父母订单登记在**钩子所属秩序**之下。仅"存在任意父母"不够——否则锚 A 的农户事件
会误投递给锚 B 的同名回流钩子；跨秩序锚互不投递。被拒事件计入
`stmHookLineageRejectedTotal` 并静默跳过（区别于撮合的来者不拒）。

### 5.3 短路与逐事件投递
`evaluateHookDefinition` 在调用求值器**之前**检查 `DecodeCompiledHook(ast).Mode`：

- `merge | anchor` → 直接返回 `{status: Register}`，不查信号时间表、不求值。
  每次到达 = 一次就绪事实 = 一次对 `(zhixu, stage, source, hook, order)` 维度的
  hookstatus upsert；幂等性由该维度唯一性承载，重复事件重放同键 upsert 无副作用。
- 其余模式走正常求值（Ready→Register / Wait→Waiting+dueAt / Impossible→Cancelled / NeedsMore→Init）。

### 5.4 执行器权威与新因果链
执行器决定成交/收齐后发 `str(new_source=true)` 并在信封声明 `parent_order_ids`；
状态机分配新单号、写 `rel_order_order` 血缘并对每个 parent 做影子注册。
MERGE 建（配对）单与 ANCHOR 分叉建单走同一通道。引擎不做 count/all-of 裁决。

## 6. 不变量（EVM 实现必须逐条满足）

- I1 merge/anchor 永不经布尔求值产生 Ready；Ready 仅来自事件投递。
- I2 k=1 merge 合法当且仅当走投递路径；任何求值入口遇之必须短路而非报错。
- I3 anchor 投递前必须完成"父母可反查且父母属于钩子秩序"的构成性过滤；merge 不过滤。
- I4 回流钩子标头恒为空；merge/anchor 目标形态约束见 §2，违反即编译期拒绝。
- I5 投递幂等键 = (zhixu, stage, source_zhixu_id, hook_name, order_id)；重放不改状态。
- I6 血缘写入只发生在显式 `str(new_source=true)+parent_order_ids` 建单事务内；
  分馏（OUTSIDE@）由引擎代行并补写父边，仅作血统追溯。

## 7. 一致性核对结论（2026-08-25 对照）

cloud（`pkg/hookdsl` + `pkg/statemachine`）与本文档**一致**：类型线格式
（hookdsl.go NodeMerge/NodeAnchor/MergeTargets/AnchorTarget）、mode 短路位置
（evaluateHookDefinition 先于求值）、通配依赖 SQL、血缘门语义、逐事件幂等投递、
bootstrap e2e k=1 观察形态（`main_ci_zhixu.yaml` child_test_complete）全部吻合。

已知张力（非缺陷，属工具面缺口）：Rust 侧不存在"只解码不求值"的公共入口，
`DecodeCompiledHook` 的 Go 实现是结构化的、而 `EvalCompiledHook` 对 k=1 merge 会报错——
任何未来引入的通用重算/审计工具必须复刻 §4 的短路规则，已在 I2 中成文。

## 8. EVM 落地状态（2026-08-25 更新）

**已落地（semantic 0.6）**：MERGE 以同单跨源扇入形态上链——合约 `InstructionOp.Merge`
（任一路在场即就绪、在场最早锚点、无等待分支，表达式形态 k≥2）；replay oracle `merge_value`
镜像；TS 编译器把 `::MERGE@(...)` 编码为 MERGE 指令并拒绝 k=1 与非裸引用目标。
边界声明：链上 MERGE 是**同订单内跨 source** 扇入；跨订单配对/建单判定仍归执行器
（cloud 全拓扑能力不变）。I2 在链上的体现：k=1 由编码层拒绝，求值层不会见到。

**未落地（后续 D6 余量）**：ANCHOR 跨订单回流需要血缘投递子系统——链上现仅有
单亲 `OrderTriggerLink`（UVPOrderLinkModule），无 rel_order_order 图与反向索引，
且 `_evaluateAffectedHooks` 按 (order, plan) 隔离发现受影响钩子，子单信号无法触达
父单钩子。实现前 ANCHOR 编译保持 fail-fast，错误信息指向本节。
