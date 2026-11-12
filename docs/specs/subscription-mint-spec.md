# Trigger 出生与 Dock 协作模型规格（Trigger & Dock Model）

> 状态：对齐基线（v3）
> 语义版本：`uvp.semantic.v1`（单一语义版本线）
> 适用：uvp-core（Rust，DSL 语义唯一权威）、uvp（Go 云侧运行时）、uvp-protocol（TS 壳层）
> 合约边界：EVM 合约的 plan 承诺、EIP-712 typed-data 与 dock 制品以 `uvp-deploy/deploy/compatibility/uvp-stack.v1.json` 及各轨 fixtures 为准；本规格不另立版本号。

---

## 0. 本文解决什么

订单如何产生、事实如何进入订单、秩序之间如何协作，由四个正交面表达：

- **事实**：已经发生、不可改写的信号；每条事实最终锚定到一个订单。
- **提交管线**：`(stage, signal, orderId?, payload, resources, auth)` 的成员检查、上下文确定与适格求值。
- **出生判定**：空订单号 + 出生信号（`sendSignals` 条目标记 `trigger: true`）→ 从出生事实派生订单号、建档，并把该信号写为首条事实。
- **跨秩序协作**：只有静态 Dock 一条通道（独立 `kind: Dock` 文档声明 A→B 发射条件；B 侧目标阶段声明 `executor.supplierType: dock`，运行期建立 binding）；同秩序内的事实连接用同单 hook。

一句话口径：`trigger: true` 是 `sendSignals` 条目上的出生标记；空订单号 + 出生信号建单；带订单号的出生信号是普通信号；非出生信号的空订单号拒绝；出生先过 `validWhen`（空订单上下文）。order 号一律从出生事实纯函数派生，调用方不可自报。

指令集为 SIGNAL / NOT / AND / OR / DELAY：携带集外指令的 plan 在 `commitPlan` 注册边界响亮拒绝。DSL 语法面只有 `sendSignals[].trigger`、同单 hook 与独立 Dock 文档声明。

---

## 1. 事实层（Fact）

### 1.1 锚定与出生提交

事实（signal）在提交时确定其锚定订单：

| 提交形态 | 信封特征 | 结果 |
|---|---|---|
| 订单内提交 | 携带 `order_id`，且订单已存在 | 落在该订单，按 first-win 吸收 |
| 出生提交 | `order_id` 为空，且 `signal` 是本阶段出生信号 | 从出生事实派生订单号、登记/创建订单，该信号成为订单首条事实 |
| 空单提交（非出生信号） | `order_id` 为空，`signal` 不是本阶段出生信号 | 具名拒绝（如 `NotTriggerSignal`） |

出生提交在提交管线上完成出生判定：出生事实自身即新订单的首条事实。同一出生事实重试幂等——同一定义、stage、signal 与载荷/幂等键得到同一订单号。

### 1.2 事实纪律三元组（不变量）

无论提交形态，一条事实必须具备：

1. **去重身份**（first-win，重复吸收）：订单内事实按 `(定义, order_id, signal_name)` 去重；出生提交的去重身份由订单号派生元组承载（同一出生事实恒同订单号）。
2. **事实标签**（全序 + tie-break）：云侧 `ready_at + seq`，链侧 `(txHash, logIndex)`。一切投影按事实标签排序，不按到达时间。
3. **溯源**：发送方域/单/阶段/信号；出生提交携带的载荷与可选 `factKey` 进入订单号派生，供幂等重试与审计使用。

**可重放是唯一不变量**：给定全部事实 + 不可变定义 + Dock 声明与 binding 记录，重放必然收敛到同一订单集合。订单上下文与事实成立解耦：空订单号的出生提交先派生订单，再写事实。

### 1.3 事实归属的语义结论

- 事实保持自己的家；出生是从出生事实**派生新身份**并记录溯源关联，不移动事实、不产生双重身份。
- 一条出生事实至多派生一个订单号；跨出生信号的并发协调归调用方/执行者，引擎只保证"同一出生事实同一订单号"。
- 订单只出现在"没有它就无法路由、无法担责"的地方。

---

## 2. 提交与跨秩序（Submission & Dock）

### 2.1 source 命名空间

- `source` 是 **zhixu 局部**的因果链身份命名空间；多个 stage 可共享同一 source（整条业务线共用一个因果身份类）。
- `stage.signal` 与 hook 头 source 只在本域解析；引用的目标 stage/signal 必须在本定义内真实存在（引用存在性校验，uvp-core `validate_hook_dependency_references` 与 Go 轨编译器校验同款）。同单 hook 的 header source 可以合法地不等于接收阶段自身的 source——语义是在本订单上等一条落在别的 source 类上的信号，而不是逐事件通知。
- 乐高原则：秩序之间无父子。Dock 是 B 侧声明的静态 A→B 订阅，双方对等；binding 建立后才产生发射关系，尚无 binding 是"尚无关系"的正常态。

### 2.2 提交管线

一次 signal 提交的输入是 `(stage, signal, orderId?, payload, resources, auth)`。语言层裁决如下，权限/签名/时限等请求级闸由各轨实现层负责：

1. **成员检查**：stage 必须存在；`signal ∈ stage.sendSignals`，否则拒绝（具名 `SIGNAL_NOT_IN_SEND_SIGNALS` 一类入口错误）。
2. **确定上下文**：`orderId` 为空 → **空订单上下文**（无 facts/files）；`orderId` 非空 → 订单必须存在，不存在即拒绝；随后在订单上下文求值。
3. **适格求值**：signal 声明了 `validWhen` 时按语言手册 §5.6 求值；不通过即拒绝（链轨 revert `SignalAdmissionRejected`，云轨类型化非重试拒绝）。出生信号在空订单上下文求值：没有既成 facts/files，否定条件为真、存在与延时成熟条件为假；仅含存在依赖的适格形态因此不会 Ready。
4. **带单提交**：`orderId` 非空时是普通信号；信号是否属于出生集不影响其普通身份。
5. **出生提交**：`orderId` 为空且该信号不是本阶段出生信号 → 拒绝（具名 `NotTriggerSignal` 一类入口错误）；该信号是出生信号 → 从出生事实派生订单号、登记/创建订单，并把该信号写为该订单首条事实，返回订单号。
6. **并发**：跨出生信号的并发协调归调用方/执行者；引擎只保证"同一出生事实同一订单号"。

各轨入口错误名与字段级校验见实现层文档；语言层要求拒绝具名、可定位到 stage/signal。

### 2.3 出生信号与出生判定

- **出生信号集**：阶段在 `sendSignals` 条目上用 `trigger: true` 标记出生信号（可空、可省略）；标记条目的 `name` 必须在本阶段 `sendSignals` 中声明（裸名或 `{stage}.{signal}` 两段式自指），同一阶段内不得重复。出生集编译期固定，随 plan 承诺进入链上产物与云侧投影。
- **多条出生信号**：一个阶段可有多个出生信号；一个 plan 可有多个出生阶段；出生阶段不必是首个阶段。每条出生信号各自是一条出生入口。
- **执行者**：出生阶段由静态 plain 执行者（individual/organization + supplierID）承接；出生权不随 dock 绑定或运行时 patch 漂移。
- **空订单上下文**：没有订单就没有 facts/files；否定条件为真，存在与延时成熟条件为假。出生阶段不需要 `receiveSignals` 才能出生；出生 hook 由出生信号合成（语言手册 §6.7）。
- **无分类**：同单 hook 与出生提交使用同一套表达式与适格规则；事实进入订单只有两条通道：同单 hook 消费订单内事实，静态 Dock 发射条件映射（§2.4）。

### 2.4 跨域：静态 Dock 声明与 binding

Dock 是 **B 侧声明的静态 A→B 时间窗订阅**：B 侧发布独立 `kind: Dock` 文档，声明"B 的某个 dock 执行者阶段的哪条输出信号，在 A 的哪条 source 上满足什么事实条件时发射"。本规格为 trigger 与 dock 的唯一权威出处；语言层字段细节见文法手册 §7。

1. **声明面**：Dock 文档字段 `spec.from{zhixu,source}` / `spec.to{zhixu,stage}` / `spec.signalMap`（B 输出信号名 → A 侧 `validWhen` 表达式）；`from.zhixu`/`to.zhixu` 是已发布定义的内容派生 uid（`zx-<32hex>`）；`to.stage` 以 `executor.supplierType: dock` 承接，其 `sendSignals` 承载输出信号、出生权由 plain 执行者阶段承载；`signalMap` 的 atoms 落在 `from.source` 的阶段的 `sendSignals` 上；同一定义内表达式只引用该 output 之外的 A 侧事实。编译产物为 `uvp.dock.v1`，身份 `dkid = zd-<32hex>` 内容派生（domain `uvp:dock-uid:v1`，剔除 `metadata.name`/`metadata.annotations` 后 canonical）。校验失败即响亮拒绝。
2. **dock 执行者阶段**：目标阶段以 `executor.supplierType: dock` 承接，并用 `receiveSignals` 声明绑定面；dock 执行者身份在运行期由订单在挂接 stage 一次性选定（stage patch），选中后固定，换选在运行期响亮拒绝。dock 阶段不进 plan 级执行者路由与候选集。
3. **binding**：`dock_link` 文件项（值 = A 订单号，canonical `0x`+64 hex）写入/改写即建立/切换 binding；一个（B 订单, 挂接 stage）至多一个当前 binding。链轨 `bindingId` 为 **7 个 32-byte word**：`keccak256(abi.encode("uvp.dock.binding_id.v1", dkid, toPlanId, toOrderId, toStageId, fromPlanId, fromOrderId))`，身份维度就是这 7 个 word；`dock_link` 的 `contentHash` 只在 poke 时认证载荷正文，不匹配即 `DockBindingContentHashMismatch`，binding 的事件、view 与事实 provenance 都以 bindingId 为准。
4. **A/B 窗口与发射**：A 窗口 = `signalMap` 表达式在 A 订单事实上的求值（锚点/延时语义同文法手册 §5.4）；B 窗口 = 输出信号 `validWhen` 在发射前的 pre-state 复核（文法手册 §5.6）。A/B 窗口交集时发射 B 输出信号；逐 output first-win（已存在的 B 事实不覆盖），发射事实的 provenance 携带 binding id。链轨 `pokeDockBinding` permissionless；窗口当前值由读面（`getStageDockBinding`/`dockOutputWindow`、`GET /v0/dock-bindings`）按当前时刻 live 求值，不持久化窗口历史。
5. **订单身份与 binding**：订单由出生通道创建、订单号恒由出生事实派生（§3）；Dock 在既有 B 订单上以 binding 承载发射，A 订单保持原身份。异常承接走治理分叉，与 dock 无关。
6. **回放**：binding 状态由（dock 执行者选择记录、`dock_link` 文件项、poke/发射事实）重建，不依赖链下持久 binding 表；回放必须收敛到同一 binding 集合与同一 B 事实集合。
7. **对等原则与单源**：dock 双方是秩序对等关系（§2.1 乐高原则）；一条 Dock 只引用一个 `from.source` 下的 atoms；终态只由本地阶段/订单结束驱动。

两轨对该语义的实现面差异见 §2.6；语法接受域差异见 `uvp-chain.md` 附录 B。

### 2.5 外部世界

- 唯一入口：执行器自发信号（str / canonical signal）；空订单号的出生提交同样是一条执行器签名的 signal。
- 外部事实名契约归 swagger。
- 外部事实没有绕过执行器的捷径：必须先成为某域的 canonical signal，才能被订单内 hook 消费或被出生管线判定。

### 2.6 两轨 Dock 错误面对照

同一错误在两轨的反馈形态系统性不同（云轨多类型化拒绝/读面诊断，链轨一律响亮原子 revert）。下表逐行从两轨实现（云轨 `/v0/dock`、`/v0/stage-patch`、`/v0/dock-bindings`；链轨 `UVPDockRegistry`/`UVPStagePatchModule`/`UVPStateMachine` revert 面）与上文条款核实——运维处置方向按本表读，不逐案读码：

| 错误/场景 | 云轨：反馈形态与处置 | 链轨：反馈形态与处置 |
|---|---|---|
| Dock 注册校验失败（from/to 未发布、output/atom 不属于声明集合、窗口指令非法、dkid 冲突） | `POST /v0/dock` 类型化拒绝：`DOCK_VALIDATION_FAILED` / `DOCK_FROM_ZHIXU_NOT_PUBLISHED` / `DOCK_TO_ZHIXU_NOT_PUBLISHED` / `DOCK_DKID_CONFLICT`，仅静态 Dock 表落库 | revert `DockFromPlanUnknown` / `DockToPlanUnknown` / `DockOutputCapabilityNotFound` / `DockAtomCapabilityNotFound` / `DockWindowInvalid` / `DockAlreadyRegistered` 等，整笔回滚 |
| dock 执行者选择冲突（同（订单,stage）换 dkid 再选） | `stage-patch` 返回 `DOCK_STAGE_EXECUTOR_ALREADY_SELECTED` | revert `StageDockExecutorAlreadySelected` |
| binding 载荷缺失（未写 `dock_link` 即 poke） | 读面保持无 binding（`dock_link` 未落 = 尚无关系） | revert `DockBindingPayloadMissing` |
| binding 载荷正文与 `contentHash` 不一致 | 文件 patch 的 `baseHash`/ops 校验拒绝（通用 stage-patch 错误面），binding 保持未建立 | revert `DockBindingContentHashMismatch` |
| binding 的 A 单号不属于 from plan / 与已选记录不一致 | 读面 `DOCK_BINDINGS_QUERY_FAILED` 诊断；读面返回空 binding | revert `DockBindingFromOrderUnknown` / `DockBindingFromOrderMismatch` |
| 窗口未满足（A 或 B 一侧为假） | 读面 `a_window`/`b_window` 的 `satisfied=false`；发射发生在窗口满足时 | poke 跳过该 output（无 revert；已满足的 output 同笔发射） |
| 重复 poke / 输出已发射 | 幂等：读面 `emitted=true`，发射保留首次事实 | 幂等：已存在 B 事实保留（first-win），poke 事件照常记录 |
| 发射写入的 capability proof 缺失/不匹配 | 云轨 dock runtime 按自身能力表校验并拒绝 | revert `DockOutputProofCountMismatch` / `DockOutputCapabilityNotFound` |
| 瞬时故障（RPC/进程/超时） | 读面重试；最新 `dock_link` 晚于最新 poke 时读面 `pending:true`（窗口按当前时刻 live 求值） | 交易原子：未上链即无效果；poke 重发由提交方按 gas 成本自行决定 |

---

## 3. 出生层（Birth）

链轨运行期共三条出生通道：outside-trigger（以出生信号开放提交）、order-link 派生建单、治理分叉 `forkOrder`。DSL 声明面只有出生信号一条：`sendSignals[].trigger: true`；Dock 协作在既有 B 订单上以 binding 承载发射（§2.4）。

| 通道 | 声明 | 机制 | 保证 |
|---|---|---|---|
| 出生信号（DSL 声明，两轨） | `sendSignals[].trigger: true`（唯一 DSL 出生声明点） | 空订单号 + 出生信号提交：从出生事实纯函数派生订单号，登记/创建订单，把该信号写为首条事实；幂等重试得到同一订单号 | **无需知情者的存在性**：任一授权提交者以同一出生事实均可幂等建单 |
| 链轨运行期出生通道（定义面不声明） | order-link 模块入口 / 治理分叉 `forkOrder` | order-link 派生建单；fork 从父单历史重放带血缘的新单（各自独立派生域） | 订单号由各通道纯函数派生、调用方不可自报；详见 `uvp-chain.md` |

- Dock 协作在既有 B 订单上以 binding 承载发射（§2.4）。
- `str` 是调用方约定的信号名；订单创建语义只由出生信号承载。
- 出生事实的派生元组显式包含可选的 `factKey`（保留后端既有 FactKey 重试语义），同一出生事实重试幂等。
- 一个出生事实至多派生一个订单号；跨出生信号的并发协调归调用方/执行者。
- 出生事实的溯源关系由事实自身携带的载荷与订单号派生承载；血缘边章唯一、first-win、落定后不可改写。

---

## 4. 语法表面

以下示例取自水果大亨演示（同一份 zhixu 定义）。示例中全部信号引用都在**同一 zhixu 定义内**——同 source 类与跨 source 类都是同秩序内的信号；跨秩序事实级联只有静态 Dock 一条通道（见 §2.4）。

```yaml
# 出生阶段：每次以空订单号提交 journey_started 即出生一单
- name: entry
  source: customer
  sendSignals: [{name: journey_started, trigger: true}, {name: str}, {name: cmp}, {name: err}]
  executor: { supplierType: organization, supplierID: journey-executor }

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

# 跨秩序：dock 执行者阶段（发射条件由独立 Dock 文档声明，见下）
- name: packing
  source: seller
  executor: { supplierType: dock }
  receiveSignals:
    START: "seller::packing.start"
  sendSignals: [{name: start}, {name: cmp}]
```

同一演示域的 Dock 文档（独立发布；uid 为示例占位，实际取已发布定义的内容派生 uid）：

```yaml
apiVersion: uvp/v0
kind: Dock
metadata:
  name: packing_service
spec:
  from:
    zhixu: zx-9c41f0d27a3e5b86c4d1af0b7e285c36
    source: seller
  to:
    zhixu: zx-9c41f0d27a3e5b86c4d1af0b7e285c36
    stage: packing
  signalMap:
    cmp: "seller::washing.cmp"
```

Stage 字段总表（当前态）：

| 字段 | 状态 |
|---|---|
| `source` | 保留：因果身份类（域内命名空间，多阶段共享） |
| `sendSignals[].trigger` | bool 出生标记：条目须 ∈ `sendSignals`、同阶段不重复；编译期固定并进 plan 承诺；出生阶段须静态 plain 执行者 |
| `receiveSignals` | 保留 map 形态；值为同单 hook |
| `sendSignals` | 保留，条目对象化 `{name, validWhen?, trigger?}`：`name` 必填非空、归一后不可重复；`validWhen` 是发射适格声明（过滤档钩子方言），条目数组——每项是完整的 `{source}::{condition}`、全体条目 header source 必须一致，缺键即无条件发射；语义权威见文法手册正本 §5.6，本规格不重述 |
| `executor` | plain：`supplierType` 为 individual/organization + `supplierID`；dock：`supplierType: dock`，以 `receiveSignals` 声明绑定面、执行者身份由运行期选定（§2.4） |
| `fileResources`、`selectedStages`、`fileWriters` | 保留 |

---

## 5. 形态对照（当前）

| 场景 | 表达 |
|---|---|
| 分馏（同源分叉，逐事实开单） | `sendSignals[].trigger: true` + 空订单号提交出生 |
| 撮合（并行输入驱动汇聚判断） | 订单内执行者写入多源事实 + 同单 hook 条件判定；配对/计数归执行器私有判断 |
| 收购回流 | 静态 Dock 订阅：`kind: Dock` 声明 A→B 发射条件（`signalMap`）+ 目标阶段 dock 执行者 binding；A/B 窗口交集发射 B 输出，父侧用同单 hook 消费回传事实 |
| 观察入口 | 同单 hook + 静态 plain 执行者 |
| 交易所开门/关门 | 执行者以 canonical signal 写入订单；订单内 hook 消费 |
| 委托 | 独立 `kind: Dock` 文档 + 目标阶段 `executor.supplierType: dock`；binding 由 `dock_link` 建立、dock 执行者一次性选择 |

---

## 6. 回放口径

- 回放基线：全部事实（订单内事实 + 出生提交事实）+ 不可变定义 + Dock 声明与 binding 记录（dock 执行者选择、`dock_link`、发射事实）。
- 出生信号：事实重放 → 纯函数派生订单号 → 同一订单集合，不漂移；同一出生事实重试不产生新单。
- hook 判决（普通表达式）：维持现有 hook_state 语义层（init/wait/ready/cxl，终态不可变）。
- 投递层（重试/退避/dead/复活）只服务普通执行者投递；dock 面不经 `ExecuteCommand`（由 Dock 声明与 binding 驱动），其错误面见 §2.6。

## 7. 版本与兼容

- 版本口径：协议制品统一为 `uvp.<artifact>.v<N>` 点号风格；语义/AST/语料/部署清单为 v1。当前语义批的产物面：`uvp.semantic.v1`、`uvp.cloudAst.v4`、`uvp.cloudArtifact.v8`、`uvp.zhixuAnalysis.v4`、`uvp.hookPlan.v7`、`uvp.onchainHookPlan.v7`、`uvp.dock.v1`、`uvp.dockBinding.v1`。同一语义只存在当前版本。
- 兼容矩阵 `uvp-stack.v1.json` 是当前版本真相：它钉住上述 artifactSchemas、合约 ABI fixture（stateMachine `0.19`、stagePatch `0.9`、lens `0.6`、dockRegistry `0.1` 等）、EIP-712 domains（productSubmit `0.17`、orderLink `0.8`、derivedSignal `0.6`、stageExecutorPatch `0.4`）和 `uvp-eth.addresses.v1`；合约 plan 承诺（publisher、hooksHash、capabilitiesRoot、executorCandidatesRoot、birthAuthorizer、deadline）与复合 `(planId, orderId)` 身份以矩阵和 fixtures 为准。
- 语法面只有 `sendSignals[].trigger`、同单 hook 与独立 Dock 文档声明。

## 8. 当前模型裁决

| 裁决 | 结论 |
|---|---|
| 出生声明点 | `sendSignals[].trigger: true` 是唯一 DSL 出生声明面；条目 ∈ `sendSignals`、无重复、编译期固定 |
| 出生通道 | 空订单号 + 出生信号出生；空订单号 + 非出生信号拒绝；带订单号的出生信号是普通信号；dock 不建单（链轨运行期另有 order-link/fork） |
| 出生适格 | 出生写入前在空订单上下文求值 `validWhen`；不通过即拒绝（链轨 `SignalAdmissionRejected`） |
| 订单号 | 由出生事实纯函数派生，调用方不可自报；派生元组包含可选 `factKey` 以保证重试幂等 |
| 出生执行者 | 必须静态 plain 执行者（非 dock、supplierID 非空）；出生权不随 dock 绑定或运行时 patch 漂移 |
| 同域事实连接 | 只用同单 hook：hook 在订单上下文求值，可引用本秩序任意阶段的信号 |
| 跨秩序 | 只有静态 Dock：独立 `kind: Dock` 文档 + 目标阶段 `supplierType: dock`；不新增 `parents` |
| 多出生信号 | 一个阶段多个出生信号、一个 plan 多个出生阶段均合法；出生阶段不必是首个阶段 |
| dock binding | `dock_link`（A 单号）写入即 binding；bindingId 由 7 个 word 的身份维度构成；A/B 窗口交集时发射，逐 output first-win（§2.4/§2.6） |
| 回放 | 给定全部事实 + 不可变定义 + Dock 声明与 binding 记录，重放收敛到同一订单集合 |
