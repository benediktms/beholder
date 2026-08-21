defmodule Beholder.Worker.Elixir.Compiler.TracerTest do
  use ExUnit.Case, async: false

  alias Beholder.Worker.Elixir.Compiler.{Collector, Tracer}

  test "captures compiler-resolved remote and local calls" do
    module = Module.concat([BeholderTracerFixture, "M#{System.unique_integer([:positive])}"])
    {:ok, collector} = Collector.start_link()
    previous_tracers = Code.get_compiler_option(:tracers)
    Code.put_compiler_option(:tracers, previous_tracers ++ [Tracer])

    on_exit(fn ->
      Code.put_compiler_option(:tracers, previous_tracers)
      if Process.alive?(collector), do: GenServer.stop(collector)
      :code.purge(module)
      :code.delete(module)
    end)

    Code.compile_string("""
    defmodule #{inspect(module)} do
      def call(values), do: helper(Enum.map(values, &to_string/1))
      defp helper(values), do: values
    end
    """)

    events = Collector.drain()

    assert Enum.any?(events, &(&1.kind == :remote_function and &1.target == "Enum"))
    assert Enum.any?(events, &(&1.kind == :local_function and &1.name == "helper"))
    assert Enum.any?(events, &(&1.kind == :module and &1.target == inspect_module(module)))
  end

  defp inspect_module(module) do
    module |> Atom.to_string() |> String.trim_leading("Elixir.")
  end
end
