defmodule Beholder.Worker.Elixir.Analyzer do
  @moduledoc false

  alias Beholder.Worker.Elixir.{Compiler, EventMapper, Observability, Snapshot}
  alias Beholder.Worker.Elixir.Snapshot.Repository

  alias Beholder.Worker.V1.{
    AnalysisCompleted,
    AnalysisDiagnostic,
    AnalyzerMetadata,
    AnalyzeEvent,
    CacheStatistics,
    RepositoryContribution
  }

  @analyzer_version "20:10:elixir-compiler:15"
  @contribution_chunk_items 2_048

  @spec analyze(Snapshot.t(), String.t()) :: {:ok, [AnalyzeEvent.t()]} | {:error, String.t()}
  def analyze(snapshot, cache_dir) do
    analyze(snapshot, cache_dir, fn _detail -> :ok end)
  end

  @spec analyze(Snapshot.t(), String.t(), (String.t() -> any())) ::
          {:ok, [AnalyzeEvent.t()]} | {:error, String.t()}
  def analyze(snapshot, cache_dir, on_progress) do
    repository = Snapshot.target(snapshot)
    analyze_repository(repository, Snapshot.contexts(snapshot), cache_dir, on_progress)
  end

  defp analyze_repository(repository, contexts, cache_dir, on_progress) do
    {contribution, runtime} =
      compiler_contribution(repository, contexts, cache_dir, on_progress)

    completed = %AnalysisCompleted{
      metadata: %AnalyzerMetadata{id: "elixir", version: metadata_version(runtime)},
      active_repositories: [repository.identity],
      cache: %CacheStatistics{misses: 1}
    }

    repository_events =
      contribution
      |> contribution_chunks()
      |> Enum.map(&%AnalyzeEvent{event: {:repository, &1}})

    {:ok, repository_events ++ [%AnalyzeEvent{event: {:completed, completed}}]}
  end

  defp compiler_contribution(repository, contexts, cache_dir, on_progress) do
    Observability.with_span(
      "worker.elixir.semantic_analysis",
      %{
        "repository" => repository.identity,
        "source.count" => length(Repository.source_inputs(repository)),
        "mix.env" => configured_mix_env()
      },
      fn ->
        case Compiler.run(repository, contexts, cache_dir, on_progress) do
          {:ok, result} ->
            contribution = EventMapper.contribution(repository, result)

            Observability.set_attributes(%{
              "entity.count" => length(contribution.entities),
              "observation.count" => length(contribution.observations),
              "diagnostic.count" => length(contribution.diagnostics)
            })

            {contribution, {result.elixir_version, result.otp_release}}

          {:error, reason} ->
            Observability.set_error(reason)

            {%RepositoryContribution{
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
             }, {System.version(), :erlang.system_info(:otp_release) |> to_string()}}
        end
      end
    )
  end

  @doc false
  def metadata_version(_runtime \\ runtime_versions()), do: @analyzer_version

  defp runtime_versions do
    {System.version(), :erlang.system_info(:otp_release) |> to_string()}
  end

  defp configured_mix_env do
    case System.get_env("BEHOLDER_ELIXIR_MIX_ENV", "") |> String.trim() do
      "" -> "dev"
      value -> value
    end
  end

  @doc false
  def contribution_chunks(contribution) do
    count =
      [
        length(contribution.entities),
        length(contribution.grpc_bindings),
        length(contribution.observations),
        length(contribution.diagnostics),
        length(contribution.replaced_diagnostic_codes),
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
          diagnostics: Enum.slice(contribution.diagnostics, offset, @contribution_chunk_items),
          replaced_diagnostic_codes:
            Enum.slice(
              contribution.replaced_diagnostic_codes,
              offset,
              @contribution_chunk_items
            )
      }
    end
  end
end
