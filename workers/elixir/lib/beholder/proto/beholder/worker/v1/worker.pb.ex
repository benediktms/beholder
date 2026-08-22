defmodule Beholder.Worker.V1.InputKind do
  @moduledoc false
  use Protobuf, enum: true, protoc_gen_elixir_version: "0.17.0", syntax: :proto3

  field :INPUT_KIND_UNSPECIFIED, 0
  field :INPUT_KIND_SOURCE, 1
  field :INPUT_KIND_PROTOBUF_DESCRIPTOR, 2
end

defmodule Beholder.Worker.V1.AnalysisPhase do
  @moduledoc false
  use Protobuf, enum: true, protoc_gen_elixir_version: "0.17.0", syntax: :proto3

  field :ANALYSIS_PHASE_UNSPECIFIED, 0
  field :ANALYSIS_PHASE_RECEIVING_SNAPSHOT, 1
  field :ANALYSIS_PHASE_ANALYZING, 2
end

defmodule Beholder.Worker.V1.AnalysisCompleteness do
  @moduledoc false
  use Protobuf, enum: true, protoc_gen_elixir_version: "0.17.0", syntax: :proto3

  field :ANALYSIS_COMPLETENESS_UNSPECIFIED, 0
  field :ANALYSIS_COMPLETENESS_COMPLETE, 1
  field :ANALYSIS_COMPLETENESS_INCOMPLETE, 2
end

defmodule Beholder.Worker.V1.GrpcBindingRole do
  @moduledoc false
  use Protobuf, enum: true, protoc_gen_elixir_version: "0.17.0", syntax: :proto3

  field :GRPC_BINDING_ROLE_UNSPECIFIED, 0
  field :GRPC_BINDING_ROLE_CLIENT, 1
  field :GRPC_BINDING_ROLE_SERVER, 2
end

defmodule Beholder.Worker.V1.Confidence do
  @moduledoc false
  use Protobuf, enum: true, protoc_gen_elixir_version: "0.17.0", syntax: :proto3

  field :CONFIDENCE_UNSPECIFIED, 0
  field :CONFIDENCE_EXACT, 1
  field :CONFIDENCE_INFERRED, 2
end

defmodule Beholder.Worker.V1.Provenance do
  @moduledoc false
  use Protobuf, enum: true, protoc_gen_elixir_version: "0.17.0", syntax: :proto3

  field :PROVENANCE_UNSPECIFIED, 0
  field :PROVENANCE_AST, 1
  field :PROVENANCE_COMPILER, 2
  field :PROVENANCE_DESCRIPTOR, 3
  field :PROVENANCE_GENERATED, 4
  field :PROVENANCE_UNIQUE_NAME_HEURISTIC, 5
end

defmodule Beholder.Worker.V1.AnalyzeRequest do
  @moduledoc false
  use Protobuf, protoc_gen_elixir_version: "0.17.0", syntax: :proto3

  oneof :request, 0

  field :start, 1, type: Beholder.Worker.V1.AnalysisStart, oneof: 0
  field :repository, 2, type: Beholder.Worker.V1.RepositoryStart, oneof: 0
  field :input, 3, type: Beholder.Worker.V1.RepositoryInput, oneof: 0
  field :finish, 4, type: Beholder.Worker.V1.AnalysisFinish, oneof: 0
end

defmodule Beholder.Worker.V1.AnalysisStart do
  @moduledoc false
  use Protobuf, protoc_gen_elixir_version: "0.17.0", syntax: :proto3

  field :workspace, 1, type: :string
end

defmodule Beholder.Worker.V1.RepositoryStart do
  @moduledoc false
  use Protobuf, protoc_gen_elixir_version: "0.17.0", syntax: :proto3

  field :identity, 1, type: :string
  field :base, 2, type: :string
  field :head, 3, proto3_optional: true, type: :string
  field :fingerprint, 4, type: :string
  field :target, 5, type: :bool
end

defmodule Beholder.Worker.V1.RepositoryInput do
  @moduledoc false
  use Protobuf, protoc_gen_elixir_version: "0.17.0", syntax: :proto3

  field :repository, 1, type: :string
  field :path, 2, type: :string
  field :content, 3, type: :bytes
  field :kind, 4, type: Beholder.Worker.V1.InputKind, enum: true
end

defmodule Beholder.Worker.V1.AnalysisFinish do
  @moduledoc false
  use Protobuf, protoc_gen_elixir_version: "0.17.0", syntax: :proto3
end

defmodule Beholder.Worker.V1.AnalyzeEvent do
  @moduledoc false
  use Protobuf, protoc_gen_elixir_version: "0.17.0", syntax: :proto3

  oneof :event, 0

  field :progress, 1, type: Beholder.Worker.V1.AnalysisProgress, oneof: 0
  field :repository, 2, type: Beholder.Worker.V1.RepositoryContribution, oneof: 0
  field :completed, 3, type: Beholder.Worker.V1.AnalysisCompleted, oneof: 0
  field :failure, 4, type: Beholder.Worker.V1.AnalysisFailure, oneof: 0
  field :contribution, 5, type: Beholder.Worker.V1.AnalysisContribution, oneof: 0
end

defmodule Beholder.Worker.V1.AnalysisProgress do
  @moduledoc false
  use Protobuf, protoc_gen_elixir_version: "0.17.0", syntax: :proto3

  field :phase, 1, type: Beholder.Worker.V1.AnalysisPhase, enum: true
  field :detail, 2, proto3_optional: true, type: :string
end

defmodule Beholder.Worker.V1.AnalysisCompleted do
  @moduledoc false
  use Protobuf, protoc_gen_elixir_version: "0.17.0", syntax: :proto3

  field :metadata, 1, type: Beholder.Worker.V1.AnalyzerMetadata
  field :active_repositories, 2, repeated: true, type: :string, json_name: "activeRepositories"
  field :cache, 6, type: Beholder.Worker.V1.CacheStatistics
end

defmodule Beholder.Worker.V1.AnalysisContribution do
  @moduledoc false
  use Protobuf, protoc_gen_elixir_version: "0.17.0", syntax: :proto3

  field :overrides, 1, repeated: true, type: Beholder.Worker.V1.DependencyOverride

  field :graphql_resolvers, 2,
    repeated: true,
    type: Beholder.Worker.V1.GraphqlResolverCandidate,
    json_name: "graphqlResolvers"

  field :diagnostics, 3, repeated: true, type: Beholder.Worker.V1.RepositoryDiagnostic
end

defmodule Beholder.Worker.V1.AnalysisFailure do
  @moduledoc false
  use Protobuf, protoc_gen_elixir_version: "0.17.0", syntax: :proto3

  field :code, 1, type: :string
  field :message, 2, type: :string
end

defmodule Beholder.Worker.V1.AnalyzerMetadata do
  @moduledoc false
  use Protobuf, protoc_gen_elixir_version: "0.17.0", syntax: :proto3

  field :id, 1, type: :string
  field :version, 2, type: :string
end

defmodule Beholder.Worker.V1.CacheStatistics do
  @moduledoc false
  use Protobuf, protoc_gen_elixir_version: "0.17.0", syntax: :proto3

  field :memory_hits, 1, type: :uint64, json_name: "memoryHits"
  field :disk_hits, 2, type: :uint64, json_name: "diskHits"
  field :misses, 3, type: :uint64
end

defmodule Beholder.Worker.V1.RepositoryContribution do
  @moduledoc false
  use Protobuf, protoc_gen_elixir_version: "0.17.0", syntax: :proto3

  field :repository, 1, type: :string
  field :completeness, 2, type: Beholder.Worker.V1.AnalysisCompleteness, enum: true
  field :entities, 3, repeated: true, type: Beholder.Worker.V1.EntityFact

  field :grpc_bindings, 4,
    repeated: true,
    type: Beholder.Worker.V1.GrpcBindingCandidate,
    json_name: "grpcBindings"

  field :observations, 5, repeated: true, type: Beholder.Worker.V1.Observation
  field :diagnostics, 6, repeated: true, type: Beholder.Worker.V1.AnalysisDiagnostic
end

defmodule Beholder.Worker.V1.EntityFact do
  @moduledoc false
  use Protobuf, protoc_gen_elixir_version: "0.17.0", syntax: :proto3

  field :id, 1, type: :string
  field :kind, 2, type: Beholder.V1.EntityKind, enum: true
  field :metadata, 3, proto3_optional: true, type: Beholder.V1.EntityMetadata
end

defmodule Beholder.Worker.V1.Observation do
  @moduledoc false
  use Protobuf, protoc_gen_elixir_version: "0.17.0", syntax: :proto3

  field :from, 1, type: :string
  field :relation, 2, type: Beholder.V1.RelationKind, enum: true
  field :to, 3, type: :string
  field :evidence, 4, type: :string
  field :confidence, 5, type: Beholder.Worker.V1.Confidence, enum: true
  field :provenance, 6, type: Beholder.Worker.V1.Provenance, enum: true
end

defmodule Beholder.Worker.V1.GrpcBindingCandidate do
  @moduledoc false
  use Protobuf, protoc_gen_elixir_version: "0.17.0", syntax: :proto3

  field :local_symbol, 1, type: :string, json_name: "localSymbol"
  field :role, 2, type: Beholder.Worker.V1.GrpcBindingRole, enum: true
  field :service, 3, type: :string
  field :method, 4, type: :string
  field :cardinality, 5, type: Beholder.V1.RpcCardinality, enum: true
  field :evidence, 6, type: :string
  field :confidence, 7, type: Beholder.Worker.V1.Confidence, enum: true
  field :provenance, 8, type: Beholder.Worker.V1.Provenance, enum: true
end

defmodule Beholder.Worker.V1.AnalysisDiagnostic do
  @moduledoc false
  use Protobuf, protoc_gen_elixir_version: "0.17.0", syntax: :proto3

  field :code, 1, type: :string
  field :severity, 2, type: Beholder.V1.AnalysisDiagnosticSeverity, enum: true
  field :path, 3, type: :string
  field :line, 4, proto3_optional: true, type: :uint32
  field :detail, 5, proto3_optional: true, type: :string
end

defmodule Beholder.Worker.V1.DependencyOverride do
  @moduledoc false
  use Protobuf, protoc_gen_elixir_version: "0.17.0", syntax: :proto3

  field :from, 1, type: :string
  field :relation, 2, type: Beholder.V1.RelationKind, enum: true
  field :unresolved_to, 3, type: :string, json_name: "unresolvedTo"
  field :resolved_to, 4, type: :string, json_name: "resolvedTo"
  field :evidence, 5, type: :string
  field :confidence, 6, type: Beholder.Worker.V1.Confidence, enum: true
  field :provenance, 7, type: Beholder.Worker.V1.Provenance, enum: true
end

defmodule Beholder.Worker.V1.GraphqlResolverCandidate do
  @moduledoc false
  use Protobuf, protoc_gen_elixir_version: "0.17.0", syntax: :proto3

  field :repository, 1, type: :string
  field :field, 2, type: :string
  field :parent, 3, proto3_optional: true, type: :string
  field :resolver, 4, type: :string
  field :evidence, 5, type: :string
end

defmodule Beholder.Worker.V1.RepositoryDiagnostic do
  @moduledoc false
  use Protobuf, protoc_gen_elixir_version: "0.17.0", syntax: :proto3

  field :repository, 1, type: :string
  field :diagnostic, 2, type: Beholder.Worker.V1.AnalysisDiagnostic
end

defmodule Beholder.Worker.V1.AnalyzerWorker.Service do
  @moduledoc false
  use GRPC.Service, name: "beholder.worker.v1.AnalyzerWorker", protoc_gen_elixir_version: "0.17.0"

  rpc(
    :Analyze,
    stream(Beholder.Worker.V1.AnalyzeRequest),
    stream(Beholder.Worker.V1.AnalyzeEvent),
    %{}
  )
end

defmodule Beholder.Worker.V1.AnalyzerWorker.Stub do
  @moduledoc false
  use GRPC.Stub, service: Beholder.Worker.V1.AnalyzerWorker.Service
end
