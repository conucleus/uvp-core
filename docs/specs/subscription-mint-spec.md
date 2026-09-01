# 订阅与铸单模型规格（Subscription & Mint Model）

> 状态：对齐基线（v1，替代 merge-anchor-delivery-spec.md）
> 语义版本：`uvp.semantic.v1`（上线前版本线整体重置为 v1：原 0.6→0.7 等开发期迭代编号全部作废，一次到位，不并存两套语义）
> 适用：uvp-core（Rust，DSL 语义唯一权威）、uvp（Go 云侧运行时）、uvp-protocol（TS 壳层）
> 合约边界：本期不改合约 ABI 与 EIP-712 typed-data；合约冻结侧仍含 op 5，工具链不再产出/校验该指令；订阅上链属未来协议扩展

---

## 0. 本文解决什么

旧模型的"跨秩序四典型"（分馏 `::OUTSIDE@`、撮合 `::MERGE@`、收购 `::ANCHOR@`、委托 signalMap）是同一根管道的四个命名捆绑包，每个捆绑包背着大量场景定语（回流方向、三段判定切分、k≥2 表达式下限等）。本规格把四典型收敛为三个正交层——**事实、路由、铸单**——并给出对应的语法表面。收敛后：

- 事实怎么进来：订阅（一种语法，两条路由规则）。
- 订单怎么出生：str 自报（免声明）或 `mint: per-fact` 代铸（唯一声明点）。
- 阶段种类：编译期定死、终生不可变。

旧关键字全部退役；externalSignals 删除；trigger 从每阶段必填入口表删除。合约冻结侧仍含 op 5，工具链不再产出/校验该指令。

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
- 订阅寻址 `@source::stage.signal` 只在本域解析。**信号层面不存在跨秩序源**：跨域唯一通道是委托 dock + signalMap（见 2.4）。
- 乐高原则：秩序之间无父子。被委托方天然存在，不因被委托需要父；可反向委托。无 dock 实例 = 无关系 = 不投递，这是"尚无关系"的正常态，不是孤儿。

### 2.2 订阅语法（receiveSignals 值）

| 表达式 | 语义 | 求值 |
|---|---|---|
| `{source}::{condition}` | 同单 hook（布尔/延时），现有语义不变 | 在订阅方自己的订单上下文内求值，判决一次（init/wait/ready/cxl） |
| `ANCHOR(@{source}::{stage}.{signal})` | 跨源订阅通道：按类寻址，逐事件投递、携带溯源 | 无表达式裁决；路由规则见 2.3 |

旧 `::OUTSIDE@` / `::MERGE@` / `::ANCHOR@` 标头、OUTSOURCE、k≥2 表达式下限、旧空标头白名单规则退役（新订阅必须空标头）。

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
- 同单 hook 表达式只能出现在有锚阶段（无锚阶段没有订单上下文可求值）。

### 2.4 跨域：委托 dock + signalMap

- 委托是一个秩序 dock 另一个秩序：A 的委托 stage 与 B 被 trigger 的入口 stage 在接缝处视为**同一个 source** 的两半；signalMap 是接缝上的对译表。
- 委托共享订单上下文（现有 zhixu 执行器 `NewSource=false` 通道不变）；事实经 signalMap 逐条映射回父阶段。
- 委托关系一次性绑定、禁 patch（现有门禁不变）。
- `rel_order_order` 语义从"父子血缘"改为**对接记录**（谁 dock 谁、映射实例、接缝两侧锚点）；表结构不变，写读两处语义与命名更新。按单路由以对接记录为落点。

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

---

## 4. 语法表面（目标）

```yaml
# 出生阶段：每事实即铸（原分馏）
- name: entry
  source: customer
  mint: per-fact
  executor: { supplierType: organization, supplierID: journey-executor }
  receiveSignals:
    JOURNEY_START: "ANCHOR(@fruit_merchant::stall_retail.retail.sold)"
  sendSignals: [str, cmp, err]

# 无锚监听：通道扇入（原撮合）
- name: exchange
  source: match
  executor: { supplierType: organization, supplierID: juice-market-executor }
  receiveSignals:
    SURPLUS_EVENT: "ANCHOR(@fruit_merchant::stall_retail.retail.surplus)"
    DEMAND_EVENT: "ANCHOR(@buyer::juice_demand.entry.requested)"
  sendSignals: [str, frozen, cmp, deal, err]

# 有锚阶段：按单路由（原收购回流；同 source 类存在 mint 声明即有锚）
- name: packing intake
  source: seller
  executor: { supplierType: organization, supplierID: fruit-merchant-executor }
  receiveSignals:
    FARMER_FRUIT_SETTLED: "ANCHOR(@farmer::farmer_orchard.packing.settled)"
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
| `executor` | 保留；委托（supplierType=zhixu + signalMap/triggerEntrance）原样 |
| `trigger` | **删除**（原必填入口表） |
| `externalSignals` | **删除** |
| `fileResources`、`selectedStages` | 保留 |

---

## 5. 六场景对照（旧 → 新）

| 场景 | 旧写法 | 新写法 |
|---|---|---|
| 分馏（汽油/顾客路线） | `::OUTSIDE@(源::t.s.sig)` | 订阅 + `mint: per-fact` |
| 撮合（k≥2 配对） | `::MERGE@(a::…, b::…)` | 无锚监听 + 多条 `ANCHOR(@…)`；配对后执行器 str 多父 |
| 收购回流 | `::ANCHOR@(裸三段)` | 有锚阶段 + `ANCHOR(@…)`（按单路由） |
| 观察入口（k=1） | k=1 MERGE | 无锚监听 + 单条 `ANCHOR(@…)` |
| 交易所开门/关门 | 无（外部 trigger + 载体单） | match source 上一个发开门/关门事实的 stage，通道锚定 |
| 委托 | signalMap + triggerEntrance | **不变** |

---

## 6. 回放口径

- 回放基线：全部事实（订单锚定 + 通道锚定）+ 不可变定义 + 对接记录。
- 代铸订单：事实重放 → 纯函数派生 ID → 同一订单集合，不漂移。
- hook 判决（普通表达式）：维持现有 hook_state 语义层（init/wait/ready/cxl，终态不可变）；订阅通道不经判决层，事实→路由→投递直通。
- 投递层（重试/退避/dead/复活）机制原样，适用于订阅通道投递。

## 7. 兼容与退役

- 版本 slate 重置（2026-08-31 裁决）：协议制品统一为 `uvp.<artifact>.v<N>` 点号风格且全部置 v1——`uvp.semantic.v1`、`uvp.cloudAst.v1`、`uvp.hookSemanticsCorpus.v1`（语料文件同步更名 semantics.v1.json）、`uvp.cloudArtifact.v1`；部署清单 `uvp-eth.addresses.v5` → `uvp-eth.addresses.v1`。开发期累积的 0.7/v2/v5 编号无兼容义务，作废。
- 兼容矩阵 `uvp-stack.v1.json` 等值断言同步重置（semanticVersion、hookCorpusSchema、cloudAstSchema、deploymentManifestSchema）。
- 合约 ABI fixture 与 EIP-712 typed-data 不动；云侧信封键字段与链上既有 `idempotencyKey` 语义对齐。
- 旧关键字（OUTSIDE/MERGE/ANCHOR 标头、OUTSOURCE、trigger 入口表、externalSignals）在两侧代码、语料、文档中清零（退役说明除外）。

## 8. 决策记录

| 决策 | 结论 | 依据 |
|---|---|---|
| 三种接收方是否可变 | 编译期定死、不可变（选项 A） | 路由语义单一可静态验证；生殖是入口行为不是流程行为；同单生殖走执行器 str |
| 铸单标记命名 | `mint: per-fact` | 语义是铸单策略，不是入口激活 |
| 链侧切换 | 一次到位，删四典型关键字 | 四类在业务模板与链上近零使用；干净切分优于并存 |
| 血缘闸门 | 不再是过滤开关，而是域内路由规则 + 域作用域本身 | "只有我的农户"由按单路由与 zhixu 局部命名空间免费获得 |
| 锚定依据 | mint 声明是编译期唯一锚定依据 | 自发 str 编译期不可见；订阅方按溯源分拣是执行器责任 |
| 孤儿 | 概念删除 | 订单天然存在，无 dock = 尚无关系，非异常态 |
| 版本 slate 重置 | 全部协议制品版本置 v1、统一 `uvp.<artifact>.v<N>` 点号风格；合约 ABI/EIP-712 冻结侧不动 | 未上线无兼容义务，累积编号（0.7/v2/v5）是噪音；矩阵等值断言兜底漏网 |

### 补充决策（2026-08-31，安全架构审查后）

| 决策 | 结论 | 依据 |
|---|---|---|
| 链下身份归属 | 身份、资质、审核归**秩序商店**（商店语境可称"平台"）；UVP DSL 只承载几何拓扑 + 事实纪律；链下身份与链上身份在商店汇合 | 担保交易秩序本身可上架商店供挑选；引擎不新增身份系统 |
| 裸跑形态 | 产品接受 UVP 裸跑：即使黑产裸跑，每个环节谁是谁靠事实留痕可审计；合法生意（如出口小汽车）走官方秩序商店获得受控管理 | 引擎层不强设身份门；留痕 + 凭据分层是信任模型 |
| /signal 凭据 | per-sender 密钥表已落地（未建档回落共享密钥）；验签主体归真注入信封（自报与主体不符即 401）；裸跑形态启动响亮告警、保留自报 sender 留痕 | 一把共享密钥泄露即全线；归真后记录里的"谁发的"来自凭据 |
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
| 模-1 静态执行者 | 出生/订阅阶段必须编译期静态绑定非委托 executor；运行时 patch 一律拒绝（既有门禁不变） | Go validator 去豁免（uvp f724212 之后批次）；uvp-core validate_mint_anchors 增查；bootstrap child.main 前置注册静态执行者、register_select 撤销对该阶段的 patch |
| 模-2 出生入口组成 | 出生入口只能是 ANCHOR 订阅；"订阅之外附加单正普通 hook"形态废除 | Go validateMintStages + zhixu_schema、uvp-core validate_mint_anchors 三处拒绝；TS 测试对齐 |
| 模-3 域边界 | 域 = zhixu 实例。订阅按类匹配只在本实例内解析；跨秩序扇入要求 rel_zhixu_dock 显式对接（双向记录，compiler 新增 POST /zhixu-dock 登记，dbops.RegisterZhixuDock）。依赖按秩序 id 显式绑定（委托接缝/同单锚定）不受 dock 门限制 | uvp core-ddl + loadAffectedHooks + 契约测试 |
| 事实标签 tie-break | hook_state.id 与 hook_delivery.id 改从共享序列 fact_label_seq 取值，标签对全部输出事实严格全序 | core-ddl |
| nonce 防重放 | 接受现状（first-win 幂等吸收），nonce 查重/升级 JWT 随商店身份落地一并做 | 决策记录 |
| DLQ 通知可靠性 | 告警语义走指标（stmDLQTotal 告警规则），持久化重投等运维真消费 DLQ 时再建 | 决策记录 |
| chain-services 暴露面 | 模-5 修正"暂不整改"的前提：CORS 默认关闭（UVP_API_CORS_ALLOWED_ORIGINS 白名单回显）；notification-profile 挂 store.supplier.notification_profile.update；管理员白名单（GOVERNANCE_ADMIN_REVIEWER_IDS）真接入鉴权。身份归商店的裁决不变 | chain-services 本批次 |
| 合约解冻批次（窗口已开） | #1 派生信号 capability 对称：跨订单派生要求目标（origin）订单 plan 声明同一 capability（审计修复方向 a）；#31 同 hook 输入内 dependencyKeys 去重；#30 README 口径改为"patch 即时接管、不可回滚恢复执行者"。#10 (planId, orderId) 复合键涉及全部模块/periphery 的订单寻址迁移，作为解冻窗口的下一个独立批次 | contracts 本批次 + forge 86/86 |

#10 残余风险说明：capability 对称后，攻击者理论上仍可镜像目标 plan 的 capability 声明（plan 公开可读）；该残余与 #10 的订单寻址迁移一并在解冻窗口下一批次处置（选项：origin 侧 link 授权）。

### 裁决落地（2026-09-01，商店=框架不=内容）

| 裁决 | 结论 | 落地 |
|---|---|---|
| 商店=框架，不=内容 | 商店（zhixu-store）类比 Shopify 只提供框架：任务字段集、证据要求、提交流程由**凝结核**（zhixu 的发布者/所有者）自己配置，作为**数据**随 zhixu 带进来；商店核心代码不得出现任何具体业务的字段名、中文标签匹配表或文件格式特判。此前商店把某个具体 zhixu 的特例（报关）当成了示例写进核心，属于写多了 | protocol 新增 `ProductTaskDTO.evidenceSpec` 加性可选字段（`{key, label, inputKind?, accept?, required?, description?}`，schema 保持 `uvp.productDto.v1`）；store workbench 改为 schema 驱动渲染，spec 缺失时降级为通用上传槽位（文件+可选文本说明），未知声明不上传前拒绝、也不静默丢弃 |
| 报关特例降级为演示配置 | 共享 demo 任务里的"报关单 PDF、报关单号、出口港口、完成时间"等特例内容从商店核心代码移除，降级为一份显式的演示配置数据（形态上等同"某凝结核自带配置"），只经通用渲染路径生效；商店核心代码 grep 不到这些业务字符串（演示配置文件与其测试除外）。MVP 不内置报关示例 | store `src/product/demo/customs-demo-config.ts`；protocol fixture `demoCustomsEvidenceSpec` 同形示例 |
| 证据文件格式校验归属 | accept 约束来自凝结核配置（`spec.accept`）；前端按 accept 校验并在 accept=pdf 时读取文件首字节做 %PDF- 快速拦截（防伪造 MIME/扩展名），服务端魔数校验仍是权威 | store workbenchSupport `validateEvidenceFileForSlot` |
| DTO 兼容口径 | `evidenceSpec` 为加性可选字段：不改变 `requiredEvidence` 开放字符串数组的既有语义，不破坏既有消费方；消费方在字段缺失时必须走降级路径而不是报错 | protocol freeze 校验（product signal map gate + verify-stack-compatibility）exit 0 |

