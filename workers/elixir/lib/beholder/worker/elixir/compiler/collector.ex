defmodule Beholder.Worker.Elixir.Compiler.Collector do
  @moduledoc false
  use GenServer

  @spec start_link(keyword()) :: GenServer.on_start()
  def start_link(options \\ []) do
    GenServer.start_link(__MODULE__, [], Keyword.put_new(options, :name, __MODULE__))
  end

  @spec record(map()) :: :ok
  def record(event) do
    GenServer.cast(__MODULE__, {:record, event})
  end

  @spec drain() :: [map()]
  def drain do
    GenServer.call(__MODULE__, :drain, :infinity)
  end

  @impl true
  def init(events), do: {:ok, events}

  @impl true
  def handle_cast({:record, event}, events), do: {:noreply, [event | events]}

  @impl true
  def handle_call(:drain, _from, events), do: {:reply, Enum.reverse(events), []}
end
