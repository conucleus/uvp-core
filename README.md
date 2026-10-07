# uvp-core

Shared Rust semantic core for UVP.

Exported surfaces:

- `uvp-hook-dsl`: Hook DSL parser, dependency extractor, and evaluator.
- `uvp-ffi`: C ABI for Go cgo callers.
- `uvp-node`: N-API module for Node/TypeScript callers.
- `uvp-cli`: command-line oracle for fixtures and CI.
- `uvp-model`: shared definition object model (serde types for Zhixu/Supplier definitions).
- `uvp-ir`: canonical identifiers and UID domain primitives.
- `uvp-compiler`: definition-to-artifact compiler (hook plan / cloud artifact) behind the compile gate.
- `uvp-replay`: replay oracle family for chain-track and cloud-track evaluation diff.
- `uvp-mc`: exhaustive model checker for zhixu definitions.

`lint_zhixu` is a development-time diagnostics surface consumed via
`uvp-cli`/`uvp-node`; it is not part of the compile gate.

The core focus is Hook DSL because it is the current
highest-risk semantic drift point between cloud UVP and EVM UVP.

The current delay contract is part of the shared semantic surface:

- `+<positive integer><unit>` is a postfix AST operator.
- Units are exactly one lowercase character from `s`, `m`, `h`, `d` (seconds,
  minutes, hours, days); the integer is a positive literal — no sign, leading
  zeros, or fractions — and each delay operator chooses its own value (subject
  to the 30-day per-delay cap).
- Compiled Cloud AST delay nodes contain `rawDuration` and `durationSeconds`.
- Runtime evaluation uses the compiled AST and signal timestamps; Cloud adapters
  persist waits in `hook_state` rather than creating one thread per wait.

See [`docs/specs/subscription-mint-spec.md`](docs/specs/subscription-mint-spec.md)
for the cross-order semantic contract and
[`../uvp/zhixu-dsl-grammar.md`](../uvp/zhixu-dsl-grammar.md)
for the Cloud-facing Zhixu DSL reference.
