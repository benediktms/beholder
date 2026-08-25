defmodule Beholder.Worker.Elixir.CompilerTest do
  use ExUnit.Case, async: false

  alias Beholder.Worker.Elixir.Compiler
  alias Beholder.Worker.Elixir.Snapshot.Repository

  test "compiles a Mix project and returns trace events from a warm build" do
    root = temp_dir("project")
    cache = temp_dir("cache")
    File.mkdir_p!(Path.join(root, "lib"))

    mix_source = """
    defmodule CompilerFixture.MixProject do
      use Mix.Project

      def project do
        [app: :compiler_fixture, version: "0.1.0", elixir: "~> 1.15"]
      end
    end
    """

    source = """
    defmodule CompilerFixture do
      def call(values), do: Enum.map(values, &to_string/1)
    end
    """

    File.write!(Path.join(root, "mix.exs"), mix_source)
    File.write!(Path.join(root, "lib/compiler_fixture.ex"), source)

    repository = %Repository{
      identity: "fixture",
      base: root,
      fingerprint: "abc123",
      inputs: [
        %{path: "mix.exs", content: mix_source, kind: :INPUT_KIND_SOURCE},
        %{path: "lib/compiler_fixture.ex", content: source, kind: :INPUT_KIND_SOURCE}
      ]
    }

    File.write!(Path.join(root, "mix.exs"), "stale live manifest")
    File.write!(Path.join(root, "lib/compiler_fixture.ex"), "stale live source")

    assert {:ok, result} = Compiler.run(repository, cache)
    assert result.status == :ok, inspect(result)
    assert Enum.any?(result.events, &(&1.kind == :remote_function and &1.target == "Enum"))

    assert Enum.any?(result.events, fn event ->
             event.kind == :module and {"call", 1} in event.definitions and
               event.file == Path.join(root, "lib/compiler_fixture.ex")
           end)

    assert [_build] = Path.wildcard(Path.join([cache, "elixir", "Zml4dHVyZQ", "build-*"]))

    assert {:ok, second} = Compiler.run(repository, cache)
    assert second.status == :ok, inspect(second)

    assert Enum.any?(second.events, fn event ->
             event.kind == :module and {"call", 1} in event.definitions
           end)

    assert Enum.any?(second.events, &(&1.kind == :remote_function and &1.target == "Enum"))

    [trace_cache] =
      Path.wildcard(Path.join([cache, "elixir", "Zml4dHVyZQ", "trace-cache-*.term"]))

    File.write!(trace_cache, "invalid")
    assert {:ok, rebuilt} = Compiler.run(repository, cache)
    assert rebuilt.status == :ok
    assert rebuilt.output =~ "Compiling 1 file"
    assert Enum.any?(rebuilt.events, &(&1.kind == :module and &1.target == "CompilerFixture"))
  end

  test "reuses unchanged traces while Mix incrementally compiles changed sources" do
    root = temp_dir("incremental-project")
    cache = temp_dir("incremental-cache")
    File.mkdir_p!(Path.join(root, "lib"))

    mix_source = """
    defmodule IncrementalFixture.MixProject do
      use Mix.Project
      def project, do: [app: :incremental_fixture, version: "0.1.0"]
    end
    """

    unchanged = "defmodule IncrementalFixture.Unchanged do\n  def call, do: :ok\nend\n"
    changed = "defmodule IncrementalFixture.Changed do\n  def first, do: :ok\nend\n"

    repository = %Repository{
      identity: "fixture",
      base: root,
      fingerprint: "first",
      inputs: [
        %{path: "mix.exs", content: mix_source, kind: :INPUT_KIND_SOURCE},
        %{path: "lib/unchanged.ex", content: unchanged, kind: :INPUT_KIND_SOURCE},
        %{path: "lib/changed.ex", content: changed, kind: :INPUT_KIND_SOURCE}
      ]
    }

    assert {:ok, first} = Compiler.run(repository, cache)
    assert first.status == :ok

    changed = "defmodule IncrementalFixture.Changed do\n  def second, do: :ok\nend\n"

    repository = %{
      repository
      | fingerprint: "second",
        inputs: [
          %{path: "mix.exs", content: mix_source, kind: :INPUT_KIND_SOURCE},
          %{path: "lib/unchanged.ex", content: unchanged, kind: :INPUT_KIND_SOURCE},
          %{path: "lib/changed.ex", content: changed, kind: :INPUT_KIND_SOURCE}
        ]
    }

    assert {:ok, second} = Compiler.run(repository, cache)
    assert second.status == :ok
    assert second.output =~ "Compiling 1 file"

    assert Enum.any?(second.events, fn event ->
             event.kind == :module and event.target == "IncrementalFixture.Unchanged" and
               {"call", 0} in event.definitions
           end)

    assert Enum.any?(second.events, fn event ->
             event.kind == :module and event.target == "IncrementalFixture.Changed" and
               {"second", 0} in event.definitions
           end)

    repository = %{
      repository
      | fingerprint: "third",
        inputs: [
          %{path: "mix.exs", content: mix_source, kind: :INPUT_KIND_SOURCE},
          %{path: "lib/unchanged.ex", content: unchanged, kind: :INPUT_KIND_SOURCE}
        ]
    }

    assert {:ok, third} = Compiler.run(repository, cache)
    assert third.status == :ok
    refute Enum.any?(third.events, &(&1[:target] == "IncrementalFixture.Changed"))

    refute File.exists?(
             Path.join([cache, "elixir", "Zml4dHVyZQ", "snapshot", "lib", "changed.ex"])
           )

    assert 1 ==
             cache
             |> Path.join("elixir/Zml4dHVyZQ/build-*")
             |> Path.wildcard()
             |> length()
  end

  test "reports an unavailable dependency without fetching it" do
    root = temp_dir("dependency-project")
    dependency = temp_dir("dependency-source")
    cache = temp_dir("dependency-cache")
    File.mkdir_p!(Path.join(root, "lib"))
    File.mkdir_p!(Path.join(dependency, "lib"))

    dependency_mix = """
    defmodule CompilerDependency.MixProject do
      use Mix.Project
      def project, do: [app: :compiler_dependency, version: "0.1.0"]
    end
    """

    dependency_source = """
    defmodule CompilerDependency do
      def value, do: :dependency
    end
    """

    File.write!(Path.join(dependency, "mix.exs"), dependency_mix)
    File.write!(Path.join(dependency, "lib/compiler_dependency.ex"), dependency_source)
    System.cmd("git", ["init", "--quiet"], cd: dependency)
    System.cmd("git", ["add", "."], cd: dependency)

    {_, 0} =
      System.cmd(
        "git",
        [
          "-c",
          "user.name=Beholder Test",
          "-c",
          "user.email=beholder@example.com",
          "-c",
          "commit.gpgsign=false",
          "commit",
          "--quiet",
          "-m",
          "fixture"
        ],
        cd: dependency
      )

    mix_source = """
    defmodule DependencyFixture.MixProject do
      use Mix.Project
      def project, do: [app: :dependency_fixture, version: "0.1.0", deps: deps()]
      defp deps, do: [{:compiler_dependency, git: "file://#{dependency}"}]
    end
    """

    source = """
    defmodule DependencyFixture do
      def call, do: CompilerDependency.value()
    end
    """

    repository = %Repository{
      identity: "fixture",
      base: root,
      fingerprint: "with-dependency",
      inputs: [
        %{path: "mix.exs", content: mix_source, kind: :INPUT_KIND_SOURCE},
        %{path: "lib/dependency_fixture.ex", content: source, kind: :INPUT_KIND_SOURCE}
      ]
    }

    assert {:ok, result} = Compiler.run(repository, cache)
    assert result.status == :error
    assert Enum.any?(result.diagnostics, &(&1.message =~ "errors on dependencies"))
    assert File.ls!(Path.join([cache, "elixir", "Zml4dHVyZQ", "deps"])) == []
  end

  test "materializes snapshot bytes instead of reading a changed checkout" do
    root = temp_dir("changed")
    cache = temp_dir("changed-cache")
    captured = Path.join(root, "captured")
    fake_mix = fake_mix(root, "cat mix.exs > #{shell_quote(captured)}")
    File.write!(Path.join(root, "mix.exs"), "changed")

    repository = %Repository{
      identity: "fixture",
      base: root,
      fingerprint: "abc123",
      inputs: [%{path: "mix.exs", content: "original", kind: :INPUT_KIND_SOURCE}]
    }

    with_env("BEHOLDER_ELIXIR_MIX_PATH", fake_mix, fn ->
      assert {:error, reason} = Compiler.run(repository, cache)
      assert reason =~ "before producing a trace"
    end)

    assert File.read!(captured) == "original"
  end

  test "runs Mix from the unique shallowest project root" do
    root = temp_dir("nested-project")
    cache = temp_dir("nested-project-cache")
    captured = Path.join(root, "captured")

    fake_mix =
      fake_mix(
        root,
        "printf '%s\\n%s\\n%s\\n%s\\n' \"$PWD\" \"$MIX_HOME\" \"$HEX_HOME\" \"$MIX_DEPS_PATH\" > #{shell_quote(captured)}"
      )

    repository = %Repository{
      identity: "fixture",
      base: root,
      fingerprint: "nested",
      inputs: [
        %{path: "src/mix.exs", content: "umbrella", kind: :INPUT_KIND_SOURCE},
        %{path: "src/apps/api/mix.exs", content: "app", kind: :INPUT_KIND_SOURCE}
      ]
    }

    with_envs(
      %{
        "BEHOLDER_ELIXIR_MIX_PATH" => fake_mix,
        "MIX_HOME" => "/toolchain/mix",
        "HEX_HOME" => "/toolchain/hex"
      },
      fn ->
        assert {:error, reason} = Compiler.run(repository, cache)
        assert reason =~ "before producing a trace"
      end
    )

    assert [working_directory, "/toolchain/mix", "/toolchain/hex", deps_path] =
             captured |> File.read!() |> String.split()

    assert String.ends_with?(working_directory, "/src")
    assert deps_path == Path.join([cache, "elixir", "Zml4dHVyZQ", "deps"])
  end

  test "materializes dependency context beside the target" do
    root = temp_dir("context")
    target_root = Path.join(root, "target")
    context_root = Path.join(root, "context")
    cache = temp_dir("context-cache")
    captured = Path.join(root, "captured")
    File.mkdir_p!(target_root)
    File.mkdir_p!(context_root)
    fake_mix = fake_mix(root, "cat ../context/mix.exs > #{shell_quote(captured)}")
    File.write!(Path.join(target_root, "mix.exs"), "target")
    File.write!(Path.join(context_root, "mix.exs"), "changed")

    target = %Repository{
      identity: "example/target",
      base: target_root,
      fingerprint: "target",
      inputs: [%{path: "mix.exs", content: "target", kind: :INPUT_KIND_SOURCE}]
    }

    context = %Repository{
      identity: "example/context",
      base: context_root,
      fingerprint: "context",
      inputs: [%{path: "mix.exs", content: "original", kind: :INPUT_KIND_SOURCE}]
    }

    with_env("BEHOLDER_ELIXIR_MIX_PATH", fake_mix, fn ->
      assert {:error, reason} = Compiler.run(target, [context], cache)
      assert reason =~ "before producing a trace"
    end)

    assert File.read!(captured) == "original"
  end

  test "isolates a checkout changed while the compiler is running" do
    root = temp_dir("changed-during-compile")
    cache = temp_dir("changed-during-compile-cache")
    marker = Path.join(root, "compiler-started")
    fake_mix = fake_mix(root, "touch #{shell_quote(marker)}\nsleep 1")
    File.write!(Path.join(root, "mix.exs"), "original")

    repository = %Repository{
      identity: "fixture",
      base: root,
      fingerprint: "abc123",
      inputs: [%{path: "mix.exs", content: "original", kind: :INPUT_KIND_SOURCE}]
    }

    with_env("BEHOLDER_ELIXIR_MIX_PATH", fake_mix, fn ->
      task = Task.async(fn -> Compiler.run(repository, cache) end)
      wait_for_file(marker)
      File.write!(Path.join(root, "mix.exs"), "changed")

      assert {:error, reason} = Task.await(task, 5_000)
      refute reason =~ "changed after the immutable snapshot was created"
    end)
  end

  test "rejects absolute Mix path dependencies outside the snapshot" do
    root = temp_dir("absolute-path")

    manifest = """
    defmodule AbsolutePath.MixProject do
      use Mix.Project
      def project, do: [app: :absolute_path, version: "0.1.0"]
      defp deps, do: [{:external, path: "/live/external"}]
    end
    """

    repository = %Repository{
      identity: "fixture",
      base: root,
      fingerprint: "absolute",
      inputs: [%{path: "mix.exs", content: manifest, kind: :INPUT_KIND_SOURCE}]
    }

    assert {:error, reason} = Compiler.run(repository, temp_dir("absolute-path-cache"))
    assert reason =~ "mix.exs declares absolute local path /live/external"
  end

  test "terminates an overdue compiler process and bounds its captured output" do
    root = temp_dir("timeout")
    cache = temp_dir("timeout-cache")
    child_pid = Path.join(root, "child-pid")

    fake_mix =
      fake_mix(
        root,
        "printf '%0256d' 0\n(trap '' TERM; sleep 30) &\necho $! > #{shell_quote(child_pid)}\nwait"
      )

    File.write!(Path.join(root, "mix.exs"), "original")

    repository = %Repository{
      identity: "fixture",
      base: root,
      fingerprint: "abc123",
      inputs: [%{path: "mix.exs", content: "original", kind: :INPUT_KIND_SOURCE}]
    }

    with_envs(
      %{
        "BEHOLDER_ELIXIR_MIX_PATH" => fake_mix,
        "BEHOLDER_WORKER_TIMEOUT_MS" => "1000",
        "BEHOLDER_WORKER_MAX_OUTPUT_BYTES" => "128"
      },
      fn ->
        assert {:error, reason} = Compiler.run(repository, cache)
        assert reason =~ "exceeded 1000ms and was terminated"
        assert reason =~ "[compiler output truncated]"
        assert byte_size(reason) < 512
        wait_for_file(child_pid)

        pid = child_pid |> File.read!() |> String.trim()
        wait_for_process_exit(pid)
      end
    )
  end

  test "reports compiler subphases from child process markers" do
    root = temp_dir("progress")
    cache = temp_dir("progress-cache")

    fake_mix =
      fake_mix(
        root,
        "printf 'BEHOLDER_PROGRESS dependency_preparation\\nBEHOLDER_PROGRESS project_compilation\\n'"
      )

    File.write!(Path.join(root, "mix.exs"), "original")

    repository = %Repository{
      identity: "fixture",
      base: root,
      fingerprint: "abc123",
      inputs: [%{path: "mix.exs", content: "original", kind: :INPUT_KIND_SOURCE}]
    }

    parent = self()

    with_env("BEHOLDER_ELIXIR_MIX_PATH", fake_mix, fn ->
      assert {:error, _reason} =
               Compiler.run(repository, [], cache, fn detail ->
                 send(parent, {:progress, detail})
               end)
    end)

    assert_received {:progress, "preparing dependencies"}
    assert_received {:progress, "compiling project"}
  end

  test "isolates Mix build directories by compilation environment" do
    root = temp_dir("build-identity")
    cache = temp_dir("build-identity-cache")
    fake_mix = fake_mix(root, "mkdir -p \"$MIX_BUILD_PATH\"")
    File.write!(Path.join(root, "mix.exs"), "original")

    repository = %Repository{
      identity: "fixture",
      base: root,
      fingerprint: "abc123",
      inputs: [%{path: "mix.exs", content: "original", kind: :INPUT_KIND_SOURCE}]
    }

    with_env("BEHOLDER_ELIXIR_MIX_PATH", fake_mix, fn ->
      with_env("BEHOLDER_ELIXIR_MIX_ENV", "dev", fn -> Compiler.run(repository, cache) end)
      with_env("BEHOLDER_ELIXIR_MIX_ENV", "test", fn -> Compiler.run(repository, cache) end)
    end)

    assert 2 ==
             cache
             |> Path.join("elixir/Zml4dHVyZQ/build-*")
             |> Path.wildcard()
             |> length()
  end

  test "reuses a Mix build directory across dependency context revisions" do
    root = temp_dir("context-build-identity")
    context_root = temp_dir("context-build-dependency")
    cache = temp_dir("context-build-cache")
    fake_mix = fake_mix(root, "mkdir -p \"$MIX_BUILD_PATH\"")
    File.write!(Path.join(root, "mix.exs"), "target")
    File.write!(Path.join(context_root, "mix.exs"), "context")

    repository = %Repository{
      identity: "fixture",
      base: root,
      fingerprint: "target",
      inputs: [%{path: "mix.exs", content: "target", kind: :INPUT_KIND_SOURCE}]
    }

    context = %Repository{
      identity: "dependency",
      base: context_root,
      fingerprint: "context-1",
      inputs: [%{path: "mix.exs", content: "context", kind: :INPUT_KIND_SOURCE}]
    }

    with_env("BEHOLDER_ELIXIR_MIX_PATH", fake_mix, fn ->
      Compiler.run(repository, [context], cache)
      Compiler.run(repository, [%{context | fingerprint: "context-2"}], cache)
    end)

    assert 1 ==
             cache
             |> Path.join("elixir/Zml4dHVyZQ/build-*")
             |> Path.wildcard()
             |> length()
  end

  defp fake_mix(root, body) do
    path = Path.join(root, "fake-mix")
    File.write!(path, "#!/bin/sh\nset -eu\n#{body}\n")
    File.chmod!(path, 0o755)
    path
  end

  defp wait_for_file(path, attempts \\ 100)
  defp wait_for_file(path, 0), do: flunk("timed out waiting for #{path}")

  defp wait_for_file(path, attempts) do
    if File.exists?(path) do
      :ok
    else
      Process.sleep(20)
      wait_for_file(path, attempts - 1)
    end
  end

  defp wait_for_process_exit(pid, attempts \\ 100)
  defp wait_for_process_exit(pid, 0), do: flunk("compiler child process #{pid} survived")

  defp wait_for_process_exit(pid, attempts) do
    if process_running?(pid) do
      Process.sleep(20)
      wait_for_process_exit(pid, attempts - 1)
    else
      :ok
    end
  end

  defp process_running?(pid) do
    case File.read("/proc/#{pid}/stat") do
      {:ok, stat} ->
        case String.split(stat, " ") do
          [_pid, _name, "Z" | _rest] -> false
          _fields -> true
        end

      {:error, :enoent} ->
        false

      {:error, _reason} ->
        case System.cmd("kill", ["-0", pid], stderr_to_stdout: true) do
          {_output, 0} -> true
          {_output, _status} -> false
        end
    end
  end

  defp shell_quote(value), do: "'" <> String.replace(value, "'", "'\\''") <> "'"

  defp with_env(name, value, fun), do: with_envs(%{name => value}, fun)

  defp with_envs(values, fun) do
    previous = Map.new(values, fn {name, _value} -> {name, System.get_env(name)} end)
    Enum.each(values, fn {name, value} -> System.put_env(name, value) end)

    try do
      fun.()
    after
      Enum.each(previous, fn
        {name, nil} -> System.delete_env(name)
        {name, value} -> System.put_env(name, value)
      end)
    end
  end

  defp temp_dir(label) do
    path =
      Path.join(
        System.tmp_dir!(),
        "beholder-elixir-#{label}-#{System.unique_integer([:positive])}"
      )

    File.rm_rf!(path)
    File.mkdir_p!(path)
    on_exit(fn -> File.rm_rf!(path) end)
    path
  end
end
