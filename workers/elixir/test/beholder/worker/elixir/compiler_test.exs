defmodule Beholder.Worker.Elixir.CompilerTest do
  use ExUnit.Case, async: false

  alias Beholder.Worker.Elixir.Compiler
  alias Beholder.Worker.Elixir.Compiler.Collector
  alias Beholder.Worker.Elixir.Compiler.TraceCache
  alias Beholder.Worker.Elixir.Snapshot.Repository

  test "deduplicates only identical trace coordinates" do
    start_supervised!(Collector)

    event = %{
      kind: :remote_function,
      file: "/tmp/example/lib/example.ex",
      line: 2,
      column: 5,
      caller_module: "Example",
      caller_function: {"call", 0},
      from_macro: false,
      target: "Target",
      name: "run",
      arity: 0
    }

    assert :ok = Collector.record(event)
    assert :ok = Collector.record(%{event | line: 20, column: 9})
    assert :ok = Collector.record(%{event | target: "Other"})

    assert ["Other", "Target", "Target"] ==
             Collector.drain()
             |> Enum.map(& &1.target)
             |> Enum.sort()
  end

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

    assert second.events == []
    refute second.events_complete?
    complete_events = Compiler.complete_events(second)

    assert Enum.any?(complete_events, fn event ->
             event.kind == :module and {"call", 1} in event.definitions
           end)

    assert Enum.any?(complete_events, &(&1.kind == :remote_function and &1.target == "Enum"))

    [trace_cache] =
      Path.wildcard(Path.join([cache, "elixir", "Zml4dHVyZQ", "trace-cache-*.term"]))

    assert %{version: 4} = trace_cache |> File.read!() |> :erlang.binary_to_term([:safe])

    File.write!(trace_cache, "invalid")
    TraceCache.clear()
    assert {:ok, rebuilt} = Compiler.run(repository, cache)
    assert rebuilt.status == :ok
    assert rebuilt.output =~ "Compiling 1 file"
    assert Enum.any?(rebuilt.events, &(&1.kind == :module and &1.target == "CompilerFixture"))

    cache_entries =
      cache
      |> Path.join("elixir/Zml4dHVyZQ/trace-cache-*.term")
      |> Path.wildcard()
      |> Enum.map(&{&1, File.read!(&1)})

    TraceCache.clear()
    assert TraceCache.all_events(trace_cache) == []
    marker = Path.join(root, "invalid-probe-compiled")
    invalid_mix = fake_mix(root, "touch #{shell_quote(marker)}", "invalid", "invalid-mix")

    with_env("BEHOLDER_ELIXIR_MIX_PATH", invalid_mix, fn ->
      assert {:error, reason} = Compiler.run(repository, cache)
      assert reason =~ "required=~> 1.15"
      assert reason =~ "could not parse Mix version"
    end)

    refute File.exists?(marker)
    assert TraceCache.all_events(trace_cache) == []

    assert cache_entries ==
             cache
             |> Path.join("elixir/Zml4dHVyZQ/trace-cache-*.term")
             |> Path.wildcard()
             |> Enum.map(&{&1, File.read!(&1)})
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

    refute Enum.any?(second.events, fn event ->
             event.kind == :module and event.target == "IncrementalFixture.Unchanged" and
               {"call", 0} in event.definitions
           end)

    assert Enum.any?(Compiler.complete_events(second), fn event ->
             event.kind == :module and event.target == "IncrementalFixture.Unchanged" and
               {"call", 0} in event.definitions
           end)

    assert Enum.any?(second.events, fn event ->
             event.kind == :module and event.target == "IncrementalFixture.Changed" and
               {"second", 0} in event.definitions
           end)

    assert Enum.any?(second.changed_events, &(&1[:target] == "IncrementalFixture.Changed"))
    refute Enum.any?(second.changed_events, &(&1[:target] == "IncrementalFixture.Unchanged"))

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

  test "fetches and compiles an unavailable dependency into the persistent cache" do
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
    assert result.status == :ok, inspect(result)

    assert Enum.any?(
             result.events,
             &(&1.kind == :remote_function and &1.target == "CompilerDependency")
           )

    assert File.dir?(Path.join([cache, "elixir", "Zml4dHVyZQ", "deps", "compiler_dependency"]))
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
      wait_for_file(marker, 500)
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
      fake_metadata_mix(
        root,
        "printf '%0256d' 0\n(trap '' TERM; sleep 30) &\necho $! > #{shell_quote(child_pid)}\nwait",
        :none,
        "27.3.4",
        "1.20.3"
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

  test "rejects an exact pin before compiling with an incompatible ambient Mix" do
    root = temp_dir("exact-toolchain")
    marker = Path.join(root, "compiled")
    mix = fake_mix(root, "touch #{shell_quote(marker)}", "1.20.4", "mix")
    repository = toolchain_repository(root, "== 1.20.3")

    with_env("PATH", prepend_path(Path.dirname(mix)), fn ->
      assert {:error, reason} = Compiler.run(repository, temp_dir("exact-toolchain-cache"))
      assert reason =~ "required=== 1.20.3"
      assert reason =~ "actual=1.20.4"
      assert reason =~ "selection_source=ambient PATH"
    end)

    refute File.exists?(marker)
  end

  test "reports a missing ambient Mix with the project requirement" do
    root = temp_dir("missing-ambient-toolchain")
    bin = temp_dir("missing-ambient-bin")
    File.ln_s!(System.find_executable("pwd"), Path.join(bin, "pwd"))
    repository = toolchain_repository(root, "== 1.20.3")

    with_envs(%{"PATH" => bin, "BEHOLDER_ELIXIR_MIX_PATH" => ""}, fn ->
      assert {:error, reason} = Compiler.run(repository, temp_dir("missing-ambient-cache"))
      assert reason =~ "required=== 1.20.3"
      assert reason =~ "actual=unavailable"
      assert reason =~ "selected_executable=unavailable"
      assert reason =~ "selection_source=ambient PATH"
      assert reason =~ "mix executable not found"
    end)
  end

  test "trusts only the materialized mise config and disables installation" do
    root = temp_dir("mise-toolchain")
    marker = Path.join(root, "compiled")
    selected = fake_mix(root, "touch #{shell_quote(marker)}", "1.20.3")
    mise = fake_mise(root, selected)
    repository = toolchain_repository(root, "== 1.20.3", "elixir = \"1.20.3\"")

    with_env("PATH", prepend_path(Path.dirname(mise)), fn ->
      assert {:error, reason} = Compiler.run(repository, temp_dir("mise-toolchain-cache"))
      assert reason =~ "before producing a trace"
    end)

    assert File.exists?(marker)
  end

  test "accepts TOML dotted and single-quoted Elixir tool declarations" do
    for {name, config} <- [
          {"dotted", "tools.elixir = \"1.20.3\""},
          {"quoted", "[tools]\n'elixir' = \"1.20.3\""}
        ] do
      root = temp_dir("mise-toml-#{name}")
      ambient_marker = Path.join(root, "ambient-compiled")
      selected_marker = Path.join(root, "selected-compiled")
      fake_mix(root, "touch #{shell_quote(ambient_marker)}", "1.20.3", "mix")
      selected = fake_mix(root, "touch #{shell_quote(selected_marker)}", "1.20.3", "selected-mix")
      mise = fake_mise(root, selected)
      repository = toolchain_repository(root, "== 1.20.3", config)

      with_env("PATH", prepend_path(Path.dirname(mise)), fn ->
        assert {:error, reason} = Compiler.run(repository, temp_dir("mise-toml-cache-#{name}"))
        assert reason =~ "before producing a trace"
      end)

      assert File.exists?(selected_marker)
      refute File.exists?(ambient_marker)
    end
  end

  test "keeps mise stderr diagnostics separate from selection outputs" do
    root = temp_dir("mise-stderr")
    marker = Path.join(root, "compiled")
    selected = fake_mix(root, "touch #{shell_quote(marker)}", "1.20.3")
    mise = fake_mise(root, selected, "mise warning\n")
    repository = toolchain_repository(root, "== 1.20.3", "elixir = \"1.20.3\"")

    with_env("PATH", prepend_path(Path.dirname(mise)), fn ->
      assert {:error, reason} = Compiler.run(repository, temp_dir("mise-stderr-cache"))
      assert reason =~ "before producing a trace"
    end)

    assert File.exists?(marker)
  end

  test "uses ambient Mix when an unrelated mise config exists without mise" do
    root = temp_dir("unrelated-mise-config")
    bin = temp_dir("unrelated-mise-bin")
    marker = Path.join(root, "compiled")

    for executable <- ["cat", "head", "pwd", "sh"] do
      File.ln_s!(System.find_executable(executable), Path.join(bin, executable))
    end

    fake_metadata_mix(
      bin,
      ": > #{shell_quote(marker)}",
      {:literal, "== 1.20.3"},
      "27.3.4",
      "1.20.3",
      "mix"
    )

    repository = toolchain_repository(root, "== 1.20.3", "tools.node = \"24\"")

    with_envs(%{"PATH" => bin, "BEHOLDER_ELIXIR_MIX_PATH" => ""}, fn ->
      assert {:error, reason} = Compiler.run(repository, temp_dir("unrelated-mise-cache"))
      assert reason =~ "before producing a trace"
    end)

    assert File.exists?(marker)
  end

  test "reapplies compiler invariants after mise environment activation" do
    root = temp_dir("mise-compiler-environment")
    marker = Path.join(root, "compiled")

    body =
      "[ \"$MIX_ENV\" = dev ] && [ \"$MIX_BUILD_PATH\" != /tmp/evil ] && [ \"$MIX_DEPS_PATH\" != /tmp/evil ] && [ \"$BEHOLDER_ELIXIR_TRACE_RESULT\" != /tmp/evil ] && [ \"$BEHOLDER_ELIXIR_FORCE_COMPILE\" = true ] && [ \"$ERL_AFLAGS\" != evil ] && touch #{shell_quote(marker)}"

    selected = fake_mix(root, body, "1.20.3")

    activation =
      "export MIX_ENV=prod MIX_BUILD_PATH=/tmp/evil MIX_DEPS_PATH=/tmp/evil BEHOLDER_ELIXIR_TRACE_RESULT=/tmp/evil BEHOLDER_ELIXIR_FORCE_COMPILE=false ERL_AFLAGS=evil"

    mise = fake_mise(root, selected, "", activation)
    repository = toolchain_repository(root, "== 1.20.3", "elixir = \"1.20.3\"")

    with_env("PATH", prepend_path(Path.dirname(mise)), fn ->
      assert {:error, reason} = Compiler.run(repository, temp_dir("mise-environment-cache"))
      assert reason =~ "before producing a trace"
    end)

    assert File.exists?(marker)
  end

  test "reports an unavailable selected mise runtime without ambient fallback" do
    root = temp_dir("missing-mise-toolchain")
    mise = fake_mise(root, nil)
    repository = toolchain_repository(root, "== 1.20.3", "elixir = \"1.20.3\"")

    with_env("PATH", prepend_path(Path.dirname(mise)), fn ->
      assert {:error, reason} = Compiler.run(repository, temp_dir("missing-mise-toolchain-cache"))
      assert reason =~ "actual=unavailable"
      assert reason =~ "selection_source=project mise config"
      assert reason =~ "configuration=mise.toml"
    end)
  end

  test "does not bypass an incompatible explicit override for a matching project runtime" do
    root = temp_dir("explicit-toolchain")
    configured_marker = Path.join(root, "configured-compiled")
    override = fake_mix(root, "true", "1.20.4")
    selected = fake_mix(root, "touch #{shell_quote(configured_marker)}", "1.20.3", "selected-mix")
    mise = fake_mise(root, selected)
    repository = toolchain_repository(root, "== 1.20.3", "elixir = \"1.20.3\"")

    with_envs(
      %{"PATH" => prepend_path(Path.dirname(mise)), "BEHOLDER_ELIXIR_MIX_PATH" => override},
      fn ->
        assert {:error, reason} = Compiler.run(repository, temp_dir("explicit-toolchain-cache"))
        assert reason =~ "selection_source=BEHOLDER_ELIXIR_MIX_PATH"
        assert reason =~ "actual=1.20.4"
      end
    )

    refute File.exists?(configured_marker)
  end

  test "accepts a permissive requirement with a newer ambient Mix patch" do
    root = temp_dir("permissive-toolchain")
    marker = Path.join(root, "compiled")
    mix = fake_mix(root, "touch #{shell_quote(marker)}", "1.20.4", "mix")
    repository = toolchain_repository(root, "~> 1.20.3")

    with_env("PATH", prepend_path(Path.dirname(mix)), fn ->
      assert {:error, reason} = Compiler.run(repository, temp_dir("permissive-toolchain-cache"))
      assert reason =~ "before producing a trace"
    end)

    assert File.exists?(marker)
  end

  test "uses a captured ancestor mise configuration for a nested Mix project" do
    root = temp_dir("nested-toolchain")
    mix = fake_mix(root, "true", "1.20.3")
    mise = fake_mise(root, mix)

    manifest =
      "defmodule Nested.MixProject do\n  use Mix.Project\n  def project, do: [app: :nested, version: \"0.1.0\", elixir: \"== 1.20.3\"]\nend\n"

    repository = %Repository{
      identity: "fixture",
      base: root,
      fingerprint: "nested-toolchain",
      inputs: [
        %{path: "apps/mise.toml", content: "elixir = \"1.20.3\"", kind: :INPUT_KIND_TOOLCHAIN},
        %{path: "apps/foo/mix.exs", content: manifest, kind: :INPUT_KIND_SOURCE}
      ]
    }

    with_env("PATH", prepend_path(Path.dirname(mise)), fn ->
      assert {:error, reason} = Compiler.run(repository, temp_dir("nested-toolchain-cache"))
      assert reason =~ "before producing a trace"
    end)
  end

  test "rejects a dynamic project requirement before invoking Mix" do
    root = temp_dir("dynamic-toolchain")
    marker = Path.join(root, "compiled")
    mix = fake_mix(root, "touch #{shell_quote(marker)}")

    source =
      "defmodule Dynamic.MixProject do\n  use Mix.Project\n  def project, do: [app: :dynamic, version: \"0.1.0\", elixir: System.get_env(\"ELIXIR_VERSION\")]\nend\n"

    repository = %Repository{
      identity: "fixture",
      base: root,
      fingerprint: "dynamic",
      inputs: [%{path: "mix.exs", content: source, kind: :INPUT_KIND_SOURCE}]
    }

    with_env("BEHOLDER_ELIXIR_MIX_PATH", mix, fn ->
      assert {:error, reason} = Compiler.run(repository, temp_dir("dynamic-toolchain-cache"))
      assert reason =~ "non-literal elixir requirement"
    end)

    refute File.exists?(marker)
  end

  test "ignores nested elixir alias entries when the project has no runtime requirement" do
    root = temp_dir("nested-elixir-alias")
    marker = Path.join(root, "compiled")
    mix = fake_mix(root, "touch #{shell_quote(marker)}")

    source =
      "defmodule Alias.MixProject do\n  use Mix.Project\n  def project, do: [app: :alias, version: \"0.1.0\", aliases: [elixir: \"run scripts/tool.exs\"]]\nend\n"

    repository = %Repository{
      identity: "fixture",
      base: root,
      fingerprint: "nested-elixir-alias",
      inputs: [%{path: "mix.exs", content: source, kind: :INPUT_KIND_SOURCE}]
    }

    with_env("BEHOLDER_ELIXIR_MIX_PATH", mix, fn ->
      assert {:error, reason} = Compiler.run(repository, temp_dir("nested-elixir-alias-cache"))
      assert reason =~ "before producing a trace"
    end)

    assert File.exists?(marker)
  end

  test "reads the requirement only from the module using Mix.Project" do
    root = temp_dir("mix-project-module")
    marker = Path.join(root, "compiled")
    mix = fake_mix(root, "touch #{shell_quote(marker)}", "1.20.4")

    source = """
    defmodule Helper do
      def project, do: [version: "0.1.0"]
    end

    defmodule Actual.MixProject do
      use Mix.Project
      def project, do: [app: :actual, version: "0.1.0", elixir: "== 1.20.3"]
    end
    """

    repository = %Repository{
      identity: "fixture",
      base: root,
      fingerprint: "mix-project-module",
      inputs: [%{path: "mix.exs", content: source, kind: :INPUT_KIND_SOURCE}]
    }

    with_env("BEHOLDER_ELIXIR_MIX_PATH", mix, fn ->
      assert {:error, reason} = Compiler.run(repository, temp_dir("mix-project-module-cache"))
      assert reason =~ "required=== 1.20.3"
      assert reason =~ "actual=1.20.4"
    end)

    refute File.exists?(marker)
  end

  test "uses the selected runtime for project metadata and tracer helpers" do
    root = temp_dir("selected-parser")
    marker = Path.join(root, "compiled")

    mix =
      fake_metadata_mix(
        root,
        "touch #{shell_quote(marker)}",
        {:literal, "== 1.20.3"},
        "erts-16.0",
        "1.20.3"
      )

    source = "syntax only the selected future runtime understands"

    repository = %Repository{
      identity: "fixture",
      base: root,
      fingerprint: "selected-parser",
      inputs: [%{path: "mix.exs", content: source, kind: :INPUT_KIND_SOURCE}]
    }

    with_env("BEHOLDER_ELIXIR_MIX_PATH", mix, fn ->
      assert {:error, reason} = Compiler.run(repository, temp_dir("selected-parser-cache"))
      assert reason =~ "before producing a trace"
    end)

    assert File.exists?(marker)
    assert File.exists?(Path.join(root, "fake-mix-helpers"))
  end

  test "fails closed when only the selected runtime can parse an absolute local path" do
    root = temp_dir("selected-path-parser")
    marker = Path.join(root, "compiled")

    mix =
      fake_metadata_mix(
        root,
        "touch #{shell_quote(marker)}",
        {:literal, "== 1.20.3"},
        "erts-16.0",
        "1.20.3",
        "fake-mix",
        nil,
        {:error, "selected mix.exs", "/live/external"}
      )

    repository = %Repository{
      identity: "fixture",
      base: root,
      fingerprint: "selected-path-parser",
      inputs: [
        %{
          path: "mix.exs",
          content: "new_runtime_syntax(path: \"/live/external\")",
          kind: :INPUT_KIND_SOURCE
        }
      ]
    }

    with_env("BEHOLDER_ELIXIR_MIX_PATH", mix, fn ->
      assert {:error, reason} = Compiler.run(repository, temp_dir("selected-path-parser-cache"))
      assert reason =~ "declares absolute local path /live/external"
    end)

    refute File.exists?(marker)
  end

  test "partitions compiler cache by the probed runtime" do
    root = temp_dir("runtime-cache")
    cache = temp_dir("runtime-cache-cache")
    first = fake_mix(root, "mkdir -p \"$MIX_BUILD_PATH\"", "1.20.3", "mix")
    repository = toolchain_repository(root, "~> 1.20.3")

    with_env("BEHOLDER_ELIXIR_MIX_PATH", first, fn ->
      assert {:error, _} = Compiler.run(repository, cache)
    end)

    second = fake_mix(root, "mkdir -p \"$MIX_BUILD_PATH\"", "1.20.4", "mix")

    with_env("BEHOLDER_ELIXIR_MIX_PATH", second, fn ->
      assert {:error, _} = Compiler.run(repository, cache)
    end)

    assert 2 == cache |> Path.join("elixir/Zml4dHVyZQ/build-*") |> Path.wildcard() |> length()
  end

  test "partitions compiler cache by the full selected Erlang runtime" do
    root = temp_dir("erlang-runtime-cache")
    cache = temp_dir("erlang-runtime-cache-cache")
    metadata = Path.join(root, "metadata")

    mix =
      fake_metadata_mix(
        root,
        "mkdir -p \"$MIX_BUILD_PATH\"",
        {:literal, "~> 1.20.3"},
        "erts-15.2.2",
        "1.20.3",
        "mix",
        metadata
      )

    repository = toolchain_repository(root, "~> 1.20.3")

    with_env("BEHOLDER_ELIXIR_MIX_PATH", mix, fn ->
      assert {:error, _reason} = Compiler.run(repository, cache)
      write_metadata(metadata, {:literal, "~> 1.20.3"}, "erts-15.2.3")
      assert {:error, _reason} = Compiler.run(repository, cache)
    end)

    assert 2 == cache |> Path.join("elixir/Zml4dHVyZQ/build-*") |> Path.wildcard() |> length()
  end

  test "partitions compiler cache when the selected mise environment changes" do
    root = temp_dir("mise-environment-cache")
    cache = temp_dir("mise-environment-cache-cache")
    invocations = Path.join(root, "invocations")
    result = Path.join(root, "result")
    requirement = "== #{System.version()}"

    File.write!(
      result,
      :erlang.term_to_binary(%{
        "status" => "ok",
        "diagnostics" => [],
        "events" => [],
        "elixir_version" => System.version(),
        "otp_release" => to_string(:erlang.system_info(:otp_release))
      })
    )

    mix =
      fake_metadata_mix(
        root,
        "mkdir -p \"$MIX_BUILD_PATH\"\nprintf '%s:%s:%s\\n' \"$BEHOLDER_ELIXIR_FORCE_COMPILE\" \"$BEHOLDER_TEST_ENV\" \"$MIX_BUILD_PATH\" >> #{shell_quote(invocations)}\ncp #{shell_quote(result)} \"$BEHOLDER_ELIXIR_TRACE_RESULT\"",
        {:literal, requirement},
        "erts-15.2.3",
        System.version()
      )

    mise = fake_mise(root, mix, "", "export BEHOLDER_TEST_ENV=\"$FEATURE_FLAG\"")

    repository =
      toolchain_repository(
        root,
        requirement,
        "[tools]\nelixir = \"#{System.version()}\"\n[env]\nBEHOLDER_TEST_ENV = \"{{env.FEATURE_FLAG}}\""
      )

    with_env("PATH", prepend_path(Path.dirname(mise)), fn ->
      with_env("FEATURE_FLAG", "one", fn ->
        assert {:ok, _result} = Compiler.run(repository, cache)
        assert {:ok, _result} = Compiler.run(repository, cache)
      end)

      with_env("FEATURE_FLAG", "two", fn ->
        assert {:ok, _result} = Compiler.run(repository, cache)
      end)
    end)

    [first, warm, changed] = invocations |> File.read!() |> String.split()
    ["true", "one", first_build] = String.split(first, ":", parts: 3)
    ["false", "one", ^first_build] = String.split(warm, ":", parts: 3)
    ["true", "two", changed_build] = String.split(changed, ":", parts: 3)
    refute changed_build == first_build

    assert 2 == cache |> Path.join("elixir/Zml4dHVyZQ/build-*") |> Path.wildcard() |> length()

    assert 2 ==
             cache
             |> Path.join("elixir/Zml4dHVyZQ/trace-cache-*.term")
             |> Path.wildcard()
             |> length()
  end

  defp toolchain_repository(root, requirement, config \\ nil) do
    mix_source =
      "defmodule Toolchain.MixProject do\n  use Mix.Project\n  def project, do: [app: :toolchain, version: \"0.1.0\", elixir: \"#{requirement}\"]\nend\n"

    inputs = [%{path: "mix.exs", content: mix_source, kind: :INPUT_KIND_SOURCE}]

    inputs =
      if config,
        do: inputs ++ [%{path: "mise.toml", content: config, kind: :INPUT_KIND_TOOLCHAIN}],
        else: inputs

    %Repository{identity: "fixture", base: root, fingerprint: requirement, inputs: inputs}
  end

  defp fake_mix(root, body, version \\ "1.20.3", name \\ "fake-mix") do
    path = Path.join(root, name)
    real_elixir = System.find_executable("elixir")

    File.write!(
      path,
      "#!/bin/sh\nset -eu\nif [ \"${1:-}\" = --version ]; then\n  printf 'Erlang/OTP 29\\n\\nMix #{version} (compiled with Erlang/OTP 29)\\n'\n  exit 0\nfi\nif [ \"${1:-}\" = run ]; then while [ \"$1\" != -e ]; do shift; done; exec #{shell_quote(real_elixir)} \"$@\"; fi\n#{body}\n"
    )

    File.chmod!(path, 0o755)
    path
  end

  defp fake_metadata_mix(
         root,
         body,
         requirement,
         otp,
         version,
         name \\ "fake-mix",
         metadata \\ nil,
         validation \\ :ok,
         helper_marker \\ nil
       ) do
    path = Path.join(root, name)
    metadata = metadata || Path.join(root, "#{name}-metadata")
    validation_path = Path.join(root, "#{name}-validation")
    helper_marker = helper_marker || Path.join(root, "#{name}-helpers")
    write_metadata(metadata, requirement, otp)
    write_validation(validation_path, validation)

    File.write!(
      path,
      "#!/bin/sh\nset -eu\nif [ \"${1:-}\" = --version ]; then printf 'Erlang/OTP 29\\n\\nMix #{version} (compiled with Erlang/OTP 29)\\n'; exit 0; fi\nif [ \"${1:-}\" = run ]; then mode=; for arg in \"$@\"; do case \"$arg\" in metadata|paths|helpers) mode=$arg;; esac; done; case \"$mode\" in metadata) cat #{shell_quote(metadata)};; paths) cat #{shell_quote(validation_path)};; helpers) : > #{shell_quote(helper_marker)};; esac; exit 0; fi\n#{body}\n"
    )

    File.chmod!(path, 0o755)
    path
  end

  defp write_metadata(path, requirement, otp) do
    encoded = {:ok, requirement, otp} |> :erlang.term_to_binary() |> Base.encode64()
    File.write!(path, "BEHOLDER_PROJECT_METADATA #{encoded}\n")
  end

  defp write_validation(path, result) do
    encoded = result |> :erlang.term_to_binary() |> Base.encode64()
    File.write!(path, "BEHOLDER_LOCAL_PATHS #{encoded}\n")
  end

  defp fake_mise(root, mix, stderr \\ "", activation \\ "")

  defp fake_mise(root, nil, _stderr, _activation) do
    path = Path.join(root, "mise")
    File.write!(path, "#!/bin/sh\nexit 1\n")
    File.chmod!(path, 0o755)
    path
  end

  defp fake_mise(root, mix, stderr, activation) do
    path = Path.join(root, "mise")
    elixir = System.find_executable("elixir")

    File.write!(
      path,
      "#!/bin/sh\nset -eu\n[ \"$MISE_SAFE\" = 1 ]\n[ \"$MISE_AUTO_INSTALL\" = false ]\n[ \"$MISE_EXEC_AUTO_INSTALL\" = false ]\nif [ \"$1\" = ls ]; then source=\"$PWD/mise.toml\"; [ -f \"$source\" ] || source=\"$PWD/../mise.toml\"; source=$(cd \"$(dirname \"$source\")\" && pwd -P)/$(basename \"$source\"); [ \"$MISE_TRUSTED_CONFIG_PATHS\" = \"$source\" ]; printf '%s' #{shell_quote(stderr)} >&2; printf '[{\"installed\":true,\"source\":{\"path\":\"%s\"}}]\\n' \"$source\"; exit 0; fi\nif [ \"$1\" = which ]; then printf '%s' #{shell_quote(stderr)} >&2; if [ \"$2\" = mix ]; then printf '%s\\n' #{shell_quote(mix)}; else printf '%s\\n' #{shell_quote(elixir)}; fi; exit 0; fi\nif [ \"$1\" = env ]; then printf '{\"FEATURE_FLAG\":\"%s\"}\\n' \"${FEATURE_FLAG:-}\"; exit 0; fi\n#{activation}\nshift 2\nexec \"$@\"\n"
    )

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

  defp prepend_path(path), do: path <> ":" <> System.get_env("PATH", "")

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
