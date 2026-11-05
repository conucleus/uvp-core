# uvp-core

Shared Rust semantic core for UVP.

Exported surfaces:

- `uvp-hook-dsl`: Hook DSL parser, dependency extractor, and evaluator.
- `uvp-ffi`: C ABI for Go cgo callers.
- `uvp-node`: N-API module for Node/TypeScript callers.
- `uvp-cli`: command-line oracle for fixtures and CI.
- `uvp-model`: shared definition object model (serde types for Zhixu/Supplier definitions).
- `uvp-ir`: canonical identifiers and UID domain primitives.
- `uvp-compiler`: shared definition validation and neutral analysis (`analyze`
  target, `uvp.zhixuAnalysis.v1`), dock declaration/linker, and the transitional
  cloud artifact (`compile_cloud_artifact`). For the chain target, `analyze` also
  runs the definition-level acceptance checks (implicit-entry recognition and
  invalid-shape rejection, stage materialization gate) and returns the parsed
  entry hook, signal capabilities, and composed admissions. It does not lower EVM
  artifacts: the chain hook-plan shell (entry hook synthesis, hook decoration,
  dependency index, executor routes) and on-chain artifact are owned by
  `uvp-protocol/packages/compiler` (TypeScript), which consumes `analyze` and
  does not re-run those checks.
- `uvp-replay`: replay oracle family for chain-track and cloud-track evaluation diff.
- `uvp-mc`: exhaustive model checker for zhixu definitions.

`analyze` is the neutral compile-request entry (shared validation — including
the chain target's implicit-entry recognition/rejection and materialization
gate — plus parsed hooks, entry hook, signal capabilities, composed admissions,
and dock state); `cloud`/`cloud_db` remain for the cloud track. The legacy
`parse` name is kept only as an `analyze` alias with `allowUnresolved` for the
single cloud-side FFI test; it is not a supported surface and cloud code does
not change for it.

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
[`docs/zhixu-dsl-grammar.md`](docs/zhixu-dsl-grammar.md)
for the canonical Zhixu DSL reference (shared by both tracks).
