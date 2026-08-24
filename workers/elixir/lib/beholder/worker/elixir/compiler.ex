defmodule Beholder.Worker.Elixir.Compiler do
  @moduledoc false

  alias Beholder.Worker.Elixir.Compiler.BeamExporter
  alias Beholder.Worker.Elixir.Snapshot.Repository

  @default_timeout_ms 900_000
  @default_max_output_bytes 1_048_576
  @termination_grace_ms 1_000
  @process_group_pid_retries 100
  @process_group_pid_retry_ms 10

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
    run(repository, [], cache_dir)
  end

  @spec run(Repository.t(), [Repository.t()], String.t()) ::
          {:ok, result()} | {:error, String.t()}
  def run(repository, contexts, cache_dir) do
    original_repositories = [repository | contexts]

    with {:ok, project_root} <- Repository.mix_project_root(repository),
         {:ok, materialized_repositories, materialization_root} <-
           materialize(original_repositories) do
      [repository | contexts] = materialized_repositories

      try do
        case run_materialized(repository, contexts, cache_dir, project_root) do
          {:ok, result} ->
            {:ok, remap_result_paths(result, materialized_repositories, original_repositories)}

          {:error, _reason} = error ->
            error
        end
      after
        File.rm_rf(materialization_root)
      end
    end
  end

  defp run_materialized(repository, contexts, cache_dir, project_root) do
    repositories = [repository | contexts]

    with :ok <- validate_local_paths(repositories),
         {:ok, mix} <- find_mix() do
      helper_ebin = BeamExporter.export!(cache_dir)
      working_dir = Path.join([cache_dir, "elixir", safe_component(repository.identity)])
      mix_env = configured_mix_env()
      build_path = Path.join(working_dir, "build-#{build_identity(repositories, mix, mix_env)}")
      result_path = Path.join(working_dir, "trace-#{System.unique_integer([:positive])}.term")
      deps_path = Path.join(working_dir, "deps")
      File.mkdir_p!(deps_path)

      env = [
        {"BEHOLDER_ELIXIR_TRACE_RESULT", result_path},
        {"MIX_BUILD_PATH", build_path},
        {"MIX_DEPS_PATH", deps_path},
        {"MIX_ENV", mix_env},
        {"ERL_AFLAGS", append_code_path(System.get_env("ERL_AFLAGS"), helper_ebin)}
      ]

      result =
        run_command(
          mix,
          ["beholder.compile"],
          Path.join(repository.base, project_root),
          env,
          configured_positive_integer(
            "BEHOLDER_ELIXIR_COMPILER_TIMEOUT_MS",
            @default_timeout_ms
          ),
          configured_positive_integer(
            "BEHOLDER_ELIXIR_MAX_OUTPUT_BYTES",
            @default_max_output_bytes
          )
        )

      finalize_run(repositories, result_path, result)
    end
  end

  @spec verify_inputs(Repository.t()) :: :ok | {:error, String.t()}
  def verify_inputs(repository) do
    Enum.reduce_while(Repository.sorted_inputs(repository), :ok, fn input, :ok ->
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

  defp materialize(repositories) do
    root =
      Path.join(
        physical_tmp_dir(),
        "beholder-elixir-snapshot-#{System.pid()}-#{System.unique_integer([:positive])}"
      )

    with :ok <- File.mkdir(root),
         {:ok, common} <- common_repository_root(repositories),
         {:ok, materialized} <- materialize_repositories(repositories, common, root) do
      {:ok, materialized, root}
    else
      {:error, reason} ->
        File.rm_rf(root)
        {:error, format_materialization_error(reason)}
    end
  end

  defp common_repository_root([]), do: {:error, "snapshot contains no repositories"}

  defp common_repository_root(repositories) do
    components = Enum.map(repositories, &(Path.expand(&1.base) |> Path.split()))
    common = Enum.reduce(components, hd(components), &common_prefix/2)

    if common == [] do
      {:error, "snapshot repositories have no common filesystem root"}
    else
      {:ok, common}
    end
  end

  defp physical_tmp_dir do
    {path, 0} = System.cmd("pwd", ["-P"], cd: System.tmp_dir!())
    String.trim(path)
  end

  defp common_prefix([head | left], [head | right]), do: [head | common_prefix(left, right)]
  defp common_prefix(_left, _right), do: []

  defp materialize_repositories(repositories, common, root) do
    Enum.reduce_while(repositories, {:ok, [], MapSet.new(), MapSet.new()}, fn repository,
                                                                              {:ok, result,
                                                                               identities, bases} ->
      relative = repository.base |> Path.expand() |> Path.split() |> Enum.drop(length(common))
      base = Path.join([root | relative])

      cond do
        MapSet.member?(identities, repository.identity) ->
          {:halt, {:error, "snapshot contains duplicate repository #{repository.identity}"}}

        MapSet.member?(bases, base) ->
          {:halt, {:error, "snapshot maps repositories to the same directory"}}

        true ->
          case materialize_inputs(repository, base) do
            :ok ->
              {:cont,
               {:ok, [%{repository | base: base} | result],
                MapSet.put(identities, repository.identity), MapSet.put(bases, base)}}

            {:error, _reason} = error ->
              {:halt, error}
          end
      end
    end)
    |> case do
      {:ok, repositories, _identities, _bases} -> {:ok, Enum.reverse(repositories)}
      {:error, _reason} = error -> error
    end
  end

  defp materialize_inputs(repository, base) do
    Enum.reduce_while(Repository.sorted_inputs(repository), :ok, fn input, :ok ->
      destination = Path.expand(input.path, base)

      if Path.type(input.path) != :relative or not within?(destination, base) do
        {:halt, {:error, "snapshot input escapes repository: #{input.path}"}}
      else
        with :ok <- File.mkdir_p(Path.dirname(destination)),
             :ok <- File.write(destination, input.content) do
          {:cont, :ok}
        else
          {:error, reason} -> {:halt, {:error, reason}}
        end
      end
    end)
  end

  defp format_materialization_error(reason) when is_binary(reason), do: reason

  defp format_materialization_error(reason),
    do: "failed to materialize snapshot: #{inspect(reason)}"

  defp remap_result_paths(result, materialized_repositories, original_repositories) do
    bases =
      Enum.zip(materialized_repositories, original_repositories)
      |> Enum.map(fn {materialized, original} ->
        {Path.expand(materialized.base), Path.expand(original.base)}
      end)
      |> Enum.sort_by(fn {materialized, _original} -> -String.length(materialized) end)

    result
    |> Map.update(:events, [], &Enum.map(&1, fn event -> remap_file(event, bases) end))
    |> Map.update(:diagnostics, [], fn diagnostics ->
      Enum.map(diagnostics, &remap_file(&1, bases))
    end)
  end

  defp remap_file(%{file: file} = value, bases) when is_binary(file) do
    Map.put(value, :file, remap_path(file, bases))
  end

  defp remap_file(value, _bases), do: value

  defp remap_path(path, bases) do
    expanded = Path.expand(path)

    Enum.find_value(bases, path, fn {materialized, original} ->
      if within?(expanded, materialized) do
        Path.join(original, Path.relative_to(expanded, materialized))
      end
    end)
  end

  defp validate_local_paths(repositories) do
    repositories
    |> Enum.flat_map(&Repository.sorted_inputs/1)
    |> Enum.filter(&configuration_input?/1)
    |> Enum.reduce_while(:ok, fn input, :ok ->
      case absolute_local_path(input.content) do
        nil -> {:cont, :ok}
        path -> {:halt, {:error, "#{input.path} declares absolute local path #{path}"}}
      end
    end)
  end

  defp configuration_input?(%{path: path}) do
    Path.basename(path) == "mix.exs" or "config" in Path.split(path)
  end

  defp configuration_input?(_input), do: false

  defp absolute_local_path(source) do
    with {:ok, quoted} <- Code.string_to_quoted(source) do
      {_quoted, path} =
        Macro.prewalk(quoted, nil, fn
          node, path when not is_nil(path) ->
            {node, path}

          {key, value} = node, nil
          when key in [:path, :apps_path] and is_binary(value) ->
            {node, absolute_path(value)}

          {:import_config, _metadata, [value]} = node, nil when is_binary(value) ->
            {node, absolute_path(value)}

          node, nil ->
            {node, nil}
        end)

      path
    else
      _invalid_source -> nil
    end
  end

  defp absolute_path(value), do: if(Path.type(value) == :absolute, do: value, else: nil)

  defp verify_repositories(repositories) do
    Enum.reduce_while(repositories, :ok, fn repository, :ok ->
      case verify_inputs(repository) do
        :ok -> {:cont, :ok}
        {:error, _reason} = error -> {:halt, error}
      end
    end)
  end

  defp find_mix do
    case System.get_env("BEHOLDER_ELIXIR_MIX_PATH") || System.find_executable("mix") do
      nil ->
        {:error, "mix executable not found for Elixir compiler worker"}

      mix ->
        if File.regular?(mix),
          do: {:ok, Path.expand(mix)},
          else: {:error, "configured Mix executable does not exist: #{mix}"}
    end
  end

  defp finalize_run(repositories, result_path, {:ok, output, exit_status}) do
    case verify_repositories(repositories) do
      :ok ->
        read_result(result_path, output, exit_status)

      {:error, _reason} = error ->
        File.rm(result_path)
        error
    end
  end

  defp finalize_run(_repositories, result_path, {:error, :timeout, output, timeout_ms}) do
    File.rm(result_path)

    {:error,
     "Mix compiler process exceeded #{timeout_ms}ms and was terminated: #{String.trim(output)}"}
  end

  defp finalize_run(_repositories, result_path, {:error, reason}) do
    File.rm(result_path)
    {:error, reason}
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

  defp build_identity(repositories, mix, mix_env) do
    repositories
    |> Enum.sort_by(& &1.identity)
    |> Enum.flat_map(&[&1.identity, &1.fingerprint])
    |> Kernel.++([
      mix_env,
      mix,
      mix_version(),
      System.version(),
      :erlang.system_info(:otp_release),
      System.get_env("ELIXIR_ERL_OPTIONS", ""),
      System.get_env("ERL_AFLAGS", ""),
      System.get_env("ERL_COMPILER_OPTIONS", "")
    ])
    |> Enum.join(<<0>>)
    |> then(&:crypto.hash(:sha256, &1))
    |> Base.url_encode64(padding: false)
  end

  defp run_command(mix, arguments, directory, env, timeout_ms, max_output_bytes) do
    {executable, arguments, group_file} = isolated_command(mix, arguments)

    port =
      Port.open(
        {:spawn_executable, executable},
        [
          :binary,
          :exit_status,
          :stderr_to_stdout,
          {:args, arguments},
          {:cd, directory},
          {:env,
           Enum.map(env, fn {name, value} ->
             {String.to_charlist(name), String.to_charlist(value)}
           end)}
        ]
      )

    deadline = System.monotonic_time(:millisecond) + timeout_ms
    target = process_target(port, group_file)
    collect_output(port, deadline, timeout_ms, max_output_bytes, {<<>>, false}, target)
  rescue
    error in [ArgumentError, ErlangError] ->
      {:error, "failed to start Mix compiler process: #{Exception.message(error)}"}
  end

  defp isolated_command(mix, arguments) do
    with setsid when not is_nil(setsid) <- System.find_executable("setsid"),
         shell when not is_nil(shell) <- System.find_executable("sh") do
      group_file =
        Path.join(
          System.tmp_dir!(),
          "beholder-elixir-process-group-#{System.pid()}-#{System.unique_integer([:positive])}.pid"
        )

      # setsid may fork when its caller is already a process-group leader, so
      # the Port PID is not always the group that owns Mix and its children.
      script = ~S(group_file=$1; shift; printf '%s' "$$" > "$group_file"; exec "$@")

      {setsid,
       ["--wait", shell, "-c", script, "beholder-process-group", group_file, mix | arguments],
       group_file}
    else
      _missing_executable -> {mix, arguments, nil}
    end
  end

  defp process_target(port, nil) do
    case Port.info(port, :os_pid) do
      {:os_pid, pid} -> {:process, pid}
      nil -> nil
    end
  end

  defp process_target(port, group_file) do
    fallback = process_target(port, nil)
    {:group_file, group_file, fallback}
  end

  defp collect_output(port, deadline, timeout_ms, max_output_bytes, output, target) do
    remaining = max(deadline - System.monotonic_time(:millisecond), 0)

    receive do
      {^port, {:data, data}} ->
        collect_output(
          port,
          deadline,
          timeout_ms,
          max_output_bytes,
          append_output(output, data, max_output_bytes),
          target
        )

      {^port, {:exit_status, status}} ->
        cleanup_target(target)
        {:ok, render_output(output), status}
    after
      remaining ->
        terminate(port, target)
        {:error, :timeout, render_output(output), timeout_ms}
    end
  end

  defp terminate(port, unresolved_target) do
    target = resolve_target(unresolved_target)

    case target do
      nil ->
        :ok

      target ->
        signal(target, "TERM")

        receive do
          {^port, {:exit_status, _status}} -> :ok
        after
          @termination_grace_ms -> :ok
        end

        # The direct process can exit before all of its descendants. Signal the
        # group again after it exits or the grace period expires so no compiler
        # children survive a timed-out enrichment.
        signal(target, "KILL")

        receive do
          {^port, {:exit_status, _status}} -> :ok
        after
          @termination_grace_ms -> :ok
        end
    end

    cleanup_target(unresolved_target)
    if Port.info(port), do: Port.close(port)
  rescue
    ArgumentError -> :ok
  end

  defp resolve_target({:group_file, group_file, fallback}) do
    case read_process_group_pid(group_file, @process_group_pid_retries) do
      {:ok, pid} -> {:group, pid}
      :error -> fallback
    end
  end

  defp resolve_target(target), do: target

  defp read_process_group_pid(_group_file, 0), do: :error

  defp read_process_group_pid(group_file, retries) do
    result =
      with {:ok, contents} <- File.read(group_file),
           {pid, ""} when pid > 0 <- Integer.parse(String.trim(contents)) do
        {:ok, pid}
      else
        _unavailable_or_invalid -> :error
      end

    case result do
      {:ok, _pid} = result ->
        result

      :error ->
        Process.sleep(@process_group_pid_retry_ms)
        read_process_group_pid(group_file, retries - 1)
    end
  end

  defp cleanup_target({:group_file, group_file, _fallback}), do: File.rm(group_file)
  defp cleanup_target(_target), do: :ok

  defp signal({kind, pid}, signal) do
    case System.find_executable("kill") do
      nil ->
        :ok

      kill ->
        target = if kind == :group, do: "-#{pid}", else: Integer.to_string(pid)
        System.cmd(kill, ["-#{signal}", "--", target], stderr_to_stdout: true)
        :ok
    end
  rescue
    _error -> :ok
  end

  defp append_output({output, truncated?}, data, max_output_bytes) do
    remaining = max(max_output_bytes - byte_size(output), 0)

    cond do
      remaining == 0 -> {output, true}
      byte_size(data) <= remaining -> {output <> data, truncated?}
      true -> {output <> binary_part(data, 0, remaining), true}
    end
  end

  defp render_output({output, false}), do: output
  defp render_output({output, true}), do: output <> "\n[compiler output truncated]"

  defp configured_positive_integer(name, default) do
    case Integer.parse(System.get_env(name, "")) do
      {value, ""} when value > 0 -> value
      _other -> default
    end
  end

  defp configured_mix_env do
    case System.get_env("BEHOLDER_ELIXIR_MIX_ENV", "") |> String.trim() do
      "" -> "dev"
      value -> value
    end
  end

  defp mix_version do
    case Application.spec(:mix, :vsn) do
      nil -> "unavailable"
      version -> to_string(version)
    end
  end

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
