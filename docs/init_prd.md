# UVP Core Init PRD

## 1. Background

UVP currently has two implementation tracks:

- Cloud UVP: Go runtime for controlled single-jurisdiction deployments, backed by PostgreSQL, workers, triggers, and service-side orchestration.
- EVM UVP: Solidity contracts plus TypeScript tooling and services for cross-border, low-trust, publicly verifiable workflows.

Both tracks share protocol concepts such as Zhixu YAML, Hook DSL, signals, hook dependencies, artifact hashes, and replay semantics. If those semantics continue to evolve independently, the same YAML may eventually mean different things on cloud and chain.

`uvp-core` exists to become the shared semantic base. It must define and verify the portable parts of UVP once, then let each runtime expose its own adapter.

## 2. Goal

Build `uvp-core` as the deterministic semantic kernel for UVP.

The core must answer:

- How a Zhixu YAML document is parsed and normalized.
- How Hook DSL is parsed, normalized, evaluated, and converted into dependencies.
- How portable validation and topology checks are performed.
- How canonical IR and semantic hashes are produced.
- How cloud, EVM, and future adapters derive their runtime-specific artifacts from the same base.
- How replay fixtures prove that different runtimes preserve the same meaning.

The first useful result is not a production runtime replacement. The first useful result is a fixture-backed oracle that can detect semantic drift between Go cloud, TypeScript/EVM, and future adapters.

## 3. Non-Goals

`uvp-core` must not become another runtime service.

It does not own:

- PostgreSQL schema execution.
- PostgreSQL triggers.
- `hookstatus` queue processing.
- Worker lease, retry, or DLQ behavior.
- Executor dispatch.
- Wallet, viem, RPC, indexer, or relayer logic.
- Product API, Store API, Order App, or UI state.
- Business domains such as payment, e-sign, disputes, governance operations, or store search.
- Production deployment topology.

Those remain adapter/runtime concerns.

## 4. Core Principle

Adapter depends on core. Core does not depend on adapter.

```text
uvp-core       = semantic truth
Go cloud       = cloud runtime adapter
TS/EVM         = chain adapter and services
Solana future  = new adapter, not new semantics
```

The core should be small, deterministic, fixture-heavy, and conservative.

## 5. Repository Shape

Target structure:

```text
uvp-core/
├── README.md
├── rust-toolchain.toml
├── Cargo.toml
├── crates/
│   ├── uvp-model/
│   ├── uvp-hook-dsl/
│   ├── uvp-ir/
│   ├── uvp-compiler/
│   ├── uvp-replay/
│   ├── uvp-adapter-cloud/
│   ├── uvp-adapter-evm/
│   ├── uvp-ffi/
│   ├── uvp-node/
│   ├── uvp-wasm/
│   └── uvp-cli/
├── fixtures/
│   ├── hook/
│   ├── zhixu/
│   ├── replay/
│   ├── cloud/
│   └── evm/
└── docs/
    ├── init_prd.md
    ├── semantic-versioning.md
    ├── portable-yaml.md
    ├── artifact-format.md
    └── adapter-contract.md
```

## 6. Module Responsibilities

### 6.1 `uvp-model`

Owns typed Zhixu and protocol data models.

It should define:

- Raw YAML model.
- Normalized model.
- Stage model.
- Signal model.
- Hook declaration model.
- Executor reference model.
- Resource reference model.
- Diagnostic model.

It must not contain cloud database rows, Solidity ABI structs, or TypeScript service DTOs.

### 6.2 `uvp-hook-dsl`

Owns Hook DSL syntax and evaluation.

Required behavior:

- Parse Hook DSL into AST.
- Normalize expressions.
- Extract positive and negative dependencies.
- Evaluate current hook state from signal timestamps and current time.
- Produce structured diagnostics.

Initial semantic coverage:

```text
source::task.stage.signal
~signal
A & B
A | B
(A)
signal + <positive integer><unit>
OUTSIDE@(source::task.stage.signal)
MERGE@(a::task.stage.signal, b::task.stage.signal)
ANCHOR@(task.stage.signal)
OUTSOURCE@(source::task.stage.signal)  # retired, parse-time rejection with migration hint
```

#### Delay operator contract

The `+` operator is a postfix AST operator. It is not a fixed global `T` value and
there is no `durationConstants` table in the semantic contract. Each user rule may
choose its own positive duration literal:

```text
buyer::task.receive.cmp +48h
buyer::task.receive.cmp +7d
buyer::(task.a.cmp & task.b.cmp) +10s
buyer::task.pay.cmp +5m & ~task.refund.cmp
```

The grammar is:

```text
condition    := orExpr
orExpr       := andExpr ("|" andExpr)*
andExpr      := unaryExpr ("&" unaryExpr)*
unaryExpr    := "~" unaryExpr | postfixExpr
postfixExpr  := primary ("+" duration)?
primary      := signal | "(" condition ")" | externalHook
duration     := positiveInteger ("s" | "m" | "h" | "d")
```

The parser accepts whitespace around the operator, but only lowercase `s`, `m`,
`h`, and `d`. Durations must be positive integers without a leading zero; zero,
negative, fractional, unknown-unit, and overflow values are rejected. A delayed
expression must contain a positive signal anchor. To delay a compound expression,
parenthesize it: `(A & B) +10s`; `A +10s & ~B` delays only `A`.

For Cloud, the canonical compiled representation is:

```json
{
  "schemaVersion": "uvp.cloudAst.v1",
  "source": "buyer",
  "mode": "normal",
  "upstreamSource": null,
  "root": {
    "type": "delay",
    "expr": { "type": "signal", "signal": "task.receive.cmp" },
    "rawDuration": "14d",
    "durationSeconds": 1209600
  }
}
```

`rawDuration` preserves the literal and `durationSeconds` is its checked conversion;
they must agree. Compiled-hook evaluation requires the schema version and rejects
legacy delay nodes that use a `delay` field. It evaluates the AST, not the original
raw expression. If the inner expression is true but the due time has not arrived,
the result is `wait`; at or after the due time it is `ready`. Missing positive
anchors produce `needs_more`, while an already-established negative guard produces
`impossible`.

The semantic core defines these results and timestamps. A Cloud adapter owns durable
wait scheduling: it should persist `wait` and `due_at` and reclaim due records, rather
than creating a thread or long-lived timer for every delayed hook.

Evaluation states:

```text
ready
wait
impossible
needs_more
```

This is the highest-priority module because it is the most likely source of drift between Go and TypeScript implementations.

### 6.3 `uvp-ir`

Owns portable canonical JSON primitives. The current crate exposes
`canonicalize`, `canonical_stringify`, and `hash_canonical`; a typed
`PortablePlan` API is still a planned boundary and must not be presented as
an exported type until it exists.

Responsibilities:

- Define the portable canonical JSON shape (a typed `PortablePlan` is not
  exported yet).
- Define canonical JSON encoding.
- Define semantic hash inputs.
- Preserve stable ordering rules.
- Preserve schema version metadata.
- Make non-portable fields explicit.

The IR is the common input to cloud, EVM, and future adapters.

### 6.4 `uvp-compiler`

Owns normalized Zhixu validation and compilation into the currently exported
hook-plan and cloud-artifact JSON shapes.

Responsibilities:

- Stage and signal validation.
- Hook dependency closure.
- Topology validation.
- Cycle detection.
- Cross-source rule validation.
- Explicit `OUTSIDE@(…)`, `MERGE@(…)` and `ANCHOR@(…)` validation; `OUTSOURCE` is retired with a parse-time migration hint.
- Portable compatibility classification.

Compatibility classes:

```text
portable
cloud_only
evm_only
experimental
```

The compiler must not silently accept a cloud-only template as portable.

### 6.5 `uvp-replay`

Owns deterministic replay oracle.

Input:

```text
PortablePlan
events[]
now
```

Output:

```text
order state
hook states
ready hooks
waiting hooks
cancelled hooks
diagnostics
```

This module verifies semantic equivalence. It does not replace Go workers or Solidity contracts.

### 6.6 `uvp-adapter-cloud`

Owns cloud artifact generation.

It should convert `PortablePlan` into data structures that Go can write to PostgreSQL:

- `global_zhixu`
- `global_stage`
- `global_hook`
- `hook_dependency`
- cloud runtime metadata

It must not connect to PostgreSQL directly.

### 6.7 `uvp-adapter-evm`

Owns EVM artifact generation.

It should convert `PortablePlan` into:

- `registerPlan` arguments.
- Instruction op arrays.
- Signal selectors.
- Metadata hash payloads.
- Contract fixture JSON.
- EIP-712-relevant typed payloads.

It must not call RPC, viem, wallets, or relayers directly.

### 6.8 `uvp-ffi`

Owns C ABI for Go cgo.

Initial API should be JSON-based to keep memory and compatibility simple:

```c
char* uvp_compile_json(const char* request_json);
char* uvp_parse_hook_json(const char* request_json);
char* uvp_eval_compiled_hook_json(const char* request_json);
char* uvp_replay_json(const char* request_json);
void uvp_free(char* ptr);
const char* uvp_core_version(void);
```

The Rust side owns allocation. Callers must release returned strings through `uvp_free`.

### 6.9 `uvp-node`

Owns Node/TypeScript binding through `napi-rs`.

Initial JSON-binding API (the native exports use these names):

```ts
compile(request: unknown): unknown
parseHook(request: unknown): unknown
evaluateHook(request: unknown): unknown
replay(request: unknown): unknown
version(): string
semanticVersion(): string
```

This is the preferred integration path for `uvp-eth` services and tools.

### 6.10 `uvp-wasm`

Owns browser/tooling WASM build.

Primary consumers:

- Wiki playground.
- Developer tools.
- Lightweight template validators.
- Offline fixture viewers.

It is not required for the first production adapter, but the export boundary should be kept compatible with `uvp-node`.

### 6.11 `uvp-cli`

Owns command-line inspection and CI oracle.

The CLI accepts one JSON request per subcommand (or `@path/to/request.json`):

```bash
uvp-core compile '{"target":"cloud","definition":{...}}'
uvp-core parse-hook '{"profile":"cloud_compat","hookName":"PAY","hook":"buyer::pay.main.cmp"}'
uvp-core eval-compiled-hook '{"profile":"cloud_compat","ast":{...},"signals":[],"now":"2026-01-01T00:00:00Z"}'
uvp-core replay '{"events":[...],"options":{"strict":true}}'
uvp-core version
```

The CLI is the first integration surface because it is easiest to use from Go, TypeScript, shell scripts, and CI.

## 7. Adapter Relationship

```text
Zhixu YAML
  -> uvp-model
  -> uvp-hook-dsl
  -> uvp-compiler
  -> uvp-ir
  -> uvp-replay
  -> adapters
```

Adapter outputs:

```text
uvp-adapter-cloud  -> Go/PostgreSQL runtime artifact
uvp-adapter-evm    -> Solidity/registerPlan artifact
uvp-adapter-solana -> future Solana instruction/account artifact
```

Future adapters may be added without changing core semantics if they can consume the existing `PortablePlan`.

## 8. Fixture Strategy

Fixtures come before broad implementation.

Initial fixture groups:

```text
fixtures/hook/
fixtures/zhixu/
fixtures/replay/
fixtures/cloud/
fixtures/evm/
```

Hook fixtures should cover:

- Single positive signal.
- Negative guard.
- AND.
- OR.
- Nested expressions.
- Delay.
- Delay made impossible by negative signal.
- Removed bare `OUTSIDE` / `OUTSOURCE` syntax.
- Explicit `OUTSIDE@(source::task.stage.signal)`.
- Explicit `MERGE@(a::task.stage.signal, b::task.stage.signal)` match entry.
- Explicit empty-header `ANCHOR@(task.stage.signal)` acquisition reflux entry.
- `OUTSOURCE` retired with a parse-time migration hint.

Zhixu fixtures should cover:

- Linear flow.
- Multi-stage dependency.
- Cross-source direct trigger.
- Static executor requirement.
- Child order / source switch.
- Portable template.
- Cloud-only template.
- EVM-only template.

Replay fixtures should cover:

- Duplicate signals.
- First-writer-wins behavior.
- Timer wait to ready.
- Negative condition cancellation.
- Ready hook claim equivalence.
- Order lineage facts.

Each fixture should declare:

```json
{
  "name": "string",
  "semanticVersion": "uvp.semantic.v1",
  "input": "...",
  "expected": "...",
  "portable": true
}
```

## 9. Versioning

`uvp-core` versioning is separate from cloud runtime and EVM contract versions.

Suggested version lines:

```text
uvp-core crate version: 0.1.x
UVP semantic version: uvp.semantic.v1
artifact schema version: cloud-artifact/0.1, evm-artifact/0.1
contract version: independent
cloud runtime version: independent
```

Every generated artifact should include:

```json
{
  "uvpCoreVersion": "0.1.0",
  "semanticVersion": "uvp.semantic.v1",
  "artifactSchemaVersion": "cloud-artifact/0.1"
}
```

Breaking changes require:

- A new semantic version.
- Updated fixtures.
- Migration notes.
- Explicit adapter compatibility status.

## 10. Integration Strategy

### 10.1 First Integration: CLI

Use `uvp-cli` first.

Why:

- Lowest integration risk.
- No cgo memory boundary at the beginning.
- Easy to call from Go tests and TS tests.
- Good for CI fixture diff.

### 10.2 Go Integration

Start with CLI or subprocess fixture checks.

Move to `uvp-ffi` only after:

- JSON request/response schemas stabilize.
- Fixture coverage is good.
- Error diagnostics are structured.

Go should continue to own:

- HTTP/gRPC services.
- PostgreSQL writes.
- Runtime transactions.
- PG trigger integration.
- Worker scheduling.
- Metrics and auth.

### 10.3 TypeScript/EVM Integration

Start with CLI or `uvp-node`.

TypeScript should continue to own:

- viem and ABI calls.
- indexer.
- relayer.
- Product API.
- deployment scripts.
- UI and developer tooling.

It should stop owning duplicated Hook DSL and artifact semantics after Rust coverage is trusted.

### 10.4 Future Solana Integration

Solana should be an adapter:

```text
PortablePlan -> Solana instruction args
PortablePlan -> account layout expectations
Solana logs/events -> replay events
```

Adding Solana should not require changing Hook DSL or canonical IR unless a genuinely new semantic primitive is introduced.

## 11. Delivery Order

### Stage 1: Repository Foundation

Create:

- `rust-toolchain.toml`
- Cargo workspace.
- Empty crates.
- Basic README.
- CI for `fmt`, `clippy`, `nextest`, and `cargo-deny`.
- Initial docs.

Completion criteria:

- `cargo fmt --check` passes.
- `cargo clippy --workspace --all-targets` passes.
- `cargo nextest run --workspace` passes.
- No production integration yet.

### Stage 2: Golden Fixtures

Create initial hook, zhixu, replay, cloud, and EVM fixtures.

Completion criteria:

- Fixtures document current expected semantics.
- Fixture schema is reviewed.
- Fixture names and categories are stable enough for CI.

### Stage 3: Hook DSL Core

Implement parser, AST, normalization, dependencies, and evaluation.

Completion criteria:

- Hook fixtures pass.
- Diagnostics are structured.
- Go and TS examples can be compared against Rust output.

### Stage 4: Model and Compiler

Implement YAML model, normalization, validation, and `PortablePlan`.

Completion criteria:

- Zhixu fixtures compile.
- Compatibility classes are emitted.
- Canonical JSON is stable.
- Semantic hash is fixture-backed.

### Stage 5: Replay Oracle

Implement deterministic replay.

Completion criteria:

- Replay fixtures pass.
- Timer, negative guard, duplicate signal, and first-writer behavior are covered.
- Replay output can be compared to cloud and EVM test outputs.

### Stage 6: Cloud and EVM Artifact Adapters

Implement artifact generation from `PortablePlan`.

Completion criteria:

- Cloud artifact fixtures pass.
- EVM artifact fixtures pass.
- Existing TS/Go artifact examples can be diffed.

### Stage 7: Export Surfaces

Implement:

- `uvp-cli`
- `uvp-ffi`
- `uvp-node`
- `uvp-wasm`

Completion criteria:

- CLI supports compile, parse-hook, eval-compiled-hook, replay, and hash.
- C header is generated through `cbindgen`.
- Node package builds through `napi-rs`.
- WASM package builds through `wasm-pack`.

### Stage 8: Downstream Adoption

Use `uvp-core` in downstream repositories.

Initial downstream use should be non-invasive:

- `uvp`: compare Go compiler output against `uvp-core` fixtures.
- `uvp-eth/uvp-protocol`: compare TS/EVM artifact output against `uvp-core`.
- `uvp-eth/uvp-wiki`: optionally power playground or docs examples.

Only after stable fixture parity should production code call Rust directly.

## 12. Acceptance Criteria

The initial `uvp-core` track is successful when:

- A portable YAML compiles into one canonical IR.
- The same IR can generate cloud and EVM artifacts.
- Hook DSL behavior is fixture-backed.
- Replay behavior is fixture-backed.
- Go cloud and TypeScript/EVM can diff against Rust outputs in CI.
- Adding a future adapter does not require changing existing semantic modules.

## 13. Risk Management

### Risk: Rust becomes a third semantic implementation

Mitigation:

- Fixtures come first.
- Compare against current Go and TS behavior.
- Do not replace production paths until fixture parity exists.

### Risk: Core becomes too large

Mitigation:

- Keep runtime and adapter concerns out.
- No database connections.
- No RPC calls.
- No UI.

### Risk: Over-stabilizing wrong semantics

Mitigation:

- Mark fixtures as `portable`, `cloud_only`, `evm_only`, or `experimental`.
- Require explicit semantic version bumps for breaking changes.

### Risk: cgo boundary becomes painful

Mitigation:

- Start with CLI.
- Use JSON ABI for FFI.
- Keep Rust allocation and free rules explicit.

### Risk: TS/EVM tooling still duplicates semantics

Mitigation:

- First use Rust as a CI oracle.
- Then route compiler/hash/replay paths through `uvp-node`.

## 14. Immediate Next Changes

Recommended commit sequence:

```text
chore: initialize uvp-core rust workspace
docs: define UVP semantic core boundaries
test: add initial hook DSL golden fixtures
feat: implement hook DSL parser and evaluator
feat: add canonical IR and hash fixtures
feat: add CLI compile and replay commands
feat: add C ABI and Node bindings
```

Do not start by wiring Go or TypeScript production code to Rust. Start by making Rust the executable semantic specification.
