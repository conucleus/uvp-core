# uvp-core

Shared Rust semantic core for UVP.

Initial exported surfaces:

- `uvp-hook-dsl`: Hook DSL parser, dependency extractor, and evaluator.
- `uvp-ffi`: C ABI for Go cgo callers.
- `uvp-node`: N-API module for Node/TypeScript callers.
- `uvp-cli`: command-line oracle for fixtures and CI.

The first cut intentionally focuses on Hook DSL because it is the current
highest-risk semantic drift point between cloud UVP and EVM UVP.
