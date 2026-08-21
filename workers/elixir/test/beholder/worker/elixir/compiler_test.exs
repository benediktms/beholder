defmodule Beholder.Worker.Elixir.CompilerTest do
  use ExUnit.Case, async: false

  alias Beholder.Worker.Elixir.Compiler
  alias Beholder.Worker.Elixir.Snapshot.Repository

  test "compiles a Mix project in a child VM and returns trace events" do
    root = temp_dir("project")
    cache = temp_dir("cache")
    File.mkdir_p!(Path.join(root, "lib"))

    mix_source = """
    defmodule CompilerFixture.MixProject do
      use Mix.Project

      def project do
        [app: :compiler_fixture, version: "0.1.0", elixir: "~> 1.15"]
      end
    end
    """

    source = """
    defmodule CompilerFixture do
      def call(values), do: Enum.map(values, &to_string/1)
    end
    """

    File.write!(Path.join(root, "mix.exs"), mix_source)
    File.write!(Path.join(root, "lib/compiler_fixture.ex"), source)

    repository = %Repository{
      identity: "fixture",
      base: root,
      fingerprint: "abc123",
      inputs: [
        %{path: "mix.exs", content: mix_source, kind: :INPUT_KIND_SOURCE},
        %{path: "lib/compiler_fixture.ex", content: source, kind: :INPUT_KIND_SOURCE}
      ]
    }

    assert {:ok, result} = Compiler.run(repository, cache)
    assert result.status == :ok, inspect(result)
    assert Enum.any?(result.events, &(&1.kind == :remote_function and &1.target == "Enum"))

    assert Enum.any?(result.events, fn event ->
             event.kind == :module and {"call", 1} in event.definitions
           end)

    assert File.dir?(Path.join([cache, "elixir", "Zml4dHVyZQ", "build-YWJjMTIz"]))
  end

  test "rejects a checkout that no longer matches the snapshot" do
    root = temp_dir("changed")
    File.write!(Path.join(root, "mix.exs"), "changed")

    repository = %Repository{
      identity: "fixture",
      base: root,
      fingerprint: "abc123",
      inputs: [%{path: "mix.exs", content: "original", kind: :INPUT_KIND_SOURCE}]
    }

    assert {:error, "mix.exs changed after the immutable snapshot was created"} =
             Compiler.run(repository, temp_dir("unused-cache"))
  end

  defp temp_dir(label) do
    path =
      Path.join(
        System.tmp_dir!(),
        "beholder-elixir-#{label}-#{System.unique_integer([:positive])}"
      )

    File.rm_rf!(path)
    File.mkdir_p!(path)
    on_exit(fn -> File.rm_rf!(path) end)
    path
  end
end
