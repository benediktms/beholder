defmodule Beholder.V1.GarbageCollectPhase do
  @moduledoc false
  use Protobuf, enum: true, protoc_gen_elixir_version: "0.17.0", syntax: :proto3

  field :GARBAGE_COLLECT_PHASE_UNSPECIFIED, 0
  field :GARBAGE_COLLECT_PHASE_CLAIMING_OBSOLETE_STATES, 1
  field :GARBAGE_COLLECT_PHASE_SWEEPING_OBSOLETE_STATES, 2
  field :GARBAGE_COLLECT_PHASE_CHECKPOINTING_DATABASE, 3
  field :GARBAGE_COLLECT_PHASE_RECLAIMING_DATABASE_SPACE, 4
end

defmodule Beholder.V1.AnalysisCompleteness do
  @moduledoc false
  use Protobuf, enum: true, protoc_gen_elixir_version: "0.17.0", syntax: :proto3

  field :ANALYSIS_COMPLETENESS_UNSPECIFIED, 0
  field :ANALYSIS_COMPLETENESS_COMPLETE, 1
  field :ANALYSIS_COMPLETENESS_INCOMPLETE, 2
end

defmodule Beholder.V1.AnalysisDiagnosticSeverity do
  @moduledoc false
  use Protobuf, enum: true, protoc_gen_elixir_version: "0.17.0", syntax: :proto3

  field :ANALYSIS_DIAGNOSTIC_SEVERITY_UNSPECIFIED, 0
  field :ANALYSIS_DIAGNOSTIC_SEVERITY_KNOWN_LIMITATION, 1
  field :ANALYSIS_DIAGNOSTIC_SEVERITY_WARNING, 2
end

defmodule Beholder.V1.EntityKind do
  @moduledoc false
  use Protobuf, enum: true, protoc_gen_elixir_version: "0.17.0", syntax: :proto3

  field :ENTITY_KIND_UNSPECIFIED, 0
  field :ENTITY_KIND_CALLABLE, 1
  field :ENTITY_KIND_GRAPHQL_FIELD, 2
  field :ENTITY_KIND_KAFKA_TOPIC, 3
  field :ENTITY_KIND_NAMESPACE, 4
  field :ENTITY_KIND_RPC, 5
  field :ENTITY_KIND_SERVICE, 6
  field :ENTITY_KIND_PROTO_ENUM, 7
  field :ENTITY_KIND_PROTO_FIELD, 8
  field :ENTITY_KIND_PROTO_FILE, 9
  field :ENTITY_KIND_PROTO_MESSAGE, 10
  field :ENTITY_KIND_PROTO_SERVICE, 11
  field :ENTITY_KIND_UNITY_PREFAB, 12
  field :ENTITY_KIND_GRAPHQL_OPERATION, 13
  field :ENTITY_KIND_GRAPHQL_TYPE, 14
  field :ENTITY_KIND_GRAPHQL_ARGUMENT, 15
  field :ENTITY_KIND_GRAPHQL_ENUM_VALUE, 16
end

defmodule Beholder.V1.GraphqlTypeKind do
  @moduledoc false
  use Protobuf, enum: true, protoc_gen_elixir_version: "0.17.0", syntax: :proto3

  field :GRAPHQL_TYPE_KIND_UNSPECIFIED, 0
  field :GRAPHQL_TYPE_KIND_ENUM, 1
  field :GRAPHQL_TYPE_KIND_INPUT, 2
  field :GRAPHQL_TYPE_KIND_INTERFACE, 3
  field :GRAPHQL_TYPE_KIND_OBJECT, 4
  field :GRAPHQL_TYPE_KIND_SCALAR, 5
  field :GRAPHQL_TYPE_KIND_UNION, 6
end

defmodule Beholder.V1.GraphqlOperationKind do
  @moduledoc false
  use Protobuf, enum: true, protoc_gen_elixir_version: "0.17.0", syntax: :proto3

  field :GRAPHQL_OPERATION_KIND_UNSPECIFIED, 0
  field :GRAPHQL_OPERATION_KIND_MUTATION, 1
  field :GRAPHQL_OPERATION_KIND_QUERY, 2
  field :GRAPHQL_OPERATION_KIND_SUBSCRIPTION, 3
end

defmodule Beholder.V1.EntityOrigin do
  @moduledoc false
  use Protobuf, enum: true, protoc_gen_elixir_version: "0.17.0", syntax: :proto3

  field :ENTITY_ORIGIN_UNSPECIFIED, 0
  field :ENTITY_ORIGIN_SOURCE, 1
  field :ENTITY_ORIGIN_GENERATED, 2
  field :ENTITY_ORIGIN_EXTERNAL_DEPENDENCY, 3
end

defmodule Beholder.V1.ProtoTypeKind do
  @moduledoc false
  use Protobuf, enum: true, protoc_gen_elixir_version: "0.17.0", syntax: :proto3

  field :PROTO_TYPE_KIND_UNSPECIFIED, 0
  field :PROTO_TYPE_KIND_ENUM, 1
  field :PROTO_TYPE_KIND_MESSAGE, 2
end

defmodule Beholder.V1.RpcCardinality do
  @moduledoc false
  use Protobuf, enum: true, protoc_gen_elixir_version: "0.17.0", syntax: :proto3

  field :RPC_CARDINALITY_UNSPECIFIED, 0
  field :RPC_CARDINALITY_UNARY, 1
  field :RPC_CARDINALITY_CLIENT_STREAMING, 2
  field :RPC_CARDINALITY_SERVER_STREAMING, 3
  field :RPC_CARDINALITY_BIDIRECTIONAL_STREAMING, 4
end

defmodule Beholder.V1.EvidenceKind do
  @moduledoc false
  use Protobuf, enum: true, protoc_gen_elixir_version: "0.17.0", syntax: :proto3

  field :EVIDENCE_KIND_UNSPECIFIED, 0
  field :EVIDENCE_KIND_AST, 1
  field :EVIDENCE_KIND_CONFIGURATION, 2
  field :EVIDENCE_KIND_DESCRIPTOR, 3
  field :EVIDENCE_KIND_GENERATED, 4
  field :EVIDENCE_KIND_INFERENCE, 5
  field :EVIDENCE_KIND_COMPILER, 6
end

defmodule Beholder.V1.RelationKind do
  @moduledoc false
  use Protobuf, enum: true, protoc_gen_elixir_version: "0.17.0", syntax: :proto3

  field :RELATION_KIND_UNSPECIFIED, 0
  field :RELATION_KIND_BINDS_CONTRACT, 16
  field :RELATION_KIND_CALLS, 1
  field :RELATION_KIND_CALLS_RPC, 2
  field :RELATION_KIND_CONSUMED_BY, 3
  field :RELATION_KIND_DEFINES, 4
  field :RELATION_KIND_IMPLEMENTED_BY, 5
  field :RELATION_KIND_PUBLISHES, 6
  field :RELATION_KIND_RESOLVED_BY, 7
  field :RELATION_KIND_SELECTS, 8
  field :RELATION_KIND_USES, 9
  field :RELATION_KIND_FIELD_OF, 10
  field :RELATION_KIND_REQUEST_TYPE, 11
  field :RELATION_KIND_RESPONSE_TYPE, 12
  field :RELATION_KIND_IMPORTS, 13
  field :RELATION_KIND_REQUIRES, 14
  field :RELATION_KIND_IMPLEMENTS, 15
  field :RELATION_KIND_CALLS_GRAPHQL, 17
end

defmodule Beholder.V1.EnrichmentSubmissionDisposition do
  @moduledoc false
  use Protobuf, enum: true, protoc_gen_elixir_version: "0.17.0", syntax: :proto3

  field :ENRICHMENT_SUBMISSION_DISPOSITION_UNSPECIFIED, 0
  field :ENRICHMENT_SUBMISSION_DISPOSITION_ENQUEUED, 1
  field :ENRICHMENT_SUBMISSION_DISPOSITION_IN_PROGRESS, 2
  field :ENRICHMENT_SUBMISSION_DISPOSITION_ALREADY_CURRENT, 3
end

defmodule Beholder.V1.JobStatus do
  @moduledoc false
  use Protobuf, enum: true, protoc_gen_elixir_version: "0.17.0", syntax: :proto3

  field :JOB_STATUS_UNSPECIFIED, 0
  field :JOB_STATUS_QUEUED, 1
  field :JOB_STATUS_WAITING, 2
  field :JOB_STATUS_RUNNING, 3
  field :JOB_STATUS_COMPLETED, 4
  field :JOB_STATUS_FAILED, 5
end

defmodule Beholder.V1.JobType do
  @moduledoc false
  use Protobuf, enum: true, protoc_gen_elixir_version: "0.17.0", syntax: :proto3

  field :JOB_TYPE_UNSPECIFIED, 0
  field :JOB_TYPE_INDEX, 1
  field :JOB_TYPE_ENRICHMENT, 2
end

defmodule Beholder.V1.JobTrigger do
  @moduledoc false
  use Protobuf, enum: true, protoc_gen_elixir_version: "0.17.0", syntax: :proto3

  field :JOB_TRIGGER_UNSPECIFIED, 0
  field :JOB_TRIGGER_AUTOMATIC, 1
  field :JOB_TRIGGER_MANUAL, 2
end

defmodule Beholder.V1.JobWaitReason do
  @moduledoc false
  use Protobuf, enum: true, protoc_gen_elixir_version: "0.17.0", syntax: :proto3

  field :JOB_WAIT_REASON_UNSPECIFIED, 0
  field :JOB_WAIT_REASON_RETRY, 1
  field :JOB_WAIT_REASON_PREREQUISITES, 2
end

defmodule Beholder.V1.IndexJobOutcome do
  @moduledoc false
  use Protobuf, enum: true, protoc_gen_elixir_version: "0.17.0", syntax: :proto3

  field :INDEX_JOB_OUTCOME_UNSPECIFIED, 0
  field :INDEX_JOB_OUTCOME_PUBLISHED, 1
  field :INDEX_JOB_OUTCOME_UNCHANGED, 2
  field :INDEX_JOB_OUTCOME_SUPERSEDED, 3
end

defmodule Beholder.V1.EnrichmentJobOutcome do
  @moduledoc false
  use Protobuf, enum: true, protoc_gen_elixir_version: "0.17.0", syntax: :proto3

  field :ENRICHMENT_JOB_OUTCOME_UNSPECIFIED, 0
  field :ENRICHMENT_JOB_OUTCOME_PUBLISHED, 1
  field :ENRICHMENT_JOB_OUTCOME_UNCHANGED, 2
  field :ENRICHMENT_JOB_OUTCOME_ALREADY_CURRENT, 3
  field :ENRICHMENT_JOB_OUTCOME_SUPERSEDED, 4
end

defmodule Beholder.V1.ClearCacheRequest do
  @moduledoc false
  use Protobuf, protoc_gen_elixir_version: "0.17.0", syntax: :proto3
end

defmodule Beholder.V1.ClearCacheResponse do
  @moduledoc false
  use Protobuf, protoc_gen_elixir_version: "0.17.0", syntax: :proto3
end

defmodule Beholder.V1.GarbageCollectRequest do
  @moduledoc false
  use Protobuf, protoc_gen_elixir_version: "0.17.0", syntax: :proto3
end

defmodule Beholder.V1.GarbageCollectEvent do
  @moduledoc false
  use Protobuf, protoc_gen_elixir_version: "0.17.0", syntax: :proto3

  oneof :event, 0

  field :progress, 1, type: Beholder.V1.GarbageCollectProgress, oneof: 0
  field :completed, 2, type: Beholder.V1.GarbageCollectResponse, oneof: 0
end

defmodule Beholder.V1.GarbageCollectProgress do
  @moduledoc false
  use Protobuf, protoc_gen_elixir_version: "0.17.0", syntax: :proto3

  field :phase, 1, type: Beholder.V1.GarbageCollectPhase, enum: true
  field :step, 2, type: :string
  field :completed_steps, 3, type: :uint32, json_name: "completedSteps"
  field :total_steps, 4, type: :uint32, json_name: "totalSteps"
  field :rows, 5, proto3_optional: true, type: :uint64
  field :completed_rows, 6, proto3_optional: true, type: :uint64, json_name: "completedRows"
  field :stale_states, 7, proto3_optional: true, type: :uint32, json_name: "staleStates"
  field :repositories, 8, proto3_optional: true, type: :uint32
end

defmodule Beholder.V1.GarbageCollectResponse do
  @moduledoc false
  use Protobuf, protoc_gen_elixir_version: "0.17.0", syntax: :proto3

  field :repository_states_queued, 1, type: :uint64, json_name: "repositoryStatesQueued"
end

defmodule Beholder.V1.GetGarbageCollectionStatusRequest do
  @moduledoc false
  use Protobuf, protoc_gen_elixir_version: "0.17.0", syntax: :proto3
end

defmodule Beholder.V1.GetGarbageCollectionStatusResponse do
  @moduledoc false
  use Protobuf, protoc_gen_elixir_version: "0.17.0", syntax: :proto3

  field :running, 1, type: :bool
  field :repository_states_queued, 2, type: :uint64, json_name: "repositoryStatesQueued"
  field :progress, 3, proto3_optional: true, type: Beholder.V1.GarbageCollectProgress
  field :repository_states_collectible, 4, type: :uint64, json_name: "repositoryStatesCollectible"
  field :reclaimable_database_pages, 5, type: :uint64, json_name: "reclaimableDatabasePages"
end

defmodule Beholder.V1.EntityRequest do
  @moduledoc false
  use Protobuf, protoc_gen_elixir_version: "0.17.0", syntax: :proto3

  field :entity, 1, type: :string
  field :workspace, 2, type: :string
end

defmodule Beholder.V1.TraversalEntityRequest do
  @moduledoc false
  use Protobuf, protoc_gen_elixir_version: "0.17.0", syntax: :proto3

  field :entity, 1, type: :string
  field :workspace, 2, type: :string
  field :max_hops, 3, proto3_optional: true, type: :uint32, json_name: "maxHops"
end

defmodule Beholder.V1.PathRequest do
  @moduledoc false
  use Protobuf, protoc_gen_elixir_version: "0.17.0", syntax: :proto3

  field :from, 1, type: :string
  field :to, 2, type: :string
  field :workspace, 3, type: :string
  field :max_hops, 4, proto3_optional: true, type: :uint32, json_name: "maxHops"
end

defmodule Beholder.V1.TraversalMetadata do
  @moduledoc false
  use Protobuf, protoc_gen_elixir_version: "0.17.0", syntax: :proto3

  field :max_hops, 1, type: :uint32, json_name: "maxHops"
  field :truncated, 2, type: :bool
end

defmodule Beholder.V1.QueryMetadata do
  @moduledoc false
  use Protobuf, protoc_gen_elixir_version: "0.17.0", syntax: :proto3

  field :revision, 1, type: :uint64
  field :view, 2, type: :string
  field :freshness, 3, type: Beholder.V1.Freshness
  field :completeness, 4, type: Beholder.V1.AnalysisCompleteness, enum: true
  field :diagnostics, 5, repeated: true, type: Beholder.V1.AnalysisDiagnostic
end

defmodule Beholder.V1.Freshness do
  @moduledoc false
  use Protobuf, protoc_gen_elixir_version: "0.17.0", syntax: :proto3

  field :stale, 1, type: :bool
  field :indexing, 2, type: :bool
  field :dirty_repositories, 3, repeated: true, type: :string, json_name: "dirtyRepositories"

  field :enriching_repositories, 4,
    repeated: true,
    type: :string,
    json_name: "enrichingRepositories"
end

defmodule Beholder.V1.AnalysisDiagnostic do
  @moduledoc false
  use Protobuf, protoc_gen_elixir_version: "0.17.0", syntax: :proto3

  field :code, 1, type: :string
  field :severity, 2, type: Beholder.V1.AnalysisDiagnosticSeverity, enum: true
  field :repository, 3, type: :string
  field :path, 4, type: :string
  field :line, 5, proto3_optional: true, type: :uint32
  field :detail, 6, proto3_optional: true, type: :string
end

defmodule Beholder.V1.EntityMetadata do
  @moduledoc false
  use Protobuf, protoc_gen_elixir_version: "0.17.0", syntax: :proto3

  oneof :metadata, 0

  field :proto_type_kind, 1,
    type: Beholder.V1.ProtoTypeKind,
    json_name: "protoTypeKind",
    enum: true,
    oneof: 0

  field :rpc_cardinality, 2,
    type: Beholder.V1.RpcCardinality,
    json_name: "rpcCardinality",
    enum: true,
    oneof: 0

  field :graphql_type_kind, 3,
    type: Beholder.V1.GraphqlTypeKind,
    json_name: "graphqlTypeKind",
    enum: true,
    oneof: 0

  field :graphql_operation_kind, 4,
    type: Beholder.V1.GraphqlOperationKind,
    json_name: "graphqlOperationKind",
    enum: true,
    oneof: 0
end

defmodule Beholder.V1.Entity do
  @moduledoc false
  use Protobuf, protoc_gen_elixir_version: "0.17.0", syntax: :proto3

  field :id, 1, type: :string
  field :kind, 2, type: Beholder.V1.EntityKind, enum: true
  field :name, 3, type: :string
  field :repository, 4, proto3_optional: true, type: :string
  field :origin, 5, type: Beholder.V1.EntityOrigin, enum: true
  field :test, 6, type: :bool
  field :metadata, 7, proto3_optional: true, type: Beholder.V1.EntityMetadata
end

defmodule Beholder.V1.Evidence do
  @moduledoc false
  use Protobuf, protoc_gen_elixir_version: "0.17.0", syntax: :proto3

  field :source, 1, type: Beholder.V1.EvidenceKind, enum: true
  field :repository, 2, proto3_optional: true, type: :string
  field :path, 3, proto3_optional: true, type: :string
  field :line, 4, proto3_optional: true, type: :uint32
  field :detail, 5, proto3_optional: true, type: :string
end

defmodule Beholder.V1.Edge do
  @moduledoc false
  use Protobuf, protoc_gen_elixir_version: "0.17.0", syntax: :proto3

  field :id, 1, type: :string
  field :from, 2, type: :string
  field :to, 3, type: :string
  field :kind, 4, type: Beholder.V1.RelationKind, enum: true
  field :confidence, 5, type: :float
  field :evidence, 6, repeated: true, type: Beholder.V1.Evidence
end

defmodule Beholder.V1.SemanticPath do
  @moduledoc false
  use Protobuf, protoc_gen_elixir_version: "0.17.0", syntax: :proto3

  field :nodes, 1, repeated: true, type: :string
  field :edges, 2, repeated: true, type: :string
end

defmodule Beholder.V1.EntityQuery do
  @moduledoc false
  use Protobuf, protoc_gen_elixir_version: "0.17.0", syntax: :proto3

  field :entity, 1, type: :string
end

defmodule Beholder.V1.SemanticPathQuery do
  @moduledoc false
  use Protobuf, protoc_gen_elixir_version: "0.17.0", syntax: :proto3

  field :from, 1, type: :string
  field :to, 2, type: :string
end

defmodule Beholder.V1.ContextResponse do
  @moduledoc false
  use Protobuf, protoc_gen_elixir_version: "0.17.0", syntax: :proto3

  field :schema, 1, type: :string
  field :metadata, 2, type: Beholder.V1.QueryMetadata
  field :query, 3, type: Beholder.V1.EntityQuery
  field :root, 4, type: Beholder.V1.Entity
  field :nodes, 5, repeated: true, type: Beholder.V1.Entity
  field :edges, 6, repeated: true, type: Beholder.V1.Edge
end

defmodule Beholder.V1.Dependency do
  @moduledoc false
  use Protobuf, protoc_gen_elixir_version: "0.17.0", syntax: :proto3

  field :entity, 1, type: :string
  field :hops, 2, type: :uint32
end

defmodule Beholder.V1.DependenciesResponse do
  @moduledoc false
  use Protobuf, protoc_gen_elixir_version: "0.17.0", syntax: :proto3

  field :schema, 1, type: :string
  field :metadata, 2, type: Beholder.V1.QueryMetadata
  field :query, 3, type: Beholder.V1.EntityQuery
  field :root, 4, type: Beholder.V1.Entity
  field :dependencies, 5, repeated: true, type: Beholder.V1.Dependency
  field :nodes, 6, repeated: true, type: Beholder.V1.Entity
  field :edges, 7, repeated: true, type: Beholder.V1.Edge
  field :traversal, 8, type: Beholder.V1.TraversalMetadata
end

defmodule Beholder.V1.Impact do
  @moduledoc false
  use Protobuf, protoc_gen_elixir_version: "0.17.0", syntax: :proto3

  field :entity, 1, type: :string
  field :hops, 2, type: :uint32
end

defmodule Beholder.V1.ImpactResponse do
  @moduledoc false
  use Protobuf, protoc_gen_elixir_version: "0.17.0", syntax: :proto3

  field :schema, 1, type: :string
  field :metadata, 2, type: Beholder.V1.QueryMetadata
  field :query, 3, type: Beholder.V1.EntityQuery
  field :root, 4, type: Beholder.V1.Entity
  field :affected, 5, repeated: true, type: Beholder.V1.Impact
  field :nodes, 6, repeated: true, type: Beholder.V1.Entity
  field :edges, 7, repeated: true, type: Beholder.V1.Edge
  field :traversal, 8, type: Beholder.V1.TraversalMetadata
end

defmodule Beholder.V1.TraceResponse do
  @moduledoc false
  use Protobuf, protoc_gen_elixir_version: "0.17.0", syntax: :proto3

  field :schema, 1, type: :string
  field :metadata, 2, type: Beholder.V1.QueryMetadata
  field :query, 3, type: Beholder.V1.SemanticPathQuery
  field :nodes, 4, repeated: true, type: Beholder.V1.Entity
  field :edges, 5, repeated: true, type: Beholder.V1.Edge
  field :paths, 6, repeated: true, type: Beholder.V1.SemanticPath
  field :traversal, 7, type: Beholder.V1.TraversalMetadata
end

defmodule Beholder.V1.WhyResponse do
  @moduledoc false
  use Protobuf, protoc_gen_elixir_version: "0.17.0", syntax: :proto3

  field :schema, 1, type: :string
  field :metadata, 2, type: Beholder.V1.QueryMetadata
  field :query, 3, type: Beholder.V1.SemanticPathQuery
  field :nodes, 4, repeated: true, type: Beholder.V1.Entity
  field :edges, 5, repeated: true, type: Beholder.V1.Edge
  field :paths, 6, repeated: true, type: Beholder.V1.SemanticPath
  field :traversal, 7, type: Beholder.V1.TraversalMetadata
end

defmodule Beholder.V1.Workspace do
  @moduledoc false
  use Protobuf, protoc_gen_elixir_version: "0.17.0", syntax: :proto3

  field :name, 1, type: :string
  field :repositories, 2, repeated: true, type: Beholder.V1.WorkspaceRepository

  field :protobuf_descriptors, 3,
    repeated: true,
    type: Beholder.V1.ProtobufDescriptorSource,
    json_name: "protobufDescriptors"

  field :enabled_plugins, 4, repeated: true, type: :string, json_name: "enabledPlugins"
end

defmodule Beholder.V1.ProtobufDescriptorSource do
  @moduledoc false
  use Protobuf, protoc_gen_elixir_version: "0.17.0", syntax: :proto3

  field :repository, 1, type: :string
  field :path, 2, type: :string
end

defmodule Beholder.V1.WorkspaceRepository do
  @moduledoc false
  use Protobuf, protoc_gen_elixir_version: "0.17.0", syntax: :proto3

  field :identity, 1, type: :string
  field :display_name, 2, type: :string, json_name: "displayName"
  field :base, 3, type: :string
  field :alternatives, 4, repeated: true, type: :string
end

defmodule Beholder.V1.RegisterWorkspaceRequest do
  @moduledoc false
  use Protobuf, protoc_gen_elixir_version: "0.17.0", syntax: :proto3

  field :name, 1, type: :string
  field :repository_paths, 2, repeated: true, type: :string, json_name: "repositoryPaths"

  field :protobuf_descriptor_paths, 3,
    repeated: true,
    type: :string,
    json_name: "protobufDescriptorPaths"

  field :enabled_plugins, 4, repeated: true, type: :string, json_name: "enabledPlugins"
end

defmodule Beholder.V1.RegisterWorkspaceResponse do
  @moduledoc false
  use Protobuf, protoc_gen_elixir_version: "0.17.0", syntax: :proto3

  field :workspace, 1, type: Beholder.V1.Workspace
end

defmodule Beholder.V1.SetWorkspacePluginRequest do
  @moduledoc false
  use Protobuf, protoc_gen_elixir_version: "0.17.0", syntax: :proto3

  field :workspace, 1, type: :string
  field :plugin, 2, type: :string
  field :enabled, 3, type: :bool
end

defmodule Beholder.V1.SetWorkspacePluginResponse do
  @moduledoc false
  use Protobuf, protoc_gen_elixir_version: "0.17.0", syntax: :proto3

  field :workspace, 1, type: Beholder.V1.Workspace
end

defmodule Beholder.V1.ListWorkspacesRequest do
  @moduledoc false
  use Protobuf, protoc_gen_elixir_version: "0.17.0", syntax: :proto3
end

defmodule Beholder.V1.ListWorkspacesResponse do
  @moduledoc false
  use Protobuf, protoc_gen_elixir_version: "0.17.0", syntax: :proto3

  field :workspaces, 1, repeated: true, type: Beholder.V1.Workspace
end

defmodule Beholder.V1.RepositoryRevision do
  @moduledoc false
  use Protobuf, protoc_gen_elixir_version: "0.17.0", syntax: :proto3

  field :source_state, 1, type: :string, json_name: "sourceState"
  field :head, 2, proto3_optional: true, type: :string
  field :analysis_identity, 3, type: :string, json_name: "analysisIdentity"
  field :incomplete, 4, type: :bool
  field :diagnostics, 5, repeated: true, type: Beholder.V1.AnalysisDiagnostic
end

defmodule Beholder.V1.RepositoryStatus do
  @moduledoc false
  use Protobuf, protoc_gen_elixir_version: "0.17.0", syntax: :proto3

  field :repository, 1, type: Beholder.V1.WorkspaceRepository
  field :revision, 2, proto3_optional: true, type: Beholder.V1.RepositoryRevision
  field :indexing, 3, type: :bool
end

defmodule Beholder.V1.RegisterRepositoryRequest do
  @moduledoc false
  use Protobuf, protoc_gen_elixir_version: "0.17.0", syntax: :proto3

  field :path, 1, type: :string
end

defmodule Beholder.V1.DeleteRepositoryRequest do
  @moduledoc false
  use Protobuf, protoc_gen_elixir_version: "0.17.0", syntax: :proto3

  field :identity, 1, type: :string
end

defmodule Beholder.V1.DeleteRepositoryResponse do
  @moduledoc false
  use Protobuf, protoc_gen_elixir_version: "0.17.0", syntax: :proto3

  field :repository_states_queued, 1, type: :uint64, json_name: "repositoryStatesQueued"
end

defmodule Beholder.V1.GetRepositoryRequest do
  @moduledoc false
  use Protobuf, protoc_gen_elixir_version: "0.17.0", syntax: :proto3

  field :identity, 1, type: :string
end

defmodule Beholder.V1.RepositoryResponse do
  @moduledoc false
  use Protobuf, protoc_gen_elixir_version: "0.17.0", syntax: :proto3

  field :repository, 1, type: Beholder.V1.RepositoryStatus
end

defmodule Beholder.V1.StopRequest do
  @moduledoc false
  use Protobuf, protoc_gen_elixir_version: "0.17.0", syntax: :proto3
end

defmodule Beholder.V1.StopResponse do
  @moduledoc false
  use Protobuf, protoc_gen_elixir_version: "0.17.0", syntax: :proto3

  field :accepted, 1, type: :bool
end

defmodule Beholder.V1.GetStatusRequest do
  @moduledoc false
  use Protobuf, protoc_gen_elixir_version: "0.17.0", syntax: :proto3
end

defmodule Beholder.V1.GetStatusResponse do
  @moduledoc false
  use Protobuf, protoc_gen_elixir_version: "0.17.0", syntax: :proto3

  field :status, 1, type: :string
  field :protocol_version, 2, type: :uint32, json_name: "protocolVersion"
  field :pid, 3, type: :uint32
end

defmodule Beholder.V1.ListJobsRequest do
  @moduledoc false
  use Protobuf, protoc_gen_elixir_version: "0.17.0", syntax: :proto3

  field :page_token, 1, proto3_optional: true, type: :string, json_name: "pageToken"
end

defmodule Beholder.V1.ListJobsResponse do
  @moduledoc false
  use Protobuf, protoc_gen_elixir_version: "0.17.0", syntax: :proto3

  field :jobs, 1, repeated: true, type: Beholder.V1.JobSummary
  field :next_page_token, 2, proto3_optional: true, type: :string, json_name: "nextPageToken"
end

defmodule Beholder.V1.GetJobRequest do
  @moduledoc false
  use Protobuf, protoc_gen_elixir_version: "0.17.0", syntax: :proto3

  field :id, 1, type: :string
end

defmodule Beholder.V1.GetJobResponse do
  @moduledoc false
  use Protobuf, protoc_gen_elixir_version: "0.17.0", syntax: :proto3

  field :job, 1, type: Beholder.V1.Job
end

defmodule Beholder.V1.SubmitIndexRequest do
  @moduledoc false
  use Protobuf, protoc_gen_elixir_version: "0.17.0", syntax: :proto3

  oneof :target, 0

  field :workspace, 1, type: :string, oneof: 0
  field :repository, 2, type: Beholder.V1.RepositoryIndexTarget, oneof: 0
end

defmodule Beholder.V1.RepositoryIndexTarget do
  @moduledoc false
  use Protobuf, protoc_gen_elixir_version: "0.17.0", syntax: :proto3

  field :repository, 1, type: :string
  field :workspace_scope, 2, proto3_optional: true, type: :string, json_name: "workspaceScope"
end

defmodule Beholder.V1.SubmitIndexResponse do
  @moduledoc false
  use Protobuf, protoc_gen_elixir_version: "0.17.0", syntax: :proto3

  field :job, 1, type: Beholder.V1.JobSummary

  field :overlapping_jobs, 2,
    repeated: true,
    type: Beholder.V1.JobSummary,
    json_name: "overlappingJobs"
end

defmodule Beholder.V1.SubmitEnrichmentRequest do
  @moduledoc false
  use Protobuf, protoc_gen_elixir_version: "0.17.0", syntax: :proto3

  field :repository, 1, type: :string
  field :workspace_scope, 2, proto3_optional: true, type: :string, json_name: "workspaceScope"
  field :worker_ids, 3, repeated: true, type: :string, json_name: "workerIds"
end

defmodule Beholder.V1.EnrichmentSubmission do
  @moduledoc false
  use Protobuf, protoc_gen_elixir_version: "0.17.0", syntax: :proto3

  field :target, 1, type: Beholder.V1.JobTarget
  field :disposition, 2, type: Beholder.V1.EnrichmentSubmissionDisposition, enum: true
  field :job, 3, proto3_optional: true, type: Beholder.V1.JobSummary
end

defmodule Beholder.V1.SubmitEnrichmentResponse do
  @moduledoc false
  use Protobuf, protoc_gen_elixir_version: "0.17.0", syntax: :proto3

  field :results, 1, repeated: true, type: Beholder.V1.EnrichmentSubmission

  field :prerequisite_jobs, 2,
    repeated: true,
    type: Beholder.V1.JobSummary,
    json_name: "prerequisiteJobs"
end

defmodule Beholder.V1.JobTarget do
  @moduledoc false
  use Protobuf, protoc_gen_elixir_version: "0.17.0", syntax: :proto3

  oneof :target, 0

  field :workspace, 1, type: :string, oneof: 0
  field :repository, 2, type: :string, oneof: 0
  field :workspace_scope, 3, proto3_optional: true, type: :string, json_name: "workspaceScope"
  field :worker_id, 4, proto3_optional: true, type: :string, json_name: "workerId"
end

defmodule Beholder.V1.JobSummary do
  @moduledoc false
  use Protobuf, protoc_gen_elixir_version: "0.17.0", syntax: :proto3

  field :id, 1, type: :string
  field :status, 2, type: Beholder.V1.JobStatus, enum: true
  field :type, 3, type: Beholder.V1.JobType, enum: true
  field :target, 4, type: Beholder.V1.JobTarget
  field :trigger, 5, type: Beholder.V1.JobTrigger, enum: true
  field :submitted_at_ms, 6, type: :uint64, json_name: "submittedAtMs"
end

defmodule Beholder.V1.IndexDestination do
  @moduledoc false
  use Protobuf, protoc_gen_elixir_version: "0.17.0", syntax: :proto3

  oneof :destination, 0

  field :workspace, 1, type: :string, oneof: 0
  field :standalone_repository, 2, type: :string, json_name: "standaloneRepository", oneof: 0
end

defmodule Beholder.V1.IndexDestinationResult do
  @moduledoc false
  use Protobuf, protoc_gen_elixir_version: "0.17.0", syntax: :proto3

  field :destination, 1, type: Beholder.V1.IndexDestination
  field :observation_count, 2, type: :uint64, json_name: "observationCount"
  field :published, 3, type: :bool
  field :outcome, 4, type: Beholder.V1.IndexJobOutcome, enum: true
end

defmodule Beholder.V1.IndexJobResult do
  @moduledoc false
  use Protobuf, protoc_gen_elixir_version: "0.17.0", syntax: :proto3

  field :destinations, 1, repeated: true, type: Beholder.V1.IndexDestinationResult
end

defmodule Beholder.V1.EnrichmentJobResult do
  @moduledoc false
  use Protobuf, protoc_gen_elixir_version: "0.17.0", syntax: :proto3

  field :target, 1, type: Beholder.V1.JobTarget
  field :expected_worker_version, 2, type: :string, json_name: "expectedWorkerVersion"
  field :outcome, 3, type: Beholder.V1.EnrichmentJobOutcome, enum: true

  field :failed_prerequisite_job_ids, 4,
    repeated: true,
    type: :string,
    json_name: "failedPrerequisiteJobIds"
end

defmodule Beholder.V1.Job do
  @moduledoc false
  use Protobuf, protoc_gen_elixir_version: "0.17.0", syntax: :proto3

  field :summary, 1, type: Beholder.V1.JobSummary
  field :attempts, 2, type: :uint32
  field :max_attempts, 3, type: :uint32, json_name: "maxAttempts"
  field :run_at_ms, 4, proto3_optional: true, type: :uint64, json_name: "runAtMs"
  field :started_at_ms, 5, proto3_optional: true, type: :uint64, json_name: "startedAtMs"
  field :completed_at_ms, 6, proto3_optional: true, type: :uint64, json_name: "completedAtMs"
  field :prerequisite_job_ids, 7, repeated: true, type: :string, json_name: "prerequisiteJobIds"

  field :wait_reason, 8,
    proto3_optional: true,
    type: Beholder.V1.JobWaitReason,
    json_name: "waitReason",
    enum: true

  field :last_error, 9, proto3_optional: true, type: :string, json_name: "lastError"
  field :warnings, 10, repeated: true, type: :string

  field :index_result, 11,
    proto3_optional: true,
    type: Beholder.V1.IndexJobResult,
    json_name: "indexResult"

  field :enrichment_result, 12,
    proto3_optional: true,
    type: Beholder.V1.EnrichmentJobResult,
    json_name: "enrichmentResult"
end

defmodule Beholder.V1.Daemon.Service do
  @moduledoc false
  use GRPC.Service, name: "beholder.v1.Daemon", protoc_gen_elixir_version: "0.17.0"

  rpc(:ClearCache, Beholder.V1.ClearCacheRequest, Beholder.V1.ClearCacheResponse, %{})

  rpc(
    :GarbageCollect,
    Beholder.V1.GarbageCollectRequest,
    stream(Beholder.V1.GarbageCollectEvent),
    %{}
  )

  rpc(
    :GetGarbageCollectionStatus,
    Beholder.V1.GetGarbageCollectionStatusRequest,
    Beholder.V1.GetGarbageCollectionStatusResponse,
    %{}
  )

  rpc(:Context, Beholder.V1.EntityRequest, Beholder.V1.ContextResponse, %{})

  rpc(
    :DeleteRepository,
    Beholder.V1.DeleteRepositoryRequest,
    Beholder.V1.DeleteRepositoryResponse,
    %{}
  )

  rpc(:Dependencies, Beholder.V1.TraversalEntityRequest, Beholder.V1.DependenciesResponse, %{})

  rpc(:GetStatus, Beholder.V1.GetStatusRequest, Beholder.V1.GetStatusResponse, %{})

  rpc(:GetRepository, Beholder.V1.GetRepositoryRequest, Beholder.V1.RepositoryResponse, %{})

  rpc(:Impact, Beholder.V1.TraversalEntityRequest, Beholder.V1.ImpactResponse, %{})

  rpc(:ListJobs, Beholder.V1.ListJobsRequest, Beholder.V1.ListJobsResponse, %{})

  rpc(:GetJob, Beholder.V1.GetJobRequest, Beholder.V1.GetJobResponse, %{})

  rpc(:SubmitIndex, Beholder.V1.SubmitIndexRequest, Beholder.V1.SubmitIndexResponse, %{})

  rpc(
    :SubmitEnrichment,
    Beholder.V1.SubmitEnrichmentRequest,
    Beholder.V1.SubmitEnrichmentResponse,
    %{}
  )

  rpc(:ListWorkspaces, Beholder.V1.ListWorkspacesRequest, Beholder.V1.ListWorkspacesResponse, %{})

  rpc(
    :RegisterWorkspace,
    Beholder.V1.RegisterWorkspaceRequest,
    Beholder.V1.RegisterWorkspaceResponse,
    %{}
  )

  rpc(
    :SetWorkspacePlugin,
    Beholder.V1.SetWorkspacePluginRequest,
    Beholder.V1.SetWorkspacePluginResponse,
    %{}
  )

  rpc(
    :RegisterRepository,
    Beholder.V1.RegisterRepositoryRequest,
    Beholder.V1.RepositoryResponse,
    %{}
  )

  rpc(:Stop, Beholder.V1.StopRequest, Beholder.V1.StopResponse, %{})

  rpc(:Trace, Beholder.V1.PathRequest, Beholder.V1.TraceResponse, %{})

  rpc(:Why, Beholder.V1.PathRequest, Beholder.V1.WhyResponse, %{})
end

defmodule Beholder.V1.Daemon.Stub do
  @moduledoc false
  use GRPC.Stub, service: Beholder.V1.Daemon.Service
end
