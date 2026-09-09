## Why

Beholder can identify a relationship but cannot explain which branch, pattern,
function clause, or overload produced each individual observation. That makes
otherwise precise semantic paths ambiguous when several call sites collapse onto
the same edge, including the Rust control-flow gap tracked by issue #180.

## What Changes

- Add structured, source-ranged context to each public evidence record.
- Model conditional arms, pattern arms, and callable clauses with one shared,
  language-neutral contract.
- Preserve that context through storage, aggregation, gRPC, CLI, MCP, and every
  typed semantic graph query.
- Populate syntactic contexts in staged Rust, Elixir, JavaScript/TypeScript,
  Svelte, and C# frontend work.
- Preserve legacy evidence strings and older clients while advancing the daemon
  protocol additively.
- Keep runtime branch evaluation, symbolic control flow, and branch entities out
  of scope.

## Capabilities

### New Capabilities

None.

### Modified Capabilities

- `semantic-graph`: Evidence gains deterministic source ranges and ordered
  syntactic context without changing semantic edge identity.
- `query-api`: Every evidence-preserving graph query exposes the same structured
  evidence context and remains compatible with legacy stored evidence.
- `analyzer-workers`: Worker contributions can transport typed evidence context
  and compiler enrichers attach callable-clause context only when resolution is
  exact and unique.

## Impact

This affects Beholder domain and DTO types, Mnestic evidence encoding/decoding,
daemon and worker protobuf mappings, generated bindings, query presentation, and
the Rust, Elixir, TypeScript/JavaScript, Svelte, and C# analysis pipelines. It
builds on the evidence-preserving graph contract finalized in
[`optimize-mcp-semantic-queries`](../archive/2026-09-09-optimize-mcp-semantic-queries/proposal.md)
without adding a database migration or a runtime dependency.
