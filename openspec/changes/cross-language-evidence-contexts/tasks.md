## 1. Shared evidence contract

- [x] 1.1 Add domain source-range, excerpt, and three-variant evidence-context types plus deterministic `beholder:evidence:v1` encoding/decoding; verify focused `beholder-domain` tests cover round trips, context order, UTF-16 positions, legacy forms, and malformed reserved payload fallback.
- [x] 1.2 Extend `EvidenceRef` and Mnestic query mapping with ranges and contexts while preserving edge aggregation and legacy fields; verify focused `beholder-dto` and `beholder-adapters-mnestic` tests distinguish same-line occurrences and return identical evidence through context, dependencies, impact, trace, why, topology, and traversal builders.
- [x] 1.3 Add additive daemon and worker protobuf fields, regenerate checked-in Rust/Elixir/Go bindings, map typed worker evidence, and advance daemon protocol 24 to 25; verify focused protocol, daemon RPC, daemon-client, Elixir worker, and TypeScript worker compatibility tests pass.
- [x] 1.4 Preserve structured evidence in raw and JSON presentation without expanding compact output; verify focused CLI and MCP serialization tests cover populated, empty, and legacy context fields.

## 2. Rust syntax and compiler evidence

- [ ] 2.1 Carry observation ranges and ordered contexts on Rust call occurrences and extract `if`/`else if`/`else` plus `match` arm evidence; verify focused `beholder-adapters-treesitter-rust` tests cover nested arms, selector/condition/guard exclusions, wildcard cases, multibyte positions, and two same-line calls.
- [ ] 2.2 Preserve syntax context through rust-analyzer recovery and add selected-target callable context only for one exact declaration range; verify focused Rust worker tests cover free functions, methods, trait dispatch, ambiguous candidates, and unavailable spans.
- [ ] 2.3 Propagate structured evidence through Rust repository resolution, compiler overrides, and Tonic-derived observations, then advance affected Rust frontend/resolver/worker fingerprints; verify focused adapter and daemon indexing tests reject stale cached identities and retain contexts on derived RPC calls.

## 3. Elixir clauses and selections

- [ ] 3.1 Preserve every Elixir call occurrence and emit distinct `Defines` evidence for same-name/same-arity clauses with complete guarded heads while retaining one function entity; verify focused `beholder-adapters-treesitter-elixir` tests cover guarded clauses, enclosing-clause order, repeated targets, and same-line calls.
- [ ] 3.2 Extract `case` pattern arms and `cond` condition clauses without assigning an arm to selector, condition, pattern, or guard calls; verify focused Elixir adapter tests cover nesting, defaults, guards, and exact excerpts/ranges.
- [ ] 3.3 Correlate Elixir compiler events per source coordinate and emit selected-target context only for a unique clause span, then advance frontend/compiler/cache identities; verify adapter and `workers/elixir` tests cover exact spans, MFA-only results, macros, internal events, and context-preserving publication.

## 4. JavaScript and TypeScript selections and overloads

- [ ] 4.1 Extract ternary and switch-statement/switch-expression contexts on JavaScript and TypeScript call occurrences; verify focused `beholder-adapters-treesitter-typescript` tests cover condition/selector exclusions, nested ternaries, default cases, fall-through ownership, and multibyte ranges.
- [ ] 4.2 Propagate structured evidence through repository, GraphQL, gRPC, and framework enrichers and add selected-target context only for one compiler-resolved overload declaration; verify focused adapter and `workers/typescript` tests cover exact overloads, ambiguous signatures, heuristic resolution, and derived observations.
- [ ] 4.3 Advance affected JavaScript/TypeScript frontend, resolver, worker, and plugin fingerprints; verify daemon indexing tests invalidate prior cached facts and republish structured evidence.

## 5. Svelte template expressions

- [ ] 5.1 Extract calls from Svelte template expressions while leaving existing instance-script analysis intact, mapping component-owned call ranges back to original UTF-16 positions; verify focused Svelte tests cover script/template coexistence, nested expressions, and absence of component/tag reference facts.
- [ ] 5.2 Attach `{#if}`, `{:else if}`, `{:else}`, and ternary contexts, preserve other observations when one expression is malformed, and advance the Svelte plugin identity; verify focused tests cover normalized else-if arms, condition exclusions, diagnostics, multibyte source, and cache invalidation.

## 6. C# switches and overloads

- [ ] 6.1 Extract switch-statement and switch-expression pattern arms with optional `when` guards on C# calls; verify focused `beholder-adapters-treesitter-csharp` tests cover selector/guard exclusions, discard cases, explicit defaults, nesting, and exact ranges.
- [ ] 6.2 Propagate structured evidence through C# resolution and dependency-injection observations, add selected-target context only for one exact declaration signature/range, and advance affected fingerprints; verify focused adapter tests cover overload success, inferred ties, DI-derived calls, and stale cache rejection.

## 7. End-to-end validation

- [ ] 7.1 Add a controlled multi-language MCP fixture proving parallel evidence, ordered nested contexts, legacy evidence, and raw/JSON preservation through `traverse_graph`; verify `scripts/test-mcp-integration.sh` passes for the fixture.
- [ ] 7.2 Run `cargo fmt --check`, focused Clippy for touched crates, all focused checks named above, generated-binding checks, `git diff --check`, and `openspec validate cross-language-evidence-contexts --strict`; leave the broader workspace suite to CI.
