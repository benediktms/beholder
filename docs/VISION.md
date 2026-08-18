# Beholder — Initial Architecture and Project Plan

## 1. Purpose

**Beholder** is a multi-repository static-analysis and architecture-intelligence platform.

Its purpose is to build and continuously maintain a semantic model of a software estate so that structural questions can be answered across:

* source files;
* modules and packages;
* repositories;
* Git clones and worktrees;
* services;
* programming languages;
* RPC boundaries;
* asynchronous messaging;
* schemas and contracts;
* backend-for-frontend layers;
* web and native clients.

Beholder should eventually answer questions such as:

```text
What calls this function?

What does this function call?

What implements this RPC?

Which services call this RPC?

Who publishes this event?

Who consumes this event?

What is the blast radius of changing this protobuf field?

Which GraphQL fields depend on this backend RPC?

Which web, iOS and Android features ultimately depend on this service?

How does service A depend on service B?

Why does Beholder believe this dependency exists?

What architectural relationships are affected by this pull request?

If repo X is analyzed from this worktree and repo Y from another worktree,
what does the resulting workspace look like?
```

The project should remain generic enough to become useful outside its initial environment, while prioritising the real architecture and conventions of the environment in which it is developed.

The initial implementation will be written in **Rust** and will use **Mnestic** as its semantic fact store and inference engine.

---

# 2. Motivation

Traditional repository-oriented code-intelligence tools are useful for understanding individual codebases, but distributed systems rarely respect repository boundaries.

A real dependency path may look like:

```text
Web client
    ↓
GraphQL operation
    ↓
GraphQL field
    ↓
Backend-for-frontend resolver
    ↓
gRPC method
    ↓
Backend service
    ↓
Kafka event
    ↓
Another service
```

The contracts describing those relationships may live somewhere else entirely.

For example:

```text
proto-registry
    declares:
    company.users.v1.Users/GetUser
```

while:

```text
users-service
    implements the RPC

billing-service
    calls the RPC
```

The repository containing the `.proto` file is neither necessarily the caller nor the provider.

The same problem appears with:

* GraphQL schemas and resolvers;
* Kafka topics and producers/consumers;
* generated API clients;
* HTTP contracts;
* internal RPC and messaging abstractions.

Beholder therefore needs a model in which:

```text
source code
interfaces
contracts
implementations
consumers
```

are independent semantic entities that can be related across language, repository and service boundaries.

---

# 3. Initial Target Environment

The initial target environment contains backend code written primarily in:

```text
Elixir
Rust
Ruby
TypeScript
```

and client code written in:

```text
TypeScript
Swift
Kotlin
```

Elixir is the dominant backend language.

Rust will be the first supported language so that Beholder can immediately analyze and dogfood its own codebase.

Important technologies include:

```text
Protocol Buffers
gRPC
Kafka
GraphQL
HTTP
```

The environment also contains:

* a central protobuf registry;
* protobuf contracts used by both gRPC and Kafka;
* a central GraphQL backend-for-frontend;
* web, iOS and Android GraphQL clients;
* organisation-specific wrappers and registration systems.

Internal conventions should be supported precisely without becoming assumptions of Beholder's generic core.

---

# 4. Product Vision

Beholder maintains a **canonical semantic model of a workspace**.

A workspace is a collection of logical repositories and contract sources:

```text
Workspace

├── proto-registry
├── graphql-bff
├── users-service
├── billing-service
├── payments-service
├── legacy-ruby-service
├── web-client
├── ios-client
└── android-client
```

The **workspace**, rather than a single repository, is the primary unit of architectural analysis.

Repositories remain important for:

* ownership;
* configuration;
* source discovery;
* revision tracking;
* incremental invalidation;
* filtering.

They are not graph boundaries.

A target end-to-end dependency path is:

```text
React component
    ↓
GraphQL operation
    ↓
GraphQL field
    ↓
Elixir resolver
    ↓
gRPC method
    ↓
Elixir handler
    ↓
Kafka topic
    ↓
Protobuf contract
    ↓
consumer
```

Beholder should eventually traverse this as one continuous semantic path.

---

# 5. Core Architectural Model

The central analysis flow is:

```text
Source material
      ↓
Frontends
      ↓
Semantic observations
      ↓
Stored facts
      ↓
Resolution and inference
      ↓
Canonical relationships
      ↓
Analysis queries
```

Concretely:

```text
                 Rust
                  │
                  │ parses and observes
                  ▼
             Semantic facts
                  │
                  ▼
                 Mnestic
                  │
                  │ resolves and derives
                  ▼
           Relationships
                  │
                  ▼
       context / trace / impact
```

The most important architectural rule is:

> **Frontends observe semantic facts. They do not directly construct arbitrary workspace-wide dependency relationships.**

This keeps parsing, protocol semantics, organisation-specific conventions and global inference cleanly separated.

---

# 6. Why Mnestic

Mnestic is not merely Beholder's persistence layer.

It serves as:

1. the durable semantic fact store;
2. the relation store;
3. a declarative inference engine.

Static analysis naturally produces facts such as:

```text
A calls B

B invokes RPC R

C implements RPC R

D publishes topic T

E consumes topic T
```

Higher-level relationships can then be derived.

Conceptually:

```text
direct_dependency(A, B) :-
    calls(A, B)

direct_dependency(A, C) :-
    calls_rpc(A, R),
    implements_rpc(C, R)

direct_dependency(D, E) :-
    publishes(D, T),
    consumes(E, T)
```

Recursive rules can derive transitive relationships:

```text
dependency(A, B) :-
    direct_dependency(A, B)

dependency(A, C) :-
    direct_dependency(A, B),
    dependency(B, C)
```

The project deliberately uses Mnestic as an opportunity to test whether **relational facts plus Datalog inference** provide a better foundation for static analysis than constructing a property graph first and implementing all semantics through imperative graph traversal.

The analysis database is derived state and must remain rebuildable, so the project can tolerate greater experimentation with this architectural choice than would be appropriate for primary business data.

---

# 7. Responsibility Boundaries

## 7.1 Rust owns

Rust should handle work fundamentally concerned with interpreting source material or executing procedural algorithms.

Examples:

```text
Git operations
filesystem discovery
content hashing
Tree-sitter parsing
AST traversal
source ranges
language-specific extraction
protobuf descriptor ingestion
GraphQL parsing
incremental changed-file discovery
procedural symbol resolution
specialised graph algorithms
daemon orchestration
```

## 7.2 Mnestic owns

Mnestic should handle semantic relations and declarative inference.

Examples:

```text
semantic fact persistence
cross-repository joins
canonical contract matching
provider/consumer matching
recursive dependency derivation
GraphQL operation-to-field relationships
gRPC caller-to-provider relationships
Kafka producer-to-consumer relationships
workspace-wide dependency analysis
```

## 7.3 Rust fixed rules

Not every analysis should be forced into Datalog.

Specialised algorithms may be implemented in Rust and exposed to Mnestic where appropriate.

Potential examples:

```text
weighted path finding
blast-radius scoring
strongly connected components
specialised shortest paths
custom symbol matching
large graph algorithms
```

The system should favour the simplest and clearest representation for each analysis.

---

# 8. Canonical Git and Workspace State Model

Git worktrees, repeated clones and agentic workflows require Beholder to distinguish several identities.

The canonical hierarchy is:

```text
LogicalRepository
    ↓
GitClone
    ↓
WorkingTree
    ↓
RepositoryState

Workspace
    ↓
WorkspaceView
    ↓
AnalysisRevision
```

These terms should be used consistently throughout the project.

---

# 9. LogicalRepository

A `LogicalRepository` represents the conceptual Git repository regardless of how many copies exist on disk.

Example:

```text
github.com/company/payments
```

Its identity should normally be derived from a canonicalised Git remote.

Equivalent remotes such as:

```text
git@github.com:company/payments.git
https://github.com/company/payments.git
ssh://git@github.com/company/payments.git
```

should normally resolve to the same logical repository.

Automatic resolution must be overrideable.

Suggested identity priority:

```text
1. explicitly configured repository identity
2. explicitly configured identity remote
3. origin
4. only available remote
5. generated local-only identity
```

All discovered remote aliases should be retained as metadata and evidence.

Repository identity must never depend permanently on an unoverrideable heuristic.

---

# 10. GitClone

A `GitClone` represents one local Git administrative repository/object store.

Linked Git worktrees belonging to the same clone share the same Git common directory.

Independent clones of the same logical repository therefore have:

```text
different GitClone identities
same LogicalRepository identity
```

The daemon should use Git's common-directory information to reconcile linked worktrees correctly.

---

# 11. WorkingTree

A `WorkingTree` represents a concrete checkout on disk.

Different worktrees may simultaneously contain:

```text
different branches
different commits
detached HEADs
different uncommitted changes
```

A worktree is therefore a filesystem location and Git checkout context.

It is not itself the semantic state Beholder indexes.

---

# 12. RepositoryState

A `RepositoryState` represents an **immutable semantic identity of the source tree being analyzed**.

Multiple clones or working trees may point at the same repository state.

For a clean repository, Git's commit/tree identity can provide an efficient state representation.

For a dirty repository, the identity must additionally account for the relevant working-tree contents.

Conceptually:

```text
RepositoryState =
    LogicalRepository
    + HEAD
    + relevant source-tree state
    + analysis-relevant configuration
```

The exact state fingerprinting algorithm is an implementation detail, but `RepositoryState` itself is a foundational abstraction.

The state should represent **what source was actually analyzed**, not merely which commit is currently checked out.

---

# 13. Workspace

A `Workspace` is a configured collection of:

```text
LogicalRepositories
ContractSources
AnalysisConfiguration
OrganisationAdapters
```

It represents the software estate to analyze.

A workspace may contain repositories located in unrelated directories and may include several independent Git clones.

---

# 14. WorkspaceView

A `WorkspaceView` selects which `RepositoryState` represents each logical repository.

Example:

```text
BaseView

repo-a → A0
repo-b → B0
repo-c → C0
```

An agent working in a separate worktree may instead need:

```text
repo-a → A1
repo-b → B0
repo-c → C0
```

without modifying the base view.

Views are therefore the primary mechanism for reasoning about:

* branches;
* dirty working trees;
* agent-created worktrees;
* alternative repository states;
* hypothetical workspace combinations.

---

# 15. Manual View Overrides in V1

View composition should remain intentionally simple in V1.

A caller starts with a base view and explicitly overrides selected repositories.

Example:

```text
BaseView:

repo-a → A0
repo-b → B0
repo-c → C0
```

Query overrides:

```text
repo-a → A1
repo-c → C1
```

Effective view:

```text
repo-a → A1
repo-b → B0
repo-c → C1
```

There is no automatic merge logic.

There is no automatic composition of agent views.

There is no automatic Git merge.

If two proposed overrides select conflicting states for the same logical repository, V1 should reject the request as ambiguous.

Automatic composition and semantic merge analysis are future capabilities.

---

# 16. AnalysisRevision

An `AnalysisRevision` is a completed analysis of an effective `WorkspaceView`.

Only completed revisions are queryable.

An analysis revision records which repository states participated in the result.

Every query result can therefore be tied to a coherent semantic workspace state.

Conceptually:

```text
AnalysisRevision 184

users-service       → RepositoryState U12
billing-service     → RepositoryState B7
graphql-bff         → RepositoryState G31
proto-registry      → RepositoryState P4
```

---

# 17. Agent and Worktree Support

Agentic development workflows commonly create linked Git worktrees or duplicate clones.

Beholder should automatically discover linked worktrees belonging to registered clones.

Example:

```text
LogicalRepository: payments

Clone A
├── main checkout
├── agent-1 worktree
└── agent-2 worktree

Clone B
└── another checkout
```

All belong to the same logical repository but may expose different repository states.

CLI and MCP clients should provide contextual information such as their current working directory.

The daemon can use this to determine which worktree the caller is operating inside and select that repository state while continuing to use the base workspace view for unrelated repositories.

This should make agent workflows work naturally without requiring explicit view configuration for ordinary single-repository worktree usage.

---

# 18. Content-Addressed Analysis Deduplication

Worktrees and repeated clones create an opportunity for significant analysis reuse.

If identical source contents appear in several repository states, syntax analysis should not be repeated unnecessarily.

The first cache scope should therefore be content-addressed.

Conceptually:

```text
SourceAnalysisKey {
    content_hash,
    language,
    frontend_version,
    configuration_hash
}
```

Cached output may include:

```text
symbol declarations
raw call observations
imports
AST-derived facts
source-level metadata
```

If three worktrees contain the same file contents:

```text
Worktree A ─┐
Worktree B ─┼─→ content hash ABC
Worktree C ─┘
```

all can reuse the same extracted observations.

---

# 19. Analysis Cache Scopes

Three distinct cache and invalidation scopes should be recognised.

## 19.1 Content scope

Keyed approximately by:

```text
content hash
language
frontend version
frontend configuration
```

Contains:

```text
AST-derived observations
symbol declarations
raw calls
imports
```

This scope can be shared across clones, worktrees and repository states.

## 19.2 Repository-state scope

Keyed approximately by:

```text
RepositoryState
resolver versions
analysis configuration
```

Contains:

```text
cross-file symbol resolution
local call relationships
language/framework relationships
```

This can be reused by every workspace view that references the same repository state.

## 19.3 Workspace-view scope

Keyed approximately by:

```text
effective WorkspaceView
rule-pack versions
contract-source states
organisation-adapter versions
```

Contains or derives:

```text
cross-repository relationships
gRPC bindings
Kafka relationships
GraphQL-to-backend relationships
workspace-wide dependency information
```

Not all cache layers need to exist in the first implementation, but the architecture must preserve these boundaries.

---

# 20. Analyzer and Rule Versioning

Unchanged source does not imply unchanged analysis.

If an Elixir frontend or resolver becomes more accurate, previously cached analysis may no longer be valid.

Every layer of derived state must therefore depend on both:

```text
input identity
analysis implementation identity
```

Versioned components include:

```text
language frontends
contract frontends
resolvers
rule packs
organisation adapters
```

Core invariant:

> **All derived state must be invalidatable when either its inputs or the implementation that derived it change.**

---

# 21. Daemon-First Architecture

Beholder should be daemon-first.

A single user-level daemon, **`beholderd`**, should manage multiple workspaces.

Conceptually:

```text
                 Client surfaces

          Admin CLI       MCP       IDE
              │            │         │
              └────────────┼─────────┘
                           │
                          gRPC
                           │
                           ▼
                ┌───────────┐
                │ beholderd │
                └─────┬─────┘
                      │
       ┌──────────────┼──────────────┐
       ▼              ▼              ▼
 Workspace Mgr   Query Service   Index Scheduler
                                      │
                          ┌───────────┼───────────┐
                          ▼           ▼           ▼
                       Git state   watchers   reconcile
                          │           │           │
                          └───────────┼───────────┘
                                      ▼
                               changed inputs
                                      │
                                      ▼
                              parallel analysis
                                      │
                                      ▼
                                   Mnestic
                                      │
                                      ▼
                            AnalysisRevision
```

The daemon owns:

```text
workspace registration
repository discovery
Git clone reconciliation
linked-worktree discovery
filesystem watching
hashing
index scheduling
worker coordination
Mnestic write coordination
query execution
analysis revision publication
```

The CLI, MCP server and future integrations should remain thin clients. The
CLI is primarily an administrative surface for daemon control, workspace
management, manual reindexing, cache maintenance, and inspection. MCP is the
primary semantic surface for context, dependencies, impact, trace, why, and
open-ended exploration. Existing CLI semantic commands may remain as
transitional compatibility and debugging surfaces, but new semantic features
should be designed and exposed through MCP first.

---

# 22. gRPC Daemon Protocol

Communication between Beholder clients and `beholderd` should use **gRPC**.

This is heavier than a simple JSON protocol, but it provides useful benefits:

* strongly typed contracts;
* generated Rust client/server code;
* backwards-compatible protocol evolution;
* server streaming;
* future client/bidirectional streaming;
* immediate dogfooding of protobuf and gRPC support.

The daemon API should be versioned from the first implementation.

Suggested layout:

```text
proto/
    beholder/v1/
        daemon.proto
        workspace.proto
        analysis.proto
        indexing.proto
```

Potential operations include:

```text
GetStatus
RegisterWorkspace
ListWorkspaces
Context
Trace
Impact
Dependencies
Why
Reindex
SubscribeWorkspace
```

The protocol should expose Beholder domain concepts rather than raw Mnestic operations.

---

# 23. Daemon Lifecycle

The CLI should expose explicit lifecycle commands:

```text
beholder daemon start
beholder daemon stop
beholder daemon status
beholder daemon run
```

Normal Beholder commands may automatically start the daemon if required.

`beholder daemon run` should run the same server in the foreground for:

```text
development
debugging
CI
containers
integration tests
```

Daemonisation should remain separate from the implementation of the server itself.

`beholderd` must enforce a single user-level instance with an operating-system file lock. The
locked PID file registers the owning process for diagnostics, but PID contents alone must never
be used as the single-instance gate: a crashed process may leave stale contents while its OS lock
is released.

---

# 24. Index Scheduling

Filesystem events should not directly become indexing jobs.

The scheduler should track **desired source state**, not an event log.

For example:

```text
foo.ex changed
foo.ex changed again
foo.ex changed again
```

should become:

```text
foo.ex = DIRTY
```

rather than:

```text
index foo v1
index foo v2
index foo v3
```

The indexing subsystem is therefore a **coalescing state-based scheduler**.

---

# 25. Index Intents

Raw input signals should be normalized into higher-level indexing intents.

Conceptually:

```rust
enum IndexIntent {
    PathsDirty,
    RepositoryHeadChanged,
    ReconcileRepository,
    ReconcileWorkspace,
    ContractSourceChanged,
    ForceReindex,
}
```

Intents may subsume each other.

Example:

```text
PathsDirty(repo-a)
+
ReconcileRepository(repo-a)
```

reduces to:

```text
ReconcileRepository(repo-a)
```

A workspace-wide reconciliation similarly supersedes narrower work.

The daemon's queue is therefore an ephemeral coordination mechanism, not a durable FIFO job log.

No external queueing infrastructure is required for V1.

---

# 26. Batching

The scheduler should form indexing batches after a short settling period.

A pure debounce is insufficient because continuous editing could postpone indexing indefinitely.

Batch formation should therefore use both:

```text
quiet-period threshold
maximum pending latency
```

The first threshold reached starts the batch.

Exact timing values should be configurable and tuned through dogfooding.

---

# 27. Parsing Work Queue

Once a batch has been formed, conventional concurrent work-queue semantics become appropriate.

Example:

```text
Batch 42

foo.ex       → worker
bar.ex       → worker
baz.rs       → worker
schema.proto → worker
```

Each job should contain the source identity it analyzed.

Conceptually:

```rust
ParseJob {
    source_unit,
    content_hash,
    generation,
}
```

Before committing a result, the daemon must verify that the analyzed input is still relevant.

If:

```text
parsed hash = A
current desired hash = B
```

the analysis of `A` should be discarded.

---

# 28. Index Storms and Reconciliation

Large Git operations may generate thousands of filesystem events.

Examples:

```text
git checkout
git pull
git merge
git rebase
git reset
large code generation
```

The scheduler should detect these storms and promote work to repository-level reconciliation.

Triggers may include:

```text
high event rate
large number of unique paths
HEAD changes
Git administrative changes
```

Instead of:

```text
7,000 watcher events
```

the scheduler can reduce the work to:

```text
ReconcileRepository(repo)
```

Git can help identify candidate changed paths when revisions differ.

Content hashes remain authoritative.

---

# 29. Filesystem Events Are Hints

Filesystem notifications are an optimization.

They are not the source of truth.

An event means:

> this source path may have changed.

Actual change should ultimately be determined from current file state and content hashes.

Core invariant:

> **Filesystem events trigger validation; content hashes determine whether source changed.**

---

# 30. Periodic Reconciliation

Watchers may miss events or behave differently across filesystems.

The daemon should periodically reconcile registered working trees.

The safety path is:

```text
periodic reconciliation
        ↓
compare known state
        ↓
detect missed changes
        ↓
emit indexing intent
```

Reconciliation provides correctness; watchers provide responsiveness.

---

# 31. Query Consistency

Only completed `AnalysisRevision`s are queryable.

While a new revision is being built:

```text
Revision 41 = COMPLETE
Revision 42 = BUILDING
```

queries continue using revision 41.

A partially updated workspace must never be exposed as a coherent result.

If the filesystem has advanced beyond the latest completed revision, query responses should include freshness metadata.

Conceptually:

```text
analysis_revision: 41
freshness: stale
indexing: true
dirty_repositories:
  - payments
```

V1 should prefer **latest completed** consistency rather than blocking every query until the index catches up.

---

# 32. Mnestic Write Coordination

The initial Mnestic configuration should use its SQLite-backed embedded storage.

Parsing and extraction can occur concurrently, but writes should be coordinated through a single workspace commit path.

Conceptually:

```text
                Index Coordinator
                        │
     ┌──────────────────┼──────────────────┐
     ▼                  ▼                  ▼
 parse worker       parse worker       parse worker
     │                  │                  │
     └──────────────────┼──────────────────┘
                        ▼
                     facts
                        │
                        ▼
               Commit Coordinator
                        │
                        ▼
                      Mnestic
```

This separates analysis concurrency from persistence semantics.

---

# 33. Semantic Domains

The model should distinguish several broad domains.

## Workspace entities

```text
Workspace
LogicalRepository
GitClone
WorkingTree
RepositoryState
WorkspaceView
AnalysisRevision
SourceUnit
```

## Code entities

```text
Namespace
Module
Type
Callable
Interface
Implementation
Component
```

## Interface entities

```text
gRPC RPC
GraphQL field
HTTP endpoint
Kafka topic
queue
subscription
```

## Contract entities

```text
Protobuf message
Protobuf field
GraphQL object
GraphQL input
OpenAPI schema
```

Language-specific constructs should be preserved through specialised relations or attributes without forcing all languages into identical semantics.

---

# 34. Stable Semantic Identity

Contract identities should be semantic rather than based on filesystem location.

Examples:

```text
proto-type://company.users.v1.User

proto-field://company.users.v1.User/email

proto-method://company.users.v1.Users/GetUser

graphql-field://Query/customer

kafka-topic://users.updated.v1

```

Moving a schema file should not recreate the contract.

Source symbol identity may initially resemble:

```text
repo://payments/elixir/MyApp.Payments/create_payment/2

repo://beholder/rust/core/resolver/resolve
```

Source locations should remain metadata, not primary identity.

---

# 35. Observations

Frontends should generally emit observations rather than final architectural conclusions.

Examples:

```text
call_observation
module_reference
import_observation

grpc_call_observation
grpc_implementation_observation

kafka_publish_observation
kafka_consume_observation

graphql_selection_observation
graphql_resolver_observation
```

Observations should preserve enough information for future resolvers to reinterpret them.

This allows improvements to resolution without necessarily reparsing unchanged source.

---

# 36. Canonical Relationships

Resolution should produce canonical relations such as:

```text
calls

implements

calls_rpc
implements_rpc

publishes
consumes

resolves
selects

carries

request_type
response_type
```

Higher-level relationships such as transitive dependency and blast radius should normally be derived from these canonical direct relationships.

---

# 37. Provenance and Confidence

Every important observation and relationship should be explainable.

Evidence should include information such as:

```text
source kind
source unit
source range
confidence
analyzer
```

Potential evidence sources include:

```text
AST
compiler
descriptor
generated code
explicit configuration
organisation adapter
heuristic
runtime
```

Confidence should be queryable rather than hidden inside opaque metadata.

Example values:

```text
1.0 compiler-resolved call
1.0 explicit configuration
1.0 protobuf descriptor

0.95 generated gRPC mapping

0.8 framework convention

0.5 ambiguous dynamic dispatch
```

Queries should eventually support minimum confidence thresholds.

---

# 38. Generated Code

Generated code should be represented explicitly.

Source units and symbols should distinguish:

```text
source
generated
external dependency
```

Generated code may be valuable for resolution:

```text
application code
    ↓
generated gRPC client
    ↓
canonical RPC
```

but should not necessarily clutter user-facing traces.

Presentation may collapse generated nodes while `why` exposes them as supporting evidence.

---

# 39. Extension Architecture

Beholder should support four extension categories.

## 39.1 Language frontends

Translate source code into semantic observations.

Initial target order:

```text
Rust
Elixir
TypeScript
Ruby
Swift
Kotlin
```

Potential later targets include:

```text
Go
Python
Java
C#
C++
```

## 39.2 Contract frontends

Ingest canonical schema systems.

Initial:

```text
Protobuf
GraphQL
```

Potential future systems:

```text
OpenAPI
JSON Schema
Thrift
AsyncAPI
```

## 39.3 Technology resolvers

Interpret observations using framework or protocol semantics.

Examples:

```text
gRPC
Kafka
GraphQL server
GraphQL client
HTTP
Phoenix
Rails
Apollo
```

## 39.4 Organisation adapters

Encode internal conventions.

Examples:

```text
InternalKafkaResolver
InternalGrpcResolver
InternalProtoRegistry
InternalGraphQLResolver
InternalServiceRegistry
```

Organisation adapters should produce ordinary semantic facts and relationships rather than adding organisation-specific primitives to Beholder's core model.

---

# 40. Rule Packs

Datalog rules should be explicit, version-controlled source code.

Possible layout:

```text
rules/

    core/
        dependency.datalog
        impact.datalog
        symbols.datalog

    protocols/
        grpc.datalog
        kafka.datalog
        graphql.datalog

    contracts/
        protobuf.datalog

    languages/
        rust.datalog
        elixir.datalog

    integrations/
        internal.datalog
```

Rule packs should be independently testable.

Example:

```text
given:

calls_rpc(A, R)
implements_rpc(B, R)

expect:

direct_dependency(A, B)
```

Parser correctness and inference correctness should remain separately testable.

---

# 41. Rust Language Support

Rust will be the first supported language.

Beholder should use its own repository as its first real analysis fixture.

Initial Rust support should include:

```text
crates
modules
functions
methods
structs
enums
traits
impl blocks
use statements
function calls
method calls
basic symbol resolution
```

The objective is useful self-analysis rather than immediate compiler-level completeness.

---

# 42. Elixir Language Support

Elixir is the highest-priority production language.

Initial support should include:

```text
defmodule
def
defp
alias
import
require
use
remote calls
local calls
struct usage
behaviours
callbacks
```

Later:

```text
protocols
macros
generated functions
framework-specific semantics
```

The analyzer must represent uncertainty instead of inventing relationships that cannot be established statically.

---

# 43. Protobuf Registry

The central protobuf repository should be treated as an authoritative contract source.

Preferred ingestion:

```text
.proto files
     ↓
Buf / protobuf compiler
     ↓
FileDescriptorSet
     ↓
Rust contract frontend
     ↓
Mnestic facts
```

Entities should include:

```text
ProtoFile
ProtoMessage
ProtoField
ProtoEnum
ProtoService
RpcMethod
```

Relationships should include:

```text
request_type
response_type
field_of
```

The repository containing the protobuf declaration must not determine runtime ownership.

---

# 44. gRPC

gRPC resolution connects application code to canonical RPC identities.

Example:

```text
Billing.fetch_user/2
       │
       │ CALLS_RPC
       ▼
grpc://company.users.v1.Users/GetUser
       ├── BINDS_CONTRACT ──▶ proto-method://company.users.v1.Users/GetUser
       │
       └── IMPLEMENTED_BY ──▶ UsersServer.get_user/2
```

Evidence may come from:

```text
generated clients
generated server behaviours
framework registration
explicit configuration
organisation-specific conventions
heuristics
```

A `.proto` file does not need to be present in either application repository.

---

# 45. Kafka

Kafka topics should be independent interface entities.

Example:

```text
Orders.publish_created/1
       │
       │ PUBLISHES
       ▼
orders.created.v1
       │
       │ CARRIES
       ▼
company.orders.v1.OrderCreated
```

A separate consumer may provide:

```text
Billing.handle_created/1
       │
       │ CONSUMES
       ▼
orders.created.v1
```

Producer-to-consumer dependency can then be inferred declaratively.

---

# 46. GraphQL

GraphQL should be a first-class interface and contract domain.

Entities should include:

```text
GraphQLSchema
GraphQLObject
GraphQLInterface
GraphQLUnion
GraphQLInput
GraphQLEnum
GraphQLScalar
GraphQLField
GraphQLOperation
```

Server relationship:

```text
Query.customer
      │
      │ RESOLVED_BY
      ▼
CustomerResolver.customer/3
```

Client relationship:

```text
GetCustomerQuery
      │
      │ SELECTS
      ▼
Query.customer
```

---

# 48. GraphQL Backend-for-Frontend

The central GraphQL BFF forms an important architectural spine.

It connects:

```text
Web
iOS
Android
```

to backend services.

Target path:

```text
React component
     ↓
GraphQL operation
     ↓
GraphQL field
     ↓
Elixir resolver
     ↓
gRPC
     ↓
backend implementation
```

The same schema should connect Swift and Kotlin GraphQL clients.

Eventually, a backend change should be traceable to user-facing functionality across all supported client platforms.

---

# 49. Workspace Configuration

Beholder should use a checked-in workspace manifest.

Conceptually:

```yaml
workspace: platform

repositories:
  users:
    remote: github.com/company/users

  billing:
    remote: github.com/company/billing

  graphql-bff:
    remote: github.com/company/graphql-bff

contract_sources:
  protobuf:
    repository: proto-registry

  graphql:
    repository: graphql-bff
```

Configuration will eventually describe:

```text
repository identity overrides
canonical remotes
contract sources
ignore rules
generated-code policies
organisation adapters
analysis settings
```

Automatic discovery should augment explicit configuration, not replace it.

---

# 50. Build and Repository Orchestration

Beholder will use **Cargo** and **Moonrepo** together from the initial commit.

Their responsibilities are deliberately separate.

```text
Cargo
    ↓
Rust packages
crate dependency graph
compilation
tests
Rust build semantics


Moonrepo
    ↓
repository task entry points
caching
CI workflows
developer commands
```

Cargo remains authoritative for the Rust workspace.

Moonrepo provides a thin repository-level task runner. Beholder is one Moon project;
Moon must not mirror Cargo's crate dependency graph or launch one Cargo process per
crate. Cargo owns package scheduling and build concurrency.

Moon should standardise tasks such as:

```text
build
check
test
lint
format
generate-proto
integration-test
benchmark
dogfood
```

Possible commands include:

```text
moon run beholder:check
moon run beholder:test
moon run beholder:lint

moon run beholder:smoke
```

The dogfooding task should eventually:

```text
build beholderd and CLI
        ↓
start an isolated daemon
        ↓
index this repository
        ↓
wait for a completed AnalysisRevision
        ↓
execute known semantic queries
        ↓
assert expected relationships
```

Moonrepo is strictly a development-time dependency of the Beholder repository.

It must not become part of Beholder's runtime workspace semantics.

The following concepts remain separate:

```text
Moon workspace
    = Beholder's development repository/projects

Cargo workspace
    = Beholder's Rust crates

Beholder Workspace
    = potentially many independent repositories analyzed as one system
```

Users of Beholder should not need Moonrepo unless they are contributing to Beholder itself.

---

# 51. Rebuildability

The analysis database is **derived state and must remain disposable**.

The canonical sources of truth are:

```text
Git repositories
workspace configuration
contract registries
schemas
frontend versions
resolver versions
rule versions
```

It must always be possible to reconstruct Mnestic state from these inputs.

This allows the project to tolerate:

```text
schema evolution
analysis model redesign
Mnestic upgrades
storage experiments
```

without making the database irreplaceable.

---

# 52. Daemon State vs Analysis State

Daemon coordination state should remain separate from semantic state.

Ephemeral daemon state includes:

```text
watch handles
dirty paths
pending intents
active batches
workers
client connections
```

Persistent semantic state includes:

```text
repositories
repository states
workspace views
analysis revisions
facts
relationships
evidence
```

Mnestic should not become Beholder's job queue.

---

# 53. Query API

The primary API should expose semantic operations rather than raw Datalog.

## `context`

```text
beholder context <entity>
```

Returns nearby semantic information:

```text
definition
repository
callers
callees
contracts
interfaces
producers
consumers
GraphQL associations
evidence
```

## `trace`

```text
beholder trace <from> <to>
```

Example:

```text
CheckoutPage
→ CheckoutQuery
→ Query.checkout
→ CheckoutResolver
→ Pricing/GetPrice
→ PricingServer.get_price
```

## Open-ended behavior traversal

Pairwise `trace <from> <to>` is useful when both endpoints are known, but
exploration often starts with only one symbol. Beholder should also support a
bounded traversal rooted at one entity that discovers reachable behavior
without requiring a target endpoint in advance.

Conceptually:

```text
beholder explore <entity>
```

The traversal should support direction, maximum depth, repository scope,
minimum confidence, relation kinds, and generated-code visibility. Its result
should be a deterministic tree or forest of reachable symbols, contracts, and
repository boundaries, with candidate endpoints, supporting evidence, and
explicit truncation metadata. Pairwise trace remains the focused way to verify
one selected path; open-ended traversal is the discovery form.

Where language analysis exposes conditional control flow, traversal must retain
the branch structure: branch-specific paths for constructs such as `if/else`,
their guards when known, and convergence after the branches rejoin. This is
distinct from merely finding multiple unrelated graph edges; full CFG and PDG
construction remains future advanced static analysis.

Both directions are first-class:

```text
downstream  what behavior does this entity lead to?
upstream    what callers and paths lead to this entity?
```

An upstream query must be able to return all bounded paths that lead to the
root code point, not only its immediate callers. Because path counts can grow
quickly in converging graphs, the API must make maximum depth, maximum paths,
and truncation explicit and deterministic.

MCP is the canonical semantic interface. It should expose the shared query
operations and structured result shapes as a thin client, so agents and IDEs
receive identical semantic answers and evidence. The CLI remains available for
administration and transitional semantic compatibility, but it should not grow
an independent query model.

## `impact`

```text
beholder impact <entity>
```

Calculates transitive blast radius.

Future filters may include:

```text
direction
maximum depth
minimum confidence
repository
relation kinds
generated-code visibility
```

## `dependencies`

```text
beholder dependencies <entity>
```

Supports direct and transitive views at:

```text
symbol
service
repository
```

levels.

## `why`

```text
beholder why <A> <B>
```

Explains why one entity depends on another by returning the supporting semantic path and evidence.

Explainability is a core feature.

---

# 54. Mnestic Time Travel

Mnestic time-travel functionality is **not part of V1**.

The authoritative state model is:

```text
RepositoryState
WorkspaceView
AnalysisRevision
```

This correctly represents Git branches and simultaneously valid worktree states.

Mnestic temporal relations may later be useful for longitudinal analysis of a canonical workspace lineage such as `main` or `master`.

Potential future questions include:

```text
When did service A begin depending on service B?

What did the canonical architecture look like last week?

When was this GraphQL dependency introduced?
```

Time travel is a future historical-analysis capability, not Beholder's core versioning model.

---

# 55. Initial Repository Structure

A likely early layout is:

```text
Cargo.toml
Cargo.lock
mise.toml
rust-toolchain.toml

.moon/
    workspace.yml

moon.yml

crates/
    cli/
    daemon/
    daemon-client/
    protocol/
    domain/
    dto/
    adapters-git/
    adapters-mnestic/
    adapters-treesitter-rust/

proto/
    beholder/v1/

rules/
```

This structure should remain pragmatic.

Do not create a separate crate for every conceptual module unless a meaningful dependency or compilation boundary exists.

The workspace follows pragmatic hexagonal architecture:

```text
CLI and future daemon composition roots
                 │
                 ▼
        domain and DTO contracts
                 ▲
                 │
   Git / Mnestic / language adapters
```

Third-party dependencies should be declared by the crate that owns them. Only broad, genuinely workspace-wide utilities such as `serde`, `tracing` or `tokio` should be versioned at workspace scope.

The Mnestic boundary is deliberately opaque. Mnestic handles, values, queries and transactions must remain inside `adapters-mnestic`; other crates communicate through Beholder-owned domain and DTO types.

Application, ports, daemon and additional adapter crates should be introduced only when a real dependency seam requires them.

---

# 56. Non-Goals for V1

The initial implementation should not prioritise:

```text
semantic embeddings
GraphRAG
LLM-generated architecture documentation
large graph visualisation UI
distributed storage
SaaS deployment
runtime tracing
perfect compiler semantics
dynamic binary plugins
dozens of programming languages
complete macro expansion
Mnestic time travel
automatic view composition
automatic Git merges
full CFG/PDG analysis
```

The initial goal is:

> **correct, explainable and continuously maintained structural intelligence.**

---

# 57. Phase 0 — Architecture Spike

Before implementing broad language support, validate the Mnestic/Datalog architectural bet.

Create a synthetic workspace containing:

```text
symbols
calls
RPCs
gRPC callers
gRPC implementations
Kafka topics
producers
consumers
GraphQL fields
GraphQL operations
multiple repositories
multiple repository states
workspace views
```

Implement:

```text
direct dependency
transitive dependency
context
trace
impact
why
```

Use a reasonably large synthetic dataset.

Initial target:

```text
~100,000 semantic entities
~1,000,000 relationships
```

The objective is not a rigorous database benchmark.

Evaluate:

```text
Datalog clarity
query ergonomics
recursive query performance
schema ergonomics
Rust integration
debugging experience
workspace-view filtering
```

### Exit criterion

Beholder is comfortable treating:

> **Mnestic relations plus Datalog inference as the foundation of the semantic analysis engine.**

If this assumption proves wrong, Phase 0 is deliberately early enough to replace it.

### Outcome — accepted 2026-08-13

Mnestic relations and Datalog inference are accepted as the semantic-engine foundation.

The spike established these operating constraints:

* use SQLite persistence by default;
* anchor recursive rules at the requested source;
* bound interactive traversal depth;
* infer minimum distance before expanding path evidence;
* parallelise independent ingestion and queries above Mnestic rather than relying on intra-query parallelism;
* batch or incrementally ingest larger corpora to limit construction-time memory.

At approximately 9.8 million synthetic relationships, the SQLite database occupied 1.3 GB, loaded in 19 seconds and served bounded context, trace and impact queries below 10 milliseconds. Bulk construction peaked at 4.37 GB RSS, making ingestion memory the first scaling constraint.

---

# 58. Phase 1 — Daemon, Core Infrastructure and Rust Dogfooding

Initialize the Cargo and Moonrepo workspaces.

Implement:

```text
stable domain IDs
logical repository identity
Git clone discovery
linked-worktree discovery
repository states
workspace views
analysis revisions

Mnestic schema
rule loading

beholderd lifecycle
gRPC daemon protocol

workspace registration

filesystem watching
content hashing
index scheduler
batching
periodic reconciliation

Tree-sitter infrastructure
Rust frontend

symbol observations
call observations
basic call resolution
```

Implement initial queries:

```text
context
trace
impact
dependencies
why
```

### Exit criterion

Beholder continuously analyzes its own repository and provides useful structural information about itself.

---

# 59. Phase 2 — Incremental Analysis and Cache Reuse

Implement:

```text
content-addressed frontend cache
frontend-version invalidation
repository-state caching
workspace-view invalidation

dirty-state fingerprints
staleness metadata
incremental fact replacement
```

### Exit criterion

Creating a worktree that changes only a small number of files reuses analysis for the unchanged majority of the repository.

---

# 60. Phase 3 — Protobuf Registry

Implement:

```text
descriptor ingestion
messages
fields
enums
services
RPCs
request types
response types
stable semantic identities
```

### Exit criterion

The central protobuf registry exists as canonical semantic relations independent of runtime ownership.

---

# 61. Phase 4 — Elixir

Implement:

```text
modules
functions
aliases
imports
requires
uses
calls
behaviours
callbacks
struct usage
```

### Exit criterion

Representative Elixir services produce useful symbol and local call relationships.

---

# 62. Phase 5 — gRPC

Implement:

```text
gRPC client observations
gRPC server observations
generated-code mapping
internal gRPC conventions
canonical RPC resolution
```

### Exit criterion

The following works in both directions across independent contract, Rust
application, and Elixir application repositories:

```text
Rust or Elixir caller
      ↓
canonical RPC
      ↓
Elixir or Rust implementation
```

The RPC also binds to the canonical Protobuf method regardless of where the
descriptor exists. Removing and restoring that contract republishes unresolved
and resolved bindings without reparsing unchanged applications.

Beholder should also successfully analyze its own CLI-to-daemon gRPC API.

---

# 63. Phase 6 — JavaScript and TypeScript Foundation

Implement:

```text
JavaScript and TypeScript symbols
functions, classes, interfaces, methods, and decorators
imports, exports, re-exports, package exports, and tsconfig path aliases
type-aware receiver, inheritance, assignment, factory, and callback resolution
asynchronous calls and JSX
checked-in generated source and test visibility
```

### Exit criterion

A source-backed call path crosses independent JavaScript or TypeScript
repositories through a shared package. Representative functional, class-based,
dependency-injected, asynchronous, and JSX paths resolve without compiler or
language-service execution; unsupported receiver flow remains explicit and
diagnosed rather than guessed.

---

# 64. Phase 7 — TypeScript Protobuf and gRPC

Implement:

```text
TypeScript generated Protobuf identities
TypeScript gRPC client observations
TypeScript gRPC server observations
canonical RPC and message resolution
```

### Exit criterion

A TypeScript caller traverses its generated client to a canonical RPC and from
there to an implementation in another registered repository. The contract
identity remains independent of generated-code layout.

---

# 65. Phase 8 — GraphQL

Implement:

```text
GraphQL schema ingestion
types
fields
inputs
Elixir resolver mappings
TypeScript gateway resolvers and schema stitching
GraphQL operations
schema selections
generated GraphQL clients
React ownership where useful
```

### Exit criterion

A path can be traced:

```text
web feature
    ↓
GraphQL operation and field
    ↓
gateway and backend resolver
    ↓
backend
```

---

# 66. Phase 9 — Query Semantics and Exploration

Make semantic exploration useful when the user knows a starting entity but not
the eventual endpoint.

Implement:

```text
open-ended behavior traversal
direction and repository scoping
bounded deterministic call trees or forests
candidate endpoint discovery
evidence-preserving traversal output
upstream path discovery to a selected code point
branch-aware paths for conditional control flow
MCP semantic tools over the shared query API
administrative CLI integration and transitional semantic compatibility
```

The existing pairwise `trace <from> <to>` query remains as a precise path
verification primitive. This phase adds the discovery-oriented query without
turning runtime execution into a prerequisite for static analysis. The MCP
server is a delivery surface, not a second semantic implementation.

---

# 67. Phase 10 — Ruby

Add legacy backend support, prioritising the relationships required for workspace-wide architectural analysis.

---

# 68. Phase 11 — Swift and Kotlin

Add enough language and GraphQL-client support to connect native applications into the central GraphQL dependency graph.

---

# 69. Post-MVP — Kafka + Protobuf and Convention Resolvers

Kafka support follows the MVP because recognizing publishers, consumers,
topics, payloads, and internal wrappers generically first requires the
workspace-configurable convention/resolver layer. Beholder's ontology remains
fixed; configuration customizes recognition rather than inventing relations.

The eventual implementation should cover canonical topic identities,
publishers, consumers, Protobuf envelopes, payload relationships, and the
blast radius of changing a carried message without relying on runtime
environment variables.

### Exit criterion

Backend changes can be traced to:

```text
Web
iOS
Android
```

---

# 69. Phase 12 — Change Impact and CI

Analyze Git changes and derive:

```text
changed symbols
changed contracts
affected services
affected GraphQL fields
affected clients
```

Target workflow:

```text
Pull Request
      ↓
Changed source/contracts
      ↓
Semantic impact
      ↓
Affected architecture
```

This should eventually become suitable for CI and pre-merge analysis.

---

# 70. Future — Manual Multi-View Compatibility Analysis

Once workspace views are stable, support explicit hypothetical combinations:

```text
BaseView
+
repo-a → AgentAState
+
repo-c → AgentCState
```

to answer:

> Are these explicitly selected changes architecturally compatible when considered together?

Composition remains caller-directed.

Automatic agent-view composition and Git merging are outside the initial scope.

---

# 71. Future — Historical Architecture

Mnestic time travel may later preserve historical relationships for the canonical workspace lineage.

Potential queries:

```text
When did A start depending on B?

What depended on this RPC at a particular point in time?

How did the architecture evolve over the last month?
```

This remains separate from the branching `RepositoryState` and `WorkspaceView` model.

---

# 72. Future — Advanced Static Analysis

Potential later work includes:

```text
control-flow graphs
program-dependence graphs
data-flow analysis
taint analysis
effect propagation
security analysis
dead dependency detection
runtime/static correlation
```

These capabilities may combine:

```text
Rust algorithms
Datalog
Mnestic fixed rules
```

---

# 73. North-Star Capability

The long-term platform should be able to answer:

```text
beholder impact proto-field://company.users.v1.User/email
```

and derive something similar to:

```text
company.users.v1.User.email
│
├── used by Users/GetUser
│   │
│   └── called by CustomerResolver.customer/3
│       │
│       └── resolves Query.customer
│           │
│           ├── Web GetCustomerQuery
│           │   └── CustomerPage
│           │
│           ├── iOS GetCustomerQuery
│           │   └── CustomerView
│           │
│           └── Android GetCustomerQuery
│               └── CustomerScreen
│
└── contained by UserUpdated
    │
    └── carried by users.updated.v1
        │
        ├── notifications-service
        └── analytics-service
```

Every step should be explainable in terms of:

```text
relationship
source
confidence
logical repository
source location
repository state
workspace view
analysis revision
```

---

# 74. Initial Success Criteria

The architecture is successful when:

1. Multiple repositories participate in one semantic workspace.

2. Duplicate clones of the same logical repository are reconciled.

3. Linked Git worktrees are automatically discovered.

4. Different worktrees can expose different repository states simultaneously.

5. Identical file contents are not unnecessarily reparsed.

6. Workspace views explicitly select which repository states participate in analysis.

7. Frontends emit observations rather than directly constructing arbitrary global dependencies.

8. Mnestic persists semantic facts and Datalog derives useful architectural relationships.

9. New languages can be added without redesigning contract analysis.

10. New contract systems can be added without redesigning language frontends.

11. Organisation-specific conventions can improve precision without contaminating the generic core.

12. Filesystem changes are continuously and incrementally indexed.

13. Index storms are coalesced rather than processed as an event log.

14. Only complete and coherent `AnalysisRevision`s are queryable.

15. Query results report when the current filesystem is newer than the latest completed revision.

16. Every important relationship can expose supporting evidence.

17. Generated code can participate in resolution without overwhelming user-facing output.

18. Analyzer and rule changes correctly invalidate stale derived data.

19. Beholder can analyze and dogfood its own Rust and gRPC architecture.

20. `context`, `trace`, `impact`, `dependencies` and `why` provide useful results.

21. The complete semantic database remains rebuildable from canonical inputs.

---

# 75. Guiding Principles

When implementing analysis features, prefer:

```text
observe
   ↓
store
   ↓
resolve
   ↓
derive
   ↓
query
```

When processing filesystem state:

```text
event
   ↓
validate
   ↓
hash
   ↓
coalesce
   ↓
analyze
   ↓
publish completed revision
```

When dealing with Git:

```text
canonical remote identity
      ↓
LogicalRepository

Git common directory
      ↓
GitClone

filesystem checkout
      ↓
WorkingTree

analyzed source state
      ↓
RepositoryState

selected repository states
      ↓
WorkspaceView

completed semantic model
      ↓
AnalysisRevision
```

When structuring the Beholder repository itself:

```text
Cargo
    = Rust build/package truth

Moonrepo
    = development workflow and task orchestration

Beholder Workspace
    = software-estate analysis model
```

The central architectural bet is:

> **A distributed software system can be represented effectively as semantic relations derived from source code, contracts and configuration; Datalog provides a natural way to infer architecture from those relations; and a continuously running daemon can maintain that model incrementally across repositories, clones and worktrees.**

The first objective is to determine whether that bet holds in practice.
