defmodule Beholder.Worker.Elixir.EventMapperTest do
  use ExUnit.Case, async: true

  alias Beholder.Worker.Elixir.EventMapper
  alias Beholder.Worker.Elixir.Snapshot.Repository

  test "maps resolved compiler calls onto baseline identities" do
    repository = %Repository{
      identity: "example",
      base: "/tmp/example",
      inputs: [%{path: "lib/example.ex", content: "", kind: :INPUT_KIND_SOURCE}]
    }

    result = %{
      status: :ok,
      diagnostics: [],
      events: [
        event(:module, %{target: "Example", definitions: [{"call", 1}, {"helper", 1}]}),
        event(:local_function, %{name: "helper", arity: 1}),
        event(:remote_function, %{target: "Enum", name: "map", arity: 2})
      ]
    }

    contribution = EventMapper.contribution(repository, result)
    observations = Enum.flat_map(contribution.fact_shards, & &1.observations)

    assert contribution.completeness == :ANALYSIS_COMPLETENESS_COMPLETE
    assert contribution.replaced_diagnostic_codes == ["elixir.macro_expansion_incomplete"]
    assert [%{owner: "repo://example/elixir-source/lib/example.ex"}] = contribution.fact_shards
    assert contribution.entities == []
    assert contribution.observations == []

    assert Enum.any?(observations, fn observation ->
             observation.from == "repo://example/elixir/Example/call/1" and
               observation.to == "repo://example/elixir/Example/helper/1" and
               observation.provenance == :PROVENANCE_COMPILER
           end)

    assert Enum.any?(observations, fn observation ->
             observation.to == "elixir-call://Enum/map/2"
           end)

    assert Enum.all?(observations, fn observation ->
             observation.evidence =~ "lib/example.ex (compiler" and
               not String.contains?(observation.evidence, "lib/example.ex:2")
           end)
  end

  test "does not attribute dependency compiler events to the target repository" do
    repository = %Repository{
      identity: "example",
      base: "/tmp/example",
      inputs: [%{path: "lib/example.ex", content: "", kind: :INPUT_KIND_SOURCE}]
    }

    dependency_event =
      event(:module, %{
        target: "Dependency",
        definitions: [{"call", 0}],
        file: "/tmp/example/deps/dependency/lib/dependency.ex",
        caller_module: "Dependency",
        caller_function: nil
      })

    contribution =
      EventMapper.contribution(repository, %{
        status: :ok,
        diagnostics: [],
        events: [dependency_event]
      })

    assert contribution.fact_shards == []
  end

  test "keeps baseline incompleteness diagnostics when compilation reports an error" do
    repository = %Repository{identity: "example", base: "/tmp/example", inputs: []}

    contribution =
      EventMapper.contribution(repository, %{
        status: :ok,
        diagnostics: [%{message: "failed", severity: "error", file: nil, position: nil}],
        events: []
      })

    assert contribution.completeness == :ANALYSIS_COMPLETENESS_INCOMPLETE
    assert contribution.replaced_diagnostic_codes == []
  end

  test "marks macro-expanded observations as inferred" do
    repository = %Repository{
      identity: "example",
      base: "/tmp/example",
      inputs: [%{path: "lib/example.ex", content: "", kind: :INPUT_KIND_SOURCE}]
    }

    contribution =
      EventMapper.contribution(repository, %{
        status: :ok,
        diagnostics: [],
        events: [
          event(:remote_function, %{target: "Generated", name: "call", arity: 0, from_macro: true})
        ]
      })

    assert [shard] = contribution.fact_shards

    assert [%{confidence: :CONFIDENCE_INFERRED, provenance: :PROVENANCE_COMPILER}] =
             shard.observations
  end

  test "reuses unchanged source shards and invalidates definition dependants" do
    EventMapper.start_cache()
    :ets.delete_all_objects(EventMapper)

    repository = %Repository{
      identity: "cached-example",
      base: "/tmp/cached-example",
      inputs: [
        %{path: "lib/a.ex", content: "", kind: :INPUT_KIND_SOURCE},
        %{path: "lib/b.ex", content: "", kind: :INPUT_KIND_SOURCE}
      ]
    }

    module_a =
      event(:module, %{
        file: "/tmp/cached-example/lib/a.ex",
        target: "A",
        caller_module: "A",
        caller_function: nil,
        definitions: [{"run", 0}]
      })

    module_b =
      event(:module, %{
        file: "/tmp/cached-example/lib/b.ex",
        target: "B",
        caller_module: "B",
        caller_function: nil,
        definitions: [{"call", 0}]
      })

    call_a =
      event(:remote_function, %{
        file: "/tmp/cached-example/lib/b.ex",
        target: "A",
        caller_module: "B",
        caller_function: {"call", 0},
        name: "run",
        arity: 0
      })

    contribution =
      EventMapper.contribution(repository, %{
        status: :ok,
        diagnostics: [],
        changed_files: [
          "/tmp/cached-example/lib/a.ex",
          "/tmp/cached-example/lib/b.ex"
        ],
        changed_events: [module_a, module_b, call_a],
        events: [module_a, module_b, call_a]
      })

    assert shard_observation_target(contribution, "lib/b.ex") ==
             "repo://cached-example/elixir/A/run/0"

    unrelated_call = %{call_a | target: "Other"}

    contribution =
      EventMapper.contribution(repository, %{
        status: :ok,
        diagnostics: [],
        changed_files: ["/tmp/cached-example/lib/a.ex"],
        changed_events: [module_a],
        events: [module_a, module_b, unrelated_call]
      })

    assert shard_observation_target(contribution, "lib/b.ex") ==
             "repo://cached-example/elixir/A/run/0"

    contribution =
      EventMapper.contribution(repository, %{
        status: :ok,
        diagnostics: [],
        changed_files: ["/tmp/cached-example/lib/a.ex"],
        changed_events: [%{module_a | definitions: []}],
        events: [%{module_a | definitions: []}, module_b, call_a]
      })

    assert shard_observation_target(contribution, "lib/b.ex") == "elixir-call://A/run/0"
  end

  defp shard_observation_target(contribution, path) do
    contribution.fact_shards
    |> Enum.find(&String.ends_with?(&1.owner, path))
    |> Map.fetch!(:observations)
    |> List.first()
    |> Map.fetch!(:to)
  end

  defp event(kind, extra) do
    Map.merge(
      %{
        kind: kind,
        file: "/tmp/example/lib/example.ex",
        line: 2,
        column: 5,
        caller_module: "Example",
        caller_function: {"call", 1},
        from_macro: false
      },
      extra
    )
  end
end
