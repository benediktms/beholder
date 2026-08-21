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
        true = :ets.insert(table, {System.unique_integer([:monotonic, :positive]), event})
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
      :ordered_set,
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
end
