defmodule Beholder.Worker.Elixir.Compiler do
  @moduledoc false

  alias Beholder.Worker.Elixir.Compiler.BeamExporter
  alias Beholder.Worker.Elixir.Snapshot.Repository

  @type result :: %{
          status: :ok | :error,
          diagnostics: [map()],
          events: [map()],
          elixir_version: String.t(),
          otp_release: String.t(),
          output: String.t()
        }

  @spec run(Repository.t(), String.t()) :: {:ok, result()} | {:error, String.t()}
  def run(repository, cache_dir) do
    with :ok <- verify_inputs(repository),
         {:ok, mix} <- find_mix() do
      helper_ebin = BeamExporter.export!(cache_dir)
      working_dir = Path.join([cache_dir, "elixir", safe_component(repository.identity)])
      build_path = Path.join(working_dir, "build-#{safe_component(repository.fingerprint)}")
      result_path = Path.join(working_dir, "trace-#{System.unique_integer([:positive])}.term")
      mix_env = System.get_env("BEHOLDER_ELIXIR_MIX_ENV", "dev")

      env = [
        {"BEHOLDER_ELIXIR_TRACE_RESULT", result_path},
        {"MIX_BUILD_PATH", build_path},
        {"MIX_ENV", mix_env},
        {"ERL_AFLAGS", append_code_path(System.get_env("ERL_AFLAGS"), helper_ebin)}
      ]

      {output, exit_status} =
        System.cmd(mix, ["beholder.compile"],
          cd: repository.base,
          env: env,
          stderr_to_stdout: true
        )

      read_result(result_path, output, exit_status)
    end
  end

  @spec verify_inputs(Repository.t()) :: :ok | {:error, String.t()}
  def verify_inputs(repository) do
    Enum.reduce_while(Repository.source_inputs(repository), :ok, fn input, :ok ->
      path = Path.expand(input.path, repository.base)

      cond do
        not within?(path, repository.base) ->
          {:halt, {:error, "snapshot input escapes repository: #{input.path}"}}

        File.read(path) == {:ok, input.content} ->
          {:cont, :ok}

        true ->
          {:halt, {:error, "#{input.path} changed after the immutable snapshot was created"}}
      end
    end)
  end

  defp find_mix do
    case System.find_executable("mix") do
      nil -> {:error, "mix executable not found for Elixir compiler worker"}
      mix -> {:ok, mix}
    end
  end

  defp read_result(result_path, output, exit_status) do
    case File.read(result_path) do
      {:ok, encoded} ->
        result = encoded |> :erlang.binary_to_term([:safe]) |> normalize_result()
        File.rm(result_path)
        {:ok, Map.put(result, :output, output)}

      {:error, _reason} ->
        {:error,
         "Mix compiler process exited with status #{exit_status} before producing a trace: #{String.trim(output)}"}
    end
  rescue
    error -> {:error, "invalid compiler trace result: #{Exception.message(error)}"}
  end

  defp append_code_path(nil, path), do: "-pa #{path}"
  defp append_code_path("", path), do: "-pa #{path}"
  defp append_code_path(flags, path), do: flags <> " -pa #{path}"

  defp safe_component(value) do
    Base.url_encode64(value, padding: false)
  end

  defp normalize_result(result) do
    %{
      status: if(result["status"] == "ok", do: :ok, else: :error),
      diagnostics: Enum.map(result["diagnostics"], &normalize_diagnostic/1),
      events: Enum.map(result["events"], &normalize_event/1),
      elixir_version: result["elixir_version"],
      otp_release: result["otp_release"]
    }
  end

  defp normalize_event(event) do
    event = atomize_keys(event)
    event = Map.update!(event, :kind, &event_kind/1)

    event =
      case event.caller_function do
        [name, arity] -> %{event | caller_function: {name, arity}}
        nil -> event
      end

    case event do
      %{definitions: definitions} ->
        %{event | definitions: Enum.map(definitions, fn [name, arity] -> {name, arity} end)}

      _event ->
        event
    end
  end

  defp normalize_diagnostic(diagnostic) do
    diagnostic = atomize_keys(diagnostic)
    Map.update!(diagnostic, :position, &normalize_position/1)
  end

  defp normalize_position([line, column]), do: {line, column}
  defp normalize_position(%{"line" => line, "column" => column}), do: {line, column}
  defp normalize_position(position), do: position

  defp atomize_keys(map) do
    Map.new(map, fn {key, value} -> {known_key(key), value} end)
  end

  defp known_key("arity"), do: :arity
  defp known_key("as"), do: :as
  defp known_key("caller_function"), do: :caller_function
  defp known_key("caller_module"), do: :caller_module
  defp known_key("column"), do: :column
  defp known_key("definitions"), do: :definitions
  defp known_key("file"), do: :file
  defp known_key("from_macro"), do: :from_macro
  defp known_key("keys"), do: :keys
  defp known_key("kind"), do: :kind
  defp known_key("line"), do: :line
  defp known_key("message"), do: :message
  defp known_key("name"), do: :name
  defp known_key("position"), do: :position
  defp known_key("severity"), do: :severity
  defp known_key("target"), do: :target

  defp event_kind("alias"), do: :alias
  defp event_kind("alias_expansion"), do: :alias_expansion
  defp event_kind("alias_reference"), do: :alias_reference
  defp event_kind("import"), do: :import
  defp event_kind("imported_function"), do: :imported_function
  defp event_kind("imported_macro"), do: :imported_macro
  defp event_kind("local_function"), do: :local_function
  defp event_kind("local_macro"), do: :local_macro
  defp event_kind("module"), do: :module
  defp event_kind("remote_function"), do: :remote_function
  defp event_kind("remote_macro"), do: :remote_macro
  defp event_kind("require"), do: :require
  defp event_kind("source_start"), do: :source_start
  defp event_kind("source_stop"), do: :source_stop
  defp event_kind("struct_expansion"), do: :struct_expansion

  defp within?(path, base) do
    relative = Path.relative_to(path, Path.expand(base))
    relative != ".." and not String.starts_with?(relative, "../") and relative != path
  end
end
