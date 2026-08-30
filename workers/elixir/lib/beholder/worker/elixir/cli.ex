defmodule Beholder.Worker.Elixir.CLI do
  @moduledoc false
  @max_message_bytes 64 * 1024 * 1024

  @spec main([String.t()]) :: no_return()
  def main(arguments) do
    with {:ok, options} <- parse(arguments),
         :ok <- prepare_socket(options.socket),
         :ok <- Beholder.Worker.Elixir.Observability.start(),
         {:ok, _} <- Application.ensure_all_started(:grpc_server),
         :ok <- configure(options.cache_dir),
         {:ok, _pid, _port} <- start_server(options.socket),
         :ok <- restrict_socket(options.socket) do
      Process.sleep(:infinity)
    else
      {:error, reason} ->
        IO.puts(:stderr, "beholder-worker-elixir: #{format_error(reason)}")
        System.halt(1)
    end
  end

  defp parse(arguments) do
    case OptionParser.parse(arguments, strict: [socket: :string, cache_dir: :string]) do
      {[socket: socket, cache_dir: cache_dir], [], []} ->
        {:ok, %{socket: Path.expand(socket), cache_dir: Path.expand(cache_dir)}}

      {_options, _remaining, invalid} when invalid != [] ->
        {:error, "invalid arguments: #{inspect(invalid)}"}

      _ ->
        {:error, "expected --socket PATH --cache-dir PATH"}
    end
  end

  defp configure(cache_dir) do
    File.mkdir_p!(cache_dir)
    Beholder.Worker.Elixir.Compiler.TraceCache.start_cache()
    Beholder.Worker.Elixir.EventMapper.start_cache()
    Application.put_env(:beholder_worker_elixir, :cache_dir, cache_dir)
    :ok
  end

  defp prepare_socket(socket) do
    File.mkdir_p!(Path.dirname(socket))

    case File.rm(socket) do
      :ok -> :ok
      {:error, :enoent} -> :ok
      {:error, reason} -> {:error, reason}
    end
  end

  defp start_server(socket) do
    GRPC.Server.start_endpoint(Beholder.Worker.Elixir.Endpoint, 0,
      max_body_size: @max_message_bytes,
      adapter_opts: [ip: {:local, socket}, num_acceptors: 1, max_connections: 1]
    )
  end

  defp restrict_socket(socket) do
    case File.chmod(socket, 0o600) do
      :ok -> :ok
      {:error, reason} -> {:error, reason}
    end
  end

  defp format_error(reason) when is_binary(reason), do: reason
  defp format_error(reason) when is_atom(reason), do: :file.format_error(reason)
  defp format_error(reason), do: inspect(reason)
end
