defmodule Beholder.Worker.Elixir.Snapshot do
  @moduledoc false

  alias Beholder.Worker.Elixir.Snapshot.Repository
  alias Beholder.Worker.V1.AnalyzeRequest

  defstruct name: nil, repositories: %{}, finished?: false

  @type t :: %__MODULE__{
          name: String.t() | nil,
          repositories: %{String.t() => Repository.t()},
          finished?: boolean()
        }

  @spec from_requests(Enumerable.t()) :: {:ok, t()} | {:error, String.t()}
  def from_requests(requests) do
    with {:ok, snapshot} <-
           Enum.reduce_while(requests, {:ok, %__MODULE__{}}, fn request, {:ok, snapshot} ->
             case push(snapshot, request) do
               {:ok, snapshot} -> {:cont, {:ok, snapshot}}
               {:error, _} = error -> {:halt, error}
             end
           end),
         :ok <- validate_finished(snapshot) do
      {:ok, snapshot}
    end
  end

  @spec repositories(t()) :: [Repository.t()]
  def repositories(%__MODULE__{repositories: repositories}) do
    repositories |> Map.values() |> Enum.sort_by(& &1.identity)
  end

  defp push(%__MODULE__{finished?: true}, %AnalyzeRequest{}),
    do: {:error, "worker request followed analysis finish"}

  defp push(%__MODULE__{name: nil} = snapshot, %AnalyzeRequest{
         request: {:start, %{workspace: workspace}}
       })
       when workspace != "" do
    {:ok, %{snapshot | name: workspace}}
  end

  defp push(%__MODULE__{name: nil}, %AnalyzeRequest{request: {:repository, _}}),
    do: {:error, "worker repository preceded analysis start"}

  defp push(%__MODULE__{name: nil}, %AnalyzeRequest{request: {:input, _}}),
    do: {:error, "worker input preceded analysis start"}

  defp push(%__MODULE__{name: nil}, %AnalyzeRequest{request: {:finish, _}}),
    do: {:error, "worker analysis omitted analysis start"}

  defp push(%__MODULE__{name: name}, %AnalyzeRequest{request: {:start, _}})
       when not is_nil(name),
       do: {:error, "worker analysis started more than once"}

  defp push(snapshot, %AnalyzeRequest{request: {:repository, repository}}) do
    if Map.has_key?(snapshot.repositories, repository.identity) do
      {:error, "worker repository appeared more than once"}
    else
      value = %Repository{
        identity: repository.identity,
        base: Path.expand(repository.base),
        head: repository.head,
        fingerprint: repository.fingerprint
      }

      {:ok, put_in(snapshot.repositories[repository.identity], value)}
    end
  end

  defp push(snapshot, %AnalyzeRequest{request: {:input, input}}) do
    case Map.fetch(snapshot.repositories, input.repository) do
      {:ok, repository} ->
        input = %{path: input.path, content: input.content, kind: input.kind}
        repository = %{repository | inputs: [input | repository.inputs]}
        {:ok, put_in(snapshot.repositories[repository.identity], repository)}

      :error ->
        {:error, "worker input references an unknown repository"}
    end
  end

  defp push(snapshot, %AnalyzeRequest{request: {:finish, _}}),
    do: {:ok, %{snapshot | finished?: true}}

  defp push(_snapshot, %AnalyzeRequest{request: nil}), do: {:error, "worker request is empty"}
  defp push(_snapshot, _request), do: {:error, "worker request has an unknown payload"}

  defp validate_finished(%__MODULE__{finished?: true}), do: :ok

  defp validate_finished(_snapshot),
    do: {:error, "worker request stream ended before analysis finish"}
end
