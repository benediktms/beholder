defmodule Beholder.Worker.Elixir.Snapshot.Repository do
  @moduledoc false

  defstruct [:identity, :base, :head, :fingerprint, inputs: []]

  @type input :: %{path: String.t(), content: binary(), kind: atom()}
  @type t :: %__MODULE__{
          identity: String.t(),
          base: String.t(),
          head: String.t() | nil,
          fingerprint: String.t(),
          inputs: [input()]
        }

  @spec source_inputs(t()) :: [input()]
  def source_inputs(%__MODULE__{inputs: inputs}) do
    inputs
    |> Enum.filter(&(&1.kind == :INPUT_KIND_SOURCE))
    |> Enum.sort_by(& &1.path)
  end

  @spec mix_project?(t()) :: boolean()
  def mix_project?(repository) do
    Enum.any?(source_inputs(repository), &(&1.path == "mix.exs"))
  end
end
