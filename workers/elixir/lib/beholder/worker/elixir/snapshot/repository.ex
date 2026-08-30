defmodule Beholder.Worker.Elixir.Snapshot.Repository do
  @moduledoc false

  defstruct [:identity, :base, :head, :fingerprint, inputs: []]

  @type input :: %{
          path: String.t(),
          content: binary(),
          content_hash: binary(),
          kind: atom()
        }
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

  @spec sorted_inputs(t()) :: [input()]
  def sorted_inputs(%__MODULE__{inputs: inputs}), do: Enum.sort_by(inputs, & &1.path)

  @spec mix_project_root(t()) :: {:ok, String.t()} | {:error, String.t()}
  def mix_project_root(repository) do
    roots =
      repository.inputs
      |> Enum.filter(&(Path.basename(&1.path) == "mix.exs"))
      |> Enum.map(&Path.dirname(&1.path))
      |> Enum.uniq()
      |> Enum.sort_by(&{length(Path.split(&1)), &1})

    case roots do
      [] ->
        {:error, "Elixir compiler enrichment target does not contain mix.exs"}

      [root] ->
        {:ok, root}

      [root, next | _] ->
        if length(Path.split(root)) == length(Path.split(next)) do
          {:error, "Elixir compiler enrichment target has multiple shallowest mix.exs files"}
        else
          {:ok, root}
        end
    end
  end
end
