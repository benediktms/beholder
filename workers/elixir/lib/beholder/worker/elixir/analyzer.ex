defmodule Beholder.Worker.Elixir.Analyzer do
  @moduledoc false

  require Logger

  alias Beholder.Worker.Elixir.{Compiler, EventMapper, Observability, Snapshot}
  alias Beholder.Worker.Elixir.Snapshot.Repository

  alias Beholder.Worker.V1.{
    AnalysisCompleted,
    AnalysisDiagnostic,
    AnalyzerMetadata,
    AnalyzeEvent,
    CacheStatistics,
    FactShard,
    RepositoryContribution
  }

  @analyzer_version "22:11:elixir-compiler:17"
  @contribution_chunk_items 2_048

  @spec analyze(Snapshot.t(), String.t()) :: {:ok, Enumerable.t()} | {:error, String.t()}
  def analyze(snapshot, cache_dir) do
    analyze(snapshot, cache_dir, fn _detail -> :ok end)
  end

  @spec analyze(Snapshot.t(), String.t(), (String.t() -> any())) ::
          {:ok, Enumerable.t()} | {:error, String.t()}
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
      |> Stream.map(&%AnalyzeEvent{event: {:repository, &1}})

    {:ok, Stream.concat(repository_events, [%AnalyzeEvent{event: {:completed, completed}}])}
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
        compiler_started = System.monotonic_time(:microsecond)

        case Compiler.run(repository, contexts, cache_dir, on_progress) do
          {:ok, result} ->
            compiler_elapsed_ms = elapsed_ms(compiler_started)
            on_progress.("mapping #{length(result.events)} compiler events")
            mapper_started = System.monotonic_time(:microsecond)
            contribution = EventMapper.contribution(repository, result)
            mapper_elapsed_ms = elapsed_ms(mapper_started)
            entity_count = Enum.sum(Enum.map(contribution.fact_shards, &length(&1.entities)))

            observation_count =
              Enum.sum(Enum.map(contribution.fact_shards, &length(&1.observations)))

            Observability.set_attributes(%{
              "compiler.elapsed_ms" => compiler_elapsed_ms,
              "compiler.event.count" => length(result.events),
              "event_mapper.elapsed_ms" => mapper_elapsed_ms,
              "entity.count" => entity_count,
              "observation.count" => observation_count,
              "fact_shard.count" => length(contribution.fact_shards),
              "diagnostic.count" => length(contribution.diagnostics)
            })

            Logger.info("Elixir compiler enrichment mapped",
              repository: repository.identity,
              compiler_elapsed_ms: compiler_elapsed_ms,
              compiler_event_count: length(result.events),
              event_mapper_elapsed_ms: mapper_elapsed_ms,
              entity_count: entity_count,
              observation_count: observation_count,
              fact_shard_count: length(contribution.fact_shards),
              diagnostic_count: length(contribution.diagnostics)
            )

            {contribution, {result.elixir_version, result.otp_release}}

          {:error, reason} ->
            Observability.set_error(reason)

            Logger.warning("Elixir compiler enrichment unavailable",
              repository: repository.identity,
              compiler_elapsed_ms: elapsed_ms(compiler_started),
              reason: reason
            )

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

  defp elapsed_ms(started) do
    (System.monotonic_time(:microsecond) - started) / 1_000
  end

  @doc false
  def contribution_chunks(contribution) do
    fact_shards = Enum.flat_map(contribution.fact_shards, &fact_shard_chunks/1)

    count =
      [
        chunk_count(contribution.entities),
        chunk_count(contribution.grpc_bindings),
        chunk_count(contribution.observations),
        chunk_count(contribution.diagnostics),
        chunk_count(contribution.replaced_diagnostic_codes),
        length(fact_shards),
        1
      ]
      |> Enum.max()

    Stream.map(0..(count - 1), fn index ->
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
            ),
          fact_shards: Enum.slice(fact_shards, index, 1)
      }
    end)
  end

  defp fact_shard_chunks(%FactShard{} = shard) do
    count =
      [length(shard.entities), length(shard.observations), 1]
      |> Enum.max()
      |> then(&div(&1 + @contribution_chunk_items - 1, @contribution_chunk_items))

    Enum.map(0..(count - 1), fn index ->
      offset = index * @contribution_chunk_items

      %FactShard{
        shard
        | entities: Enum.slice(shard.entities, offset, @contribution_chunk_items),
          observations: Enum.slice(shard.observations, offset, @contribution_chunk_items)
      }
    end)
  end

  defp chunk_count(items),
    do: div(length(items) + @contribution_chunk_items - 1, @contribution_chunk_items)
end
