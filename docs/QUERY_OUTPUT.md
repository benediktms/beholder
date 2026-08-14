# Typed query output

Beholder query output has four layers:

```text
Mnestic named rows
        |
        v
query-specific mapper in beholder-adapters-mnestic
        |
        v
typed results in beholder-dto
        |
        v
presentation projection in beholder-presentation
        |
        +-- compact human output
        +-- evidence-first why output
        +-- full raw output
        +-- versioned JSON
```

`ContextResult`, `DependenciesResult`, `ImpactResult`, `TraceResult`, and
`WhyResult` own their query shape. They share Beholder-owned `EntityRef`,
`SemanticEdge`, `EvidenceRef`, `SemanticPath`, and freshness metadata. Each JSON
document carries a query-specific `beholder.<query>.v1` schema identifier.

Mnestic rows and values stop at the storage adapter. The gRPC service exposes a
query-specific response for each command and contains no generic headers or row
values. Renderers consume typed results only.

Compact projection may hide structural namespaces and collapse generated or
supporting nodes. It never mutates storage facts or the typed result. JSON and
`--raw` always retain every mapped node, edge, path, confidence value, and piece
of evidence returned by the semantic query.

Entities distinguish first-party source, generated source, and external
dependencies. Test, spec, and benchmark symbols are marked separately. Compact
output hides tests and non-source dependencies by default; `--include-tests`
restores tests, and impact output groups them by evidence file. JSON and `--raw`
remain complete regardless of presentation flags.
