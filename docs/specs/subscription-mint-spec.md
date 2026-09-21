# 订阅与铸单模型规格（Subscription & Mint Model）

> 状态：对齐基线（v1）
> 语义版本：`uvp.semantic.v1`（单一语义版本线，不并存两套语义）
> 适用：uvp-core（Rust，DSL 语义唯一权威）、uvp（Go 云侧运行时）、uvp-protocol（TS 壳层）
> 合约边界：EVM 合约当前冻结为 `UVPStateMachine` 0.11、`UVPDockingModule` 4.3 及其余 module fixtures；六域 PlanCommit（publisher、hooksHash、capabilitiesRoot、dockRoutesRoot、dockInterfaceRoot、deadline）、复合 `(planId, orderId)` 身份、dock roots 和 EIP-712 typed-data 必须与 `uvp-stack.v1.json` 等值。工具链不产出指令集外的指令/入口。

---

## 0. 本文解决什么

"跨秩序四典型"（分馏 `::OUTSIDE@`、撮合扇入标头、收购 `::ANCHOR@`、委托 signalMap）若各自作为命名捆绑包，每个捆绑包都要背大量场景定语（回流方向、三段判定切分、k≥2 表达式下限等），无法正交组合。本规格以三个正交层——**事实、路由、铸单**——表达跨秩序协作，并给出对应的语法表面：

- 事实怎么进来：订阅（一种语法，两条路由规则）。
- 订单怎么出生：str 自报（免声明）或 `mint: per-fact` 代铸（唯一声明点）。
- 阶段种类：编译期定死、终生不可变。

指令集为 SIGNAL / NOT / AND / OR / DELAY：扇入类指令不在枚举与编码门内，携带集外指令的 plan 在 `commitPlan` 注册边界响亮拒绝。语法面不含 OUTSIDE/ANCHOR 标头、OUTSOURCE、externalSignals 与 trigger 入口表。

---

## 1. 事实层（Fact）

### 1.1 两种锚定

事实（signal）按信封内容归类，**由发送方在每次发送时自由选择**（想发就发原则的延伸）：

| 锚定 | 信封特征 | 存储 | 例子 |
|---|---|---|---|
| 订单锚定 | 携带 `order_id` | individual_record（现有，first-win） | 农户 cmp 落在农户单上 |
| 通道锚定 | 无 `order_id`，携带去重键 `fact_key` | 源级事实存储（新增） | 成交播报 deal、开门/关门 |

### 1.2 事实纪律三元组（不变量）

无论锚定方式，一条事实必须具备：

1. **去重身份**（first-win，重复吸收）：
   - 订单锚定：`(域, order_id, signal_name)`（现有键）。
   - 通道锚定：`(域, source, stage, signal, fact_key)`；`fact_key` 由发送方提供（成交编号等业务键），信封新增字段。
2. **事实标签**（全序 + tie-break）：云侧 `ready_at + seq`，链侧 `(txHash, logIndex)`。一切投影按事实标签排序，不按到达时间。
3. **溯源**：发送方域/单/阶段/信号，及发送方选择携带的关联订单引用（如成交事实携带买卖双方订单 id，供代铸复制血缘）。

**可重放是唯一不变量**：给定全部事实（两种锚定）+ 不可变定义，重放必然收敛到同一订单集合。订单上下文不是事实成立的必要属性。

### 1.3 事实归属的语义结论

- 事实保持自己的家；铸单是从事实**派生新身份**并记录溯源关联，不移动事实、不产生双重身份。
- 开门/关门/成交播报是事实流，不是身份。订单只出现在"没有它就无法路由、无法担责"的地方。

---

## 2. 路由层（Routing）

### 2.1 source 命名空间

- `source` 是 **zhixu 局部**的因果链身份命名空间；多个 stage 可共享同一 source（整条业务线共用一个因果身份类）。
- 订阅寻址 `@source::task.stage.signal` 只在本域解析：目标的 source 类必须由本 zhixu 定义内的 stage 声明（引用存在性校验，uvp-core `validate_hook_dependency_references` 与 Go 轨编译器校验 §7.5 同款）。**订阅语法没有直接跨秩序形态**——跨秩序协作的合法形态是订阅阶段经 executor 绑定承接投递：同域类订阅配静态 executor（订阅阶段必须静态绑定，禁止运行时 patch）；跨秩序事实级联走 `supplierType: zhixu` 委托 dock + signalMap（见 2.4），resolved route 的 source seam 即接缝处的 source 声明。
- 乐高原则：秩序之间无父子。被委托方天然存在，不因被委托需要父；可反向委托。无 dock 实例 = 无关系 = 不投递，这是"尚无关系"的正常态，不是孤儿。

### 2.2 订阅语法（receiveSignals 值）

| 表达式 | 语义 | 求值 |
|---|---|---|
| `{source}::{condition}` | 同单 hook（布尔/延时），现有语义不变 | 在订阅方自己的订单上下文内求值，判决一次（init/wait/ready/cxl） |
| `::ANCHOR(@{source}::{task}.{stage}.{signal})` | 跨源订阅通道：按类寻址，逐事件投递、携带溯源 | 无表达式裁决；路由规则见 2.3 |

`::OUTSIDE@` / `::ANCHOR@` 标头与 `OUTSOURCE` 不在语法面内：解析器词法识别这三类关键字并**精确拒绝**，报错统一指引入口 `::ANCHOR(@source::task.stage.signal)` 订阅。扇入类标头、k≥2 表达式下限、空标头白名单规则均不在词表内，按通用语法错误拒绝（订阅必须空标头）。

### 2.3 三种接收方（编译期定死，选项 A）

| 接收方种类 | 判定（编译期） | 收事实方式 | 收到后 |
|---|---|---|---|
| 出生阶段 | 阶段声明 `mint: per-fact` | 按 source 类扇入（铸前无单可锚） | 每事实引擎代铸一单，本阶段为其出生阶段 |
| 有锚阶段 | 其 source 类在本域内存在 `mint: per-fact` 声明 | 按单路由：事实沿对接记录（域内血缘/dock 实例）到达订阅方订单 | 推进（同单事实累积） |
| 无锚监听 | 其 source 类在本域内无任何 mint 声明 | 按 source 类扇入 | 执行器自行处理（配对、计数等私有判断） |

规则细节：

- 种类终生不可变，禁止运行时 patch（沿用既有禁 patch 门禁风格）。
- **mint 声明是编译期唯一的锚定依据**。执行器自发 str 出的订单，编译期不可见；订阅此类来源的阶段一律扇入，多张同源单时订阅方执行器按溯源自行分拣。
- mint 阶段自身的订阅一律扇入（铸前无单）。
- 同单 hook（`{source}::{condition}`）的 header source 类必须在本域声明（引用存在性校验，与订阅目标同款）；接收方阶段自身是否"有锚"只决定 ANCHOR 订阅的路由方式（按单/扇入），不限制同单 hook 的可声明面——同单 hook 在订阅方订单上下文内求值，而订单对该 zhixu 的全部阶段可见（fixture `cross_source_direct_trigger.json`：无 mint 声明的 watch 阶段挂同单 hook 合法编译）。

### 2.4 跨域：委托 dock + 接口映射

- 委托是一个秩序 dock 另一个秩序：目标定义在 `spec.dockInterface` 发布**具名接口 map**（接口名 → {orderModes, inputs, outputs}），调用方 stage 在 `executor.zhixuExecutorConfig` 按接口名引用，`inputMap`/`signalMap` 是接缝上的对译表；A 的委托 stage 与 B 被绑定的端口在接缝处视为**同一个 source** 的两半（单源 seam，被绑定端口范围）。
- `target.zhixu` 填目标定义的 `metadata.name`（slug）或显式 `null`（动态选择）。DSL 壳不携带任何派生身份（`metadata.uid` 不是作者可写字段）；身份权威分治——链轨由 uvp-protocol TS 从内容派生 `zx-<32hex>`（内幕），云轨由 DB 唯一 name + 主键承载，共享 core 产物为中性形状（`zhixuName` 键，无 uid/hash/root 字段）。`order.mode` 闭集 {new, existing}：`new` 建独立子订单（恰好一条 input 绑定 = 出生锚，云轨幂等=建立自然唯一键、链轨幂等=链上确定性承诺），`existing` 连接既有目标订单、不建单（建立时回填已成立的接口输出事实）——`existing` 与 `target: null` 的组合语义单点收敛于下「对等挂接五点」。
- 委托声明至少一项输入或输出映射（无需虚构 str/cmp 映射满足格式）；接口输出不自动置任何一方为终态，终态只由本地阶段/订单结束驱动。
- 委托共享订单上下文（现有 zhixu 执行器 `NewSource=false` 通道不变）；事实经 signalMap 逐条映射回父阶段。
- 委托关系一次性绑定、禁 patch（现有门禁不变）。
- `rel_order_order` 语义为**对接记录**（谁 dock 谁、映射实例、接缝两侧锚点），不是"父子血缘"；表结构不变。按单路由以对接记录为落点。
- dock 链深度上限两种计数口径（常量同为 `MAX_DOCK_DEPTH = 8`，静态更严）：静态 linker（uvp-core `dock.rs`）按**定义节点数**计——本定义 root 记 1、启动图深 >8 拒绝，即最多 7 条静态 dock 边；链上 `UVPDockingModule` 按 **dock 边数**计——parent 深度 ≥8 才拒绝，即最多 8 条 dock 边。两者观测面不同：静态 linker 只看编译期启动图，链上闸计数运行时累计的 dock 边。

**对等挂接五点（`order.mode=existing` + `target: null`）。本规格是该语义的唯一权威出处，两册文法手册（两轨各一册）与实现手册只留一句话摘要与指针：**

1. **一等对等挂接语法**：`existing` 与 `target: null` 是对等挂接的一等作者语法——与 `new` + 静态目标共用同一 `zhixuExecutorConfig` 键闭集、同一校验族（D 码）与同一具名接口发布面（`orderModes` 含 `existing` 的接口），不是附属变体或运行时私约；挂接双方仍是对等关系（§2.1 乐高原则），不产生层级包含。
2. **拼批 / N:1**：同一目标运行可被多个调用方挂接——一个订单可由一次 `new` 对接创建，此后被任意多次 `existing` 对接引用；每个调用方按（本地订单, 本地阶段）各成一条对接实例（`dock_instance` UNIQUE(本地秩序, 本地订单, 本地阶段)），共享同一目标订单，任何一方都不重复建单。
3. **运行时选择与生命周期**：`target: null` 的 route 编译为未解析声明面（`uvp.dockRoute.unresolved.v1`，本地校验全量保留、不进 link），运行时由选择记录（`/dock-selection`，目标按定义 uid 寻址；`existing` 的目标订单由记录的 orderRef 指定）钉住为 resolved 行；定义重发布时钉住行回到未解析形态，下次建立按当时选择重新钉住。
4. **DDL 已支持**：云轨 DDL 已承载全部所需结构——`dock_route_selection`（UNIQUE(父定义, 本地阶段)，目标定义 uid + 接口名 + 可选 orderRef）、`dock_instance`（order_mode 闭集含 `existing`）、按（实例, 端口）的投递唯一键（`dock_input_delivery`/`dock_output_delivery`）；该形态是纯运行时能力，无 schema 缺口。
5. **链轨能力缺口**：链轨（uvp-eth 侧编译器）暂不支持该形态——`existing` 与未解析 target 在 on-chain 编译边界按 `UNRESOLVED_DOCK_TARGET` 响亮拒绝，不静默降级。拒绝发生在编译期且响亮：依赖该形态的调用方切到链轨裁决器不会遭遇静默语义分叉，切链裁决器今天安全。两轨逐语法点的接受/拒绝对照见 uvp-eth `zhixu-dsl-grammar.md`（链轨册）§10 第 5 条。

### 2.5 外部世界

- 唯一入口：执行器自发信号（str / canonical signal），执行器是否、何时发送取决于其业务事务。
- externalSignals 删除；外部事实名契约归 swagger。
- 外部事实没有直连订阅的捷径：必须先经执行器变成某域的 canonical signal，才能被订阅或被引擎消费。

---

## 3. 铸单层（Mint）

| 通道 | 声明 | 机制 | 保证 |
|---|---|---|---|
| 执行器 str | 无需声明 | `new_source=true`，可携带 `parent_order_ids` 自报血缘；无需父 | 执行器私有的铸造判定（配对、挑选、自发开单） |
| 引擎 per-fact 代铸 | `mint: per-fact`（唯一声明点） | 每到达（扇入）事实，订单 ID 从事实纯函数派生（现 deriveOutsideOrderID 模式：域+阶段+订阅+上游引用），投递事务内 RegisterOrder 幂等重入 | **无需知情者的存在性**：投递失败只延迟，重放不漂移身份 |

- 代铸订单的溯源父从事实的关联订单引用复制（如 deal 携带买卖双方订单 id）。
- 一个事实最多铸一次单（按去重身份幂等）。
- 血缘边章唯一：一条订单血缘边至多一个构成性事实章（断言边的构成性由 `constituting_signal` 单章承载）；边章 first-win、落定后不可改写——后到的异目标构成性事实被响亮拒绝，不提供改写或补盖通道。

---

## 4. 语法表面（目标）

以下示例取自水果大亨演示（同一份 zhixu 定义）：示例中全部 `@target` 的
source 类（`fruit_merchant`/`buyer`/`farmer` 等）都由**同一 zhixu 内**的其他
stage 声明——"跨 source 类"不等于"跨秩序"；跨秩序事实级联只有委托 dock
一条通道（见 2.1/2.4）。

```yaml
# 出生阶段：每事实即铸（原分馏）
- name: entry
  source: customer
  mint: per-fact
  executor: { supplierType: organization, supplierID: journey-executor }
  receiveSignals:
    JOURNEY_START: "::ANCHOR(@fruit_merchant::stall_retail.retail.sold)"
  sendSignals: [str, cmp, err]

# 无锚监听：通道扇入（原撮合）
- name: exchange
  source: match
  executor: { supplierType: organization, supplierID: juice-market-executor }
  receiveSignals:
    SURPLUS_EVENT: "::ANCHOR(@fruit_merchant::stall_retail.retail.surplus)"
    DEMAND_EVENT: "::ANCHOR(@buyer::juice_demand.entry.requested)"
  sendSignals: [str, frozen, cmp, deal, err]

# 有锚阶段：按单路由（原收购回流；同 source 类存在 mint 声明即有锚）
- name: packing intake
  source: seller
  executor: { supplierType: organization, supplierID: fruit-merchant-executor }
  receiveSignals:
    FARMER_FRUIT_SETTLED: "::ANCHOR(@farmer::farmer_orchard.packing.settled)"
  sendSignals: [str, frozen, cmp, err]

# 同单推进：普通 hook（原语义）
- name: washing
  source: seller
  executor: { supplierType: organization, supplierID: farmer-executor }
  receiveSignals:
    WASH_READY: "seller::farmer_orchard.picking.cmp"
  sendSignals: [str, cmp, err]
```

Stage 字段总表（目标态）：

| 字段 | 状态 |
|---|---|
| `source` | 保留，升格为因果身份类（域内命名空间，多阶段共享） |
| `mint` | 新增，可选，仅 `per-fact`；由出生阶段声明，是该类铸单的唯一声明点 |
| `receiveSignals` | 保留 map 形态；值为普通 hook 或 ANCHOR 订阅 |
| `sendSignals` | 保留 |
| `executor` | 委托为 supplierType=zhixu + zhixuExecutorConfig{target(目标定义name|null), interface, order.mode∈{new,existing}, inputMap, signalMap→目标接口端口名；至少一映射，new 恰一条 input 绑定} |
| `trigger` | **删除**（原必填入口表） |
| `externalSignals` | **删除** |
| `fileResources`、`selectedStages` | 保留 |

---

## 5. 六场景对照（旧 → 新）

| 场景 | 旧写法 | 新写法 |
|---|---|---|
| 分馏（汽油/顾客路线） | `::OUTSIDE@(源::t.s.sig)` | 订阅 + `mint: per-fact` |
| 撮合（k≥2 配对） | 旧扇入标头（k≥2） | 无锚监听 + 多条 `::ANCHOR(@…)`；配对后执行器 str 多父 |
| 收购回流 | `::ANCHOR@(裸三段)` | 有锚阶段 + `::ANCHOR(@…)`（按单路由） |
| 观察入口（k=1） | 旧扇入标头（k=1） | 无锚监听 + 单条 `::ANCHOR(@…)` |
| 交易所开门/关门 | 无（外部 trigger + 载体单） | match source 上一个发开门/关门事实的 stage，通道锚定 |
| 委托 | 目标 dockInterface 端口 + inputMap/signalMap，独立子订单 | 支持：具名接口 + order.mode（new 建子单/existing 接既有单），inputMap/signalMap 对译目标接口端口 |

---

## 6. 回放口径

- 回放基线：全部事实（订单锚定 + 通道锚定）+ 不可变定义 + 对接记录。
- 代铸订单：事实重放 → 纯函数派生 ID → 同一订单集合，不漂移。
- hook 判决（普通表达式）：维持现有 hook_state 语义层（init/wait/ready/cxl，终态不可变）；订阅通道不经判决层，事实→路由→投递直通。
- 投递层（重试/退避/dead/复活）机制原样，适用于订阅通道投递。

## 7. 版本与兼容

- 版本口径：协议制品统一为 `uvp.<artifact>.v<N>` 点号风格，语义/AST/语料/部署清单为 v1；云执行产物为结构化复合身份信封，使用 `uvp.cloudArtifact.v2`（Go/Rust/部署矩阵必须一致）。即：`uvp.semantic.v1`、`uvp.cloudAst.v1`、`uvp.hookSemanticsCorpus.v1`（语料文件 semantics.v1.json）、`uvp.cloudArtifact.v2`；部署清单 `uvp-eth.addresses.v1`。不存在 0.7/v2/v5 编号制品，无兼容义务。
- 兼容矩阵 `uvp-stack.v1.json` 是当前版本真相：它钉住 `hookPlan.v2`、`onchainHookPlan.v3`、`cloudArtifact.v2`、dock 制品（`uvp.dockInterfaceArtifact.v2`、`uvp.dockRoute.v2`、`uvp.dock.resolution.v2`、`uvp.dockRoute.unresolved.v1`——后者以 `dockRouteUnresolved` 键进入矩阵 artifactSchemas，与 uvp-core `DOCK_ROUTE_UNRESOLVED_SCHEMA_VERSION`、Go `DockRouteUnresolvedSchemaVersion` 常量同值互认）、合约 ABI fixture、EIP-712 domains 和 `uvp-eth.addresses.v1`。
- `UVPStateMachine` 0.11 的六域 PlanCommit（含 capabilitiesRoot）、`SignalSubmitted`/`HookReady` 事件与 `(planId, orderId)` 复合键，以及 `UVPDockingModule` 4.3 的 open/input/output boundary（output 端口叶为 V3：叶直接钉绑定侧的 targetSourceId/targetSignalId 事实键分量）必须由 bindings、bootstrap、indexer、replay 和共享 fixture 一起消费。
- OUTSIDE/ANCHOR 标头、OUTSOURCE、trigger 入口表、externalSignals 不在语法面、两侧语料与两侧文档表述内。其中 trigger 入口表与 externalSignals 零存在；`::OUTSIDE@` / `::ANCHOR@` 标头与 `OUTSOURCE` 由解析器词法识别并精确拒绝，报错统一指引 `::ANCHOR(@source::task.stage.signal)` 订阅入口，扇入类标头按通用语法错误拒绝（语法面排除的完整口径见 2.2）。

## 8. 决策记录

| 决策 | 结论 | 依据 |
|---|---|---|
| 三种接收方是否可变 | 编译期定死、不可变（选项 A） | 路由语义单一可静态验证；生殖是入口行为不是流程行为；同单生殖走执行器 str |
| 铸单标记命名 | `mint: per-fact` | 语义是铸单策略，不是入口激活 |
| 链侧切换 | 一次到位，删四典型关键字 | 四类在业务模板与链上近零使用；干净切分优于并存 |
| 血缘闸门 | 不再是过滤开关，而是域内路由规则 + 域作用域本身 | "只有我的农户"由按单路由与 zhixu 局部命名空间免费获得 |
| 锚定依据 | mint 声明是编译期唯一锚定依据 | 自发 str 编译期不可见；订阅方按溯源分拣是执行器责任 |
| 孤儿 | 概念删除 | 订单天然存在，无 dock = 尚无关系，非异常态 |
| 有锚订阅阶段绑定 zhixu 委托执行者 | 合法形态（现行例外口径）：编译放行，属订阅-铸单模型的许可形态 | 文法 §7 第 6 条——有锚订阅阶段（本 source 类存在 mint 声明，订阅 route=order 按单投递、委托信封可携带订单锚定）是 zhixu 委托执行者的唯一许可宿主；无锚扇入订阅 + zhixu 委托仍拒绝（uvp-core validate_subscription_delegation，UVP-01） |
| 版本 slate 与 dock v2 冻结 | 语义/AST/语料保持 v1；HookPlan、CloudArtifact 为 v2，OnchainHookPlan 为 v3，dock 制品为 `uvp.dockInterfaceArtifact.v2`/`uvp.dockRoute.v2`/`uvp.dock.resolution.v2`；合约 ABI/EIP-712 以 `uvp-stack.v1.json` 和 fixtures 的 0.11/4.3 等值为准 | 结构化 dock identity、六域 PlanCommit 和复合订单键已进入 wire；任何一侧继续消费旧 v1/v0.8 fixture 都会造成跨轨漂移 |
| 出生通道键并集的 mint∪mint 臂（分叉登记，非决策） | 三轨未收敛，按各侧行为如实登记：uvp-core（Rust）编译期拒绝——一事实扇出多条 mint 出生线时各铸一单，"该事实对应哪个订单"三线发散，"一事一单"在编译期收口（uvp-core `validate_birth_channel_key_uniqueness`，三臂全拒）；合约注册门与 TS 编译器放行——一事实扇出多条 mint 出生线是产品现行形态（customs 基准 plan：`order::registered` 同时出生执行者选择与资源发布两阶段），同一 mint 出生上下文内物化、不产生幻影阶段（合约 `UVPPlanRegistration._registerPlanHook` 按 dock 标志分界，TS 按 orderTriggerKind=dock 分界）。mint∪dock / dock∪dock 两臂三轨一致拒绝（合约 `DuplicateBirthChannelKey`、TS 镜像、本仓编译期） | 与注册表 uvp-constraints.v1.json 规则 `birth-channel-key-union-uniqueness` 的 ruling 同口径；mint∪mint 的收敛方向是裁决项，本规格不替产品预设 |

### 补充决策（2026-08-31，安全架构审查后）

| 决策 | 结论 | 依据 |
|---|---|---|
| 链下身份归属 | 身份、资质、审核归**秩序商店**（商店语境可称"平台"）；UVP DSL 只承载几何拓扑 + 事实纪律；链下身份与链上身份在商店汇合 | 担保交易秩序本身可上架商店供挑选；引擎不新增身份系统 |
| 裸跑形态 | 产品接受 UVP 裸跑：即使黑产裸跑，每个环节谁是谁靠事实留痕可审计；合法生意（如出口小汽车）走官方秩序商店获得受控管理 | 引擎层不强设身份门；留痕 + 凭据分层是信任模型 |
| /signal 凭据 | per-sender 密钥表已落地（与部署级共享密钥是二选一的显式模式，无回落：per-sender 模式下未建档 sender 一律拒绝）；验签主体归真注入信封（自报与主体不符即 401）；裸跑形态启动响亮告警、保留自报 sender 留痕 | 一把共享密钥泄露即全线；归真后记录里的"谁发的"来自凭据 |
| 血缘边构成者 | rel_order_order 增加 constituted_by（asserted/mint）+ constituent_sender；触发器代写断言边留信封 sender，mint 边标引擎 | 裸跑下断言边只留痕不设门；门语义收紧（构成权收回父侧 + 事实核对）待商店主体体系落地 |
| patch selector | applyStageExecutorUpdate 校验 selector 阶段存在且 selectedStages 覆盖目标；主体对 selector 的控制权归属校验挂起 | 现网主链路（平台）不传 selector 不受影响；挂起项与商店主体词汇表一并设计 |
| chain-services 自报头 | x-uvp-* 自报头永不作为权威；治理/运维/evidence 入口的身份由商店前置签发 | 自报 header 即得管理员 + 服务持 registry owner key 真实上链是对抗路确认的边界缺口；随商店落地整改 |

### 补充决策（2026-08-31 第二批，用户裁决落地）

| 决策 | 结论 | 依据 |
|---|---|---|
| 血缘门收紧（已落地） | rel_order_order 增加 constituting_signal：断言边只有被本钩子订阅的那条事实亲手盖章才构成投递依据（门核对 constituting_signal=订阅目标）；mint 边免检。伪造必须把关联声明塞进伪造事实信封本身 | 构成权收回父侧的引擎侧可实现形态；executor 主体归属等商店词汇表 |
| patch 主体归属（定案） | 统一 selectedStages 口径：多个阶段可 select 同一目标阶段；selector 校验按"提供即校验存在且覆盖目标"执行。RBAC（谁能写谁）暂缓，待商店主体体系一并设计 | 现阶段唯一 patch 主链路是平台；提前建 RBAC 过度设计 |
| chain-services 自报头（定案） | 缺口已登记，暂不整改；x-uvp-* 永不作为权威的口径不变，整改随秩序商店集成落地 | 当前无对外暴露面；商店落地时统一收口 |

### 裁决落地（2026-09-01，用户逐项拍板）

| 裁决 | 结论 | 落地 |
|---|---|---|
| 模-1 静态执行者 | 出生/订阅阶段必须编译期静态绑定执行者（出生阶段必须非委托 executor；有锚订阅阶段允许绑定 zhixu 委托的唯一例外见上表"有锚订阅阶段绑定 zhixu 委托执行者"）；运行时 patch 一律拒绝（既有门禁不变） | Go validator 去豁免（uvp f724212 之后批次）；uvp-core validate_mint_anchors 增查；bootstrap child.main 前置注册静态执行者、register_select 撤销对该阶段的 patch |
| 模-2 出生入口组成 | 出生入口只能是 ANCHOR 订阅；"订阅之外附加单正普通 hook"形态废除 | Go validateMintStages + zhixu_schema、uvp-core validate_mint_anchors 三处拒绝；TS 测试对齐 |
| 模-3 域边界 | 域 = zhixu 实例。订阅按类匹配只在本实例内解析；跨秩序扇入要求 rel_zhixu_dock 显式对接（双向记录，compiler 新增 POST /zhixu-dock 登记，dbops.RegisterZhixuDock）。依赖按秩序 id 显式绑定（委托接缝/同单锚定）不受 dock 门限制 | uvp core-ddl + loadAffectedHooks + 契约测试 |
| 事实标签 tie-break | hook_state.id 与 hook_delivery.id 改从共享序列 fact_label_seq 取值，标签对全部输出事实严格全序 | core-ddl |
| nonce 防重放 | HMAC 入口的 nonce 查重已落地（`(senderID, nonce)` 原子 check-and-record，TTL 缓存、进程内单实例——多副本需共享存储）；升级 JWT 随商店身份落地一并做。未开 HMAC 的入口退化为 first-win 幂等吸收 | 决策记录 |
| DLQ 通知可靠性 | 告警语义走指标（stmDLQTotal 告警规则），持久化重投等运维真消费 DLQ 时再建 | 决策记录 |
| chain-services 暴露面 | 模-5 修正"暂不整改"的前提：CORS 默认关闭（UVP_API_CORS_ALLOWED_ORIGINS 白名单回显）；notification-profile 挂 store.supplier.notification_profile.update；管理员白名单（GOVERNANCE_ADMIN_REVIEWER_IDS）真接入鉴权。身份归商店的裁决不变 | chain-services 本批次 |
| 合约解冻批次（窗口已开） | #1 派生信号 capability 对称：跨订单派生要求目标（origin）订单 plan 声明同一 capability；#31 同 hook 输入内 dependencyKeys 去重；#30 README 口径："patch 即时接管、不可回滚恢复执行者"。#10 (planId, orderId) 复合键涉及全部模块/periphery 的订单寻址迁移，作为解冻窗口的下一个独立批次 | contracts 本批次 + forge 86/86 |

#10 残余风险说明：capability 对称后，攻击者理论上仍可镜像目标 plan 的 capability 声明（plan 公开可读）；该残余与 #10 的订单寻址迁移一并在解冻窗口下一批次处置（选项：origin 侧 link 授权）。

模-3 域边界张力说明（待裁决，如实披露）：`rel_zhixu_dock` 门目前只在云侧投递路径落地（core-ddl + `loadAffectedHooks` + 契约测试，登记入口 `POST /zhixu-dock`）；该门在链轨/uvp-core 编译边界是否同步强制（或明确不强制）、以及 `rel_zhixu_dock` 登记与委托 dock（2.4）两条通道的职责分界，均尚无统一口径——裁决落定前，本规格不替任何一侧预设强制语义。

### 裁决落地（2026-09-01，商店=框架不=内容）

| 裁决 | 结论 | 落地 |
|---|---|---|
| 商店=框架，不=内容 | 商店（zhixu-store）类比 Shopify 只提供框架：任务字段集、证据要求、提交流程由**凝结核**（zhixu 的发布者/所有者）自己配置，作为**数据**随 zhixu 带进来；商店核心代码不得出现任何具体业务的字段名、中文标签匹配表或文件格式特判，也不内置任何具体业务的示例 | protocol 新增 `ProductTaskDTO.evidenceSpec` 加性可选字段（`{key, label, inputKind?, accept?, required?, description?}`，schema 保持 `store-product-schema.v1`，即 protocol `ProductTaskDTO` 的 `StoreProductSchemaVersion` 字面量）；store workbench 按 schema 驱动渲染，spec 缺失时降级为通用上传槽位（文件+可选文本说明），未知声明不上传前拒绝、也不静默丢弃 |
| 报关特例只作演示配置 | 共享 demo 任务里的"报关单 PDF、报关单号、出口港口、完成时间"等特例内容只存在于一份显式的演示配置数据（形态上等同"某凝结核自带配置"），只经通用渲染路径生效；商店核心代码 grep 不到这些业务字符串（演示配置文件与其测试除外）。MVP 不内置报关示例 | store `src/product/demo/customs-demo-config.ts`；protocol fixture `demoCustomsEvidenceSpec` 同形示例 |
| 证据文件格式校验归属 | accept 约束来自凝结核配置（`spec.accept`）；前端按 accept 校验并在 accept=pdf 时读取文件首字节做 %PDF- 快速拦截（防伪造 MIME/扩展名），服务端魔数校验仍是权威 | store workbenchSupport `validateEvidenceFileForSlot` |
| DTO 兼容口径 | `evidenceSpec` 为加性可选字段：消费方在字段缺失时必须走降级路径而不是报错。任务 DTO 单轨携带证据契约：不设 `requiredEvidence`，缺失 spec 即无凭证槽位（不臆造通用槽位、不报错） | protocol freeze 校验（product signal map gate + verify-stack-compatibility）exit 0 |
