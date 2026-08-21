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

  @analyzer_version "18:9:elixir-compiler:3"
  @contribution_chunk_items 2_048

  @spec analyze(Snapshot.t(), String.t()) :: {:ok, [AnalyzeEvent.t()]} | {:error, String.t()}
  def analyze(snapshot, cache_dir) do
    repository = Snapshot.target(snapshot)

    if Repository.mix_project?(repository) do
      analyze_repository(repository, cache_dir)
    else
      {:error, "Elixir compiler enrichment target does not contain mix.exs"}
    end
  end

  defp analyze_repository(repository, cache_dir) do
    {contribution, runtime} = compiler_contribution(repository, cache_dir)

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

  defp compiler_contribution(repository, cache_dir) do
    Observability.with_span(
      "worker.elixir.semantic_analysis",
      %{
        "repository" => repository.identity,
        "source.count" => length(Repository.source_inputs(repository)),
        "mix.env" => System.get_env("BEHOLDER_ELIXIR_MIX_ENV", "dev")
      },
      fn ->
        case Compiler.run(repository, cache_dir) do
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
  def metadata_version({elixir_version, otp_release} \\ runtime_versions()) do
    mix_env = System.get_env("BEHOLDER_ELIXIR_MIX_ENV", "dev")
    "#{@analyzer_version}:mix-#{mix_env}:elixir-#{elixir_version}:otp-#{otp_release}"
  end

  defp runtime_versions do
    {System.version(), :erlang.system_info(:otp_release) |> to_string()}
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
