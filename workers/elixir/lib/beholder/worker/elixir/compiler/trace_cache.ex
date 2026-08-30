defmodule Beholder.Worker.Elixir.Compiler.TraceCache do
  @moduledoc false

  use GenServer

  @disk_version 3
  @legacy_disk_version 2

  def start_cache do
    case Process.whereis(__MODULE__) do
      nil ->
        case GenServer.start(__MODULE__, %{}, name: __MODULE__) do
          {:error, {:already_started, pid}} -> {:ok, pid}
          result -> result
        end

      pid ->
        {:ok, pid}
    end
  end

  def load(path) do
    {:ok, _pid} = start_cache()
    GenServer.call(__MODULE__, {:load, path}, :infinity)
  end

  def merge(path, load_status, changed_inputs, changed_events, status) do
    GenServer.call(
      __MODULE__,
      {:merge, path, load_status, changed_inputs, changed_events, status},
      :infinity
    )
  end

  def all_events(path) do
    GenServer.call(__MODULE__, {:all_events, path}, :infinity)
  end

  @doc false
  def clear do
    case Process.whereis(__MODULE__) do
      nil -> :ok
      _pid -> GenServer.call(__MODULE__, :clear)
    end
  end

  @impl true
  # ponytail: worker-lifetime cache; add LRU eviction when multi-repository RSS shows pressure.
  def init(state), do: {:ok, state}

  @impl true
  def handle_call({:load, path}, _from, state) do
    case Map.fetch(state, path) do
      {:ok, _entry} ->
        {:reply, :warm, state}

      :error ->
        case read_entry(path) do
          {:ok, entry} -> {:reply, :cold, Map.put(state, path, entry)}
          :error -> {:reply, :miss, Map.put(state, path, empty_entry())}
        end
    end
  end

  def handle_call(
        {:merge, path, load_status, changed_inputs, changed_events, status},
        _from,
        state
      ) do
    entry = Map.get(state, path, empty_entry())

    invalidated =
      MapSet.union(changed_inputs, MapSet.new(changed_events, & &1.file))

    grouped_events = Enum.group_by(changed_events, & &1.file)

    candidate = %{
      shards:
        entry.shards
        |> Map.drop(MapSet.to_list(invalidated))
        |> Map.merge(encode_shards(grouped_events)),
      traced_files: update_traced_files(entry.traced_files, invalidated, grouped_events)
    }

    events_complete? = load_status != :warm
    events = if events_complete?, do: decode_all(candidate), else: changed_events

    result = %{
      events: events,
      events_complete?: events_complete?,
      changed_files: MapSet.to_list(invalidated),
      traced_files: MapSet.to_list(candidate.traced_files)
    }

    if status == :ok do
      write_entry(path, candidate)
      {:reply, result, Map.put(state, path, candidate)}
    else
      {:reply, result, state}
    end
  end

  def handle_call({:all_events, path}, _from, state) do
    {:reply, state |> Map.get(path, empty_entry()) |> decode_all(), state}
  end

  def handle_call(:clear, _from, _state), do: {:reply, :ok, %{}}

  defp read_entry(path) do
    with {:ok, encoded} <- File.read(path),
         cache <- :erlang.binary_to_term(encoded, [:safe]) do
      decode_entry(cache)
    else
      _missing_or_invalid -> :error
    end
  rescue
    _invalid_cache -> :error
  end

  defp decode_entry(%{version: @legacy_disk_version, events: events}) when is_list(events) do
    if valid_events?(events), do: {:ok, entry_from_events(events)}, else: :error
  end

  defp decode_entry(%{version: @disk_version, shards: shards, traced_files: traced_files})
       when is_map(shards) and is_list(traced_files) do
    entry = %{shards: shards, traced_files: MapSet.new(traced_files)}

    if valid_entry?(entry), do: {:ok, entry}, else: :error
  end

  defp decode_entry(_cache), do: :error

  defp entry_from_events(events) do
    grouped_events = Enum.group_by(events, & &1.file)

    %{
      shards: encode_shards(grouped_events),
      traced_files: update_traced_files(MapSet.new(), MapSet.new(), grouped_events)
    }
  end

  defp empty_entry, do: %{shards: %{}, traced_files: MapSet.new()}

  defp encode_shards(grouped_events) do
    Map.new(grouped_events, fn {file, events} ->
      {file, :erlang.term_to_binary(events, compressed: 1)}
    end)
  end

  defp decode_all(entry) do
    entry.shards
    |> Enum.sort_by(&elem(&1, 0))
    |> Enum.flat_map(fn {_file, encoded} -> :erlang.binary_to_term(encoded, [:safe]) end)
  end

  defp update_traced_files(traced_files, invalidated, grouped_events) do
    grouped_events
    |> Enum.reduce(MapSet.difference(traced_files, invalidated), fn {file, events}, traced ->
      if Enum.any?(events, &(&1.kind == :source_start)) do
        MapSet.put(traced, file)
      else
        traced
      end
    end)
  end

  defp valid_entry?(entry) do
    Enum.all?(entry.traced_files, &is_binary/1) and
      Enum.all?(entry.shards, fn
        {file, encoded} when is_binary(file) and is_binary(encoded) ->
          case safe_decode(encoded) do
            {:ok, events} -> valid_events?(events) and Enum.all?(events, &(&1.file == file))
            :error -> false
          end

        _invalid_shard ->
          false
      end)
  end

  defp valid_events?(events) do
    is_list(events) and Enum.all?(events, &(is_map(&1) and is_binary(&1[:file])))
  end

  defp safe_decode(encoded) do
    {:ok, :erlang.binary_to_term(encoded, [:safe])}
  rescue
    _invalid_cache -> :error
  end

  defp write_entry(path, entry) do
    temporary = "#{path}.#{System.unique_integer([:positive])}.tmp"

    encoded =
      :erlang.term_to_binary(%{
        version: @disk_version,
        shards: entry.shards,
        traced_files: MapSet.to_list(entry.traced_files)
      })

    try do
      with :ok <- File.write(temporary, encoded),
           :ok <- File.rename(temporary, path) do
        :ok
      end
    after
      File.rm(temporary)
    end
  end
end
