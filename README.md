# uvp-core

Shared Rust semantic core for UVP.

Exported surfaces:

- `uvp-hook-dsl`: Hook DSL parser, dependency extractor, and evaluator.
- `uvp-ffi`: C ABI for Go cgo callers.
- `uvp-node`: N-API module for Node/TypeScript callers.
- `uvp-cli`: command-line oracle for fixtures and CI.

The core focus is Hook DSL because it is the current
highest-risk semantic drift point between cloud UVP and EVM UVP.

The current delay contract is part of the shared semantic surface:

- `+<positive integer><unit>` is a postfix AST operator.
- Units are lowercase `s`, `m`, `h`, and `d`; every rule may choose its own value.
- Compiled Cloud AST delay nodes contain `rawDuration` and `durationSeconds`.
- Runtime evaluation uses the compiled AST and signal timestamps; Cloud adapters
  persist waits in `hookstatus` rather than creating one thread per wait.

See [`docs/specs/subscription-mint-spec.md`](docs/specs/subscription-mint-spec.md)
for the cross-order semantic contract and
[`/Users/uyhendu/project/miniprogram/uvp/zhixu-dsl-grammar.md`](/Users/uyhendu/project/miniprogram/uvp/zhixu-dsl-grammar.md)
for the Cloud-facing Zhixu DSL reference.
