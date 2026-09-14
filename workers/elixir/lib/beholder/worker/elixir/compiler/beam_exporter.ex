defmodule Beholder.Worker.Elixir.Compiler.BeamExporter do
  @moduledoc false

  @sources [
    Path.expand("collector.ex", __DIR__),
    Path.expand("tracer.ex", __DIR__),
    Path.expand("task.ex", __DIR__)
  ]

  for source <- @sources, do: @external_resource(source)

  @spec sources() :: [String.t()]
  def sources, do: @sources

  @doc false
  def identity(sources \\ @sources) do
    sources
    |> Enum.map(&{Path.basename(&1), File.read!(&1)})
    |> :erlang.term_to_binary([:deterministic])
    |> then(&:crypto.hash(:sha256, &1))
    |> Base.url_encode64(padding: false)
  end

  @spec compile_script() :: String.t()
  def compile_script do
    ~S"""
    ["helpers", output | sources] = System.argv()
    File.mkdir_p!(output)

    Enum.each(sources, fn source ->
      Enum.each(Code.compile_file(source), fn {module, bytecode} ->
        File.write!(Path.join(output, Atom.to_string(module) <> ".beam"), bytecode)
      end)
    end)
    """
  end
end
