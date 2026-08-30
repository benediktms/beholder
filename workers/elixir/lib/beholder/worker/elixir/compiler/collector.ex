defmodule Beholder.Worker.Elixir.Compiler.Collector do
  @moduledoc false
  use GenServer

  @table __MODULE__

  @spec start_link(keyword()) :: GenServer.on_start()
  def start_link(options \\ []) do
    GenServer.start_link(__MODULE__, [], Keyword.put_new(options, :name, __MODULE__))
  end

  @spec record(map()) :: :ok
  def record(event) do
    case :ets.whereis(@table) do
      :undefined ->
        :ok

      table ->
        true = :ets.insert(table, {event_key(event), event})
        :ok
    end
  end

  @spec drain() :: [map()]
  def drain do
    GenServer.call(__MODULE__, :drain, :infinity)
  end

  @impl true
  def init([]) do
    :ets.new(@table, [
      :named_table,
      :set,
      :public,
      read_concurrency: true,
      write_concurrency: true
    ])

    {:ok, nil}
  end

  @impl true
  def handle_call(:drain, _from, state) do
    events = @table |> :ets.tab2list() |> Enum.map(&elem(&1, 1))
    :ets.delete_all_objects(@table)
    {:reply, events, state}
  end

  defp event_key(%{kind: kind, file: file}) when kind in [:source_start, :source_stop],
    do: {kind, file}

  defp event_key(%{kind: :module, file: file, target: target}),
    do: {:module, file, target}

  defp event_key(
         %{
           kind: kind,
           file: file,
           caller_module: caller_module,
           caller_function: caller_function,
           from_macro: from_macro
         } = event
       ) do
    {
      kind,
      file,
      caller_module,
      caller_function,
      from_macro,
      Map.get(event, :target),
      Map.get(event, :name),
      Map.get(event, :arity)
    }
  end

  defp event_key(event), do: {:event, event}
end
