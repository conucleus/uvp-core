# 约束注册表 harness（crates/uvp-compiler/tests/constraints_registry.rs）
# 的单源在兄弟仓 uvp-protocol：作为 miniprogram 子模块检出时祖先链上没有
# 该仓，harness 的逐级向上默认寻径落空，本目标注入路径使开箱可跑
#（uvp-eth 独立检出布局则直接命中默认寻径，无需本目标）。
UVP_CONSTRAINTS_PATH ?= /Users/uyhendu/project/uvp-eth/uvp-protocol/protocol/uvp-constraints.v1.json

.PHONY: test-constraints
test-constraints:
	UVP_CONSTRAINTS_PATH=$(UVP_CONSTRAINTS_PATH) cargo test -p uvp-compiler --test constraints_registry
