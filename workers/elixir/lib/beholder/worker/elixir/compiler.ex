defmodule Beholder.Worker.Elixir.Compiler do
  @moduledoc false

  alias Beholder.Worker.Elixir.Compiler.BeamExporter
  alias Beholder.Worker.Elixir.Compiler.ProjectMetadata
  alias Beholder.Worker.Elixir.Compiler.TraceCache
  alias Beholder.Worker.Elixir.Snapshot.Repository

  @default_timeout_ms 900_000
  @default_max_output_bytes 1_048_576
  @termination_grace_ms 1_000
  @process_group_pid_retries 100
  @process_group_pid_retry_ms 10
  @materialization_manifest_version 1
  @dependency_progress "BEHOLDER_PROGRESS dependency_preparation"
  @compilation_progress "BEHOLDER_PROGRESS project_compilation"
  @project_metadata_prefix "BEHOLDER_PROJECT_METADATA "
  @local_paths_prefix "BEHOLDER_LOCAL_PATHS "
  @project_metadata_path Path.expand("compiler/project_metadata.ex", __DIR__)
  @external_resource @project_metadata_path
  @project_metadata_script File.read!(@project_metadata_path) <>
                             "\nBeholder.Worker.Elixir.Compiler.ProjectMetadata.emit(System.argv())"

  @type result :: %{
          status: :ok | :error,
          diagnostics: [map()],
          events: [map()],
          events_complete?: boolean(),
          changed_events: [map()],
          changed_files: [String.t()],
          traced_files: [String.t()],
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
    run(repository, contexts, cache_dir, fn _detail -> :ok end)
  end

  @spec run(Repository.t(), [Repository.t()], String.t(), (String.t() -> any())) ::
          {:ok, result()} | {:error, String.t()}
  def run(repository, contexts, cache_dir, on_progress) do
    original_repositories = [repository | contexts]
    working_dir = Path.join([cache_dir, "elixir", safe_component(repository.identity)])
    materialization_root = Path.join(working_dir, "snapshot")
    materialization_manifest = Path.join(working_dir, "snapshot-inputs.term")

    with {:ok, project_root} <- Repository.mix_project_root(repository),
         {:ok, materialized_repositories, changed_inputs} <-
           materialize(
             original_repositories,
             materialization_root,
             materialization_manifest
           ) do
      [repository | contexts] = materialized_repositories

      case run_materialized(
             repository,
             contexts,
             changed_inputs,
             cache_dir,
             project_root,
             on_progress
           ) do
        {:ok, result} ->
          {:ok, remap_result_paths(result, materialized_repositories, original_repositories)}

        {:error, _reason} = error ->
          error
      end
    end
  end

  defp run_materialized(
         repository,
         contexts,
         changed_inputs,
         cache_dir,
         project_root,
         on_progress
       ) do
    repositories = [repository | contexts]

    directory = Path.join(repository.base, project_root)

    with {:ok, command} <- find_mix_for_metadata(repository, project_root),
         :ok <- validate_local_paths(repositories, command, cache_dir, directory),
         {:ok, requirement, otp} <-
           project_metadata(repository, project_root, command, cache_dir),
         command = Map.put(command, :required, requirement),
         {:ok, runtime} <- probe_mix(command, Path.join(repository.base, project_root)),
         runtime = Map.put(runtime, :otp, otp),
         :ok <- validate_requirement(requirement, runtime, command),
         working_dir = Path.join([cache_dir, "elixir", safe_component(repository.identity)]),
         mix_env = configured_mix_env(),
         identity = build_identity(repositories, command, runtime, mix_env),
         {:ok, helper_ebin} <- compile_helpers(command, cache_dir, identity, directory) do
      build_path = Path.join(working_dir, "build-#{identity}")
      trace_cache_path = Path.join(working_dir, "trace-cache-#{identity}.term")
      trace_cache_status = TraceCache.load(trace_cache_path)
      result_path = Path.join(working_dir, "trace-#{System.unique_integer([:positive])}.term")
      deps_path = Path.join(working_dir, "deps")
      File.mkdir_p!(deps_path)

      env =
        [
          {"BEHOLDER_ELIXIR_TRACE_RESULT", result_path},
          {"MIX_BUILD_PATH", build_path},
          {"MIX_DEPS_PATH", deps_path},
          {"MIX_ENV", mix_env},
          {"BEHOLDER_ELIXIR_FORCE_COMPILE",
           if(trace_cache_status == :miss, do: "true", else: "false")},
          {"ERL_AFLAGS", append_code_path(System.get_env("ERL_AFLAGS"), helper_ebin)}
        ] ++ command_env(command)

      result =
        run_command(
          compiler_command(command, env),
          ["beholder.compile"],
          directory,
          env,
          configured_positive_integer(
            "BEHOLDER_WORKER_TIMEOUT_MS",
            @default_timeout_ms
          ),
          configured_positive_integer(
            "BEHOLDER_WORKER_MAX_OUTPUT_BYTES",
            @default_max_output_bytes
          ),
          on_progress
        )

      on_progress.("validating compiler result")

      with {:ok, result} <- finalize_run(repositories, result_path, result) do
        on_progress.("merging compiler trace cache")
        {:ok, merge_trace_cache(result, changed_inputs, trace_cache_path, trace_cache_status)}
      end
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

  defp materialize(repositories, root, manifest_path) do
    with :ok <- File.mkdir_p(root),
         {:ok, root} <- physical_path(root),
         {:ok, common} <- common_repository_root(repositories),
         {:ok, materialized} <- map_materialized_repositories(repositories, common, root),
         previous = read_materialization_manifest(manifest_path),
         identities = materialization_identities(materialized),
         {:ok, removed} <- remove_stale_inputs(root, identities, previous),
         {:ok, changed} <- materialize_inputs(materialized, previous),
         :ok <- write_materialization_manifest(manifest_path, identities) do
      {:ok, materialized, MapSet.union(removed, changed)}
    else
      {:error, reason} -> {:error, format_materialization_error(reason)}
    end
  end

  defp physical_path(path) do
    case System.cmd("pwd", ["-P"], cd: path) do
      {physical, 0} -> {:ok, String.trim(physical)}
      {output, status} -> {:error, "failed to resolve snapshot path (#{status}): #{output}"}
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

  defp common_prefix([head | left], [head | right]), do: [head | common_prefix(left, right)]
  defp common_prefix(_left, _right), do: []

  defp map_materialized_repositories(repositories, common, root) do
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
          {:cont,
           {:ok, [%{repository | base: base} | result],
            MapSet.put(identities, repository.identity), MapSet.put(bases, base)}}
      end
    end)
    |> case do
      {:ok, repositories, _identities, _bases} -> {:ok, Enum.reverse(repositories)}
      {:error, _reason} = error -> error
    end
  end

  defp materialize_inputs(repositories, previous) do
    Enum.reduce_while(repositories, {:ok, MapSet.new()}, fn repository, {:ok, changed} ->
      case materialize_repository_inputs(repository, previous) do
        {:ok, repository_changed} ->
          {:cont, {:ok, MapSet.union(changed, repository_changed)}}

        {:error, _reason} = error ->
          {:halt, error}
      end
    end)
  end

  defp materialize_repository_inputs(repository, previous) do
    Enum.reduce_while(Repository.sorted_inputs(repository), {:ok, MapSet.new()}, fn input,
                                                                                    {:ok, changed} ->
      destination = Path.expand(input.path, repository.base)
      identity = input_identity(input)

      if Path.type(input.path) != :relative or not within?(destination, repository.base) do
        {:halt, {:error, "snapshot input escapes repository: #{input.path}"}}
      else
        with {:ok, changed?} <-
               write_if_changed(destination, input.content, identity, previous) do
          changed = if changed?, do: MapSet.put(changed, destination), else: changed
          {:cont, {:ok, changed}}
        else
          {:error, reason} -> {:halt, {:error, reason}}
        end
      end
    end)
  end

  defp write_if_changed(path, content, identity, previous) when is_map(previous) do
    if Map.get(previous, path) == identity do
      {:ok, false}
    else
      write_input(path, content)
    end
  end

  defp write_if_changed(path, content, _identity, nil) do
    if File.read(path) == {:ok, content},
      do: {:ok, false},
      else: write_input(path, content)
  end

  defp write_input(path, content) do
    with :ok <- File.mkdir_p(Path.dirname(path)),
         :ok <- File.write(path, content) do
      {:ok, true}
    end
  end

  defp remove_stale_inputs(root, identities, previous) do
    expected = Map.keys(identities) |> MapSet.new()

    existing =
      if is_map(previous),
        do: Map.keys(previous),
        else: files_under(root)

    with {:ok, removed} <-
           existing
           |> Enum.reject(&MapSet.member?(expected, &1))
           |> Enum.reduce_while({:ok, MapSet.new()}, fn path, {:ok, removed} ->
             case File.rm(path) do
               :ok -> {:cont, {:ok, MapSet.put(removed, path)}}
               {:error, reason} -> {:halt, {:error, reason}}
             end
           end) do
      if MapSet.size(removed) > 0, do: remove_empty_directories(root)
      {:ok, removed}
    end
  end

  defp materialization_identities(repositories) do
    repositories
    |> Enum.flat_map(fn repository ->
      Enum.map(Repository.sorted_inputs(repository), fn input ->
        {Path.expand(input.path, repository.base), input_identity(input)}
      end)
    end)
    |> Map.new()
  end

  defp input_identity(input) do
    content_hash =
      Map.get_lazy(input, :content_hash, fn -> :crypto.hash(:sha256, input.content) end)

    {input.kind, content_hash}
  end

  defp read_materialization_manifest(path) do
    with {:ok, encoded} <- File.read(path),
         %{version: @materialization_manifest_version, inputs: inputs} when is_map(inputs) <-
           :erlang.binary_to_term(encoded, [:safe]),
         true <-
           Enum.all?(inputs, fn
             {path, {kind, hash}} when is_binary(path) and is_atom(kind) and is_binary(hash) ->
               true

             _invalid_input ->
               false
           end) do
      inputs
    else
      _missing_or_invalid -> nil
    end
  rescue
    _invalid_manifest -> nil
  end

  defp write_materialization_manifest(path, inputs) do
    temporary = "#{path}.#{System.unique_integer([:positive])}.tmp"

    encoded =
      :erlang.term_to_binary(%{
        version: @materialization_manifest_version,
        inputs: inputs
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

  defp remove_empty_directories(root) do
    root
    |> directories_under()
    |> Enum.sort_by(&String.length/1, :desc)
    |> Enum.each(&File.rmdir/1)

    :ok
  end

  defp directories_under(root) do
    case File.ls(root) do
      {:ok, names} ->
        Enum.flat_map(names, fn name ->
          path = Path.join(root, name)

          case File.lstat(path) do
            {:ok, %{type: :directory}} -> [path | directories_under(path)]
            _not_directory -> []
          end
        end)

      {:error, _reason} ->
        []
    end
  end

  defp files_under(root) do
    case File.ls(root) do
      {:ok, names} ->
        Enum.flat_map(names, fn name ->
          path = Path.join(root, name)

          case File.lstat(path) do
            {:ok, %{type: :directory}} -> files_under(path)
            {:ok, _stat} -> [path]
            {:error, _reason} -> []
          end
        end)

      {:error, _reason} ->
        []
    end
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
    |> Map.put(:trace_cache_bases, bases)
    |> Map.update(:events, [], &Enum.map(&1, fn event -> remap_file(event, bases) end))
    |> Map.update(:changed_events, [], &Enum.map(&1, fn event -> remap_file(event, bases) end))
    |> Map.update(:changed_files, [], &Enum.map(&1, fn path -> remap_path(path, bases) end))
    |> Map.update(:traced_files, [], &Enum.map(&1, fn path -> remap_path(path, bases) end))
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

  defp validate_local_paths(repositories, command, cache_dir, directory) do
    inputs =
      repositories
      |> Enum.flat_map(fn repository ->
        repository
        |> Repository.sorted_inputs()
        |> Enum.filter(&configuration_input?/1)
        |> Enum.map(&{&1.path, Path.join(repository.base, &1.path)})
      end)

    paths = Enum.map(inputs, &elem(&1, 1))

    case selected_script(
           command,
           @project_metadata_script,
           ["paths" | paths],
           cache_dir,
           directory
         ) do
      {:ok, output, _stderr, 0} ->
        case decode_probe(output, @local_paths_prefix) do
          {:ok, :ok} ->
            :ok

          {:ok, {:error, path, :invalid_syntax}} ->
            {:error,
             "#{input_name(inputs, path)} could not be parsed by the selected Elixir runtime"}

          {:ok, {:error, path, absolute}} when is_binary(absolute) ->
            {:error, "#{input_name(inputs, path)} declares absolute local path #{absolute}"}

          _invalid ->
            {:error, "selected Elixir runtime returned invalid local path validation metadata"}
        end

      {:ok, _output, _stderr, status} ->
        {:error, "selected Elixir runtime failed local path validation with status #{status}"}

      {:error, _reason} ->
        {:error, "selected Elixir runtime failed local path validation"}
    end
  end

  defp input_name(inputs, path) do
    Enum.find_value(inputs, path, fn {name, absolute} -> if absolute == path, do: name end)
  end

  defp configuration_input?(%{path: path}) do
    Path.basename(path) == "mix.exs" or "config" in Path.split(path)
  end

  defp configuration_input?(_input), do: false

  defp verify_repositories(repositories) do
    Enum.reduce_while(repositories, :ok, fn repository, :ok ->
      case verify_inputs(repository) do
        :ok -> {:cont, :ok}
        {:error, _reason} = error -> {:halt, error}
      end
    end)
  end

  defp project_metadata(repository, project_root, command, cache_dir) do
    path = Path.join([repository.base, project_root, "mix.exs"])
    directory = Path.dirname(path)

    case selected_script(
           command,
           @project_metadata_script,
           ["metadata", path],
           cache_dir,
           directory
         ) do
      {:ok, output, _stderr, 0} ->
        decode_project_metadata(output, command)

      {:ok, output, stderr, _status} ->
        {:error,
         toolchain_error(
           nil,
           "unavailable",
           command.resolved_mix,
           "project manifest",
           command.configs,
           command_diagnostic(output, stderr, "selected runtime could not parse mix.exs")
         )}

      {:error, reason} ->
        {:error,
         toolchain_error(
           nil,
           "unavailable",
           command.resolved_mix,
           "project manifest",
           command.configs,
           reason
         )}
    end
  end

  defp selected_script_command(%{source: "project mise config"} = command, arguments) do
    env = System.find_executable("env") || "env"

    inherited = [
      "ERL_AFLAGS=#{System.get_env("ERL_AFLAGS", "")}",
      "ELIXIR_ERL_OPTIONS=#{System.get_env("ELIXIR_ERL_OPTIONS", "")}",
      "ERL_COMPILER_OPTIONS=#{System.get_env("ERL_COMPILER_OPTIONS", "")}"
    ]

    {%{
       command
       | prefix: ["exec", "--", env] ++ inherited ++ [command.resolved_elixir]
     }, ["-e" | arguments]}
  end

  defp selected_script_command(command, arguments) do
    {command,
     [
       "run",
       "--no-mix-exs",
       "--no-start",
       "--no-compile",
       "--no-deps-check",
       "--no-elixir-version-check",
       "-e"
       | arguments
     ]}
  end

  defp selected_script(command, script, arguments, cache_dir, directory) do
    {script_command, script_arguments} =
      selected_script_command(command, [script, "--" | arguments])

    directory = selected_script_directory(command, cache_dir, directory)

    bounded_separated_command(script_command, script_arguments, directory, command_env(command))
  end

  defp selected_script_directory(%{source: "project mise config"}, _cache_dir, directory),
    do: directory

  defp selected_script_directory(_command, cache_dir, _directory) do
    directory = Path.join([cache_dir, "elixir", "toolchain-probe"])
    File.mkdir_p!(directory)
    directory
  end

  defp decode_project_metadata(output, command) do
    with {:ok, {:ok, requirement, otp}} when is_binary(otp) <-
           decode_probe(output, @project_metadata_prefix) do
      case requirement do
        :none ->
          {:ok, nil, otp}

        :dynamic ->
          {:error,
           toolchain_error(
             nil,
             "unavailable",
             command.resolved_mix,
             "project manifest",
             command.configs,
             "mix.exs has a non-literal elixir requirement"
           )}

        {:literal, requirement} ->
          case Version.parse_requirement(requirement) do
            {:ok, parsed} ->
              {:ok, {requirement, parsed}, otp}

            :error ->
              {:error,
               toolchain_error(
                 requirement,
                 "unavailable",
                 command.resolved_mix,
                 "project manifest",
                 command.configs,
                 "invalid Elixir version requirement"
               )}
          end
      end
    else
      {:ok, {:error, reason}} when is_binary(reason) ->
        {:error,
         toolchain_error(
           nil,
           "unavailable",
           command.resolved_mix,
           "project manifest",
           command.configs,
           reason
         )}

      _invalid ->
        {:error,
         toolchain_error(
           nil,
           "unavailable",
           command.resolved_mix,
           "project manifest",
           command.configs,
           "selected runtime returned invalid project metadata"
         )}
    end
  end

  defp decode_metadata_term(binary) do
    :erlang.binary_to_term(binary, [:safe])
  rescue
    ArgumentError -> :error
  end

  defp decode_probe(output, prefix) do
    with [encoded] <-
           Regex.run(~r/^#{prefix}([A-Za-z0-9+\/=]+)$/m, output, capture: :all_but_first),
         {:ok, binary} <- Base.decode64(encoded) do
      {:ok, decode_metadata_term(binary)}
    else
      _invalid -> :error
    end
  end

  defp compile_helpers(command, cache_dir, identity, directory) do
    output = Path.join(cache_dir, "compiler-helper-ebin-#{identity}")
    complete = Path.join(output, ".complete")

    if File.regular?(complete) do
      {:ok, output}
    else
      File.mkdir_p!(output)

      case selected_script(
             command,
             BeamExporter.compile_script(),
             ["helpers", output | BeamExporter.sources()],
             cache_dir,
             directory
           ) do
        {:ok, _output, _stderr, 0} ->
          File.write!(complete, "")
          {:ok, output}

        {:ok, _output, _stderr, status} ->
          {:error,
           "selected Elixir runtime failed to compile tracer helpers with status #{status}"}

        {:error, _reason} ->
          {:error, "selected Elixir runtime failed to compile tracer helpers"}
      end
    end
  end

  defp find_mix_for_metadata(repository, project_root) do
    case find_mix(repository, project_root, nil) do
      {:ok, _command} = selected ->
        selected

      {:error, _reason} = unavailable ->
        case best_effort_project_requirement(repository, project_root) do
          nil -> unavailable
          requirement -> find_mix(repository, project_root, requirement)
        end
    end
  end

  defp best_effort_project_requirement(repository, project_root) do
    path = Path.join([repository.base, project_root, "mix.exs"])

    with {:ok, {:literal, requirement}, _otp} <- ProjectMetadata.read(path),
         {:ok, parsed} <- Version.parse_requirement(requirement) do
      {requirement, parsed}
    else
      _unavailable -> nil
    end
  end

  defp find_mix(repository, project_root, requirement) do
    case System.get_env("BEHOLDER_ELIXIR_MIX_PATH", "") |> String.trim() do
      "" -> find_project_or_ambient_mix(repository, project_root, requirement)
      path -> direct_mix(path, "BEHOLDER_ELIXIR_MIX_PATH", [], requirement)
    end
  end

  defp find_project_or_ambient_mix(repository, project_root, requirement) do
    configs = applicable_toolchain_configs(repository, project_root)

    if Enum.any?(configs, &selects_elixir?/1) do
      directory = Path.join(repository.base, project_root)
      paths = Enum.map(configs, & &1.path)

      case System.find_executable("mise") do
        nil ->
          {:error,
           toolchain_error(
             required_text(requirement),
             "unavailable",
             "unavailable",
             "project mise config",
             paths,
             "mise executable not found"
           )}

        mise ->
          case mise_source(mise, directory, configs) do
            {:ok, _source} ->
              find_mise_mix(mise, directory, configs, requirement)

            :none ->
              find_ambient_mix(requirement)

            {:error, reason} ->
              {:error,
               toolchain_error(
                 required_text(requirement),
                 "unavailable",
                 "unavailable",
                 "project mise config",
                 paths,
                 reason
               )}
          end
      end
    else
      find_ambient_mix(requirement)
    end
  end

  defp find_ambient_mix(requirement) do
    case System.find_executable("mix") do
      nil ->
        {:error,
         toolchain_error(
           required_text(requirement),
           "unavailable",
           "unavailable",
           "ambient PATH",
           [],
           "mix executable not found"
         )}

      path ->
        direct_mix(path, "ambient PATH", [], requirement)
    end
  end

  defp applicable_toolchain_configs(repository, project_root) do
    root = if project_root == ".", do: [], else: Path.split(project_root)

    repository.inputs
    |> Enum.filter(fn input -> Path.basename(input.path) in ["mise.toml", ".tool-versions"] end)
    |> Enum.filter(fn input ->
      directory =
        input.path |> Path.dirname() |> then(&if(&1 == ".", do: [], else: Path.split(&1)))

      Enum.take(root, length(directory)) == directory
    end)
    |> Enum.map(&Map.put(&1, :absolute_path, Path.join(repository.base, &1.path)))
    |> Enum.sort_by(& &1.path)
  end

  defp selects_elixir?(%{path: path, content: content}) do
    if Path.basename(path) == ".tool-versions" do
      Regex.match?(~r/^\s*elixir\s+\S+/m, content)
    else
      case TomlElixir.decode(content) do
        {:ok, %{"tools" => tools}} when is_map(tools) -> Map.has_key?(tools, "elixir")
        {:ok, values} -> Map.has_key?(values, "elixir")
        {:error, _reason} -> true
      end
    end
  end

  defp find_mise_mix(mise, directory, configs, requirement) do
    paths = Enum.map(configs, & &1.path)
    required = required_text(requirement)

    with {:ok, mix} <- mise_executable(mise, "mix", directory, configs),
         {:ok, elixir} <- mise_executable(mise, "elixir", directory, configs),
         {:ok, environment_identity} <- mise_environment_identity(mise, directory, configs) do
      {:ok,
       %{
         executable: mise,
         prefix: ["exec", "--", mix],
         resolved_mix: mix,
         resolved_elixir: elixir,
         source: "project mise config",
         configs: configs,
         environment_identity: environment_identity,
         required: requirement
       }}
    else
      {:error, executable, reason} ->
        {:error,
         toolchain_error(
           required,
           "unavailable",
           executable,
           "project mise config",
           paths,
           reason
         )}
    end
  end

  defp mise_environment_identity(mise, directory, configs) do
    case bounded_mise_command(mise, ["env", "--json"], directory, configs) do
      {:ok, output, _stderr, 0} ->
        case Jason.decode(output) do
          {:ok, environment} when is_map(environment) ->
            identity =
              environment
              |> Enum.sort()
              |> :erlang.term_to_binary([:deterministic])
              |> then(&:crypto.hash(:sha256, &1))
              |> Base.url_encode64(padding: false)

            {:ok, identity}

          _invalid ->
            {:error, "unavailable", "mise returned invalid environment metadata"}
        end

      {:ok, _output, _stderr, _status} ->
        {:error, "unavailable", "mise could not resolve the selected environment"}

      {:error, _reason} ->
        {:error, "unavailable", "mise could not resolve the selected environment"}
    end
  end

  defp mise_executable(mise, name, directory, configs) do
    case bounded_mise_command(mise, ["which", name], directory, configs) do
      {:ok, path, _stderr, 0} ->
        path = path |> String.trim() |> Path.expand()

        if File.regular?(path),
          do: {:ok, path},
          else: {:error, path, "mise resolved a missing #{name} executable"}

      {:ok, output, stderr, _status} ->
        {:error, "unavailable",
         command_diagnostic(output, stderr, "mise could not resolve #{name}")}

      {:error, reason} ->
        {:error, "unavailable", reason}
    end
  end

  defp direct_mix(path, source, configs, requirement) do
    path = Path.expand(path)

    if File.regular?(path),
      do:
        {:ok,
         %{
           executable: path,
           prefix: [],
           resolved_mix: path,
           source: source,
           configs: configs,
           environment_identity: "",
           required: requirement
         }},
      else:
        {:error,
         toolchain_error(
           required_text(requirement),
           "unavailable",
           path,
           source,
           configs,
           "configured Mix executable does not exist"
         )}
  end

  defp mise_env(configs) do
    trusted = Enum.map_join(configs, path_separator(), &Path.expand(&1.absolute_path))
    isolation = Path.join(System.tmp_dir!(), "beholder-mise-config-#{System.pid()}")

    [
      {"MISE_SAFE", "1"},
      {"MISE_AUTO_INSTALL", "false"},
      {"MISE_EXEC_AUTO_INSTALL", "false"},
      {"MISE_CONFIG_DIR", Path.join(isolation, "config")},
      {"MISE_GLOBAL_CONFIG_FILE", Path.join(isolation, "global.toml")},
      {"MISE_SYSTEM_CONFIG_DIR", Path.join(isolation, "system")},
      {"MISE_TRUSTED_CONFIG_PATHS", trusted}
    ]
  end

  defp path_separator, do: if(match?({:win32, _}, :os.type()), do: ";", else: ":")

  defp command_env(%{source: "project mise config", configs: configs}), do: mise_env(configs)
  defp command_env(_command), do: []

  defp compiler_command(%{source: "project mise config"} = command, env) do
    invariant_names = [
      "BEHOLDER_ELIXIR_TRACE_RESULT",
      "BEHOLDER_ELIXIR_FORCE_COMPILE",
      "MIX_BUILD_PATH",
      "MIX_DEPS_PATH",
      "MIX_ENV",
      "ERL_AFLAGS"
    ]

    invariants =
      for {name, value} <- env, name in invariant_names, do: "#{name}=#{value}"

    %{
      command
      | prefix:
          ["exec", "--", System.find_executable("env") || "env"] ++
            invariants ++ [command.resolved_mix]
    }
  end

  defp compiler_command(command, _env), do: command

  defp mise_source(mise, directory, configs) do
    with {:ok, output, stderr, 0} <-
           bounded_mise_command(mise, ["ls", "elixir", "--current", "--json"], directory, configs),
         {:ok, records} when is_list(records) <- Jason.decode(output) do
      case records do
        [] ->
          :none

        [%{"source" => %{"path" => path}, "installed" => installed} | _] ->
          if Enum.any?(configs, &(Path.expand(&1.absolute_path) == Path.expand(path))) do
            if installed,
              do: {:ok, path},
              else: {:error, "selected Elixir runtime is not installed"}
          else
            :none
          end

        _records ->
          {:error, "mise returned an invalid Elixir selection: #{String.trim(stderr)}"}
      end
    else
      {:ok, output, stderr, _status} ->
        {:error, command_diagnostic(output, stderr, "mise could not resolve Elixir")}

      {:error, reason} ->
        {:error, reason}

      _ ->
        {:error, "mise returned invalid JSON for the selected Elixir runtime"}
    end
  end

  defp bounded_mise_command(mise, arguments, directory, configs) do
    bounded_separated_command(
      %{executable: mise, prefix: []},
      arguments,
      directory,
      mise_env(configs)
    )
  end

  defp bounded_separated_command(source_command, arguments, directory, env) do
    shell = System.find_executable("sh")
    temporary = Base.url_encode64(:crypto.strong_rand_bytes(12), padding: false)
    stderr_path = Path.join(System.tmp_dir!(), "beholder-mise-#{temporary}.stderr")
    status_path = Path.join(System.tmp_dir!(), "beholder-mise-#{temporary}.status")

    if shell do
      command = %{
        executable: shell,
        prefix:
          [
            "-c",
            ~S"""
            stderr=$1; status=$2; max=$3; shift 3; exec 3>&1
            { "$@" 2>&1 1>&3; printf '%s' "$?" >"$status"; } | { head -c "$max"; cat >/dev/null; } >"$stderr"
            exit "$(cat "$status")"
            """,
            "beholder-mise",
            stderr_path,
            status_path,
            Integer.to_string(@default_max_output_bytes),
            source_command.executable
          ] ++ source_command.prefix
      }

      try do
        case bounded_command(command, arguments, directory, env) do
          {:ok, output, status} -> {:ok, output, read_diagnostic(stderr_path), status}
          {:error, reason} -> {:error, append_diagnostic(reason, stderr_path)}
        end
      after
        File.rm(stderr_path)
        File.rm(status_path)
      end
    else
      {:error, "sh executable not found while isolating mise diagnostics"}
    end
  end

  defp read_diagnostic(path) do
    case File.read(path) do
      {:ok, diagnostic} -> diagnostic
      {:error, _reason} -> ""
    end
  end

  defp append_diagnostic(reason, path) do
    case String.trim(read_diagnostic(path)) do
      "" -> reason
      diagnostic -> "#{reason}: #{diagnostic}"
    end
  end

  defp command_diagnostic(output, stderr, default) do
    [stderr, output]
    |> Enum.map(&String.trim/1)
    |> Enum.reject(&(&1 == ""))
    |> Enum.join(": ")
    |> empty_default(default)
  end

  defp empty_default("", default), do: default
  defp empty_default(value, _default), do: value

  defp bounded_command(command, arguments, directory, env) do
    case run_command(
           command,
           arguments,
           directory,
           env,
           configured_positive_integer("BEHOLDER_WORKER_TIMEOUT_MS", @default_timeout_ms),
           @default_max_output_bytes,
           fn _detail -> :ok end
         ) do
      {:ok, output, status} ->
        {:ok, output, status}

      {:error, :timeout, output, timeout_ms} ->
        {:error, "toolchain command exceeded #{timeout_ms}ms: #{String.trim(output)}"}

      {:error, reason} ->
        {:error, reason}
    end
  end

  defp probe_mix(command, directory) do
    case bounded_command(command, ["--version"], directory, command_env(command)) do
      {:ok, output, 0} ->
        case Regex.run(~r/Mix ([^\s]+)/, output) do
          [_, version] ->
            case Version.parse(version) do
              {:ok, _parsed} ->
                {:ok,
                 %{
                   version: version,
                   output: output
                 }}

              :error ->
                {:error,
                 toolchain_error(
                   required_text(command.required),
                   "unavailable",
                   command.resolved_mix,
                   command.source,
                   command.configs,
                   "could not parse Mix version: #{String.trim(output)}"
                 )}
            end

          _ ->
            {:error,
             toolchain_error(
               required_text(command.required),
               "unavailable",
               command.resolved_mix,
               command.source,
               command.configs,
               "could not parse Mix version: #{String.trim(output)}"
             )}
        end

      {:ok, output, _} ->
        {:error,
         toolchain_error(
           required_text(command.required),
           "unavailable",
           command.resolved_mix,
           command.source,
           command.configs,
           String.trim(output)
         )}

      {:error, reason} ->
        {:error,
         toolchain_error(
           required_text(command.required),
           "unavailable",
           command.resolved_mix,
           command.source,
           command.configs,
           reason
         )}
    end
  end

  defp validate_requirement(nil, _runtime, _command), do: :ok

  defp validate_requirement({requirement, parsed}, runtime, command) do
    if Version.match?(runtime.version, parsed),
      do: :ok,
      else:
        {:error,
         toolchain_error(
           requirement,
           runtime.version,
           command.resolved_mix,
           command.source,
           command.configs,
           "version requirement does not match"
         )}
  end

  defp toolchain_error(requirement, actual, executable, source, configs, detail) do
    paths =
      Enum.map_join(configs, ",", fn config ->
        if is_map(config), do: config.path, else: config
      end)

    "Elixir toolchain unavailable: required=#{requirement || "none"}, actual=#{actual}, selected_executable=#{executable}, selection_source=#{source}, configuration=#{paths}. #{detail}"
  end

  defp required_text(nil), do: nil
  defp required_text({requirement, _parsed}), do: requirement

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

  @doc false
  def complete_events(%{events_complete?: true, events: events}), do: events

  def complete_events(%{
        trace_cache_path: trace_cache_path,
        trace_cache_bases: bases
      }) do
    trace_cache_path
    |> TraceCache.all_events()
    |> Enum.map(&remap_file(&1, bases))
  end

  def complete_events(%{events: events}), do: events

  defp merge_trace_cache(result, changed_inputs, cache_path, trace_cache_status) do
    changed_events = result.events

    cache =
      TraceCache.merge(
        cache_path,
        trace_cache_status,
        changed_inputs,
        changed_events,
        result.status
      )

    result
    |> Map.merge(cache)
    |> Map.put(:changed_events, changed_events)
    |> Map.put(:trace_cache_path, cache_path)
  end

  defp append_code_path(nil, path), do: "-pa #{path}"
  defp append_code_path("", path), do: "-pa #{path}"
  defp append_code_path(flags, path), do: flags <> " -pa #{path}"

  defp build_identity(repositories, command, runtime, mix_env) do
    repositories
    |> Enum.sort_by(& &1.identity)
    |> Enum.flat_map(&[&1.identity, Path.expand(&1.base)])
    |> Kernel.++(Enum.flat_map(command.configs, &[&1.path, &1.content]))
    |> Kernel.++([
      mix_env,
      command.executable,
      Enum.join(command.prefix, <<0>>),
      command.resolved_mix,
      command.source,
      command.environment_identity,
      BeamExporter.identity(),
      runtime.version,
      runtime.otp,
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

  defp run_command(command, arguments, directory, env, timeout_ms, max_output_bytes, on_progress) do
    {executable, arguments, group_file} =
      isolated_command(command.executable, command.prefix ++ arguments)

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

    collect_output(
      port,
      deadline,
      timeout_ms,
      max_output_bytes,
      {<<>>, false},
      target,
      on_progress,
      {<<>>, MapSet.new()}
    )
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

  defp collect_output(
         port,
         deadline,
         timeout_ms,
         max_output_bytes,
         output,
         target,
         on_progress,
         progress
       ) do
    remaining = max(deadline - System.monotonic_time(:millisecond), 0)

    receive do
      {^port, {:data, data}} ->
        progress = report_progress(progress, data, on_progress)

        collect_output(
          port,
          deadline,
          timeout_ms,
          max_output_bytes,
          append_output(output, data, max_output_bytes),
          target,
          on_progress,
          progress
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

  defp report_progress({tail, reported}, data, on_progress) do
    buffer = tail <> data

    reported =
      [
        {@dependency_progress, "preparing dependencies"},
        {@compilation_progress, "compiling project"}
      ]
      |> Enum.reduce(reported, fn {marker, detail}, reported ->
        if marker not in reported and :binary.match(buffer, marker) != :nomatch do
          on_progress.(detail)
          MapSet.put(reported, marker)
        else
          reported
        end
      end)

    tail_bytes = max(byte_size(@dependency_progress), byte_size(@compilation_progress)) - 1

    tail =
      binary_part(
        buffer,
        max(byte_size(buffer) - tail_bytes, 0),
        min(byte_size(buffer), tail_bytes)
      )

    {tail, reported}
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
