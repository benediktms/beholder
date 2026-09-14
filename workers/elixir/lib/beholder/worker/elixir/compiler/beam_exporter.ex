defmodule Beholder.Worker.Elixir.Compiler.BeamExporter do
  @moduledoc false

  @source_paths [
    Path.expand("collector.ex", __DIR__),
    Path.expand("tracer.ex", __DIR__),
    Path.expand("task.ex", __DIR__)
  ]

  for source <- @source_paths, do: @external_resource(source)

  @sources Enum.map(@source_paths, &{Path.basename(&1), File.read!(&1)})

  @doc false
  def identity(sources \\ @sources) do
    sources
    |> :erlang.term_to_binary([:deterministic])
    |> then(&:crypto.hash(:sha256, &1))
    |> Base.url_encode64(padding: false)
  end

  @spec materialize!(String.t(), String.t()) :: [String.t()]
  def materialize!(cache_dir, identity) do
    directory = Path.join(cache_dir, "compiler-helper-sources-#{identity}")
    File.mkdir_p!(directory)

    Enum.map(@sources, fn {name, content} ->
      path = Path.join(directory, name)
      File.write!(path, content)
      path
    end)
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
