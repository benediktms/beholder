## Context

See [proposal.md](proposal.md) for motivation. Today `Observation` owns an opaque
`Evidence` string, Mnestic stores that string as part of observation identity, and
query construction parses only legacy `path:line` evidence into `EvidenceRef`.
Language models usually retain a line or byte offset but discard their enclosing
syntax before observation construction. Compiler workers can replace candidate
targets, but their protocols cannot describe a selected declaration clause.

The existing evidence string must remain readable and stable enough for published
fact shards. Public query types are Beholder-owned; Mnestic types must remain
inside the storage adapter. The daemon is currently protocol 24. Frontend and
plugin versions already participate in cache identities and must advance whenever
their emitted facts change.

## Goals / Non-Goals

**Goals:**

- Define one reusable evidence-context vocabulary for all frontends.
- Preserve individual call-site provenance even when semantic edges aggregate.
- Carry exact source text and UTF-16 ranges from extraction through every query.
- Make baseline syntax useful without overstating compiler certainty.
- Roll out in independently testable language slices.

**Non-Goals:**

- Control-flow graphs, path-condition evaluation, or symbolic execution.
- Runtime claims about which branch or overload executed.
- New entities or relations for branches, patterns, or clauses.
- Rust `if let`, Elixir `with`/`receive`/`try`, standalone pattern bindings, or
  general Svelte component/tag reference indexing in this change.
- GUI rendering.

## Decisions

### 1. Put the shared vocabulary in the domain and project it into DTO/protobuf types

Add domain-owned `SourcePosition`, `SourceRange`, `SourceExcerpt`, and
`EvidenceContext` values. `EvidenceContext` is a closed enum with three variants:

```text
ConditionArm {
  construct: if | cond | ternary | template_if,
  arm: then | else_if | else | clause | consequence | alternative,
  condition: optional SourceExcerpt,
  arm_range: SourceRange
}
PatternArm {
  construct: match | case | switch_statement | switch_expression,
  selector: optional SourceExcerpt,
  pattern: optional SourceExcerpt,
  guard: optional SourceExcerpt,
  is_default: bool,
  arm_range: SourceRange
}
CallableClause {
  role: declaration | enclosing | selected_target,
  signature: SourceExcerpt,
  guard: optional SourceExcerpt,
  definition_range: SourceRange
}
```

`EvidenceRef` gains an optional observation range and an ordered context list.
DTO and protobuf types mirror the domain values at their boundaries instead of
making frontends depend on transport types.

Alternative: define per-language context structures. Rejected because consumers
would need language switches for equivalent syntax and propagation code would be
duplicated. Alternative: create branch entities. Rejected because these values
describe an observation site, not durable semantic identity.

### 2. Keep `Evidence` storage-compatible with one domain-owned codec

Keep `Evidence` as the persisted string and add constructors/decoding in the
domain crate. Structured values use the reserved form
`beholder:evidence:v1:<compact-json>`. A fixed serde struct order makes encoding
deterministic. The payload carries legacy path/line/detail plus the observation
range and contexts; provenance and repository remain derived from the stored
observation and edge endpoints as they are today.

Decoding first attempts the reserved v1 format, then falls back to the existing
`path`, `path:line`, and `path:line · detail` rules. Invalid reserved payloads also
take the legacy path so corrupted or future data cannot make a query fail. Worker
protocol conversion builds domain values and calls the same codec. This retains
the existing Mnestic relation schema and garbage-collection behavior.

Alternative: add Mnestic columns/relations for each context. Rejected because the
query only needs evidence-local data and the migration would not improve graph
acquisition. Alternative: parse JSON directly in Mnestic. Rejected because the
format belongs to Beholder's domain boundary.

### 3. Context describes syntax, not accumulated logical predicates

Frontends attach the arm lexically containing a call. An `else` carries only its
immediately governing condition; it does not synthesize negations for earlier
arms. Calls used to evaluate a condition, selector, pattern, or guard receive only
already-enclosing outer contexts. Nested contexts are emitted outer-first.

The complete order is:

1. enclosing callable clause, when present;
2. lexical control contexts, outermost to innermost;
3. exact selected callable target, when present.

Exact source text is sliced from the original input without normalization.
Positions are zero-based UTF-16 and end-exclusive, matching public protocol
conventions; the existing one-based `EvidenceRef.line` remains unchanged.

Alternative: report the full logical predicate needed to reach an arm. Rejected
because it requires semantic evaluation and would produce misleading results for
short-circuiting, side effects, and language-specific pattern rules.

### 4. Preserve occurrences before aggregating edges

Every frontend carries range and context on its call occurrence until it creates
an `Observation`. Resolution and framework enrichers copy that evidence instead
of rebuilding `path:line`. Elixir removes target-only call deduplication and emits
one `Defines` evidence record per same-name/same-arity source clause while keeping
the existing function entity identity. Existing edge aggregation remains keyed by
semantic endpoints and relation; its evidence set distinguishes occurrences by
their encoded range/context.

### 5. Require exact, unique compiler evidence for selected targets

Baseline syntax can always emit declaration and enclosing callable contexts.
`selected_target` is added only when a compiler result names one source declaration
range for that exact call coordinate:

- Rust: rust-analyzer recovery supplies the exact function or method declaration.
- Elixir: a compiler event must identify a unique clause span; module/function/arity
  alone is insufficient.
- TypeScript/JavaScript: the language service must identify one resolved signature
  declaration, not merely an overload set.
- C#: compiler enrichment must identify one declaration signature/range.

Heuristic resolution, ambiguous overloads, macro-generated calls, and events that
cannot be correlated to one source coordinate emit no selected-target context.
This avoids turning resolution confidence into false source precision.

### 6. Stage frontend work behind the shared contract

1. Foundation: domain codec/types, DTOs, Mnestic mapping, daemon and worker
   protobufs, generated bindings, protocol 25, and presentation round trips.
2. Rust: `if`/`else if`/`else`, `match`, rust-analyzer recovery, compiler
   overrides, and Tonic-derived observations.
3. Elixir: clause-preserving definitions/calls, guarded heads, `case`, `cond`,
   and exact-coordinate compiler correlation.
4. JavaScript/TypeScript: ternary, switch statement/expression, framework and
   GraphQL propagation, and exact overload selection.
5. Svelte: retain script analysis and additionally parse calls from template
   expressions with template-if/ternary context owned by the component module.
6. C#: switch statement/expression context, `when` guards, DI propagation, and
   exact overload selection.

Each stage advances the affected frontend, resolver, compiler, plugin, or daemon
fingerprint so cached facts cannot mix old and new evidence shapes.

For Svelte, reuse the existing Svelte syntax tree to locate embedded expression
text, parse only those expressions with the existing JavaScript/TypeScript
analyzer, and translate their byte spans back to original UTF-16 positions.
Malformed template expressions add a diagnostic while preserving script and other
valid template observations.

## Risks / Trade-offs

- [Encoded evidence strings become larger] → keep compact JSON, measure a focused
  Mnestic fixture, and avoid storing derived predicate chains.
- [UTF-8 parser offsets can drift from UTF-16 protocol positions] → centralize
  conversion and test multibyte text plus same-line calls.
- [Framework enrichers can silently discard context] → use propagation tests for
  Tonic, GraphQL, Svelte, and dependency-injection derived observations.
- [Compiler versions expose incomplete source spans] → omit selected-target
  context unless one exact range is available.
- [Staged rollout produces mixed legacy and structured evidence] → make decoding
  per-record and keep all query schemas tolerant of both forms.

## Migration Plan

Ship the additive daemon and worker fields with protocol 25 before frontend
emitters. Existing stored workspaces need no migration and remain queryable.
Advancing analysis fingerprints causes changed frontends to republish structured
evidence naturally. Rollback reads newly encoded evidence through the legacy path;
it may show the opaque payload as detail but does not corrupt graph storage.
