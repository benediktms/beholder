defmodule Beholder.Worker.Elixir.EventMapper do
  @moduledoc false

  alias Beholder.Worker.Elixir.Snapshot.Repository
  alias Beholder.Worker.Elixir.Compiler

  alias Beholder.Worker.V1.{
    AnalysisDiagnostic,
    EntityFact,
    FactShard,
    Observation,
    RepositoryContribution
  }

  @call_kinds [
    :imported_function,
    :imported_macro,
    :local_function,
    :local_macro,
    :remote_function,
    :remote_macro
  ]

  @doc false
  def start_cache do
    case :ets.whereis(__MODULE__) do
      :undefined -> :ets.new(__MODULE__, [:named_table, :set, :public, read_concurrency: true])
      table -> table
    end
  end

  @spec contribution(Repository.t(), map()) :: RepositoryContribution.t()
  def contribution(repository, result) do
    source_paths = source_paths(repository)
    complete = complete?(result)

    %RepositoryContribution{
      repository: repository.identity,
      completeness:
        if(complete,
          do: :ANALYSIS_COMPLETENESS_COMPLETE,
          else: :ANALYSIS_COMPLETENESS_INCOMPLETE
        ),
      grpc_bindings: [],
      diagnostics: diagnostics(repository, result),
      replaced_diagnostic_codes:
        if(complete, do: ["elixir.macro_expansion_incomplete"], else: []),
      fact_shards: cached_fact_shards(repository, result, source_paths)
    }
  end

  defp complete?(result) do
    result.status == :ok and
      Enum.all?(result.diagnostics, &(&1.severity not in [:error, "error"]))
  end

  defp definitions(repository, events, source_paths) do
    Enum.reduce(events, %{}, fn
      %{kind: :module, target: module, definitions: definitions} = event, modules
      when is_binary(module) ->
        if source_path(repository, event.file, source_paths) do
          Map.put(
            modules,
            module,
            {source_path(repository, event.file, source_paths), MapSet.new(definitions)}
          )
        else
          modules
        end

      _event, modules ->
        modules
    end)
  end

  defp entities(repository, module, functions) do
    module_id = module_id(repository.identity, module)

    [%EntityFact{id: module_id, kind: :ENTITY_KIND_NAMESPACE}]
    |> Kernel.++(
      Enum.map(functions, fn {name, arity} ->
        %EntityFact{
          id: callable_id(repository.identity, module, name, arity),
          kind: :ENTITY_KIND_CALLABLE
        }
      end)
    )
  end

  defp cached_fact_shards(
         repository,
         %{changed_events: changed_events, changed_files: changed_files} = result,
         source_paths
       )
       when is_list(changed_events) and is_list(changed_files) do
    key = {repository.identity, Path.expand(repository.base)}

    cached =
      case :ets.whereis(__MODULE__) do
        :undefined -> nil
        _table -> :ets.lookup(__MODULE__, key) |> List.first() |> then(&if(&1, do: elem(&1, 1)))
      end

    changed_paths =
      changed_files
      |> MapSet.new(&source_path(repository, &1, source_paths))
      |> MapSet.delete(nil)

    {definitions, events, impacted_paths} =
      case cached do
        nil ->
          events = Compiler.complete_events(result)
          {definitions(repository, events, source_paths), events, source_paths}

        cached ->
          definitions =
            cached.definitions
            |> Enum.reject(fn {_module, {path, _functions}} ->
              MapSet.member?(changed_paths, path)
            end)
            |> Map.new()
            |> Map.merge(definitions(repository, changed_events, source_paths))

          definition_fingerprints = definition_fingerprints(definitions)

          interface_changed =
            Map.keys(definition_fingerprints)
            |> Kernel.++(Map.keys(cached.definition_fingerprints))
            |> Enum.any?(fn module ->
              Map.get(definition_fingerprints, module) !=
                Map.get(cached.definition_fingerprints, module)
            end)

          removed_paths =
            cached.shards
            |> Map.keys()
            |> Enum.reject(&MapSet.member?(source_paths, &1))
            |> MapSet.new()

          if interface_changed do
            {definitions, Compiler.complete_events(result), source_paths}
          else
            {definitions, changed_events, MapSet.union(changed_paths, removed_paths)}
          end
      end

    {changed_shards, _changed_dependencies} =
      build_fact_shards(repository, events, definitions, source_paths, impacted_paths)

    cached = cached || %{shards: %{}}

    state = %{
      definitions: definitions,
      definition_fingerprints: definition_fingerprints(definitions),
      shards:
        cached.shards
        |> Map.drop(MapSet.to_list(impacted_paths))
        |> Map.merge(changed_shards)
    }

    if :ets.whereis(__MODULE__) != :undefined, do: :ets.insert(__MODULE__, {key, state})
    state.shards |> Map.values() |> Enum.sort_by(& &1.owner)
  end

  defp cached_fact_shards(repository, result, source_paths) do
    definitions = definitions(repository, result.events, source_paths)

    {shards, _dependencies} =
      build_fact_shards(repository, result.events, definitions, source_paths, source_paths)

    shards |> Map.values() |> Enum.sort_by(& &1.owner)
  end

  defp build_fact_shards(repository, events, definitions, source_paths, included_paths) do
    entities_by_path =
      definitions
      |> Enum.filter(fn {_module, {path, _functions}} ->
        MapSet.member?(included_paths, path)
      end)
      |> Enum.group_by(fn {_module, {path, _functions}} -> path end)
      |> Map.new(fn {path, modules} ->
        entities =
          modules
          |> Enum.flat_map(fn {module, {_path, functions}} ->
            entities(repository, module, functions)
          end)
          |> Enum.sort_by(& &1.id)

        {path, entities}
      end)

    {observations_by_path, dependencies_by_path} =
      events
      |> Enum.filter(&(&1.kind in @call_kinds or not is_nil(module_relation(&1.kind))))
      |> Enum.reduce({%{}, %{}}, fn event, {observations, dependencies} ->
        case source_path(repository, event.file, source_paths) do
          path when not is_nil(path) ->
            if MapSet.member?(included_paths, path) do
              dependencies =
                case dependency_module(event) do
                  module when is_binary(module) ->
                    Map.update(dependencies, path, MapSet.new([module]), &MapSet.put(&1, module))

                  _module ->
                    dependencies
                end

              observations =
                case observation(repository, event, definitions, source_paths) do
                  nil ->
                    observations

                  observation ->
                    Map.update(observations, path, [observation], &[observation | &1])
                end

              {observations, dependencies}
            else
              {observations, dependencies}
            end

          _path ->
            {observations, dependencies}
        end
      end)

    observations_by_path =
      Map.new(observations_by_path, fn {path, observations} ->
        observations =
          observations
          |> Enum.uniq_by(&{&1.from, &1.relation, &1.to, &1.evidence})
          |> Enum.sort_by(&{&1.from, &1.relation, &1.to, &1.evidence})

        {path, observations}
      end)

    shards =
      entities_by_path
      |> Map.keys()
      |> Kernel.++(Map.keys(observations_by_path))
      |> Enum.uniq()
      |> Map.new(fn path ->
        entities = Map.get(entities_by_path, path, [])
        observations = Map.get(observations_by_path, path, [])

        {path,
         %FactShard{
           repository: repository.identity,
           producer: "elixir",
           owner: "repo://#{repository.identity}/elixir-source/#{path}",
           version: shard_version(entities, observations),
           entities: entities,
           observations: observations
         }}
      end)

    {shards, dependencies_by_path}
  end

  defp definition_fingerprints(definitions) do
    Map.new(definitions, fn {module, {_path, functions}} ->
      {module, shard_version([], Enum.sort(MapSet.to_list(functions)))}
    end)
  end

  defp dependency_module(%{kind: kind, caller_module: module})
       when kind in [:local_function, :local_macro],
       do: module

  defp dependency_module(%{kind: kind, target: module}) when kind in @call_kinds, do: module
  defp dependency_module(%{target: module}), do: module

  defp shard_version(entities, observations) do
    :sha256
    |> :crypto.hash(:erlang.term_to_binary({entities, observations}, [:deterministic]))
    |> Base.encode16(case: :lower)
  end

  defp observation(repository, event, definitions, source_paths) when event.kind in @call_kinds do
    with from when not is_nil(from) <- caller_id(repository, event, source_paths),
         to when not is_nil(to) <- call_target(repository, event, definitions),
         evidence when not is_nil(evidence) <- evidence(repository, event, source_paths) do
      %Observation{
        from: from,
        relation: :RELATION_KIND_CALLS,
        to: to,
        evidence: evidence,
        confidence: confidence(event),
        provenance: :PROVENANCE_COMPILER
      }
    else
      _ -> nil
    end
  end

  defp observation(repository, event, definitions, source_paths) do
    with relation when not is_nil(relation) <- module_relation(event.kind),
         from when not is_nil(from) <- caller_id(repository, event, source_paths),
         target when is_binary(target) <- event.target,
         evidence when not is_nil(evidence) <- evidence(repository, event, source_paths) do
      %Observation{
        from: from,
        relation: relation,
        to: module_target(repository.identity, target, definitions),
        evidence: evidence,
        confidence: confidence(event),
        provenance: :PROVENANCE_COMPILER
      }
    else
      _ -> nil
    end
  end

  defp caller_id(
         repository,
         %{caller_module: module, caller_function: {name, arity}},
         _source_paths
       )
       when is_binary(module) do
    callable_id(repository.identity, module, name, arity)
  end

  defp caller_id(repository, %{caller_module: module}, _source_paths) when is_binary(module),
    do: module_id(repository.identity, module)

  defp caller_id(repository, event, source_paths) do
    case source_path(repository, event.file, source_paths) do
      nil -> nil
      path -> "repo://#{repository.identity}/elixir-source/#{path}"
    end
  end

  defp call_target(repository, event, definitions) do
    module =
      case event.kind do
        kind when kind in [:local_function, :local_macro] -> event.caller_module
        _kind -> event.target
      end

    if is_binary(module) do
      if definitions
         |> Map.get(module, {nil, MapSet.new()})
         |> elem(1)
         |> MapSet.member?({event.name, event.arity}) do
        callable_id(repository.identity, module, event.name, event.arity)
      else
        "elixir-call://#{module}/#{event.name}/#{event.arity}"
      end
    end
  end

  defp module_target(repository, module, definitions) do
    cond do
      Map.has_key?(definitions, module) -> module_id(repository, module)
      String.starts_with?(module, ":") -> "erlang-module://#{String.trim_leading(module, ":")}"
      true -> "elixir-module://#{module}"
    end
  end

  defp module_relation(:import), do: :RELATION_KIND_IMPORTS
  defp module_relation(:require), do: :RELATION_KIND_REQUIRES

  defp module_relation(kind)
       when kind in [:alias, :alias_expansion, :alias_reference, :struct_expansion],
       do: :RELATION_KIND_USES

  defp module_relation(_kind), do: nil

  defp evidence(repository, event, source_paths) do
    with path when not is_nil(path) <- source_path(repository, event.file, source_paths) do
      suffix = if event.from_macro, do: " via macro expansion", else: ""
      "#{path} (compiler #{event.kind}#{suffix})"
    end
  end

  defp confidence(%{from_macro: true}), do: :CONFIDENCE_INFERRED
  defp confidence(_event), do: :CONFIDENCE_EXACT

  defp diagnostics(repository, result) do
    compiler_diagnostics =
      Enum.map(result.diagnostics, fn diagnostic ->
        %AnalysisDiagnostic{
          code: diagnostic_code(diagnostic.severity),
          severity: diagnostic_severity(diagnostic.severity),
          path: diagnostic_path(repository, diagnostic.file),
          line: diagnostic_line(diagnostic.position),
          detail: diagnostic.message
        }
      end)

    traced_files =
      result
      |> Map.get_lazy(:traced_files, fn ->
        result.events
        |> Enum.filter(&(&1.kind == :source_start))
        |> Enum.map(& &1.file)
      end)
      |> MapSet.new(&relative_path(repository, &1))
      |> MapSet.delete(nil)

    coverage_diagnostics =
      repository
      |> Repository.source_inputs()
      |> Enum.filter(&(Path.extname(&1.path) == ".ex"))
      |> Enum.reject(&MapSet.member?(traced_files, normalize_path(&1.path)))
      |> Enum.map(fn input ->
        %AnalysisDiagnostic{
          code: "elixir.compiler.source_not_traced",
          severity: :ANALYSIS_DIAGNOSTIC_SEVERITY_KNOWN_LIMITATION,
          path: normalize_path(input.path),
          detail: "Mix did not compile this source in the selected environment"
        }
      end)

    compiler_diagnostics ++ coverage_diagnostics
  end

  defp diagnostic_code("warning"), do: "elixir.compiler.warning"
  defp diagnostic_code(_severity), do: "elixir.compiler.error"

  defp diagnostic_severity("warning"), do: :ANALYSIS_DIAGNOSTIC_SEVERITY_WARNING
  defp diagnostic_severity(_severity), do: :ANALYSIS_DIAGNOSTIC_SEVERITY_WARNING

  defp diagnostic_path(_repository, nil), do: "mix.exs"

  defp diagnostic_path(repository, path) do
    relative_path(repository, path) || normalize_path(path)
  end

  defp diagnostic_line({line, _column}) when is_integer(line) and line > 0, do: line
  defp diagnostic_line(line) when is_integer(line) and line > 0, do: line
  defp diagnostic_line(_position), do: nil

  defp relative_path(repository, path) when is_binary(path) do
    expanded = Path.expand(path, repository.base)
    relative = Path.relative_to(expanded, repository.base)

    if relative == expanded or relative == ".." or String.starts_with?(relative, "../") do
      nil
    else
      normalize_path(relative)
    end
  end

  defp relative_path(_repository, _path), do: nil

  defp source_path(repository, path, source_paths) do
    with relative when not is_nil(relative) <- relative_path(repository, path),
         true <- MapSet.member?(source_paths, relative) do
      relative
    else
      _ -> nil
    end
  end

  defp source_paths(repository) do
    repository
    |> Repository.source_inputs()
    |> MapSet.new(&normalize_path(&1.path))
  end

  defp normalize_path(path), do: String.replace(path, "\\", "/")

  defp module_id(repository, module), do: "repo://#{repository}/elixir/#{module}"

  defp callable_id(repository, module, name, arity),
    do: "#{module_id(repository, module)}/#{name}/#{arity}"
end
