defmodule Beholder.Worker.Elixir.Compiler.BeamExporter do
  @moduledoc false

  @modules [
    Beholder.Worker.Elixir.Compiler.Collector,
    Beholder.Worker.Elixir.Compiler.Tracer,
    Mix.Tasks.Beholder.Compile
  ]

  @spec export!(String.t()) :: String.t()
  def export!(cache_dir) do
    output = Path.join(cache_dir, "compiler-helper-ebin")
    File.mkdir_p!(output)

    Enum.each(@modules, fn module ->
      {^module, bytecode, filename} = :code.get_object_code(module)
      File.write!(Path.join(output, Path.basename(to_string(filename))), bytecode)
    end)

    output
  end
end
