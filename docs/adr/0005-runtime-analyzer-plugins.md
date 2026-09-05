# ADR 0005: Runtime analyzer plugins

> [!NOTE]
> This ADR is retained for architectural rationale and rejected alternatives. The
> current behavioral contract is [`runtime-plugins`](../../openspec/specs/runtime-plugins/spec.md).

- Status: proposed
- Date: 2026-08-24

## Context

ADR 0001 isolates language-native semantic analyzers in worker executables.
Those workers are part of Beholder's own analyzer catalog and primarily replace
syntax heuristics with compiler-backed facts. Beholder also needs an extension
boundary for recognition that is specific to an organization, private library,
or heavily modified framework and therefore cannot be maintained sensibly in
the organization-neutral core.

Configuration alone is not that boundary. A list of framework-specific module
names, handler functions, table fields, or decoder options merely moves a parser
into configuration and couples Beholder to every supported library shape. It
also cannot reliably interpret arbitrary source constructs. Conversely, loading
Rust dynamic libraries would bind plugins to Rust's unstable ABI and to the
daemon's dependency graph.

The extension API must let independently maintained code recognize additional
canonical facts without letting it redefine Beholder's ontology, depend on
another plugin's output, or compromise publication of the baseline graph. It
must also cross repository boundaries: a recognizer may need selected files from
context repositories and canonical entities already established by language and
contract frontends.

## Decision

### One analyzer plugin boundary

Beholder supports one runtime plugin kind: an analyzer executable using a
versioned Protobuf/gRPC protocol. Labels such as message broker, REST framework,
or library adapter describe what a plugin recognizes; they do not create
different transports, lifecycle types, or SDKs.

The daemon is the gRPC client and process owner. It starts each analysis in a
job-scoped child process on a private local socket, streams immutable inputs,
receives a typed contribution, and then allows the process to exit. Plugins are
not loaded into the daemon process and are not separately managed daemons. The
asynchronous worker lifecycle remains behind Beholder's existing
`WorkspaceEnricher` boundary, so baseline indexing and semantic reads do not
wait for plugin analysis.

The existing `AnalyzerWorker.Analyze` service remains the contribution
transport. A sibling discovery service exposes `Describe`, allowing an
executable to declare:

- its stable plugin ID and exact plugin API version;
- target and context file selectors;
- baseline entity and relation kinds it needs to read; and
- entity and relation kinds it may contribute.

File selectors initially cover extensions, exact file names, and path suffixes.
The descriptor is a compatibility and input declaration, not an arbitrary
capability system. The daemon rejects undeclared output kinds.

### Inputs and deterministic composition

An analysis job receives only the selected immutable files and semantic facts
needed by its descriptor. Target files are distinguished from read-only context
files. Semantic input consists of selected entities and observations from the
target repository's current baseline contribution, streamed in bounded chunks;
it does not include another plugin's contribution.

This preserves deterministic, order-independent composition and prevents
plugin dependency cycles. A plugin contribution is owned and replaced by that
plugin independently. Its identity includes the target baseline identity,
selected context input identities, plugin ID, executable digest, and plugin API
version. A result is published only while those inputs still match the current
workspace state.

Plugins extend recognition, not the ontology. Entity and relation kinds remain
closed types owned by Beholder. A contribution may refer to a supplied baseline
entity or an entity defined through a checked constructor. Both the SDK and the
daemon validate entity-address schemes, metadata, relation endpoints,
repository ownership, referenced entities, declared output kinds, and evidence
paths before publication.

Payload contracts are optional semantic facts. For example, a message-broker
recognizer may contribute a callable `publishes` relationship to a Kafka topic
and a topic `consumed_by` relationship to another callable without identifying
the payload format. When independent evidence identifies a supported contract,
the plugin may additionally contribute `binds_contract`. Kafka recognition does
not imply Protobuf, and a non-Protobuf flow remains connected.

### Public Rust SDK

Beholder provides a thin Rust SDK from this repository for plugins to consume as
a Git dependency. It is a supported public API even though it is not published
to crates.io. Its primary authoring surface is equivalent to:

```rust
pub trait Analyzer: Send + Sync + 'static {
    fn descriptor(&self) -> Descriptor;

    fn analyze(
        &self,
        context: &AnalysisContext<'_>,
        output: &mut Output,
    ) -> Result<(), AnalysisError>;
}

pub async fn serve(analyzer: impl Analyzer) -> Result<(), ServeError>;
```

The analyzer callback is synchronous because recognition is CPU- and
input-bound and does not need to expose the transport runtime. The SDK owns the
asynchronous gRPC server, request streaming, cancellation, trace propagation,
and graceful shutdown. Once a complete request has arrived, the SDK runs
`Analyzer::analyze` with `tokio::task::spawn_blocking`; plugin CPU work therefore
does not occupy the plugin's Tokio runtime threads. Each job-scoped process runs
exactly one analyzer callback. A plugin may use its own parser or dependencies
behind the callback; Beholder does not expose raw tree-sitter nodes or require a
shared parser API.

`AnalysisContext` exposes workspace and target identity, selected files,
selected baseline semantic facts, and cancellation state. `Output` exposes
checked operations to define entities, relate entity references, emit structured
evidence and diagnostics, and mark a contribution incomplete. Entity references
are opaque, and there is no raw Protobuf or arbitrary entity-URI escape hatch.

The SDK crate uses `#![deny(missing_docs)]`. Every public module, type, trait,
function, field, and enum variant has Rustdoc explaining its contract and
invariants. Crate-level documentation explains installation assumptions, the
one-job lifecycle, inputs, outputs, validation, failure handling, and
OpenTelemetry behavior. It includes a compiling minimal analyzer example and
compiling examples for the primary semantic output operations. A plugin author
must be able to use the generated documentation without reading generated
Protobuf or SDK internals. Breaking public SDK changes require explicit plugin
API and protocol evolution.

### Installation and activation

Plugins are trusted native executables. Installing one is an explicit local
administrative action, not a repository-controlled side effect, and process
isolation is not a security sandbox.

The daemon owns a plugin registry under `BEHOLDER_STATE_DIR`. Installation runs
`Describe` with a bounded deadline, validates the plugin ID and descriptor,
computes the executable's SHA-256 digest, and copies it immutably to a path
derived from plugin ID and digest. The registry persists the ID, digest, and
validated descriptor. It does not persist a redundant executable path.

Workspace configuration stores only enabled plugin IDs. It does not contain a
plugin revision, application-library version, executable path, or
framework-specific recipe. The executable digest supplies analyzer and cache
identity. Enabled plugin IDs and digests are included in workspace analysis
identity so disabling or replacing a plugin removes stale contributions.

The administrative CLI owns install, replace, list, remove, enable, and disable
operations. The initial implementation loads the registry when the daemon
starts and uses stop, mutate, restart for changes rather than introducing a
dynamic catalog. A missing or invalid installed executable produces a typed
startup diagnostic for that plugin and does not prevent the daemon from serving
the baseline graph.

Daemon startup reads and validates only registry records and managed executable
paths. It does not execute plugin code, call `Describe`, or start plugin
processes. The validated descriptors form an immutable catalog of
`WorkspaceEnricher` proxies for that daemon lifetime. Workspace scheduling
selects enabled IDs from this catalog and evaluates their generic file selectors
without loading their executables.

### Scheduling and execution isolation

Plugin jobs enter the existing enrichment queue only after the baseline
revision has been published. The daemon materializes their selected files and
baseline semantic facts into an immutable request before starting the child;
plugins cannot make re-entrant semantic-store queries or daemon RPCs while they
analyze it.

The worker proxy performs process startup, socket connection, request streaming,
and response streaming asynchronously. The synchronous analyzer callback runs
only in the child process's blocking pool. The daemon event loop therefore
remains available to serve queries, watch repositories, schedule baseline work,
and cancel obsolete enrichments while a plugin is computing.

The initial implementation retains the existing serialized enrichment lane
instead of adding another concurrency controller. Process connection, complete
analysis, and graceful shutdown each have daemon-owned finite deadlines. A
timeout or cancellation drops the RPC, kills the child process, removes its
private socket, and emits a typed diagnostic before the next queued enrichment
runs. A slow plugin may increase enrichment latency, but it cannot indefinitely
block the enrichment queue, baseline indexing, or the daemon's query service.
Parallel plugin jobs may be introduced only after measurements justify the
additional CPU, memory, and cancellation policy.

### Failure handling and observability

A plugin failure is non-fatal. Beholder retains the last complete baseline
revision and publishes typed diagnostics rather than a partial or unvalidated
contribution. Cancellation terminates obsolete jobs, and a bounded graceful
shutdown period lets a completed worker flush telemetry before the daemon kills
an unresponsive child.

The SDK initializes Beholder's existing tracing and OTLP bridge, extracts W3C
trace context from gRPC metadata, and makes plugin-created spans children of the
daemon's enrichment span. Each plugin reports as its own service using the
existing worker service-name convention and inherits exporter, resource,
sampling, and disable configuration. SDK-owned spans include plugin ID,
executable digest, workspace, and target repository. Telemetry initialization
or export failure never fails indexing.

## Consequences

- Private recognizers can evolve independently while producing the same typed
  semantic language as built-in analyzers.
- Generated Protobuf remains an internal transport concern; plugin authors use
  a small, documented, validated Rust API.
- New semantic entity or relation kinds still require a deliberate Beholder
  ontology and protocol change rather than unilateral plugin extension.
- Plugin jobs incur process startup and semantic-input transport costs. Bounded
  streaming avoids repeating the eager worker transport's memory failure; a
  persistent worker model requires measurements before reconsideration.
- The synchronous SDK callback blocks only a blocking-pool thread in its
  job-scoped child process. Registry loading and enrichment transport remain
  daemon-owned and non-blocking with respect to request handling.
- Serial enrichment provides a bounded resource model. A slow plugin delays
  later enrichments until its deadline, so queue latency must be observable
  before introducing parallel execution.
- Rust dynamic libraries are rejected because Rust has no stable plugin ABI.
  Lua and WebAssembly are rejected because they add runtimes while still
  requiring the same domain-specific host API.
- Hardcoded framework configuration is rejected as the extension boundary.
  Configuration remains appropriate for activation and generic input selection,
  not for encoding a private source parser.
- Persistent plugin daemons, arbitrary capabilities, raw wire access,
  plugin-to-plugin dependencies, and SDK-owned parsers are deferred until a
  demonstrated need outweighs their additional contracts.
- Existing in-process framework adapters remain in place. Extracting them into
  workers may later test protocol compatibility, but is not required to deliver
  runtime plugins.
