.PHONY: golden-vectors

# 承诺面金向量再生成 + 漂移闸：生成权威在链轨 TS compiler（姊妹仓
# uvp-protocol），文件钉在本仓 fixtures/commitments/——Rust（uid 格）与
# Foundry CommitmentGoldenVectors.t.sol 消费同一份。重生成后文件有未提交
# 变更即失败，提示审查后提交；git diff 只读，不改写任何状态。
golden-vectors:
	@if [ ! -d ../uvp-protocol ]; then \
		echo "error: sibling checkout ../uvp-protocol not found — run from a uvp-eth workspace layout" >&2; \
		exit 1; \
	fi
	cd ../uvp-protocol && pnpm --filter @uvp-eth/compiler generate:commitment-golden-vectors
	@if ! git diff --quiet -- fixtures/commitments/golden-vectors.v1.json; then \
		echo "金向量有漂移，审查后提交（fixtures/commitments/golden-vectors.v1.json）"; \
		git --no-pager diff --stat -- fixtures/commitments/golden-vectors.v1.json; \
		exit 1; \
	fi
	@echo "golden vectors up to date"
