defmodule Mix.Tasks.Beholder.Compile do
  @moduledoc false
  use Mix.Task

  alias Beholder.Worker.Elixir.Compiler.{Collector, Tracer}

  @impl true
  def run(_arguments) do
    helper_path = __MODULE__ |> :code.which() |> to_string() |> Path.dirname()
    Code.prepend_path(helper_path)
    Code.ensure_loaded!(Collector)
    Code.ensure_loaded!(Tracer)

    result_path = System.fetch_env!("BEHOLDER_ELIXIR_TRACE_RESULT")

    {status, diagnostics, events} =
      case prepare_dependencies() do
        :ok ->
          IO.puts("BEHOLDER_PROGRESS project_compilation")
          trace_compile()

        {:error, diagnostics} ->
          {:error, diagnostics, []}
      end

    result =
      stringify(%{
        status: status,
        diagnostics: normalize_diagnostics(diagnostics),
        events: events,
        elixir_version: System.version(),
        otp_release: :erlang.system_info(:otp_release) |> to_string()
      })

    result_path |> Path.dirname() |> File.mkdir_p!()
    File.write!(result_path, :erlang.term_to_binary(result, compressed: 6))
  end

  defp prepare_dependencies do
    try do
      IO.puts("BEHOLDER_PROGRESS dependency_preparation")
      Mix.Task.run("deps.loadpaths")
      :ok
    rescue
      exception -> {:error, [%{message: Exception.format(:error, exception, __STACKTRACE__)}]}
    catch
      kind, reason -> {:error, [%{message: Exception.format(kind, reason, __STACKTRACE__)}]}
    end
  end

  defp trace_compile do
    {:ok, _collector} = Collector.start_link()
    previous_tracers = Code.get_compiler_option(:tracers)
    Code.put_compiler_option(:tracers, previous_tracers ++ [Tracer])

    try do
      {status, diagnostics} = compile()
      {status, diagnostics, Collector.drain()}
    after
      Code.put_compiler_option(:tracers, previous_tracers)
    end
  end

  defp compile do
    try do
      case Mix.Task.run("compile", ["--return-errors"]) do
        {:ok, diagnostics} -> {:ok, diagnostics}
        {:error, diagnostics} -> {:error, diagnostics}
        :ok -> {:ok, []}
        other -> {:error, [%{message: "unexpected compile result: #{inspect(other)}"}]}
      end
    rescue
      exception ->
        {:error, [%{message: Exception.format(:error, exception, __STACKTRACE__)}]}
    catch
      kind, reason ->
        {:error, [%{message: Exception.format(kind, reason, __STACKTRACE__)}]}
    end
  end

  defp normalize_diagnostics(diagnostics) do
    Enum.map(List.wrap(diagnostics), fn
      %{message: message} = diagnostic ->
        %{
          message: to_string(message),
          severity: diagnostic |> Map.get(:severity, :error) |> to_string(),
          file: normalize_file(Map.get(diagnostic, :file)),
          position: Map.get(diagnostic, :position)
        }

      diagnostic ->
        %{message: inspect(diagnostic), severity: :error, file: nil, position: nil}
    end)
  end

  defp normalize_file(nil), do: nil
  defp normalize_file(file), do: to_string(file)

  defp stringify(nil), do: nil
  defp stringify(value) when is_boolean(value), do: value
  defp stringify(value) when is_atom(value), do: Atom.to_string(value)

  defp stringify(value) when is_tuple(value),
    do: value |> Tuple.to_list() |> Enum.map(&stringify/1)

  defp stringify(value) when is_list(value), do: Enum.map(value, &stringify/1)

  defp stringify(value) when is_map(value) do
    Map.new(value, fn {key, item} -> {stringify(key), stringify(item)} end)
  end

  defp stringify(value), do: value
end
