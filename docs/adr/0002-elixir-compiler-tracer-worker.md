# ADR 0002: Elixir compiler-tracer worker

- Status: proposed
- Date: 2026-08-20

## Context

ADR 0001 introduced language-specific native analyzer workers so Beholder can
publish a fast Tree-sitter baseline before progressively adding compiler-backed
semantic facts. The Rust worker can start automatically because rust-analyzer
does not normally execute the indexed project's build-time code.

Elixir exposes compiler tracers through the `Code` module. A tracer implements
`trace/2` and receives compiler events together with the current `Macro.Env`.
Events cover aliases, imports, requires, local and remote function calls, macro
calls, struct expansion, and lexical-context boundaries. The parallel compiler
also exposes file and module callbacks and structured diagnostics.

Compiler tracing is an event stream rather than a rust-analyzer-style semantic
query database. Beholder must collect and persist the events produced during a
compilation. A no-change Mix compilation produces no complete event stream, so
the worker needs an independently fingerprinted build location or must reuse a
previous complete Beholder contribution.

Elixir compilation may execute arbitrary project and dependency code through
`mix.exs`, configuration, macros, module attributes, and custom compilers. A
separate worker isolates the daemon from VM crashes and toolchain dependencies,
but process separation alone is not a security boundary.

## Decision

### Manual worker invocation

The CLI exposes one analyzer-neutral command:

```text
beholder enrich <analyzer>
```

The command runs the named worker against the latest completed syntax baseline.
It is available for every registered worker, including workers that also run
automatically, so users can explicitly request or retry an enrichment without a
language-specific CLI surface.

Worker registration declares its default activation policy. The Rust worker
remains automatic and may also be invoked manually. The Elixir worker is manual
only. A newer syntax baseline marks its previous contribution stale but does not
automatically queue another Elixir compilation.

CI integration, non-interactive trust flags, persistent watch mode, and
repository-configured automatic execution are outside this decision.

### Elixir compiler worker

The Elixir analyzer is a language-specific worker using the typed bidirectional
gRPC protocol defined by ADR 0001. It runs compilation in a dedicated BEAM VM
using the workspace's selected Elixir and Erlang/OTP toolchain.

Beholder ships the tracer. The indexed repository does not implement `trace/2`
and does not need to change its Mix configuration. Before compiling, the worker
loads the tracer and appends it to the VM's existing compiler tracers. Existing
project tracers remain installed.

The worker uses the configured Mix environment, defaulting to `dev`, so
conditional compilation and dependency selection reflect the ordinary project.
It uses a Beholder-owned build path and does not intentionally start the project
application. This does not prevent macros, configuration, custom compilers, or
other compilation hooks from executing code.

The tracer performs minimal synchronous work. It normalizes each event and
forwards it to a collector process; graph construction and gRPC publication do
not run inside `trace/2`.

The worker contributes, where available:

- expanded alias, import, and require targets;
- local, imported, and statically named remote function calls;
- local, imported, and remote macro calls;
- function-versus-macro classification;
- struct expansion and module references;
- compile-time, export, and runtime dependency evidence;
- caller module and function context;
- compiler diagnostics and per-source coverage.

Events are mapped onto baseline source identities using the compiler file and
localized line and column metadata. Events introduced by macro expansion retain
macro provenance and remain approximate when the compiler cannot identify one
unambiguous source site.

Dynamic dispatch remains explicit. Calls through dynamic module values,
`apply/3`, protocols, registries, process messaging, or other runtime mechanisms
are not rewritten to exact targets unless separate evidence justifies that
relationship.

### Trust and failure handling

`beholder enrich elixir` warns that compilation executes trusted repository and
dependency code and requires interactive confirmation before the worker starts.
The initial implementation does not persist automatic trust.

Repository-controlled configuration may tune analysis but cannot grant
permission to execute the repository. A repository therefore cannot opt itself
into compiler execution merely by committing Beholder configuration.

Compilation failure is non-fatal. The syntax revision remains queryable, and
the worker publishes typed diagnostics plus coverage for inputs that were
enriched, excluded, unresolved, or failed. Partial contributions may publish
only when they are internally complete and still match the immutable input
fingerprint.

### Identity and reuse

The Elixir contribution fingerprint includes at least:

- the immutable syntax baseline fingerprint;
- analyzer and worker versions;
- Elixir and Erlang/OTP versions;
- the selected Mix environment;
- relevant Mix project, lock, and configuration inputs;
- analysis-relevant dependency and compiler identity.

A matching completed contribution may be reused without recompiling. A worker
result publishes as a new graph revision only if its input still matches the
current baseline. A later run atomically replaces only the Elixir analyzer's
previous contribution.

## Consequences

- Beholder gains compiler-resolved Elixir aliases, imports, macros, and direct
  calls without requiring repositories to install a plugin.
- The generic manual command provides a consistent way to trigger, retry, and
  eventually inspect all native analyzer workers.
- Elixir enrichment is deliberately less automatic than Rust enrichment
  because it crosses an arbitrary-code-execution boundary.
- A dedicated BEAM VM isolates compiler options, loaded modules, crashes, and
  language dependencies from the Rust daemon, but it does not sandbox host
  filesystem or network access.
- Cold enrichment incurs Elixir dependency loading, compilation time, build
  storage, and potentially significant memory use.
- Compiler traces improve static call precision but do not make dynamic Elixir
  dispatch fully resolvable.
- Source locations for macro-generated events may be less precise than ordinary
  source calls and must remain evidence-backed rather than silently promoted to
  certainty.

## Rejected alternatives

### Require repositories to install a Beholder tracer

Rejected because compiler tracers can be installed by the worker and requiring
project changes would make enrichment harder to adopt and version consistently.

### Start Elixir enrichment automatically

Rejected because merely indexing a newly cloned repository must not silently
execute its build-time code.

### Allow repository configuration to enable automatic execution

Rejected because trust cannot be granted by the code that is about to execute.

### Treat compiler manifests as the complete semantic interface

Rejected because manifests are optimized for Mix compilation, expose less
call-site information than tracer events, and are not a stable replacement for
Beholder-owned analyzer contributions.
