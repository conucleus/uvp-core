# Zhixu DSL 语言手册（语言层正本）

> **三层文档契约**：
> - **白皮书**（`uvp-eth` 仓 `whitepaper.md`）= 愿景与用法——为什么这样设计、四类跨行业形态（分馏/撮合/委托/收购）应该怎么用。
> - **本册（语言层正本）** = `uvp-core` 的镜像——DSL 的语法、语义与编译裁决，全部对应 uvp-core（Rust）的解析器/求值器/linker；本册只写 uvp-core 裁决的东西（onchain 产物面的附加校验若由 core validate 执行，留在本册并标注 target 条件）。本册是**单一正本**：云轨（`uvp` 仓）与链轨（`uvp-eth` 仓）共享，两轨根下同名 `zhixu-dsl-grammar.md` 只是薄扉页。
> - **实现层** = 各轨代码文档——云侧组件/表/API/运行期机制见 `uvp` 仓 `uvp-cloud.md`；链侧工程边界与链上编译/注册/运行期闸见 `uvp-eth` 根 `uvp-chain.md`（含附录 B 两轨接受对照矩阵）。
>
> 判据：一句话问"这是 uvp-core 的裁决，还是某轨运行时的动作？"——前者在本册，后者在实现层；"应该怎么用"的论述在白皮书。

本册按 `uvp-core` 当前代码边界整理，以以下 crate 为准：

- `uvp-core/crates/uvp-model/src/lib.rs`（定义模型与 strict 形状）与 `uvp-core/crates/uvp-compiler/src/dock.rs`（dock source/linker 权威）
- `uvp-core/crates/uvp-compiler/src/validate/`（定义级校验权威门，随 Rust compile 裁决；onchain 产物线的阶段物化门在同模块；sendSignals/validWhen 的适格门 D026-D032 在同 crate 的 `artifact/mod.rs`）
- `uvp-core/crates/uvp-hook-dsl`（§5 表达式语法/语义/求值的唯一解析器与求值器）
- `uvp-core/crates/uvp-ir`（§2.1 定义 uid 派生单源）

跨秩序协作的权威规格是 uvp-core `docs/specs/subscription-mint-spec.md`，本册给语言层结论；两轨运行期结论分别见 `uvp-cloud.md`（云轨）与 `uvp-chain.md`（链轨——附录 B 是两轨逐语法点接受对照的唯一出处）。

---

## 0. 版本、范围与阅读路径

- 覆盖对象：`kind: Zhixu`。云轨另有运行时注册对象 `kind: Supplier`（形态与机制见 `uvp-cloud.md`）；链上轨道没有 `Supplier` 顶层对象——供应商 profile、handler、能力标签、审核状态都是 Store 链下数据（见 `uvp-chain.md`）。
- 编译器注册类型为：`apiVersion: uvp/v0` + `kind: Zhixu`（大小写敏感）。
- dock 声明内嵌在 `spec.dockInterface` 与 stage executor 里，没有独立 schemaVersion 键，也不存在 `apiVersion: uvp.dock.*` 写法；未知字段按响亮拒绝（uvp-core `deny_unknown_fields` 口径，各轨提交入口的枚举闭集见实现层文档）。
- 证据正文不进语言层裁决；各轨存储边界（链上只应保存状态、哈希、metadata URI、签名、权限和事件）见实现层文档。
- 示例来源是冻结的中性示例域（白皮书四原型 + 水果加工演示域）；产品层词表不进入本册。

按任务的阅读路径：

| 你要做什么 | 读 |
|---|---|
| 写第一个定义 | §1 → §3 → §4 → §5 → §8.1 |
| 跨秩序协作 / 跨源订阅 | §1 三轴表 → §6 / §7 → §8 |
| 定义被编译器拒绝 | 出错字段所在章（§4/§5/§7）的约束行 |
| 计划要上链 / 两轨行为对照 | 链轨实现层 `uvp-chain.md`（阶段物化门详述、出生提交通道、链上编译规则、附录 B 对照矩阵） |
| 投递没发生 / 运行期解析 | `uvp-cloud.md`（云轨时序红线与运行期执行者解析） |

---

# Ⅰ 模型

## 1. 核心模型与三轴地图

五个概念：

- **秩序（Zhixu）**：一份可复用的定义文档，声明一组阶段与它们的协作规则；内容派生身份（§2）。
- **阶段（stage）**：秩序内一台完备的状态机，`{stage}` 单段寻址、秩序内全局唯一。
- **信号事实（signal fact）**：已经发生、不可改写的事件；分订单锚定与通道锚定两种形态（§6.1），接收方按去重身份与事实标签吸收（各轨事实载体与标签形态见实现层文档）。
- **执行者（executor）**：承接某阶段投递的主体——在某次订单、某个 stage 上实际被绑定、被授权并提交 signal 的主体（各轨主体形态——注册 supplier / 授权钱包——见实现层文档）；或另一条秩序（委托）。
- **订单（order）**：定义的一次执行轨迹；订单号一律由出生事实纯函数派生，调用方不可自报（§6.3/§6.4）。

跨 zhixu 协作由三个正交问题构成，每个问题有专属机制——**这张表是全册的地图**：

| 问题 | 机制 | 权威章 |
|---|---|---|
| 事实怎么流到我的订单上 | **同域跨源订阅** `::ANCHOR(@…)`（逐事件投递；投递路由是各轨运行时语义，见实现层文档）；**跨秩序没有订阅直连**，事实经委托 dock 的端口对译注入与回传 | §6、§7 |
| 新订单从哪里来 | 订单创建承诺的通道：执行器 `str` 自报、出生阶段 `mint: per-fact` 代铸（§6.3）、dock `order.mode=new` 铸子单（§7）；订单号一律从出生事实纯函数派生、调用方不可自报——各轨出生通道的运行期全集见实现层文档 | §6.3、§7.5 |
| 阶段的执行者是谁 | 定义静态 `supplierID`；定义级 zhixu 委托（静态 `target` 或 `target:null` 动态选择——挂接建立期选定并终身钉住）；订单级执行者指派/交接不承载委托绑定（§7.4/§7.5，各轨 patch 面见实现层文档） | §4、§7.3 |

两条边界一句话（全册唯一表述，各处只留指针）：

- **"跨 source 类" ≠ "跨秩序"**：订阅寻址只在本域解析；跨秩序事实级联只有委托 dock 一条通道。
- **外部事实的唯一入口是执行者自发 signal**（链上 = 授权钱包按 EIP-712 提交的 signal）；任何声明本身都不会发送 signal，系统不主动向外部收集事实。

四类跨行业形态（分馏/撮合/委托/收购）与机制的对应叙述见白皮书 §2.2；本册 §6.1 只留机制侧的形态表。

## 2. 身份与不变量

### 2.1 内容派生身份（uid）

定义身份是内容派生 uid `zx-<32hex>`——preimage 为定义文档 canonical JSON 剔除展示字段（`metadata.name`、`metadata.annotations`）后的形态，`keccak256("uvp:definition-uid:v2:" + canonical)` 取摘要前 32 hex；`metadata.labels` 是内容，参与派生。同一内容跨环境、跨轨得到同一 uid：重复发布是幂等 no-op，内容一变即新身份——不存在「重发布一个定义」的概念。唯一限定（YAML 裸标量）：「同一内容」以装载器解析后的结果为准——裸标量（`010`、`Y`/`yes`/`on`、`0b101`、`1_000` 一类）在 Rust / TS / Go 三轨装载器下解析结果真分歧（`uvp-core` `fixtures/yaml/yaml-scalar-parity.v1.json` 语料逐格钉住），同一 YAML 文件跨轨可派生出不同 uid；值位书写一律用带引号字符串是作者纪律（对照行见 `uvp-chain.md` 附录 B）。DSL 壳不携带任何身份——`metadata.uid` 一旦出现即按未知字段响亮拒绝（uvp-core `deny_unknown_fields` 口径），uid 恒由发布方按内容派生后写入，作者不手写。权威实现是 Rust `uvp_ir::derive_definition_uid`（domain 常量 `uvp:definition-uid:v2`）；链轨 TS 编译器同公式、共享金向量互钉（见 `uvp-chain.md`）。日志与错误消息按 `name(uid前8hex)` 渲染。云轨运行时零哈希，机器面寻址键即该 uid；跨秩序引用一律按 uid 精确指向（§7.3）。

### 2.2 事实不可改写与 first-win

事实不是布尔值：布尔可以在 true/false 之间变化，已经发生的信号事实不能通过改写字段重新变成"没有发生"。同一事实的重复外部提交按 first-win 幂等吸收（不重复推进、不再过发射适格筛）——引擎内部写入口自带存在性预检，冲突即数据流事故；互斥事实按接收时序裁决——先成立者成为事实，后到者留在审计记录里但不再激活已判负的分支。表达式描述的是时间线上事实是否存在，不是可被后写值覆盖的 true/false。

### 2.3 阶段种类不可变

阶段种类（出生/有锚/无锚监听，判定见 §6.2）由 `mint` 与订阅声明在编译期定死，终生不可变；订阅/出生阶段的 `executor_configs` 终生禁 patch。订单创建后事实不可改写，终态一经落定不被后续事实改写——运行侧终态与资金不变量的轨道形态见实现层文档。

---

# Ⅱ 语言

## 3. 顶层结构

`Zhixu` 顶层结构：

```yaml
apiVersion: uvp/v0
kind: Zhixu
metadata:
  ...
spec:
  platform: {...}
  nucleation: {...}
  stages:
    - name: ...
  dockInterface: {...}   # 可选：目标侧发布的具名接口（§7.2）
```

| 路径 | 类型 | 必填 | 说明 |
|---|---|---|---|
| `apiVersion` | `string` | 是 | `uvp/v0`。 |
| `kind` | `string` | 是 | 必须是 `Zhixu`（大小写敏感）。 |
| `metadata` | `object` | 是 | 对象元数据。 |
| `spec` | `object` | 是 | 秩序规格定义。 |

### 3.1 metadata 与身份分治

| 路径 | 类型 | 必填 | 说明 |
|---|---|---|---|
| `metadata.name` | `string` | 是 | 定义的技术名，slug 形态 `^[a-z][a-z0-9_-]{0,99}$`（字节 ≤100）。纯展示属性：不是身份、不是寻址键——`zhixuExecutorConfig.target.zhixu` 与 resolution manifest 都按内容派生 uid 寻址。 |
| `metadata.labels` | `map[string]string` | 否 | 标签，是定义内容的一部分，参与身份派生（两轨同公式，§2.1）。 |
| `metadata.annotations` | `map[string]string` | 否 | 自由注解，永不参与任何身份/哈希推导（`version` 键无特殊待遇）。 |

身份派生规则见 §2.1。

### 3.2 spec 基础字段

**`spec.platform`**——声明该定义面向的部署轨道（`cloud` / `blockchain` / `cbdc`）：它是轨道声明，不是分层主张。`cbdc` 为预留枚举值：零消费、无行为，取值不参与任何裁决。校验口径双层：语义权威（Rust uvp-core）只要求非空字符串；各轨提交入口另有枚举闭集（见实现层文档）。

| 路径 | 类型 | 必填 | 说明 |
|---|---|---|---|
| `spec.platform.type` | `string` | 是 | 轨道声明（`cloud` / `blockchain` / `cbdc`；轨道皮肤值见实现层文档）。 |
| `spec.platform.provider` | `string` | 否 | 提供者标签，无行为语义（轨道皮肤值见实现层文档）。 |
| `spec.platform.network` | `string` | 否 | 网络/运行域标签；预留、无行为语义——取值进入定义 uid 派生（§2.1）与各轨产物承诺，改名即身份分叉、无语义变化。 |
| `spec.platform.version` / `.params` | `string` / `map` | 否 | 平台版本/扩展参数。 |

**`spec.nucleation`**：`id`（必填，凝结核主体标识）+ `params`（可选）。凝结核字段是必填的形式面：除参与定义身份派生（§2.1）外无任何协议语义——预留、无行为。

**`spec.stages`**：

| 路径 | 类型 | 必填 | 约束 |
|---|---|---|---|
| `spec.stages` | `array` | 是 | 至少 1 个。 |
| `spec.stages[].name` | `string` | 是 | 秩序内全局不可重复。 |

**`spec.dockInterface`**（可选）：目标侧对外发布的具名接口 map，完整语义见 §7.2。

---

## 4. 阶段

编译期尺寸闸：每定义 `stages` ≤ 256、`receiveSignals` 通道全定义合计 ≤ 512（各 stage 通道数求和过同一道闸，非每 stage 各有 512 预算；报错按全定义总数计）；超限编译拒绝。

| 路径 | 类型 | 必填 | 说明/约束 |
|---|---|---|---|
| `name` | `string` | 是 | 秩序内不可重复。 |
| `source` | `string` | 是 | 该阶段信号源标识（因果身份类，路由语义见 §6.1）。上限 36 字节、plain identifier——与 hook 标头 source、订阅目标 `@source` 类同限。 |
| `mint` | `string` | 否 | 阶段级 per-fact 出生声明（仅 `per-fact`）：出生阶段的订阅事实逐条成为新订单的出生事实，订单号从出生事实纯函数派生、幂等可重放（§6.4；各轨出生提交通道见实现层文档）。声明后阶段种类终生不可变（§2.3）。 |
| `executor` | `object` | 否 | 执行者配置：`supplierType`/`supplierID`/`selectableResource` 见下表；`supplierType=zhixu` 的委托声明（`zhixuExecutorConfig`）见 §7.3。 |
| `selectedStages` | `[]string` | 否 | 目标阶段引用列表，格式 `{stage}`，每项必须引用真实存在的阶段——当前阶段的执行者可代这些阶段做执行者选择/绑定（selector 语义）。 |
| `sendSignals` | `[]object` | 否 | 本阶段发送信号声明，条目 `{name, validWhen?}` 键闭集（闭集外键 loader 拒绝）。`name` 必填非空——裸局部名或 `stage.signal` 两段式自指全名，编译期归一为裸局部名，归一后不可重复（重复 capability D031）。`validWhen` 可选：`[]string` 发射适格声明——每项是完整的 `{source}::{condition}`、全体项 header source 必须一致（D032），缺省即无条件发射（空数组/空项拒绝，D027）；列表语义见 §5.6。含 `::` 的形态非法。 |
| `receiveSignals` | `map[string]string` | 否 | key 是通道名称（不可含 `.` 或 `#`、不含任何空白字符，长度 1-36 字节）；value 是 Hook DSL（§5）：普通同单 hook 或跨源订阅（§6）。投递的运行期语义见实现层文档（云轨 `uvp-cloud.md` §7）。 |
| `fileResources` | `map[string]FileResource` | 否 | 文件资源句柄映射（字段形态见附录 A）；句柄承载内容哈希、metadata URI 或短标识一类的引用，存储/投影的轨道边界见实现层文档。 |

**executor 字段**（`supplierType != zhixu` 时）：

| 路径 | 类型 | 必填 | 说明 |
|---|---|---|---|
| `executor.supplierType` | `string` | 是 | 枚举：`individual` / `organization` / `zhixu`。 |
| `executor.supplierID` | `string` | 是（individual/organization） | 只要给出 executor 块且 supplierType 为二者之一即无条件必填，缺省或空白编译期硬拒绝，长度上限 36 字节；`selectedStages` 锚定不豁免存在性（缺 supplierID 的执行器即使被锚定也是"看似绑定"，产物里没有投递目标）。`supplierType=zhixu` 时禁止。 |
| `executor.selectableResource` | `map[string]FileResource` | 否 | 阶段对供应商可选资源描述。 |

**执行者拓扑约束**（uvp-core 裁决）：

- 阶段完全不给出 `executor` 块时，必须可被其他阶段通过 `selectedStages` 覆盖到，否则编译拒绝（"没有分配 executor，也没有被 selectedStages 引用"）。覆盖解决的是"无 executor 块"的可达性，不豁免 supplierID 存在性。
- 订阅/出生阶段必须编译期静态指定执行者，按阶段种类细分见 §6.2；出生阶段（`mint: per-fact`）只能显式 `supplierID`，与 zhixu 委托互斥。
- ⚠ **阶段物化门（onchain 产物面，core 裁决、带 target 条件）**：uvp-core 在 onchain（hookPlan）产物线执行该闸——链上阶段只能由本阶段 order-trigger（mint/dock）或 EMIT_READY hook 物化，每个阶段都必须编译出至少一个带物化位的 hook：零 `receiveSignals`（仅 `sendSignals`）的阶段（编译为零 hook、永不可物化、无恢复路径），以及「无静态执行者、仅被 `selectedStages` 覆盖却声明 `receiveSignals`」的阶段（hook 一律编译为 flags=0 纯 watcher，形态一旦上链即死锁且无恢复路径）一律编译期整体拒绝；cloud 产物面免检。闸的完整叙述（EMIT_READY 编译形态、`submitSignal` 恒 revert `UnknownHook` 等链上后果）见 `uvp-chain.md`。
- ⚠ 当前代码不存在 `stage.type`、`selectionStages`、单数形 `selectedStage`/`signals`/`receivedSignals` 字段名——表达"谁有权指定别的阶段执行者"只看 `selectedStages`。
- ⚠ 对会创建新订单的阶段，订单创建承诺只有两条声明通道：执行器 `str` 与 `mint: per-fact`（§6.3）；不要把订单创建语义放在普通业务 signal 上，订单号不可自报（§1）。

onchain 实现层的附加接受域见 `uvp-chain.md`（uvp-eth 根）：

- 同一 signalKey 只允许单一阶段的非 trigger hook 观察——首个观察者钉死所属阶段，跨阶段双重观察整体拒绝（TS 预检 + 合约双闸）；
- 全计划去重后的依赖键（signalKey）总数 ≤ 1024（`commitPlan` 同口径 revert `TooManyDependencies`）；
- 上述闸的动机、规避形态与更多链上编译/注册期闸已随原文迁至该文档。

---

## 5. 表达式

本章是表达式语言的语言层正本：语法、语义约束、延时深语义、否决位，以及两个消费位的差异。字段叙事在 §4，本章不复述。

### 5.1 基本格式与文法

统一格式 `{source}::{condition}`；`source` 可以为空，但仅允许订阅 `::ANCHOR(@source::stage.signal)` 使用空标头；`condition` 不能为空。原子条件两种：内部信号 `stage.signal`（同单推进），跨源订阅 `::ANCHOR(@…)`（逐事件投递，语义见 §6）。

条件表达式的形式文法（EBNF 语法骨架——两轨共用同一 uvp-core 解析器与同一语义语料，本节即唯一出处）：

```text
condition    := orExpr
orExpr       := andExpr ("|" andExpr)*
andExpr      := unaryExpr ("&" unaryExpr)*
unaryExpr    := "~" unaryExpr | postfixExpr
postfixExpr  := primary ("+" duration)?
primary      := signal | "(" condition ")" | subscriptionHook
duration     := positiveInteger ("s" | "m" | "h" | "d")
```

操作符：`&` 与、`|` 或、`~` 非、`+<正整数><单位>` 延时后缀（单位小写 `s/m/h/d`）。`+` 是后缀运算符，时间字面量随表达式编译进 AST；运行时以编译结果为准，不重新解析原文。链上编译为 compact instructions（`SIGNAL` / `NOT` / `AND` / `OR` / `DELAY`），`DELAY` 由状态机的显式 `pokeTimer`（keeper 动作）补时重算（链上消费形态见 `uvp-chain.md`）。

**混合运算符优先级（由紧到松）**：`+<时长>` > `~` > `&` > `|`。无括号时唯一解析：`A +5s & ~B | C` → `((A +5s) & (~B)) | C`——延时只绑定紧邻的原子或括号表达式，`&` 先结合，`|` 最后结合。

### 5.2 语义约束

- `~` 只表达"在正向信号到达/求值时刻之前仍未出现"；操作数必须是裸信号引用（组合子式、订阅都不允许；单独 `~A` 无意义直接拒绝）。需要"组合未发生"语义时，把各正向延时条件与逐信号否定条件并列书写。延时表达式作为操作数的唯一例外是否定位延时负门（§5.5）。
- `|` 的每个分支都必须包含至少一个正向信号锚点：`~A | B`、`~A | ~B` 都被拒绝。
- 延时必须大于零、无前导零/小数/负数、单位只能 `s/m/h/d`；换算为秒溢出或超过 30 天上限（`2592000s`）编译期拒绝。
- 解析嵌套深度上限 120（Rust `uvp-core` 的 `MAX_PARSE_DEPTH`；深层括号或连续 `~` 会打满解析栈）。
- 求值期 `anchor + durationSeconds` 溢出（含历史持久化的超限定义）返回求值错误，进入有界失败路径，不允许 panic 跨 FFI 边界 abort 进程。
- 延时只绑定它前面的原子或括号表达式：`A +14d & ~B` 只延时 `A`；要延时整个组合写 `(A & B) +14d`。
- 延时操作数必须包含至少一个正向信号锚点；只有否定条件不能作为计时起点。

**订阅原子的语法规则**（语义见 §6）：订阅必须是完整的 hook 条件——不能与 `&`/`|`/`~`/延时混用，不能带 source 标头；目标必须是 `@source::stage.signal` 两段式，且 source 类与目标都必须在本定义内真实存在。"事件 + 条件"的过滤需求用双阶段建模（§8.3）。

**普通同单 hook 的 header source 语义**：header source 类必须是本 zhixu 内声明的 source 类，且编译期硬校验它必须等于条件中引用的每个目标 stage 的 source（不一致直接拒绝）。它不要求等于接收阶段自身的 source——同单 hook 在订阅方自己的订单上下文内求值，订单对该 zhixu 的全部阶段可见。因此"普通 hook header ≠ 接收阶段自身 source"合法，但语义是**在本订单上等一条跨源信号**，不是"别的订单上每发生一次就通知我"——后者（逐事实通知）必须写订阅。把两者写混编译器不拦：hook 会在自己的订单上下文里永远等不到落在别的订单上的事实。

### 5.3 示例（有效/无效）

有效：

```text
buyer::main.cmp
buyer::main.cmp & ~refund.cmp
buyer::(pay.cmp +5s) & ~refund.cmp
buyer::(pay.cmp | ship.cmp) +5s
buyer::ship.cmp & ~(cancel.cmp +14d)
seller::((quality.flagged +3d) & ~picking.suspended & packing.settled)
::ANCHOR(@fruit_merchant::retail.sold)
::ANCHOR(@buyer::juice_demand.requested)
```

无效：

```text
buyer:main.cmp                          # 少了 "::"
::main.cmp                              # 空标头只允许订阅
buyer::                                 # condition 为空
buyer::main.cmp && settle.cmp           # "&&" 非支持操作符
buyer::~main.cmp                        # 单独的 "~A" 无意义
buyer::main.cmp +0s                     # 延时必须大于零
buyer::+01h                             # 不接受前导零
buyer::main.cmp +1w                     # 单位只能是 s/m/h/d
buyer::main.cmp +1.5h                   # 不接受小数
buyer::main.cmp +5s +10s                # 连写按语法错误拒绝（§5.4）
buyer::b.cmp & ~((a.cmp +5s) +10s)      # 否决位操作数内禁止嵌套延时
buyer::ANCHOR(@seller::main.cmp) & other.cmp   # 订阅必须是完整条件
buyer::ANCHOR(@seller::main.cmp)        # 订阅必须空标头
::ANCHOR(seller::main.cmp)              # 目标必须 @source::stage.signal
::ANCHOR(@seller::listing)              # 目标必须两段式
::ANCHOR(@::main.cmp)                   # source 类不能为空
```

语法层有效不等于定义层可编译：两个订阅示例的 source 都必须由所在 zhixu 的阶段声明、目标在本定义内真实存在（§6.2），否则编译报 `subscription source ... is not a declared source in this zhixu`。

### 5.4 延时深语义

**求值锚点**——延时锚点 = 左侧表达式的"成熟时刻"：纯信号＝接收时刻。三线（core/合约/回放 oracle）同口径：

> AND 的求值锚点取两操作数锚点中较晚者（各操作数的锚点为其事实的最近接收时刻）；等待中的 AND 取两操作数到期时刻中较晚者。OR 只让**已就绪且有锚**的分支竞争锚点——无锚分支（如衰减否决位的 value=true 真空臂）永不出线，有锚对无锚恒胜、双方有锚取早者、平局保持先到（左）分支；等待中的 OR 取最早的非零到期时刻。

推论：`A&B|C` 且 `min(A,B)<C≤max(A,B)` 时，OR 分支的锚定取 C 而非 AND 分支——AND 尚在等待时无权竞争 OR 的锚点；C 恰等于 `max(A,B)` 的等号点上两者等价。延时到期时刻本身成为新的锚点：`(A+5s)&B` 的锚点取"A+5s 到期时刻"与"B 接收时刻"中较晚者。求值输出四态（uvp-core 求值器 `EvalState`，snake_case 序列化；Go 侧 `hookdsl` 同词表）：`ready`（成熟）/ `wait`（等待中，携带预期到期时刻）/ `impossible`（按当前单调存在语义已不可能再满足）/ `needs_more`（需更多事实方可再判）——本册下文「判 cxl」的叙述对应 `impossible` 终态；持久层状态词汇与熔断形态属实现层（见 `uvp-cloud.md`）。

**成熟边界的跨轨口径**：负事实恰在到期时刻之后极短窗口内到达的边界组合，两轨物理不同——云轨在 DB 时钟域给 `(due, due+1ms]` 闭区间宽限，窗内到达的负事实仍参与裁决、判负赢（求值器与回放同值同窗）；链轨按 `block.timestamp` 秒域、同秒内以块内交易序裁决，无毫秒域宽限。该边界组合两轨可分叉，属登记在案的跨轨差异（`uvp-chain.md` 附录 B 对照行；各自的实现层口径见 `uvp-cloud.md` §6.1 与 `uvp-chain.md` §6）。

**嵌套延时一律拒绝**（正位与否决位同闸）：Delay 的操作数子树内不得再含任何延时节点，`((A +5s) +10s)` 应写作 `(A +15s)`——链式对锚点是纯加法，嵌套只会让单段 30d 上限被逐段叠加绕过（uvp-core 解析层 `reject_nested_delay`，钩子位/适格位两档同闸）。时长全部按整秒精度折算。不带括号的连写 `A +5s +10s` 按语法错误拒绝（后缀延时只消费一次）。

**锚点必须来自事实（表达力边界）**：延时是相对延时，锚点永远是某条事实的接收时刻（或由它链式派生的到期时刻），引擎没有墙钟原语——周期性语义（"每月 1 号触发"）与绝对期限（"截止到固定日期"）不能直接表达。链上求值器按 anchored 语义执行本条：延时操作数无锚（锚定事实尚未到达）时恒判非 Ready（返回未决中性值，绝不把「无锚」当纪元 0 时刻起算伪造锚）；无锚延时形态另被注册闸挡在链外（§5.6——`commitPlan` 要求 DELAY 操作数含正向信号锚点，注册即拒而不是留死代码上链，见 `uvp-chain.md`）。建模模式：上游执行者（转化者）在自己的业务事务里解读日历，把"到期/应触发"表达为一条事实，本域以普通 hook 或订阅消费（日期裁决权留在调用方，回放只需事实即可收敛）。

### 5.5 否定位延时负门 `B & ~(A+duration)`

`~` 的操作数为延时表达式（衰减否决位）当且仅当该 `~(…)` 是 `&` 的直接子项时合法；根位、`|` 分支位、`~` 操作数（双重否定）、延时操作数内一律编译期拒绝，报错写明位置要求。禁用不是词表洁癖：否决位的就绪会衰减（内层成熟即翻假），而延时的成熟是永久的——外层延时若锚在已过期的否决上，到期重评会把早已翻假的否决当真放行。

链上注册门同款双闸（`UVPPlanRegistration._validateHook` 的 vetoTerm/vetoInside 位——手工 plan 防线，见 `uvp-chain.md`）：指令计划里 `NOT` 仅在操作数为裸 `SIGNAL` 或延时产出时合法，否决位出现在根位、`OR` 分支位或任何 `DELAY` 操作数内一律 revert。

三态语义（同一形态随求值时刻的窗口判定；链上在交易内即时判定——`_notValue` 按当时的 `block.timestamp` 对内层到期时刻做窗口比较，不持久化中间态；语义语料与 Go 对拍测试同形态钉死）：

- A 缺席：否决分支永不到达成熟，放行（无限期，无有效期）；
- A 在案未熟：`~(A+duration)` 此刻成立，放行，有效期至 `A+duration`——此后内层翻就绪、否决位翻 cxl；
- A 已熟（成熟边界含端点）：否决成立，所在 `&` 判 cxl。

求值输出的 `expiresAt`/链上求值格的 `dueAt` 承载上述有效期（仅就绪态携带，0=无期限）。它只是元数据，不进调度：`&` 组合对就绪成员取最紧有效期、`|` 获胜分支原样上浮，但调度只为"变真"设闹钟、衰减永不为"变假"唤醒；回放 oracle 与云轨求值器同形。

### 5.6 两个消费位：钩子位与适格位

同一表达式语言有两个消费位，共享同一解析器、AST 与求值器，**差别只在校验档**：

| | 钩子位（receiveSignals 的 value） | 适格位（sendSignals 的 validWhen） |
|---|---|---|
| 语义 | 同单判决：事实到达/唤醒时求值，驱动推进 | 发射适格：外部提交到达时按 pre-state（不含本发）判定"此刻允不允许这一发" |
| 正锚要求 | 根与 `\|` 分支必须含正向锚点 | 无此要求——裸衰减根（如 `buyer::~(validate.rejected + 14d)`）合法 |
| 否决位合法位 | 仅 `&` 直接子项 | 否定无位置限制——根位、`\|` 分支、`&` 子项、延时操作数内均合法（过滤只在一拍求值、不进调度，`(~A)+5s`、`(A & ~B)+5s` 均接受）；仍拒两点：`~` 操作数词表与钩子位相同（裸信号或延时表达式——双重否定 `~(~A)`、组合否定 `~(A&B)` 拒），嵌套延时禁令两档同闸（延时操作数内不得再含延时节点，`(A & ~(B+14d))+5s` 按嵌套延时报错） |
| 订阅原子 | 可作完整条件 | 禁止（适格是本单状态判定，D030） |
| 相同项 | duration 字面量语法与 30 天上限、`~` 操作数词表、两段式寻址与身份闸、深度上限 | 同左 |

**列表形态**：`validWhen` 是 `{source}::{condition}` 的完整表达式列表。逐项独立过筛（适格位是最宽容档，规则项内全覆盖）；全体项 header source 必须一致（否则编译拒绝——适格产物只携带单一 source 标签，D032）；编译期折叠为一棵 AND（N=1 形态不变），依赖取并集，`rawExpression` 逐项以数组保留进 admissions 供诊断。缺键=无条件发射；空数组/空项拒绝（D027，提示删键）。

**跨轨叙述的拆词纪律**：「请求级门 → 吸收 → 适格」三层各自独立，勿混称为一个「适格」。「请求级门」指鉴权/签名/deadline/提交者授权一类入口闸（链轨 `submitSignalFor` 的先序闸；云轨 HTTP 入口与 HMAC 单 sender 语义）——链轨的订单级 submitter 授权住在这一层，不在适格层；「吸收」指重复事实的 first-win 幂等（不再过任何筛）；「适格」仅指本节 `validWhen` 的发射过滤（pre-state 求值）。云轨的 validWhen 时间窗与链轨的提交者授权分属第 3/1 层、物理不同；两轨在第 2/3 层同位同序（见下段）。

**适格位的求值与次序**：幂等吸收先于适格求值（已落库的重复外部提交 first-win 吸收、不再过适格，at-least-once 重试不得被过期窗口 revert），但**后于**请求级门——链轨 `submitSignalFor` 的 deadline/签名/提交者授权与属主阶段就绪门都排在吸收之前：过期或未授权的重放先响亮 revert（`ExpiredSignalSignature` / `UnauthorizedSignalSubmitter`）而非被静默吸收。该次序是刻意选择：把吸收挪到请求级门之前会开「无有效签名探测事实存在性」面，现序方向更严。新事实在提交时对 pre-state 整棵求值（时钟取运行侧权威时钟，链上为 `block.timestamp`）；**整棵 admit-iff-Ready**（`value && !wait && !cancel`）：Ready 准入，Wait/Impossible/NeedsMore 整笔拒绝为类型化错误（云轨为类型化非重试拒绝，消息点名信号与失败表达式；链轨 revert `SignalAdmissionRejected`，携带 orderId/sourceId/signalId），不是逐项投票。**整批原子性**：链轨一笔交易多发射时一个不适格即整批 revert——EVM 原生原子性，接受为设计（见 `uvp-chain.md`）。

**拒绝面**：对事实流静默（拒绝即零事实行、零投递行，绝不落 marker 事实）、对发送方响亮（类型化拒绝，可定位是哪发被哪个闸拒）。拒绝后补齐依赖事实再重新提交即可（该发未落库，重新提交不是重复；重新提交会再过一次筛）。

**作用域**：仅外部提交过筛；引擎内部产事实路径不经此面（dock 投递与回填、per-fact 代铸入口再投递、派生信号与模块内产事实——链轨 `*FromModule` 族调用卡点以下层级）。无单外部出生（开放出生提交）照常过筛（pre-state 空集，正锚表达式自然拒绝——关上"gated 信号绕窗铸单"的口子）。编译期拒绝：出生锚信号与无锚通道阶段不得声明 `validWhen`（声明即死代码，D029）；表达式引用本信号自身即自证无效（D028）；引用的每个 `stage.signal` 受引用存在性约束——目标信号须在目标 stage 的 `sendSignals` 中声明（dock 输入端口的 mailbox 通道是唯一豁免，D013）。适格族拒绝文案逐项带下标 `validWhen[i]` 定位（D027–D032 全族同格式）。

**链上双闸（见 `uvp-chain.md`）**：适格声明编为与钩子同款的 CompactHook 指令计划，以旗标位 8（`HOOK_FLAG_ADMISSION`）随 hooksHash 进 plan 承诺——适格声明不留未签名面。注册门 `_validateAdmission`（`_validateHook` 的过滤档兄弟）在 `commitPlan` 时校验指令计划：与钩子档同源的保留闸是 NOT 操作数词表（裸 SIGNAL 或延时产出）、**延时操作数正向锚点**与嵌套延时禁令——过滤档书写层按共享词表接受 `(~A)+5s` 一类纯否定延时操作数（一拍求值下瞬时值良定义，云轨同词表），链轨对它的裁决是**注册期拒绝**（无锚延时在求值侧是恒未决死代码，commitPlan 挡下而非留死代码上链）。提交卡点 `_requireSignalAdmission`（`_submitSignal` 内、幂等吸收之后）按 `block.timestamp` 求值——无锚延时恒非 Ready（§5.4 anchored 语义，求值侧防御纵深）。无适格声明的 plan 由订单级 `admissionsDeclared` 位短路——零额外冷读。

```yaml
sendSignals:
  - name: str
  - name: approved        # 无 validWhen = 无条件发射
  - name: paid
    validWhen:
      - "buyer::validate.approved"
      - "buyer::~validate.rejected"
```

### 5.7 字符与命名建议

`Hook.Validate()` 只检查外层 `source::condition` 壳格式；字符集、语法和语义由 Rust `uvp-core` 统一判断。为避免跨运行环境歧义：标识符只用字母/数字/下划线（避免中划线）；阶段信号引用严格两段式；延时单位小写。

---

# Ⅲ 协作语义

## 6. 同域跨源：订阅与铸单

本章是跨源协作语义的语言层正本（权威规格 uvp-core `docs/specs/subscription-mint-spec.md`；各轨运行期结论见实现层文档）。跨源协作统一为**事实 / 路由 / 铸单**三个正交层。

### 6.1 三层模型与四类形态

**事实层**：事实有两种锚定形态——订单锚定（挂在确定订单上求值）与通道锚定（无订单上下文、必须携带去重键，§6.4）；事实纪律三元组：去重身份、事实标签、溯源，可重放是唯一不变量（first-win 幂等吸收）。提交不存在批量或原子多信号语义：裁决单元是单条信号，按接收序逐条裁决、first-win 是唯一口径（边界叙述见白皮书 §5.2）。各轨事实载体与标签形态见实现层文档（链上 = 授权钱包提交的 signal 事件，事实标签 `(txHash, logIndex)` 与 `submittedAt`；云轨 = 信封落库）。

**路由层**：`source` 是 zhixu 局部的因果身份类命名空间（多阶段可共享）。订阅 `::ANCHOR(@…)` 按类寻址；投递路由由接收方锚定状态裁决（route=order 沿对接记录按单投递，血缘门拦截无关联到达 / route=fanin 按类扇入），由编译器按域内 `mint` 声明推导注入——路由的执行与投递形态是各轨运行时语义，见实现层文档（链上没有独立的订阅投递子系统，链上编译规则存根见 §6.7）。

**铸单层**：两条声明通道——执行器 `str`（自报血缘，免声明）与阶段 `mint: per-fact`（唯一声明点，订单号从事实纯函数派生、幂等可重放）。

机制侧形态表（用法与概念叙述见白皮书 §2.2）：

| 业务形态 | 现行表达 |
|---|---|
| 分馏（同源分叉，逐事实开单） | 出生阶段 `mint: per-fact` + 订阅 `::ANCHOR(@…)` |
| 撮合（并行输入驱动汇聚判断） | 无锚监听阶段 + 多条订阅 + 执行器自行配对；成交后执行器 `str` 自报多父血缘 |
| 委托（拆单委托执行） | `supplierType: zhixu` 委托对接 + `inputMap`/`signalMap`（§7） |
| 收购（拆单并行 + 回源汇聚） | 有锚阶段订阅 `::ANCHOR(@…)`：事实沿对接记录按单回流 |

### 6.2 三种接收方与阶段种类

| 接收方种类 | 判定（编译期） | 收事实方式 | 收到后 |
|---|---|---|---|
| 出生阶段 | 阶段声明 `mint: per-fact` | 按 source 类扇入（铸前无单可锚） | 每事实引擎代铸一单，本阶段为其出生阶段 |
| 有锚阶段 | 接收阶段自身的 source 类在本 zhixu 内存在 `mint` 声明 | 按单路由：事实沿对接记录到达订阅方订单 | 推进（同单事实累积） |
| 无锚监听 | 接收阶段自身的 source 类在本域内无任何 mint 声明 | 按 source 类扇入 | 执行者自行处理（配对、计数等私有判断） |

判定依据是**接收阶段自身的 source 类**是否有 mint 声明——不是"订阅目标的 source 类"。

规则细节：

- 种类终生不可变（§2.3），禁止运行时 patch。
- mint 声明是编译期唯一的锚定依据；执行器自发 str 出的订单编译期不可见，订阅此类来源的阶段一律扇入。
- mint 阶段自身的订阅一律扇入（铸前无单）；且 mint 阶段订阅自身 source 类被拒绝（代铸单发出的事实会再次命中本订阅，形成无界代铸链）。代铸环防护不止自环：经其他阶段订阅间接回流到 mint 声明的跨阶段代铸环同样编译期拒绝；「一事一单」由出生键编译期查重承载——mint 出生键 ∪ dock entrance 键并集内不得重复，同一上游事实至多铸出一单（uvp-core 代铸环检测与出生键查重两道闸）。
- 出生阶段最小形态：必须至少有一条 `::ANCHOR(@…)` 订阅通道，且 `receiveSignals` 只能是订阅条目——per-fact 出生由事实驱动，同单 hook 在铸单前没有可求值的订单上下文。
- 订阅 source 必须由本 zhixu 的某个阶段声明（订阅寻址只在本域解析），目标 stage/signal 必须在本定义内真实存在；跨秩序的事实级联没有订阅直连，唯一通道是委托 dock（§7）。
- 订阅阶段必须编译期静态给出执行者：通常静态 `executor.supplierID`；唯一例外是**有锚订阅阶段**允许绑定 zhixu 委托（此时禁止 `supplierID`）。无锚扇入订阅阶段禁止 zhixu 委托——委托信封只携带同订单子信号，扇入投递没有订单上下文；`mint` 出生阶段与 zhixu 委托互斥（出生代铸与委托 dock 子单是两种互斥的订单创建路径）。

### 6.3 生命周期词汇与建单判定

信号名 `str` / `cmp` / `err` / `cxl` 是**调用方约定**，引擎对它们没有内建语义；建单判定只认通道特征，不认信号名：

- 语言层承诺的订单创建通道只有三条：执行器 `str` 自报（首条有效信号申请/确认订单号）、出生阶段 `mint: per-fact`（§6.2）、dock `order.mode=new`（§7.5）；订单号一律由出生事实纯函数派生、调用方不可自报（§6.4）。
- 各轨建单判定的接受口径与出生通道全集是实现层语义：云轨引擎只认 `new_source=true` 且 `order_id` 为空的信号（其余信号必须携带 `order_id`，缺单在摄入期确定性拒绝；被引用订单号无条件幂等注册——见 `uvp-cloud.md`）；链轨合约只认出生通道（ORDER_TRIGGER_MINT 开放提交、模块派生建单、dock、治理分叉 `forkOrder`——见 `uvp-chain.md`）。两轨口径对照见 `uvp-chain.md` 附录 B。

约定俗成的生命周期词汇：`str` 是执行器生命周期中的第一条有效 signal（"我接了这个阶段实例"）；不要把订单创建语义放在普通业务 signal 上；`cmp`/`err`/`cxl` 都应建立在已存在的订单上；当前协议不要求单独的 `ack` 创建语义。同名异物提示一处：本节的 `cxl` 是调用方约定的信号名；§5.4 求值输出词表中没有 `cxl`——「裁决不可能成立」对应的求值状态是 `impossible`，与这里的信号名仅字面相近、无语义关联。

### 6.4 通道事实与幂等键

**语言层不变量**：通道锚定事实（无订单上下文）必须携带去重键，缺键在入口确定性拒绝；事实按去重身份 first-win 落定——重复事实吸收为 no-op，不重复扇入、不重复代铸、不重复推进；`str` 自报与代铸的订单号都从事实纯函数派生、幂等可重入。去重键是**调用方可控的事实身份**：同一业务事实被报以两个不同键会得到两个结果（两张单或两次投递），事实身份纪律归调用方——引擎不解析事实正文，无从代为判重。

各轨去重键与订单号派生元组是实现层语义，两轨对照见 `uvp-chain.md` 附录 B：云轨按 `(zhixu, stage, signal, fact_key)` first-win 落通道事实表、代铸/自报单号按逐字段长度前缀的段族纯函数派生（段清单与拼接次序以云轨仓 Go 实现 `pkg/statemachine/facts_individual.go` 的 `deriveBootstrapOrderID` 为唯一权威，详见 `uvp-cloud.md`）；链轨出生单号从 `(planId, sourceId, signalId, payloadHash)` 纯函数派生（载荷哈希镜像进身份，同一事实恒定同单号），`idempotencyKey` 是签名请求的防重放辅助字段、不参与订单号派生（详见 `uvp-chain.md`）。

### 6.5 保证清单

血缘构成性（域内路由即边界）、无需知情者的存在性（出生通道 + mint 代铸）、可重放（事实记录可重放，各轨事实载体见 §6.1/实现层文档）、判定权归属（配对/计数私有，引擎只代行恒等铸造）、跨域显式（dock + 端口 map 唯一通道）——五条全部存活。

### 6.6 出生提交通道（链上）

本节原叙述的开放提交请求体、capability 词表闸与 creator 先到先得等链上运行期机制已迁至链轨实现层文档 `uvp-chain.md`（uvp-eth 根）；语言层的出生声明面见 §4 `mint`、§6.2、§6.3（存根保号，既有 § 引用不断链）。出生提交者授权（birthAuthorizer 承诺）见 uvp-chain.md §4.1。

### 6.7 订阅条目的链上编译规则

本节原叙述的订阅条目指令编译、钩子位标志全集与依赖键 ≤1024 闸已迁至链轨实现层文档 `uvp-chain.md`（uvp-eth 根）；语言层的订阅语义见 §6.1/§6.2（存根保号，既有 § 引用不断链）。

---

## 7. 跨秩序：dock 声明

本章是跨秩序对接的语言层正本（`spec.dockInterface` 与 `executor.zhixuExecutorConfig` 的声明语义与 link 期校验，权威=uvp-core `dock.rs`）。运行期机制（实例建立、投递、patch、选择记录）见两轨实现层文档（`uvp-cloud.md` / `uvp-chain.md`）。

### 7.1 对接模型

一条 Zhixu 可以作为另一条 Zhixu 某个 stage 的执行接口——**当前阶段选择另一条 Zhixu 作为执行者，并通过信号映射完成一次秩序对接**。两条 Zhixu 是对等挂接（"空间站对接"），不是层级包含：

- 对接秩序可以有自己的入口、运行链路、订单实例与履约/验收/回传逻辑；
- 当前阶段是否推进，取决于 `signalMap` 把哪些远端 signal 映射回了本阶段；
- `supplierType=zhixu` 的核心语义是"能力秩序对接"，不表达流程嵌套；一条秩序可被多条秩序复用，也可同时对接多个 peer。

### 7.2 目标侧：spec.dockInterface

`dockInterface` 是本秩序对外公开的**具名接口 map**（可发布多个，如建单型与纯回传型）；其他秩序按接口名引用其中一个接口，只引用端口名，不读取本秩序内部的 Hook 表达式。接口名与端口名同规则：`^[a-z][a-z0-9_]{0,31}$`；不存在 `schemaVersion`/`kind`/`access`/`terminal` 等键——出现即按未知字段响亮拒绝。接口随不可变定义一起发布。

| 路径 | 类型 | 必填 | 说明 |
|---|---|---|---|
| `orderModes` | `[]string` | 是 | `{new, existing}` 的非空子集，无重复。`new` = 每次委托请求一次新的远端运行；`existing` = 连接明确指定的既有运行。模式不被接口允许时对接被响亮拒绝，不自动降级。`new ∈ orderModes` 时接口必须至少有一个 input 端口（建单型服务必须有入口，D025）。 |
| `inputs.<port>.hook` | `string` | 是（每个 input 端口） | `<stage>#<receiveHookName>`，引用本定义内真实存在的 receive hook（同一物理 hook 全定义内只能被一个端口发布）。hook 表达式必须是单一正向本域 atom、source 同域、地址落在所属 stage（D013）——atom 信号不要求 ∈ sendSignals，它是 dock 注入的输入事实（mailbox 通道）。 |
| `outputs.<port>.signal` | `string` | 是（每个 output 端口） | `<source>::<stage>.<signal>`，指向本定义内真实存在的 canonical signal，且该信号在所属 stage 的 `sendSignals` 中声明（D014）。 |

`inputs`/`outputs` 可整体省略（= 空集合）。output 端口没有终态语义：接口输出不把任何一方置为终态，结束语义只由本地阶段/订单结束驱动。

### 7.3 调用方侧：executor.zhixuExecutorConfig

`zhixuExecutorConfig` 是键闭集 `{target, interface, order, inputMap, signalMap}`（闭集外键——如残留的 `schemaVersion`/`target.version`/`order.idPolicy`——按未知字段响亮拒绝）：

| 路径 | 类型 | 必填 | 说明 |
|---|---|---|---|
| `target` | `object\|null` | 是（键必填） | `{zhixu: "<目标定义 uid>"}` 为静态目标，必须是内容派生身份 uid（`zx-<32hex>`）——引用按内容精确指向，目标内容变即新 uid、旧 route 恒指旧 uid，升级由引用方显式改绑新 uid 重新发布；显式 `null` 表示运行时动态选择（声明语义见 §7.4）。target 只携带 `zhixu` 一个键。 |
| `interface` | `string` | 是 | 目标接口名（与端口名同规则）。取值先 trim 再校验。 |
| `order.mode` | `string` | 是 | `new` \| `existing` 闭集。`new` = 本次委托请求一次新的远端运行并连接它；`existing` = 连接明确指定的既有运行，不隐式创建新业务订单。link 期要求 mode ∈ 目标接口 `orderModes`（D020，不静默替代）。 |
| `inputMap` | `map[string]string` | 条件 | 本地 `receiveSignals` 通道名 → 目标接口 input 端口名（key 必须是本 stage 声明的通道，D005）。`order.mode=new` 时**恰好一条**绑定（建单入口需要确定的出生锚，D010）；`mode=existing` 时 0..N 条。 |
| `signalMap` | `map[string]string` | 条件 | 本地 `sendSignals` 信号名 → 目标接口 output 端口名（key 必须是本 stage 声明的信号，D006）。不强制 `str/cmp`——是否需要业务信号映射由实际 Stage 决定。 |

`inputMap`/`signalMap` **至少一张非空**（D019）；value 都是端口名（不是 Hook DSL、不带 source 前缀），key 与 value 不 trim——两端空白按非法名拒绝；每个目标端口在一条 route 内至多绑定一次；`supplierType: zhixu` 时禁止 `supplierID`（D001）。容量按 D016——`inputMap` ≤8、`signalMap` ≤16、接口端口 ≤64；目标引用 ≤256 按 D008；`signalMap` key 为两段式信号名归一后的本地名，长度 ≤26 字节且不含 `.`。委托图不允许环且深度受 D015 上限约束（计数口径见 §7.5 运行时约定）。

link 期裁决的语义权威是 Rust `uvp-core`（`dock.rs`）：目标接口与端口必须存在于目标定义的 `spec.dockInterface`、方向匹配（D009）；本地编译（D001-D006、D010、D016、D019）→ resolution manifest 链接（D008、D009、D012、D015、D020）→ profile 编译。同一条 route 引用的全部端口必须来自同一个目标 source（单源接口，D012）——一个 stage 不能把 `str` 映射到远端 payment source 又把 `cmp` 映射到远端 shipping source；需要消费多个 source 时拆 stage 或经路由型秩序汇聚。静态目标 route 缺 manifest 的可运行编译返回 `UNRESOLVED_DOCK_TARGET`；`target: null` 的 route 本地校验全量保留，产物以未解析形态（`uvp.dockRoute.unresolved.v1`）随 `unresolvedDockRoutes` 携带（各轨消费见 §7.4）。

```yaml
executor:
  supplierType: zhixu
  zhixuExecutorConfig:
    target:
      zhixu: zx-9c41f0d27a3e5b86c4d1af0b7e285c36
    interface: production_service
    order:
      mode: new
    inputMap:
      EXECUTE: execute      # new 模式恰好一条 input 绑定（出生锚）
    signalMap:
      str: started
      cmp: completed
```

「`existing` + `target: null`」对等挂接（一等语法、拼批 N:1 与运行时语义）的唯一权威表述见 uvp-core `docs/specs/subscription-mint-spec.md` §2.4；两轨逐语法点对照见 `uvp-chain.md` 附录 B。

### 7.4 动态选择的声明语义（target: null）

`target: null` 是定义面声明：接口名、端口 map、`order.mode` 照常写全，产物以未解析声明面（`uvp.dockRoute.unresolved.v1`）随定义发布、不进 link。运行期解析与钉住机制见两轨实现层文档：云轨由运行时**选择记录**逐单解析并钉住（按（父秩序, 父订单, 父阶段）分键登记目标 uid，见 `uvp-cloud.md` §10）；链轨消费入口是 `attachDockedOrder`（候选集从 resolution manifest 派生——routeHash 目标槽 = 候选集 root，挂接时携 membership proof 选定目标并终身钉住，同意门三腿之一，幂等检查先于同意门），链轨唯一的动态路由拒绝是 `order.mode=new`（合约没有 new 模式动态路由的消费方，编译期按 `UNRESOLVED_DOCK_MODE` 响亮拒绝、不静默降级）——详见 `uvp-chain.md` 附录 B 对照行。

### 7.5 子订单身份与单源对接原则

**子订单为独立订单**：子订单身份由对接事实确定性推导、与父订单号逻辑独立；重试同一出生锚得到相同 ID（幂等），不同 route 或不同父 stage 得到不同 ID。推导规格与各轨铸造机制见实现层文档（链轨 docking module 的 `linkedOrderId` 双 preimage 规格、云轨建立事务内常规建单——`uvp-chain.md` / `uvp-cloud.md`）。

**单源执行通道**：一个 stage 对外发出的 signal 应属同一个 source 语境——`signalMap` 把当前阶段挂接到某一个远端 source 的入口和回传链路，禁止任意拼接远端 signal（D012）。

**运行时约定**：

- dock 链深度上限 D015（常量 `MAX_DOCK_DEPTH = 8`）：静态 linker 按**定义节点数**计——本定义 root 记 1，最多 7 条静态 dock 边；链上另有按 **dock 边数**计的第二口径（同数字、不同计数语义），见 `uvp-chain.md` 附录 B。
- 接口输出不自动把任何一方置为终态；终态只由本地阶段/订单结束驱动。
- 委托执行者随定义发布，逐单 patch 不承载委托绑定；各轨 patch 面（链轨 assign/handoff 闭集与计划候选集 fail-closed、非协作换人走治理分叉；云轨订单级 stage patch 改写 supplier 与 fileResources）见 `uvp-cloud.md` §10 与 `uvp-chain.md` 附录 B 对照行。
- 各轨子单铸造/挂接与端口交付的运行期机制见实现层文档：链轨 `order.mode=new` 经 `openDockedOrder` 一笔交易内原子完成子单创建、link 登记与出生锚写入，`existing` 经 `attachDockedOrder` 对等挂接既有目标单，输入/输出经 `submitDockedInput`/`submitDockedSignal` 交付（`uvp-chain.md`）；云轨建立事务内铸子单、input 端口每端口恰一次交付、output 建立时回填（`uvp-cloud.md` §7）。

### 7.6 非法字段提示

`triggerEntrance`、zhixu executor 上的 `supplierID`、把 Hook 表达式直接写进 `signalMap`，都不是合法输入；loader / linker 在入口拒绝。跨秩序调用统一走 `zhixuExecutorConfig` 的具名接口对接。

### 7.7 外部事实与秩序间上下文传播

外部事实只有一条建模路径：**执行器自发 signal 即入口**（§1）。委托 dock 在这条路径上的角色是端口对译：外部事实先进入 backend/executor，执行者明确发送的 canonical UVP signal（链上=授权钱包按 EIP-712 提交）才进入 UVP 信号集合；`inputMap`/`signalMap` 是端口对译表。

---

# Ⅳ 示例

## 8. 示例与建模范式

> 示例中的 `platform.type: blockchain`、`provider: eth`、`network: base` 等取值属**轨道皮肤**：示例任选一条轨道的皮肤值演示，不必两份——换成另一轨的皮肤值，同形定义同样可编译（除非另注）。轨道差异与各轨附加接受域见实现层文档（`uvp-cloud.md` / `uvp-chain.md`）。

### 8.1 最小可编译示例（起步模板）

```yaml
apiVersion: uvp/v0
kind: Zhixu
metadata:
  name: minimal_zhixu
spec:
  platform:
    type: blockchain
    provider: eth
    network: base
  nucleation:
    id: core_org
  stages:
    - name: main
      source: buyer
      # 执行器自发 signal 即入口（§1）。READY 是过链上阶段物化门的最小
      # 增量：一条 receiveSignals 通道 + 静态执行者——带静态执行者阶段的
      # receive hook 编译为 EMIT_READY（§4），观察目标用本阶段已声明的
      # 信号（引用存在性，§5.6）。
      receiveSignals:
        READY: "buyer::main.str"
      sendSignals: [{name: str}, {name: cmp}, {name: err}]
      executor:
        supplierType: individual
        supplierID: supplier_init_001
```

该形态 cloud 与链上 target 均可编译（onchain 阶段物化门的最小过门增量＝一条 `receiveSignals` 通道＋静态执行者，§4）。但「可编译」不等于「可运行」：链上订单创建走出生通道（§6.3/§6.6 存根——开放提交需要 mint 出生钩子、dock 需要 `dockInterface` 入口端口，本模板两者皆无，详见 `uvp-chain.md`），且单阶段的首次物化依赖出生通道或来自已物化阶段的信号。可运行的全链最小形态见 §8.2 与白皮书 §4.2（出生阶段在订单创建时物化，EMIT_READY 沿依赖链铺开）。

### 8.2 全字段注释示例

```yaml
apiVersion: uvp/v0
kind: Zhixu
metadata:
  name: settlement_full_demo
  labels:
    biz: settlement
spec:
  platform:
    type: blockchain
    provider: eth
    network: base
    version: "0.1"
    params:
      chainId: "84532"
  nucleation:
    id: core_org
  dockInterface:
    settlement_service:
      orderModes: [new]
      inputs:
        # input 端口是 mailbox（§7.2/D013）：引用的钩子必须是"单一正向
        # atom、地址落在所属 stage"的通道，承接 dock 注入的输入事实；
        # EXECUTE 这类本地触发通道不承担端口发布。
        execute:
          hook: main#SETTLE_REQUEST
      outputs:
        started:
          signal: buyer::main.str
        completed:
          signal: buyer::main.cmp
  stages:
    - name: choose_executor
      source: buyer
      selectedStages:
        - "main"
      # 链上物化门（§4）：带静态执行者的 receive hook 编译为 EMIT_READY，
      # 是合法物化路径。观察目标避开 executor_selected——该 signalKey
      # 已被 main 观察，跨阶段双重观察会被整体拒绝（§4 指针块，
      # onchain 实现层接受域）。
      receiveSignals:
        START: "settlement::on_deal.cmp"
      sendSignals:
        - name: executor_selected
      executor:
        supplierType: organization
        supplierID: selector_org_001
        selectableResource:
          capability_profile:
            fileType: http
            httpFile:
              url: "https://example.com/capability.json"
      fileResources:
        rule_file:
          fileType: local
          localFile:
            path: "/data/rules/settlement.json"

    - name: main
      source: buyer
      receiveSignals:
        EXECUTE: "buyer::choose_executor.executor_selected"
        # settlement_service.execute 端口的 mailbox 通道：单一正向
        # atom 指向本 stage（D013），dock 注入的输入事实落在这里；
        # EXECUTE 仍是本地出生/触发通道（inputMap 的绑定键）。
        SETTLE_REQUEST: "buyer::main.settle_request"
        TIMEOUT: "buyer::(choose_executor.executor_selected + 30m) & ~main.cmp"
      sendSignals:
        - name: str
        - name: cmp
        - name: err
      executor:
        supplierType: zhixu
        zhixuExecutorConfig:
          target:
            zhixu: zx-9c41f0d27a3e5b86c4d1af0b7e285c36
          interface: settlement_service
          order:
            mode: new
          inputMap:
            EXECUTE: execute
          signalMap:
            str: started
            cmp: completed
            err: failed
      fileResources:
        cos_receipt:
          fileType: txcloud
          txCloudFile:
            bucket: "evidence-bucket"
            region: "ap-guangzhou"
            objectKey: "orders/{{order_id}}/receipt.json"

    # 订阅寻址只在本域解析：出生阶段锚定的 @trading source 必须先由本
    # zhixu 的阶段声明，订阅才能通过引用存在性校验（跨秩序没有订阅直连）。
    - name: deal
      source: trading
      # 每阶段一条带物化位的 hook：PLACE 编译为 EMIT_READY；观察目标
      # on_deal.str 全计划仅本阶段观察，无跨阶段双重观察（§4）。
      receiveSignals:
        PLACE: "settlement::on_deal.str"
      sendSignals:
        - name: closed
      executor:
        supplierType: organization
        supplierID: trading_executor_001

    # 出生阶段：每条 trading 域成交事实铸一单（编译为 SIGNAL 指令并置
    # ORDER_TRIGGER_MINT 位标志，现实成立后由计划授权的出生提交者
    # （birthAuthorizer）提交出生——链上细节见 uvp-chain.md）。
    - name: on_deal
      source: settlement
      mint: per-fact
      receiveSignals:
        ON_DEAL: "::ANCHOR(@trading::deal.closed)"
      sendSignals:
        - name: str
        - name: cmp
          # 发射适格（§5.6）：触发事实 str 在案时 cmp 才可发。validWhen
          # 不得声明在 signalMap relay 目标上——main 的 str/cmp/err 经
          # dock 输出中继写事实、绕过适格面，D029 编译期拒收。
          validWhen:
            - "settlement::on_deal.str"
      executor:
        supplierType: organization
        supplierID: settlement_executor_001
```

示例与编译边界逐条自洽：每个阶段都编译出至少一个带物化位的 hook；同一 signalKey 无跨阶段双重观察——`executor_selected` 只被 `main` 观察，`on_deal.str`、`deal.closed` 等各只有一个观察阶段（「同一 signalKey 只允许单一阶段观察」是 onchain 实现层接受域，见 §4 指针块与 `uvp-chain.md`——"同一事实驱动多个阶段"应拆分为各阶段观察不同的目标信号，而不是让多个阶段订阅同一 signalKey）；`on_deal.cmp` 演示发射适格。

### 8.3 建模范式

**延时 + 多否定组合（同单判决）**：见 §5.3 第 5 条——`(A +3d) & ~B & C`。"某事实成立后经过一个窗口仍未被否定即推进"。

**多源汇聚**：撮合类没有汇聚表达式，现行表达是三件套（无锚监听阶段 + 多条订阅 + 执行器配对）：

```yaml
- name: exchange            # 无锚监听：按类扇入，无表达式裁决
  source: match
  executor: { supplierType: organization, supplierID: juice-market-executor }
  receiveSignals:
    SURPLUS_EVENT: "::ANCHOR(@fruit_merchant::retail.surplus)"
    DEMAND_EVENT: "::ANCHOR(@buyer::juice_demand.requested)"
  sendSignals: [{name: str}, {name: frozen}, {name: cmp}, {name: deal}, {name: err}]
```

哪些供给与哪些需求属于同一笔成交是执行器的私有判断；成交后创建独立因果链的新订单（各轨建单通道见 §6.3 与实现层文档）。

**按单路由（收购回流）**：接收阶段自身 source 类在域内被出生阶段声明 mint 时，事实沿对接记录回流到确定的订单，血缘是否成立由对接记录构成性裁决（无关联到达被拦截）：

```yaml
- name: packing             # 有锚阶段：seller 类在域内被声明 mint
  source: seller
  executor: { supplierType: organization, supplierID: fruit-merchant-executor }
  receiveSignals:
    FARMER_FRUIT_SETTLED: "::ANCHOR(@farmer::packing.settled)"
  sendSignals: [{name: str}, {name: frozen}, {name: cmp}, {name: err}]
```

**过滤型订阅的双阶段建模**：订阅必须是 hook 的全部条件（§5.2），"事件 + 条件"拆成两个阶段：

```yaml
- name: demand_intake       # 阶段一：无锚监听，逐事实投递给执行者
  source: buyer
  executor: { supplierType: organization, supplierID: demand-desk-executor }
  receiveSignals:
    DEMAND: "::ANCHOR(@buyer::juice_demand.requested)"
  sendSignals: [{name: noted}, {name: rejected}]

- name: demand_screen       # 阶段二：同单判决，条件过滤在普通 hook 上
  source: buyer
  executor: { supplierType: organization, supplierID: demand-desk-executor }
  receiveSignals:
    SCREEN: "buyer::demand_intake.noted & ~demand_intake.rejected"
  sendSignals: [{name: accepted}, {name: err}]
```

阶段一执行者按业务判断把事实回写为 canonical signal；阶段二用普通 hook 在同单上下文内做条件判定——判定权归调用方，引擎不解读事件正文。

---

## 附录 A 字段速查表

| 路径 | 类型 | 必填 | 一句话约束 | 章 |
|---|---|---|---|---|
| `apiVersion` / `kind` | string | 是 | `uvp/v0` / `Zhixu`（大小写敏感） | §3 |
| `metadata.name` | string | 是 | slug `^[a-z][a-z0-9_-]{0,99}$`，纯展示 | §3.1 |
| `metadata.labels` | map | 否 | 参与身份派生（两轨同公式） | §3.1 |
| `metadata.annotations` | map | 否 | 不参与任何身份/哈希 | §3.1 |
| `spec.platform.type` | string | 是 | 轨道声明（cloud/blockchain/cbdc；皮肤值见实现层） | §3.2 |
| `spec.nucleation.id` | string | 是 | 凝结核标识 | §3.2 |
| `spec.stages` | array | 是 | 至少 1 个 | §3.2 |
| `spec.dockInterface` | map | 否 | 具名接口发布（§7.2） | §7 |
| `stage.name` | string | 是 | 秩序内唯一 | §4 |
| `stage.source` | string | 是 | ≤36B plain identifier | §4 |
| `stage.mint` | string | 否 | 仅 `per-fact`；种类不可变 | §4/§6 |
| `stage.selectedStages` | []string | 否 | `{stage}` 真实存在 | §4 |
| `stage.sendSignals[].name` | string | 是 | 裸名或两段式自指全名，归一唯一 | §4 |
| `stage.sendSignals[].validWhen` | []string | 否 | 发射适格（过滤档）；每项完整 `{source}::{condition}`，header source 全等 | §5.6 |
| `stage.receiveSignals` | map | 否 | key=通道名（1-36B，无 `.`/`#`/空白）；value=hook 或订阅 | §4/§5/§6 |
| `executor.supplierType` | string | 是 | individual/organization/zhixu | §4 |
| `executor.supplierID` | string | 条件 | 非 zhixu 必填（≤36B）；zhixu 禁止 | §4 |
| `executor.zhixuExecutorConfig` | object | 条件 | zhixu 必填（§7.3） | §7 |
| `executor.selectableResource` | map | 否 | 可选资源描述 | §4 |
| `dockInterface.<名>.orderModes` | []string | 是 | {new,existing} 非空子集 | §7.2 |
| `dockInterface.<名>.inputs.<port>.hook` | string | 是 | `<stage>#<hook>` 单一正向本域 atom | §7.2 |
| `dockInterface.<名>.outputs.<port>.signal` | string | 是 | `<source>::<stage>.<signal>` 且 ∈ sendSignals | §7.2 |
| `zhixuExecutorConfig.target` | object\|null | 是 | `{zhixu: uid}` 或 null（动态选择，§7.4） | §7.3/§7.4 |
| `zhixuExecutorConfig.interface` | string | 是 | 目标接口名 | §7.3 |
| `zhixuExecutorConfig.order.mode` | string | 是 | new/existing ∈ 目标 orderModes | §7.3 |
| `zhixuExecutorConfig.inputMap` | map | 条件 | mode=new 恰一条；通道须已声明 | §7.3 |
| `zhixuExecutorConfig.signalMap` | map | 条件 | 信号须已声明；端口至多绑一次 | §7.3 |
| `fileResources.<key>.fileType` | string | 是 | local/http/txcloud/plain_text | 附录 |
| `fileResources.<key>.localFile.path` | string | 条件 | fileType=local 时 | 附录 |
| `fileResources.<key>.httpFile.url` | string | 条件 | fileType=http 时 | 附录 |
| `fileResources.<key>.txCloudFile.{bucket,region,objectKey}` | string | 条件 | fileType=txcloud 时 | 附录 |
| `fileResources.<key>.plainText.content` | []byte | 条件 | fileType=plain_text 时（生产证据不用 plain_text） | 附录 |

---

## 附录 B 两轨接受对照矩阵

本附录（两轨对同一语法点接受/拒绝的对照矩阵——含 `Supplier` 对象、订阅链上编译、物化门、依赖键闸、出生单号公式、嵌套延时双闸、`target:null+new` 裁决 B18、动态路由绑定裁决 C8，以及手工 plan 作者自负纪律）已整体迁至链轨实现层文档 `uvp-chain.md`（uvp-eth 根）附录 B：本正本只承载语言层，两轨接受域差异一律以该处为唯一出处（旧双册时代指向本附录的跨册指针已随扉页重定向）。
