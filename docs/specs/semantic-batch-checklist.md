# 语义批七落点 Checklist（模板）

> 状态：流程模板（零代码），v1
> 用法：每开一个语义批（任何 Hook DSL 语法/求值语义/语义语料/artifact 版本的改动），把本模板整节复制到批工作记录，开批照单执行、关批逐项打勾；未打勾项即批未收口。
> 定位：本 checklist 与 B13 清单指针机器互补——**checklist 防遗漏**（开批列出全部必须落点，关批逐项销号），**指针机器防失真**（清单「权威位置」指针做存在性对账）。两者缺一：无 checklist 则落点靠人记（daea1ea 批下游失真 12+ 处的教训），无指针机器则记下的位置悄悄失效（10.65 式失真 11 天无人知）。

## 批信息

- 批名/主题：
- 改动的语义点（一句话每条）：
- 是否触及承诺面（uid/planHash/hooksHash 等身份公式）：是 / 否
- 是否触及链上求值（合约求值器/注册门）：是 / 否
- 两轨同批对齐凭证（云轨批次 id / 链轨批次 id）：

## 七落点

### 1. core 实现 + 语料 + 金向量（uvp-core）

- [ ] Rust 实现（crates/ 对应模块）已改，`cargo check` + 触碰 crate 测试绿
- [ ] 语义语料 `fixtures/hook/semantics.v1.json` 补钉新语义的格子（evalCases/replayCases/invalidCases/delayLiteralCases 按需）
- [ ] `fixtures/hook/semantics.v1.meta.json` 的 contentSha256 已在同一提交重算更新（三线钉：Rust/TS/链轨 harness 任一检出旧语料都会红）
- [ ] 金向量（uid/planHash/hooksHash，单源 uvp-core/fixtures/commitments/golden-vectors.v1.json，uvp-core 根 `make golden-vectors` 重算→diff）已再生并 diff 确认。注意：该目标仅 uvp-eth 布局可跑（硬依赖姊妹仓 `../uvp-protocol` 的 TS 生成器；miniprogram 检出旁无该目录，目标会响亮报错退出）——须在 uvp-eth 检出执行后把再生文件同步到另一份 uvp-core 副本
- [ ] 语料 schema 若有扩展：两个 TS harness 消费线同批接线（不得只改 Rust 侧留旧线）

### 2. 语法册正本（uvp-core/docs/zhixu-dsl-grammar.md，单源）

- [ ] 正本 uvp-core/docs/zhixu-dsl-grammar.md 对应章节已更新（注意 §5.4/§5.6/§5.5/适格位否决域等历史易错半面）；两轨根下同名扉页不承载语义、无需随改
- [ ] 正本附录 A 需要补的接受域行已补；两轨接受域差异行补进 uvp-eth 根 `uvp-chain.md` 附录 B（不写运行时差异）

### 3. 云轨实现层文档

- [ ] uvp-cloud.md（或对应实现手册）端点/错误码/指标叙述已对齐
- [ ] 云轨若需实现跟进（Go 侧编译器/引擎），已在云轨工作区立同批条目并双向登记

### 4. 链轨 TS 承诺面（uvp-protocol）

- [ ] TS 编译器/校验器承诺面（重算、金向量消费、zhixu-loader）已同批对齐
- [ ] TS 侧 harness（semantics/lint/yaml parity 语料消费）接线并绿

### 5. 合约（若涉链上求值/注册门）

- [ ] 不涉链上求值则本项勾「不适用」并写明判定依据
- [ ] 涉及时：合约求值器/门已改，forge 测试随语义更新，EIP-170 字节码预算复核
- [ ] replay oracle（uvp-core crates/uvp-replay transition）与合约新语义同批对齐，链回放不假红
- [ ] fixture 再生（版本 bump）+ 兼容矩阵 `uvp-stack.v1.json` contractAbiFixtures 随迁

### 6. wiki（uvp-wiki）

- [ ] ZH 对应概念页（hook/signal/trigger/evaluation 等）已更新
- [ ] EN 对应页同批更新（EN 是历史孤儿高发面：版本矩阵、validWhen 页）

### 7. 规格 + 附录 B 差异行

- [ ] `docs/specs/subscription-mint-spec.md` 需要改写的条款已改（本规格是跨秩序协作唯一权威出处）
- [ ] 两轨运行差异需要登记的，登记到 uvp-eth 根 `uvp-chain.md` 附录 B（语法点接受域）或 wiki 运行差异页（运行时行为），不与 spec 抢权威

## 关批动作

- [ ] 双侧验证证据（两轨测试运行记录/链接）贴回批记录
- [ ] 上文全部勾选或写明不适用依据；任一未闭合项转台账并注明原因
