# 两轨接受与机制对照矩阵

本文件是云轨（`uvp`）与链轨（`uvp-eth`）对同一份 Zhixu/Dock 定义在**编译接受域、身份模型与运行机制**上的唯一对照出处，由两轨共享的 `uvp-core` 单源维护。

> **分类前缀说明**（凡存在两轨差异的行必须标出，无前缀 = 两轨同口径、无取舍差异）：
> - `〔范围裁决〕`：产品或架构范围上的取舍，非技术不可能，解除条件为协议演进。
> - `〔链物理〕`：区块链环境本身的物理限制（如区块时间戳、无求值超时、交易原子性）。
> - `〔gas/DoS 边界〕`：链上经济与防攻击上限，数值可随版本调整。
> - `〔实现不对称〕`：同一语义下两轨具体的工程实现方式不同（如错误码、去重键、时钟域或过滤容错）。

---

## 1. 两轨对照矩阵

| 语法/机制点 | 云轨（uvp） | 链轨（uvp-eth） | 跨轨后果 |
|---|---|---|---|
| `〔范围裁决〕` `spec.platform.type` / `.network` 与跨轨移植 | 声明 `cloud` 等运行域；`spec.platform` **不参与** `definitionUid` 计算 | 声明 `blockchain` 等运行域；`spec.platform` **不参与** `definitionUid`，但进入链轨 `planHash` 计算 | 同一份定义把 `cloud` 改成 `blockchain` 或改 `network` **不会改变 UID**，Dock 里的 `from.zhixu`/`to.zhixu` 引用无需更新；链轨 plan 身份会随 platform 改变，需重新生成链轨产物 |
| `〔范围裁决〕` `kind: Supplier` 注册对象 | 接受（云轨独有运行时注册对象，见 `uvp-cloud.md` §1） | 不存在该对象（供应商档案/KYC 属 Store 链下数据） | 链轨不消费 `kind: Supplier` YAML；供应商档案归链下 Store 管理 |
| `〔范围裁决〕` 供应商身份注册与三类 Registry 分工 | 云侧 `local` supplier registry 维护运行时对象与回调 handler | 分工明确的三件套：<br>1. `UVPSupplierRegistry`（链上）：供应商自注册产生 `pending` SID（`bytes32` UUID），registrar 激活为 `active` 后才可参与 plan 绑定、patch/dock 指派与发信号；SID↔account 永久 1:1 绑定，无轮换、revoke 不释放地址；<br>2. `UVPIdentityRegistry`（链上）：只做线下主体与链上账户的绑定/撤销存证，产品面作信任背书，不参与状态机发信权校验；<br>3. `zhixu-registry`（链下库）：非用户面的 permissionless 数据层，只存 `ZhixuRecord`/`IdentityClaim`，登记不等于背书 | 两轨身份底座不同：云侧靠运营方库内档案，链侧靠 `UVPSupplierRegistry` 强注册 + `active` 校验；`UVPIdentityRegistry` 与 `zhixu-registry` 不承担状态机发信权 |
| `〔范围裁决〕` 供应商状态映射 | 支持 `active` / `disabled` / `revoked` | 支持 `pending` / `active` / `suspended` / `revoked` | 状态映射：`active` ↔ `active`、`disabled` ↔ `suspended`、`revoked` ↔ `revoked`；链轨的 `pending`（自注册待激活态）在云轨无对应状态 |
| `sendSignals` 裸信号名 | 接受 | 接受（编译为 `relation=current` 同单能力） | 两轨完全一致，无跨轨差异 |
| `sendSignals[].validWhen`（发射适格声明） | 接受（过滤档解析进 `admissions` 数组，入口求值不通过返回类型化非重试拒绝） | 接受（折叠为 AND 指令计划、带 `HOOK_FLAG_ADMISSION` 位 8 进 `hooksHash`；不通过 revert `SignalAdmissionRejected`） | 语法与静态校验两轨一致；但见下方「Dock 内部 output 过 `validWhen` 筛」的实现不对称 |
| 阶段出生信号（`sendSignals[].trigger: true`） | 接受（`triggers` 投影进云产物；空 `order_id` + 出生信号建单） | 接受（每条出生信号合成 `SIGNAL` + `ORDER_TRIGGER_MINT / EMIT_READY` hook） | 两轨同语义：出生信号是唯一 DSL 出生声明面 |
| 出生信号的出生适格（`validWhen`） | 接受（出生前按空订单上下文求值；不通过为类型化非重试拒绝） | 接受（创建订单前按空订单上下文求值；不通过 revert `SignalAdmissionRejected`） | 两轨同口径：出生写入与普通信号同过适格面 |
| `〔实现不对称〕` 出生订单号与幂等键 | 运行期由 `gen_random_uuid()` 随机生成 `order_id`；出生请求**必填** `fact_key`，按 `(zhixu_id, sender_id, fact_key)` 去重归并到既有订单 | 运行期由合约 `_newOrderId`（nonce 递增）生成 `orderId`；出生请求**必填** `idempotencyKey`，按 `(submitter, idempotencyKey)` 去重 | 两轨都已改为「运行期生成订单号 + 出生必填显式幂等键」，不再由载荷哈希派生订单号；跨轨差异在于去重作用域与重放响应（见下一行） |
| `〔范围裁决〕` at-least-once 重试对「出生重放」的响应 | 静默幂等吸收：同一 `(zhixu_id, sender_id, fact_key)` 重试返回原 `order_id`（`duplicate=true`） | 分入口处理：`triggerOrderFromOutsideFor` 重复命中 `(submitter, idempotencyKey)` 时 revert `DuplicateTriggerKey`（携带 `existingOrderId`）；`birthAuthorizer` 合约直接入口同 order+signal **no-op 吸收**；普通重复事实 `submitSignal` **first-win 幂等吸收** | 重试方在两轨拿到的响应面不同（云轨返回原单号成功，链轨外部签名入口 revert 但携带 `existingOrderId`），调用方不能假设错误面同构 |
| `〔范围裁决〕` 请求级鉴权门与发信权 | HTTP 入口（`/v0/signals` 等）挂**全局/部署级 HMAC 验签**（`httpAuth.sharedSecret` 或 `httpAuth.senders` 密钥表），校验请求的 `sender_id` 与时间戳/nonce，**不按阶段执行者（per-stage executor）鉴权** | 只认 source 阶段的 `assigned supplier account`（active patch → 计划 default → dynamic 未指派为 0），经 `UVPSupplierRegistry.accountOf` 实时解析且 SID 必须 `active`；支持 EIP-712 签名代付 `submitSignalFor`（EOA ECDSA / 合约 EIP-1271，`msg.sender` 仅作 relayer） | 云轨入口是全局服务级 HMAC 鉴权，链轨入口是阶段级强绑定钱包签名；跨轨叙事与客户端不得假设存在同一张阶段授权表 |
| `〔范围裁决〕` 订单级信号授权 `SignalAuthorization` | 云轨保留订单级事实/权限记录 | 已彻底删除（相关结构、存储、事件与 `authorizations` 参数全部移除），发信权完全由 `assigned supplier account` 承载 | 链轨不存在订单级 submitter allowlist，对账与 SDK 不得再引用该事件面 |
| 静态 Dock 注册与 binding 建立（`kind: Dock`、`dock_link`、poke） | `/v0/dock` 注册静态 Dock；选中 dock supplier、写 `dock_link` 后按 A/B 窗口 live 求值发射 | `UVPDockRegistry.registerDock`（dkid=`zd-<32hex>`）校验 A/B capability proof；`selectStageDockSupplier` 一次性选定 supplier；`dock_link`（A 单号）确定 binding（7 word `bindingId`），`pokeDockBinding` 逐 output first-win 发射 | 语义同构，核心差异在窗口时钟域（DB 时钟 vs `block.timestamp`）与簿记/授权门（见下行） |
| `〔范围裁决〕` Dock 交付簿记、窗口历史与授权门 | 读时 live view 派生（binding 不落库、无持久窗口历史）；单运营方信任域内鉴权（选中即授权） | 链上永久事件（`DockBindingPoked`/`DockOutputSubmitted`），历史窗口时间不上链；须由 selector 阶段调用 `selectStageDockSupplier`（或携 EIP-712 签名 `selectStageDockSupplierFor`）一次性选定且不可换选；写 `dock_link` 后任何人可 poke | 历史窗口不可直接对比，对账需按事件 + 当前窗口；信任模型差异（运营方鉴权 vs 链上 selector 签名门）须显式披露 |
| `〔实现不对称〕` Dock 内部 output 过 `validWhen` 筛的现行行为 | 内部 dock output 发射目前仍会过目标 `validWhen` 筛；不满足时报错并按投递策略重试 | 内部 dock output 发射目前也会过目标 `validWhen` 筛；不满足时通过 `return false` **静默跳过**该 output，不 revert 整笔 poke 交易 | 属已知实现分歧（理想设计为「内部事实不过适格筛」）：云轨表现为报错重试，链轨表现为单条静默跳过（待后续统一） |
| `〔范围裁决〕` 零 `receiveSignals` 且零出生信号的阶段（仅 `sendSignals`） | 接受（云产物无链上物化面，该阶段无 hook） | 拒绝（阶段物化门：零 hook 永不可物化，由 `uvp-core analyze` 校验） | 云轨接受、链轨拒绝；该形态在链上没有物化与恢复路径 |
| 出生集非空阶段的成员与执行者校验 | 拒绝非法形状（共享 `uvp-core analyze` 校验） | 拒绝非法形状（同一共享 `uvp-core analyze` 校验） | 两轨同拒（单源共享校验，非轨差异） |
| 多出生信号 / 与 dock 并存 | 接受（多条出生信号合法；dock 是 binding，不建单） | 接受（多条 trigger hook 合法；dock 阶段带 dock 标记，不建单） | 两轨同口径：读面必须显式枚举/选择出生线 |
| `〔范围裁决〕` dynamic 阶段（无 `executor` 块、仅被 `selectedStages` 覆盖且声明 `receiveSignals`） | 接受（云 runtime 按云侧执行者解析承接） | 接受（须被静态执行者 `selectedStages` 可达；hook 带 `HOOK_FLAG_DYNAMIC_EXECUTOR` 位 32；运行期由 selector 在首信号前指派 `supplierId`，未指派 revert `StageSupplierNotAssigned`） | 两轨都接受该定义形态；运行期指派机制不同（云侧动态配置 vs 链上强注册 + 首信号前指派门） |
| 同一 signalKey 被多个阶段/hook 观察 | 接受（依赖索引多对多，无阶段 owner） | 接受（依赖索引多对多，各阶段按自身物化位独立物化并宣告 `HookReady`） | 两轨同口径：多观察完全合法 |
| `〔gas/DoS 边界〕` 依赖键总数 / 单键 hook 扇出 | 无此校验（云产物不设上限） | TS 预检 + 合约双闸（依赖键总数 ≤ 1024、单键 hook 扇出 ≤ 16） | 规模上限是链上防 DoS 成本边界，不改变语言语义 |
| `〔范围裁决〕` `spec.stages[].fileWriters`（跨阶段文件写授权） | 接受：stage 级 allowlist（target 声明谁可写；self 隐式），由请求 bearer 身份 + allowlist 校验 | 接受：折叠为 `UVP_FILE_WRITER_V1` capability 叶进 `capabilitiesRoot`，由 writer 阶段 `assigned supplier account` 提交；链轨 artifact 另约束每阶段 ≤ 8 | 允许名单语义同义；链轨受 artifact 容量 ≤ 8 限制（非共享 DSL 语义） |
| `〔实现不对称〕` 文件 patch 语义与存储（`add`/`replace`/`remove` + `baseHash`） | 原子应用到 `order_stage_record.file_resources`（JSONB），可带内容；`baseHash` 为规范化 JSON 的 **sha256** | `UVPFileResourceModule` 只存 `contentHash + handle`（正文不上链）；`baseHash` 为阶段 `filesHash`（rolling **keccak256**） | 跨轨不得互认 `baseHash`，也不得假设链上可读取文件正文 |
| `〔范围裁决〕` 文件 patch 的提交面（direct + 纯 patch） | 无链上签名域；信号信封直接携带 `fileResources` | `submitSignal` / `submitSignalFor` 均接受尾部 `(filesDigest, filePatch)`；纯文件 patch 用 `signalId=0, sourceId=0, payloadHash=0`，不计业务信号、不跑 admission/hook | 链轨支持纯文件 patch 入口与代付；两轨均不让纯 patch 触碰业务信号计数 |
| `〔实现不对称〕` `uvp.cloudArtifact.v1` 的 `fileWriters` | v1 内可选 additive 字段（仅非空 stage 输出） | 链轨不消费该 artifact（使用 `uvp.onchainHookPlan.v1`） | 消费方须容忍缺省，缺省即「无跨阶段写授权」 |
| `〔实现不对称〕` 批量信号提交 | HTTP `/v0/signals` **不提供批量提交**，仅支持单条提交 | 单笔合约交易可原子发射多条信号；任一条 `validWhen` 不通过则**整批 revert**，单条事实仍按 first-win 吸收 | 两轨提交粒度不同：云轨单条独立提交，链轨可借助 EVM 交易原子性打包多条发射 |
| `〔实现不对称〕` 嵌套延时形态的裁决 | 编译期拒绝（`uvp-core` 解析层 `reject_nested_delay`） | 编译期拒绝（同左）+ `commitPlan` 链上注册门再拒（手工 plan 防线） | 正规编译器路径两轨同拒；链轨额外对绕过编译器的手工 plan 兜底 |
| `〔实现不对称〕` YAML 裸标量装载 | Go 装载：`Y`/`yes`/`on`→布尔、`010`→8、`1_000`→1000 | TS 装载：`010`→10、其余按字符串 | 同一未加引号的 YAML 跨轨可能派生出不同 UID（Rust 正本按 `"010"` 字符串）；所有标量值位一律加引号是作者纪律 |
| `〔链物理〕` 成熟裁决边界 | `(due, due+1ms]` 闭区间宽限：窗内到达的负事实仍参与裁决、判负赢（DB 时钟域） | `block.timestamp` 秒域、同秒内按块内交易序裁决，无毫秒宽限 | 负事实恰在 `due` 后瞬间到达时两轨结果可能分叉 |
| `〔链物理〕` 求值超时熔断（`poison` 终态） | 有：FFI 连续求值超时熔断为 `poison`，人工 revive 是唯一出口 | 无：链上交易无求值超时熔断概念 | 跨轨对账时须剔除云轨 `poison` 态订单/钩子 |

---

## 2. 链轨运行机制摘要（Dock 与文件 Patch）

### 2.1 Dock 注册与 Binding（`UVPDockRegistry` / `UVPStagePatchModule`）

- **注册（permissionless）**：`registerDock(fromPlanId, fromSourceId, toPlanId, toStageId, toSourceId, docHash, outputs[])` 逐 atom/output 验证 A/B plan capability Merkle proof（不通过 revert `DockOutputCapabilityNotFound` / `DockAtomCapabilityNotFound`）。`dkid = bytes32(bytes16(docHash))`（即 `zd-<32hex>`），重复注册 revert `DockAlreadyRegistered`。读面 `getDock` / `dockOutputCount` / `getDockOutput` 入参为 **dkid word**。
- **选定 dock supplier（一次性）**：由 selector 阶段调用 `selectStageDockSupplier`（或携 EIP-712 签名的 `selectStageDockSupplierFor`），携带 `{selectorStageId, targetStageId, dockId, supplierId, patchNonce}`。合约校验 `HOOK_FLAG_DOCK_EXECUTOR`、selector 绑定、SID `active` 及关系一致性；同 `dkid+supplierId` 重放幂等吸收，换选 revert `StageDockSupplierAlreadySelected`。选中 SID 的 supplier account 即后续 output 写入的 submitter。
- **binding 身份与窗口发射**：`bindingId = keccak256(abi.encode(DOCK_BINDING_ID_DOMAIN, dkid, toPlanId, toOrderId, toStageId, fromPlanId, fromOrderId))`（7 word，不含 payload hash）。`dock_link` 文件项的 `contentHash` 仅在 `pokeDockBinding` 时核验载荷正文（不匹配 revert `DockBindingContentHashMismatch`）。`pokeDockBinding` 按 `block.timestamp` 求 A 窗口并复核 B 侧 `validWhen`，逐 output first-win 发射（永久事件 `DockBindingPoked` / `DockOutputSubmitted`），任何人均可 poke。

### 2.2 阶段文件映射与文件 Patch（`UVPFileResourceModule`）

- **职责与键值**：文件映射归 `UVPFileResourceModule` 管理，核心 `UVPStateMachine` 仅作为唯一信任入口转发调用。键值结构为 `key -> (contentHash, handle)`（`handle` ≤ 512 字节，正文不上链）。**链上不定义 `fileType` 闭集**，业务类型由应用层自定义。
- **原子操作与无 nonce**：支持 `add`（键不存在）、`replace`（键存在）、`remove`（键存在），任一失败整笔回滚；`baseHash` 必填且须等于当前阶段 `filesHash`。文件映射无 patch nonce，重复 `patchHash` 按 first-win 吸收；**纯文件 patch 不计入业务信号计数、不跑 admission、不触发 hook**，且不受 `stage supplier patch` 首信号时序门（`StageAlreadyHasSignal`）约束。
- **写权与事件**：写权主体为 writer 阶段的 `assigned supplier account`；跨阶段写入需额外提供 target 阶段 `fileWriters` 的 `UVP_FILE_WRITER_V1` capability proof（失败 revert `FileWriterUnauthorized`）。每笔落账发 `StageFilePatchApplied` 与逐 op `StageFileOpApplied` 事件供索引器重放。

> **手工 plan 作者自负（两轨通用纪律）**：编译器是唯一正规入口。绕过编译器手工构造的 plan 不受编译期语义门保护，缺陷会在运行期才暴露（例如链轨手工构造「复合出生钩子」在出生时先被判 Cancelled，导致 `InvalidTriggerHook` 整笔回滚）。手工构造 plan 或 artifact 的一切后果由作者自负。

---

## 3. 待办事项

以下跨仓与对齐事项已登记为后续任务，不在本轮修改：

1. **云轨版本镜像同步**：`uvp-utils` 中的 `SemanticVersion` 与 `CloudArtifactSchema` 镜像须与本矩阵保持同步（`pnpm verify:stack-compatibility` 会校验 sibling checkout 的 `pkg/version/version.go`，缺失即 fail-closed）。
2. **`fileWriters` 共享 DSL 约束**：引用闭集、不可自指、去重等校验须收敛进 `uvp-core/protocol/uvp-constraints.v1.json` 共享校验，链轨 artifact 的每阶段 ≤ 8 capacity 约束归链轨保留。
3. **内部 Dock output 过滤分歧收敛**：当前两轨在 Dock 内部 output 发射时仍会过目标 `validWhen` 筛（链轨静默 `return false` 跳过，云轨报错重试），与「内部产事实不过适格筛」的理想设计存在偏差，待后续排期统一收口。
