# Trigger 出生与 Dock 协作模型规格（Trigger & Dock Model）

> 状态：对齐基线（v2）
> 语义版本：`uvp.semantic.v1`（单一语义版本线，不并存两套语义）
> 适用：uvp-core（Rust，DSL 语义唯一权威）、uvp（Go 云侧运行时）、uvp-protocol（TS 壳层）
> 合约边界：EVM 合约的 plan 承诺、EIP-712 typed-data 与 dock 制品以 `uvp-deploy/deploy/compatibility/uvp-stack.v1.json` 及各轨 fixtures 为准；本规格不另立版本号。

---

## 0. 本文解决什么

订单如何产生、事实如何进入订单、秩序之间如何协作，由四个正交面表达：

- **事实**：已经发生、不可改写的信号；每条事实最终锚定到一个订单。
- **提交管线**：`(stage, signal, orderId?, payload, resources, auth)` 的成员检查、上下文确定与适格求值。
- **出生判定**：空订单号 + 阶段 `trigger` 集中的信号 → 从出生事实派生订单号、建档，并把该信号写为首条事实。
- **跨秩序协作**：只有委托 dock 一条通道（`spec.dockInterface` + `executor.zhixuExecutorConfig` 的端口对译）；同秩序内的事实连接用同单 hook。

一句话口径：`trigger` 是 `sendSignals` 的可空子集；空订单号 + trigger 信号建单；带订单号的 trigger 信号是普通信号；非 trigger 的空订单号拒绝；出生先过 `validWhen`（空订单上下文）。order 号一律从出生事实纯函数派生，调用方不可自报。

指令集为 SIGNAL / NOT / AND / OR / DELAY：携带集外指令的 plan 在 `commitPlan` 注册边界响亮拒绝。DSL 语法面只有 `stage.trigger`、同单 hook 与 dock 声明。

---

## 1. 事实层（Fact）

### 1.1 锚定与出生提交

事实（signal）在提交时确定其锚定订单：

| 提交形态 | 信封特征 | 结果 |
|---|---|---|
| 订单内提交 | 携带 `order_id`，且订单已存在 | 落在该订单，按 first-win 吸收 |
| 出生提交 | `order_id` 为空，且 `signal ∈ stage.trigger` | 从出生事实派生订单号、登记/创建订单，该信号成为订单首条事实 |
| 非法空单提交 | `order_id` 为空，`signal ∉ stage.trigger` | 具名拒绝（如 `NotTriggerSignal`） |

出生提交在提交管线上完成出生判定：出生事实自身即新订单的首条事实。同一出生事实重试幂等——同一定义、stage、signal 与载荷/幂等键得到同一订单号。

### 1.2 事实纪律三元组（不变量）

无论提交形态，一条事实必须具备：

1. **去重身份**（first-win，重复吸收）：订单内事实按 `(定义, order_id, signal_name)` 去重；出生提交的去重身份由订单号派生元组承载（同一出生事实恒同订单号）。
2. **事实标签**（全序 + tie-break）：云侧 `ready_at + seq`，链侧 `(txHash, logIndex)`。一切投影按事实标签排序，不按到达时间。
3. **溯源**：发送方域/单/阶段/信号；出生提交携带的载荷与可选 `birthKey`/`factKey` 进入订单号派生，供幂等重试与审计使用。

**可重放是唯一不变量**：给定全部事实 + 不可变定义 + dock 对接记录，重放必然收敛到同一订单集合。订单上下文不是事实成立的必要属性：空订单号的出生提交先派生订单，再写事实。

### 1.3 事实归属的语义结论

- 事实保持自己的家；出生是从出生事实**派生新身份**并记录溯源关联，不移动事实、不产生双重身份。
- 一条出生事实至多派生一个订单号；跨 trigger 的并发协调归调用方/执行者，引擎只保证"同一出生事实同一订单号"。
- 订单只出现在"没有它就无法路由、无法担责"的地方。

---

## 2. 提交与跨秩序（Submission & Dock）

### 2.1 source 命名空间

- `source` 是 **zhixu 局部**的因果链身份命名空间；多个 stage 可共享同一 source（整条业务线共用一个因果身份类）。
- `stage.signal` 与 hook 头 source 只在本域解析；引用的目标 stage/signal 必须在本定义内真实存在（引用存在性校验，uvp-core `validate_hook_dependency_references` 与 Go 轨编译器校验同款）。同单 hook 的 header source 可以合法地不等于接收阶段自身的 source——语义是在本订单上等一条落在别的 source 类上的信号，而不是逐事件通知。
- 乐高原则：秩序之间无父子。被委托方天然存在，不因被委托需要父；可反向委托。无 dock 实例 = 无关系 = 不投递，这是"尚无关系"的正常态，不是孤儿。

### 2.2 提交管线

一次 signal 提交的输入是 `(stage, signal, orderId?, payload, resources, auth)`。语言层裁决如下，权限/签名/时限等请求级闸由各轨实现层负责：

1. **成员检查**：stage 必须存在；`signal ∈ stage.sendSignals`，否则拒绝（具名 `SIGNAL_NOT_IN_SEND_SIGNALS` 一类入口错误）。
2. **确定上下文**：`orderId` 为空 → **空订单上下文**（无 facts/files）；`orderId` 非空 → 订单必须存在，不存在即拒绝；随后在订单上下文求值。
3. **适格求值**：signal 声明了 `validWhen` 时按语言手册 §5.6 求值；不通过即拒绝（链轨 revert `SignalAdmissionRejected`，云轨类型化非重试拒绝）。出生信号在空订单上下文求值：没有既成 facts/files，否定条件为真、存在与延时成熟条件为假；仅含存在依赖的适格形态因此不会 Ready。
4. **带单提交**：`orderId` 非空时是普通信号；信号是否属于 trigger 集不影响其普通身份。
5. **出生提交**：`orderId` 为空且 `signal ∉ stage.trigger` → 拒绝（具名 `NotTriggerSignal` 一类入口错误）；`signal ∈ stage.trigger` → 从出生事实派生订单号、登记/创建订单，并把该信号写为该订单首条事实，返回订单号。
6. **并发**：跨 trigger 的并发协调归调用方/执行者；引擎只保证"同一出生事实同一订单号"。

各轨入口错误名与字段级校验见实现层文档；语言层要求拒绝具名、可定位到 stage/signal。

### 2.3 Trigger 与出生判定

- **trigger 集**：阶段声明 `trigger: [<signal>, ...]`，可空、可省略；非空时必须逐项展开为 `{stage}.{signal}` 并在本阶段 `sendSignals` 中声明，不得重复。trigger 集编译期固定，随 plan 承诺进入链上产物与云侧投影。
- **多 trigger**：一个阶段可有多个 trigger 信号；一个 plan 可有多个 trigger 阶段；trigger 阶段不必是首个阶段。每条 trigger 信号各自是一条出生入口。
- **执行者**：trigger 阶段必须静态 plain 执行者（individual/organization + supplierID），不得为 zhixu 委托，不接受运行时 patch；出生权不随委托或 patch 漂移。
- **空订单上下文**：没有订单就没有 facts/files；否定条件为真，存在与延时成熟条件为假。trigger 阶段不需要 `receiveSignals` 才能出生；出生 hook 由 trigger 信号合成（语言手册 §6.7）。
- **无分类**：同单 hook 与 trigger 出生使用同一套表达式与适格规则；事实进入订单只有两条通道：同单 hook 消费订单内事实，委托 dock 端口对译。

### 2.4 跨域：委托 dock + 接口映射

- 委托是一个秩序 dock 另一个秩序：目标定义在 `spec.dockInterface` 发布**具名接口 map**（接口名 → {orderModes, inputs, outputs}），调用方 stage 在 `executor.zhixuExecutorConfig` 按接口名引用，`inputMap`/`signalMap` 是接缝上的对译表；A 的委托 stage 与 B 被绑定的端口在接缝处视为**同一个 source** 的两半（单源 seam，被绑定端口范围）。
- `target.zhixu` 填目标定义的内容派生身份 uid（`zx-<32hex>`）或显式 `null`（动态选择）。DSL 壳不携带任何身份（`metadata.uid` 不是作者可写字段）；uid 由定义内容派生（preimage 剔除 `metadata.name`/`metadata.annotations` 等展示字段），同内容跨轨同 uid、重复发布幂等，内容一变即新身份——派生公式跨轨同族（链轨 uvp-protocol TS 与云轨 FFI 以共享金向量互钉），云轨以该 uid 为 `global_zhixu` 主键、运行时零哈希；共享 core 产物为中性形状（`zhixuName` 仅展示投影，无 hash/root 字段）。`order.mode` 闭集 {new, existing}：`new` 建独立子订单（恰好一条 input 绑定 = 出生锚，云轨幂等=建立自然唯一键、链轨幂等=链上确定性承诺），`existing` 连接既有目标订单、不建单（建立时回填已成立的接口输出事实）——`existing` 与 `target: null` 的组合语义单点收敛于下「对等挂接五点」（语法与结构层）及「对等挂接 · 运行时语义」五点（运行时层）。
- 委托声明至少一项输入或输出映射（无需虚构 str/cmp 映射满足格式）；接口输出不自动置任何一方为终态，终态只由本地阶段/订单结束驱动。
- 委托路由只在对译端口上携带信号；事实经 signalMap 逐条映射回父阶段，接口输出不自动把任何一方置为终态。
- 委托关系一次性绑定、禁 patch（现有门禁不变）。
- `rel_order_order` 语义为**对接记录**（谁 dock 谁、映射实例、接缝两侧锚点），不是"父子血缘"；表结构不变。
- dock 链深度上限两种计数口径（常量同为 `MAX_DOCK_DEPTH = 8`，静态更严）：静态 linker（uvp-core `dock.rs`）按**定义节点数**计——本定义 root 记 1、启动图深 >8 拒绝，即最多 7 条静态 dock 边；链上 `UVPDockingModule` 按 **dock 边数**计——parent 深度 ≥8 才拒绝，即最多 8 条 dock 边。两者观测面不同：静态 linker 只看编译期启动图，链上闸计数运行时累计的 dock 边。

**对等挂接五点（`order.mode=existing` + `target: null`）。本规格是该语义的唯一权威出处，文法手册正本（uvp-core/docs/zhixu-dsl-grammar.md）与实现手册只留一句话摘要与指针：**

1. **一等对等挂接语法**：`existing` 与 `target: null` 是对等挂接的一等作者语法——与 `new` + 静态目标共用同一 `zhixuExecutorConfig` 键闭集、同一校验族（D 码）与同一具名接口发布面（`orderModes` 含 `existing` 的接口），不是附属变体或运行时私约；挂接双方仍是对等关系（§2.1 乐高原则），不产生层级包含。
2. **拼批 / N:1**：同一目标运行可被多个调用方挂接——一个订单可由一次 `new` 对接创建，此后被任意多次 `existing` 对接引用；每个调用方按（本地订单, 本地阶段）各成一条对接实例（`dock_instance` UNIQUE(本地秩序, 本地订单, 本地阶段)），共享同一目标订单，任何一方都不重复建单。
3. **运行时选择与生命周期**：`target: null` 的 route 编译为未解析声明面（`uvp.dockRoute.unresolved.v1`，本地校验全量保留、不进 link），运行时由选择记录（`/dock-selection`，目标按定义 uid 寻址；`existing` 的目标订单由记录的 orderRef 指定）**按父订单逐单**补齐：选择记录按（父秩序, 父订单, 父阶段）分键——执行秩序按模板版本编译、同模板的全部订单共享（秩序, 阶段），共享一条记录会让后续订单错绑到别人的目标；补齐结果钉在该父订单的对接实例行（resolved 快照 + 目标身份），**不回写定义行**——定义行的未解析声明永驻，首单建立不得把后续订单钉死在自己的选择上。
4. **DDL 已支持**：云轨 DDL 已承载全部所需结构——`dock_route_selection`（UNIQUE(父定义, 父订单, 本地阶段)，目标定义 uid + 接口名 + 可选 orderRef）、`dock_instance`（order_mode 闭集含 `existing`）、按（实例, 端口）的投递唯一键（`dock_input_delivery`/`dock_output_delivery`）；该形态是纯运行时能力，无 schema 缺口。
5. **链轨承接已落地**：链轨（uvp-eth 侧编译器）对该形态的承接定性为**能力缺口而非语义分叉**，缺口已补齐（`UVPDockingModule`）：`existing` 路由进 on-chain 产物、经 `attachDockedOrder` 挂接既有目标单（同意门三腿之一）；未解析 target 以 `unresolvedDockRoutes` 声明面进同一产物（候选集从 resolution manifest 派生、routeHash 目标槽为候选集 root），运行时由携 membership proof 的 attach 动态选定、终身钉住。on-chain 编译边界唯一保留的动态拒绝是 `order.mode=new` 的动态路由（`UNRESOLVED_DOCK_MODE`——合约无 new 模式动态路由的消费方）；`UNRESOLVED_DOCK_TARGET` 的现行唯一含义是静态目标路由缺 resolution manifest。两处拒绝均发生在编译期且响亮，不静默降级。补齐状态以兼容矩阵（`uvp-deploy/deploy/compatibility/uvp-stack.v1.json`，§7）为准，不在此登记；两轨逐语法点的接受/拒绝对照见 uvp-eth 根 `uvp-chain.md` 附录 B。

**对等挂接 · 运行时语义（两轨生效）。上五点钉住语法与结构层（一等语法、拼批、选择记录、DDL、链轨承接）；以下五点是挂接自建立时刻起生效的运行时语义——同意、回填、交付、解析、同构，同属本规格唯一权威出处：**

1. **挂接同意模型**：建立挂接（`existing` 连接既有目标订单）的动作方必须命中目标侧同意集合；语义两轨统一，同意集合的形态各取其信任域。链轨是无许可世界（任何人可发交易、接口承诺 word 公开可复算），同意必须证明到链上：目标订单创建者（creator）、目标订单在任执行者、或目标侧显式授权（EIP-712 attach permit）三者之一，三者皆缺即响亮拒绝（`UVPDockingModule._requireAttachConsent`）。云轨经产品裁决钉死为永久单运营方信任域：三条腿在单信任域内坍缩为同一主体——`/dock-selection` 选择记录持有凭据的提交方即同意集合，鉴权边界外无挂接建立入口，`pkg/docking` 建立路径不另设进程内同意门；这是该语义在单信任域下的完整形态，不是待补齐的缺口。该前提若变更（云轨承载多方互挂），须先为同意模型定义身份原语（云轨无地址，身份是 API 凭据主体），属协议演进而非补实现。
2. **回填口径（两轨唯一语义）**：挂接建立时，目标接口输出中在该时点已成立的事实按 signalMap 回填父侧。回填是引用不是搬运：事实保持自己的家（§1.3），历史不改写、事实不重发。云轨现行回填行为即唯一语义，链轨不另立口径。
3. **input 恰一次交付**：对接实例的每个 input 端口恰交付一次；此后对该端口的提交一律是幂等重放面——吸收、不交付新事实，幂等由（实例, 端口）投递唯一键承载（上五点第 4 条）。跨单的持续信息流只有 output 方向（signalMap 回传）。
4. **`null` 解析时点（建立期解析、终身钉住）**：`target: null` 只在挂接建立期由选择权威经选择记录（`/dock-selection`）解析为具体目标——目标定义与 `existing` 的 orderRef——解析结果对该挂接终身钉住，不随运行推进重开；目标升级（引用方显式改绑新 uid 重新发布，旧 route 恒指旧 uid）只影响后续建立（上五点第 3 条）。选择权威即选择记录的写入方：云轨为 `/dock-selection` 选择记录的提交方（记录按（父秩序, 父订单, 父阶段）逐单登记——见上五点第 3 条，一个父订单缺自己的记录即等待，别单的记录不满足它）；链轨为携候选集 membership proof 发起 `attachDockedOrder` 的一方（链轨无独立选择记录面，选定即随建立交易写入实例身份、终身钉住）。缺选择记录属等待性错误、不猜单（A07 existing 明确端点：缺 orderRef 时待解析，不猜「最新一单」）；处置面两轨不同：云轨退避重试、计入投递预算、预算耗尽落死信（§6 投递层同款纪律），链轨无投递预算与死信——挂接交易缺选择材料即响亮 revert，重试由提交方（保活件/中继/任何人）无界自行决定（逐错误对照见 2.6）。
5. **挂接后与 `new` 完全同构**：两模式的全部差异都在建立动作上（`new` 建新单、`existing` 挂接既有单）；建立之后走同一套运行时语义——input 每端口恰交付一次（第 3 条）、output 经 signalMap 回传父侧、终态只由本地阶段/订单结束驱动（接口输出不自动置任何一方为终态）。建立之后不存在第二条语义路径。

本运行时五点在两轨均生效：第 2、3、5 点两轨逐字统一；第 4 点语义统一、处置面各取其轨（云轨投递预算+死信，链轨无预算无死信、失败即响亮 revert 后无界重发）；第 1 点语义统一、同意集合形态各取信任域（见该点）。补齐状态仍以兼容矩阵（`uvp-deploy/deploy/compatibility/uvp-stack.v1.json`，§7）为准，不在 spec 登记状态——第 1 点的两个形态是语义本身，无补齐可言、不进矩阵。

### 2.5 外部世界

- 唯一入口：执行器自发信号（str / canonical signal）；空订单号的出生提交同样是一条执行器签名的 signal。
- 外部事实名契约归 swagger。
- 外部事实没有绕过执行器的捷径：必须先成为某域的 canonical signal，才能被订单内 hook 消费或被出生管线判定。

### 2.6 两轨投递错误面对照

同一错误在两轨的反馈形态系统性相反（云轨多静默/预算化，链轨一律响亮原子 revert）。下表逐行从两轨实现（云轨投递层与 `loadAffectedHooks` 门；链轨 `UVPStateMachine`/`UVPDockingModule` revert 面）与上文条款核实——运维处置方向按本表读，不逐案读码：

| 错误/场景 | 云轨：反馈形态与处置 | 链轨：反馈形态与处置 |
|---|---|---|
| 建立前投递（对接记录/dock 实例未建立，事实先到投递通道） | 静默不可达：无排队、无回扫，事后补建对接不补投（模-3 时序纪律） | 响亮 revert（`submitDockedSignal` 对未开实例 revert `DockNotOpened`），事实从未被接受；建立后重发即达 |
| attach 前已成立的目标接口输出 | 登记后无回扫，错过即错过 | 结构性自愈：attach 后按 output 绑定逐条重放回填（回填=重放，permissionless） |
| 挂接/投递授权缺失 | 单运营方信任域内鉴权边界即同意集合，边界外无建立入口（401/403，见 §2.4 运行时第 1 点） | 响亮 revert（`DockAttachConsentRequired`：目标订单创建者/在任执行者/EIP-712 attach permit 三腿皆缺） |
| 重复投递/重复挂接 | （实例, 端口）投递唯一键幂等吸收 | 交付账本幂等：成功挂接的重放对任何人 `return false`；output 绑定二次提交幂等吸收，同本地键异 payload 落 `SignalAlreadyExists` fail-closed |
| 目标事实未成立（回写时目标单还没有该事实） | 投递不可达/等待，随预算退避 | 响亮 revert（`DockOutputNotReady`），原子无副作用，事实成立后重试即达 |
| 缺选择记录/未解析路由（`target: null` 未补齐） | 等待性错误：退避重试、计投递预算、预算耗尽落死信；不猜单（§2.4 运行时第 4 点） | 响亮 revert（attach 携 membership proof，缺证/越界即拒）；无预算无死信，重试由提交方无界决定 |
| 瞬时故障（进程/网络/超时） | 退避重试计入预算，预算耗尽落 dead，可复活（§6） | 交易原子：未上链即无效果；重发无界（gas 成本即重试成本），无 dead 概念 |
| 求值超时/毒化 | poison 熔断 + 人工 revive（在线求值可超时的失败物理学） | 原子 revert（gas 界内全有或全无，无 poison 概念） |

---

## 3. 出生层（Birth）

| 通道 | 声明 | 机制 | 保证 |
|---|---|---|---|
| trigger 出生 | 阶段 `trigger` 集（唯一声明点） | 空订单号 + trigger 信号提交：从出生事实纯函数派生订单号，登记/创建订单，把该信号写为首条事实；幂等重试得到同一订单号 | **无需知情者的存在性**：任一授权提交者以同一出生事实均可幂等建单 |
| dock `order.mode=new` | 目标接口 `orderModes` 含 `new` + 调用方 `zhixuExecutorConfig` | 委托建立时创建独立子订单并连接（链轨 `openDockedOrder` 原子完成；云轨建立事务内建单） | 子订单身份由对接事实确定性推导；重试同一出生锚得到相同 ID |

- `str` 是调用方约定的信号名，不是特殊出生信号；订单创建语义只放在 `trigger` 集与 dock 建立动作上。
- 出生事实的派生元组显式包含可选的 `birthKey`/`factKey`（保留后端既有 FactKey 重试语义），同一出生事实重试幂等。
- 一个出生事实至多派生一个订单号；跨 trigger 的并发协调归调用方/执行者。
- 出生事实的溯源关系由事实自身携带的载荷与订单号派生承载；血缘边章唯一、first-win、落定后不可改写。

---

## 4. 语法表面

以下示例取自水果大亨演示（同一份 zhixu 定义）。示例中全部信号引用都在**同一 zhixu 定义内**——同 source 类与跨 source 类都是同秩序内的信号；跨秩序事实级联只有委托 dock 一条通道（见 2.4）。

```yaml
# 出生阶段：每次以空订单号提交 journey_started 即出生一单
- name: entry
  source: customer
  trigger: [journey_started]
  executor: { supplierType: organization, supplierID: journey-executor }
  sendSignals: [{name: journey_started}, {name: str}, {name: cmp}, {name: err}]

# 同单汇聚判断：执行者把多源事实写入本订单，hook 只做订单内判定
- name: exchange
  source: match
  executor: { supplierType: organization, supplierID: juice-market-executor }
  receiveSignals:
    PAIR_READY: "match::exchange.surplus_noted & match::exchange.demand_noted"
  sendSignals: [{name: surplus_noted}, {name: demand_noted}, {name: deal}, {name: err}]

# 同单推进：普通 hook（订单上下文，引用本订单内其他阶段的信号）
- name: washing
  source: seller
  executor: { supplierType: organization, supplierID: farmer-executor }
  receiveSignals:
    WASH_READY: "seller::packing.cmp"
  sendSignals: [{name: str}, {name: cmp}, {name: err}]

# 跨秩序：委托 dock（接口发布 + 端口对译）
- name: packing
  source: seller
  executor: { supplierType: zhixu, zhixuExecutorConfig: {target: null, interface: packing_service, order: {mode: existing}, inputMap: {START: start}, signalMap: {cmp: completed}} }
  receiveSignals:
    START: "seller::packing.start"
  sendSignals: [{name: cmp}]
```

Stage 字段总表（当前态）：

| 字段 | 状态 |
|---|---|
| `source` | 保留：因果身份类（域内命名空间，多阶段共享） |
| `trigger` | 可空、可省略；非空时为 `sendSignals` 子集、无重复；编译期固定并进 plan 承诺；trigger 阶段须静态 plain 执行者 |
| `receiveSignals` | 保留 map 形态；值为同单 hook |
| `sendSignals` | 保留，条目对象化 `{name, validWhen?}`：`name` 必填非空、归一后不可重复；`validWhen` 是发射适格声明（过滤档钩子方言），条目数组——每项是完整的 `{source}::{condition}`、全体条目 header source 必须一致，缺键即无条件发射；语义权威见文法手册正本 §5.6，本规格不重述 |
| `executor` | 委托为 supplierType=zhixu + zhixuExecutorConfig{target(目标定义uid|null), interface, order.mode∈{new,existing}, inputMap, signalMap→目标接口端口名；至少一映射，new 恰一条 input 绑定} |
| `fileResources`、`selectedStages` | 保留 |

---

## 5. 形态对照（当前）

| 场景 | 表达 |
|---|---|
| 分馏（同源分叉，逐事实开单） | 阶段 `trigger` 集 + 空订单号提交出生 |
| 撮合（并行输入驱动汇聚判断） | 订单内执行者写入多源事实 + 同单 hook 条件判定；配对/计数归执行器私有判断 |
| 收购回流 | 委托 dock `order.mode=new` 建子单，output 经 signalMap 回传父侧；父侧用同单 hook 消费回传事实 |
| 观察入口 | 同单 hook + 静态 plain 执行者 |
| 交易所开门/关门 | 执行者以 canonical signal 写入订单；订单内 hook 消费 |
| 委托 | `spec.dockInterface` 具名接口 + `executor.zhixuExecutorConfig` 的 `inputMap`/`signalMap`（new 建子单 / existing 挂接既有单） |

---

## 6. 回放口径

- 回放基线：全部事实（订单内事实 + 出生提交事实）+ 不可变定义 + dock 对接记录。
- trigger 出生：事实重放 → 纯函数派生订单号 → 同一订单集合，不漂移；同一出生事实重试不产生新单。
- hook 判决（普通表达式）：维持现有 hook_state 语义层（init/wait/ready/cxl，终态不可变）。
- 投递层（重试/退避/dead/复活）机制原样，适用于 dock 投递——本条与回放基线均为云轨口径；链轨投递错误面（响亮 revert、无预算无死信）见 2.6 对照表。

## 7. 版本与兼容

- 版本口径：协议制品统一为 `uvp.<artifact>.v<N>` 点号风格；语义/AST/语料/部署清单为 v1。当前语义批的产物面：`uvp.semantic.v1`、`uvp.cloudAst.v2`、`uvp.cloudArtifact.v6`、`uvp.zhixuAnalysis.v2`、`uvp.hookPlan.v5`、`uvp.onchainHookPlan.v5`；dock 制品沿用 `uvp.dockInterfaceArtifact.v2`、`uvp.dockRoute.v3`/`uvp.dock.resolution.v2`/`uvp.dockRoute.unresolved.v1`（后者以 `dockRouteUnresolved` 键进入矩阵 artifactSchemas，与 uvp-core `DOCK_ROUTE_UNRESOLVED_SCHEMA_VERSION`、Go `DockRouteUnresolvedSchemaVersion` 常量同值互认）。同一语义只存在当前版本，不并存旧产物。
- 兼容矩阵 `uvp-stack.v1.json` 是当前版本真相：它钉住上述 artifactSchemas、合约 ABI fixture、EIP-712 domains 和 `uvp-eth.addresses.v1`；合约 plan 承诺（publisher、hooksHash、capabilitiesRoot、dockRoutesRoot、dockInterfaceRoot、executorCandidatesRoot、birthAuthorizer、deadline）与复合 `(planId, orderId)` 身份以矩阵和 fixtures 为准。
- 语法面只有 `stage.trigger`、同单 hook 与 dock 声明。

## 8. 当前模型裁决

| 裁决 | 结论 |
|---|---|
| 出生声明点 | `stage.trigger` 是唯一出生声明面；trigger 是 `sendSignals` 的可空子集、无重复、编译期固定 |
| 出生通道 | 空订单号 + trigger 信号出生；空订单号 + 非 trigger 信号拒绝；带订单号的 trigger 信号是普通信号 |
| 出生适格 | 出生写入前在空订单上下文求值 `validWhen`；不通过即拒绝（链轨 `SignalAdmissionRejected`） |
| 订单号 | 由出生事实纯函数派生，调用方不可自报；派生元组包含可选 `birthKey`/`factKey` 以保证重试幂等 |
| trigger 执行者 | 必须静态 plain 执行者（非 zhixu、supplierID 非空）；出生权不随委托或运行时 patch 漂移 |
| 同域事实连接 | 只用同单 hook：hook 在订单上下文求值，可引用本秩序任意阶段的信号 |
| 跨秩序 | 只有委托 dock：`spec.dockInterface` + `executor.zhixuExecutorConfig`；不新增 `parents` |
| 多 trigger | 一个阶段多个 trigger 信号、一个 plan 多个 trigger 阶段均合法；trigger 阶段不必是首个阶段 |
| 回放 | 给定全部事实 + 不可变定义 + dock 对接记录，重放收敛到同一订单集合 |
