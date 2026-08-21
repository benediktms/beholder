defmodule Beholder.Worker.Elixir.Analyzer do
  @moduledoc false

  alias Beholder.Worker.Elixir.{Compiler, EventMapper, Snapshot}
  alias Beholder.Worker.Elixir.Snapshot.Repository

  alias Beholder.Worker.V1.{
    AnalysisCompleted,
    AnalysisDiagnostic,
    AnalyzerMetadata,
    AnalyzeEvent,
    CacheStatistics,
    RepositoryContribution
  }

  @analyzer_version "18:9:elixir-compiler:1"
  @contribution_chunk_items 2_048

  @spec analyze(Snapshot.t(), String.t()) :: {:ok, [AnalyzeEvent.t()]} | {:error, String.t()}
  def analyze(snapshot, cache_dir) do
    case Enum.filter(Snapshot.repositories(snapshot), &Repository.mix_project?/1) do
      [repository] ->
        analyze_repository(repository, cache_dir)

      [] ->
        {:error, "Elixir compiler enrichment target does not contain mix.exs"}

      _repositories ->
        {:error, "Elixir compiler enrichment requires exactly one target repository"}
    end
  end

  defp analyze_repository(repository, cache_dir) do
    contribution =
      case Compiler.run(repository, cache_dir) do
        {:ok, result} ->
          EventMapper.contribution(repository, result)

        {:error, reason} ->
          %RepositoryContribution{
            repository: repository.identity,
            completeness: :ANALYSIS_COMPLETENESS_INCOMPLETE,
            diagnostics: [
              %AnalysisDiagnostic{
                code: "elixir.compiler.unavailable",
                severity: :ANALYSIS_DIAGNOSTIC_SEVERITY_WARNING,
                path: "mix.exs",
                detail: reason
              }
            ]
          }
      end

    completed = %AnalysisCompleted{
      metadata: %AnalyzerMetadata{id: "elixir", version: @analyzer_version},
      active_repositories: [repository.identity],
      cache: %CacheStatistics{misses: 1}
    }

    repository_events =
      contribution
      |> contribution_chunks()
      |> Enum.map(&%AnalyzeEvent{event: {:repository, &1}})

    {:ok, repository_events ++ [%AnalyzeEvent{event: {:completed, completed}}]}
  end

  @doc false
  def contribution_chunks(contribution) do
    count =
      [
        length(contribution.entities),
        length(contribution.grpc_bindings),
        length(contribution.observations),
        length(contribution.diagnostics),
        1
      ]
      |> Enum.max()
      |> then(&div(&1 + @contribution_chunk_items - 1, @contribution_chunk_items))

    for index <- 0..(count - 1) do
      offset = index * @contribution_chunk_items

      %{
        contribution
        | entities: Enum.slice(contribution.entities, offset, @contribution_chunk_items),
          grpc_bindings:
            Enum.slice(contribution.grpc_bindings, offset, @contribution_chunk_items),
          observations: Enum.slice(contribution.observations, offset, @contribution_chunk_items),
          diagnostics: Enum.slice(contribution.diagnostics, offset, @contribution_chunk_items)
      }
    end
  end
end
