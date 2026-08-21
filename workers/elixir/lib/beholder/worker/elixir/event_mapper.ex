defmodule Beholder.Worker.Elixir.EventMapper do
  @moduledoc false

  alias Beholder.Worker.Elixir.Snapshot.Repository

  alias Beholder.Worker.V1.{
    AnalysisDiagnostic,
    EntityFact,
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

  @spec contribution(Repository.t(), map()) :: RepositoryContribution.t()
  def contribution(repository, result) do
    definitions = definitions(repository, result.events)

    observations =
      result.events
      |> Enum.filter(&(&1.kind in @call_kinds or not is_nil(module_relation(&1.kind))))
      |> Enum.map(&observation(repository, &1, definitions))
      |> Enum.reject(&is_nil/1)
      |> Enum.uniq_by(&{&1.from, &1.relation, &1.to, &1.evidence})

    %RepositoryContribution{
      repository: repository.identity,
      completeness: completeness(result.status),
      entities: entities(repository, definitions),
      grpc_bindings: [],
      observations: observations,
      diagnostics: diagnostics(repository, result)
    }
  end

  defp definitions(repository, events) do
    Enum.reduce(events, %{}, fn
      %{kind: :module, target: module, definitions: definitions} = event, modules
      when is_binary(module) ->
        if source_path(repository, event.file) do
          Map.put(modules, module, MapSet.new(definitions))
        else
          modules
        end

      _event, modules ->
        modules
    end)
  end

  defp entities(repository, definitions) do
    Enum.flat_map(definitions, fn {module, functions} ->
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
    end)
  end

  defp observation(repository, event, definitions) when event.kind in @call_kinds do
    with from when not is_nil(from) <- caller_id(repository, event),
         to when not is_nil(to) <- call_target(repository, event, definitions),
         evidence when not is_nil(evidence) <- evidence(repository, event) do
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

  defp observation(repository, event, definitions) do
    with relation when not is_nil(relation) <- module_relation(event.kind),
         from when not is_nil(from) <- caller_id(repository, event),
         target when is_binary(target) <- event.target,
         evidence when not is_nil(evidence) <- evidence(repository, event) do
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

  defp caller_id(repository, %{caller_module: module, caller_function: {name, arity}})
       when is_binary(module) do
    callable_id(repository.identity, module, name, arity)
  end

  defp caller_id(repository, %{caller_module: module}) when is_binary(module),
    do: module_id(repository.identity, module)

  defp caller_id(repository, event) do
    case source_path(repository, event.file) do
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
      if Map.get(definitions, module, MapSet.new())
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

  defp evidence(repository, event) do
    with path when not is_nil(path) <- source_path(repository, event.file) do
      location = if event.line in [nil, 0], do: path, else: "#{path}:#{event.line}"
      suffix = if event.from_macro, do: " via macro expansion", else: ""
      "#{location} (compiler #{event.kind}#{suffix})"
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
      result.events
      |> Enum.filter(&(&1.kind == :source_start))
      |> MapSet.new(&relative_path(repository, &1.file))
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

  defp completeness(:ok), do: :ANALYSIS_COMPLETENESS_COMPLETE
  defp completeness(:error), do: :ANALYSIS_COMPLETENESS_INCOMPLETE

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

  defp source_path(repository, path) do
    with relative when not is_nil(relative) <- relative_path(repository, path),
         true <- MapSet.member?(source_paths(repository), relative) do
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
