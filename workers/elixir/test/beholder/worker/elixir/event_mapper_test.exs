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

    assert contribution.completeness == :ANALYSIS_COMPLETENESS_COMPLETE

    assert Enum.any?(contribution.observations, fn observation ->
             observation.from == "repo://example/elixir/Example/call/1" and
               observation.to == "repo://example/elixir/Example/helper/1" and
               observation.provenance == :PROVENANCE_COMPILER
           end)

    assert Enum.any?(contribution.observations, fn observation ->
             observation.to == "elixir-call://Enum/map/2"
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

    assert contribution.entities == []
    assert contribution.observations == []
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
