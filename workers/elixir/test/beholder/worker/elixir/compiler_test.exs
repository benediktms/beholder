defmodule Beholder.Worker.Elixir.CompilerTest do
  use ExUnit.Case, async: false

  alias Beholder.Worker.Elixir.Compiler
  alias Beholder.Worker.Elixir.Snapshot.Repository

  test "compiles a Mix project in a child VM and returns trace events" do
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

    assert {:ok, result} = Compiler.run(repository, cache)
    assert result.status == :ok, inspect(result)
    assert Enum.any?(result.events, &(&1.kind == :remote_function and &1.target == "Enum"))

    assert Enum.any?(result.events, fn event ->
             event.kind == :module and {"call", 1} in event.definitions
           end)

    assert [_build] = Path.wildcard(Path.join([cache, "elixir", "Zml4dHVyZQ", "build-*"]))
  end

  test "rejects a checkout that no longer matches the snapshot" do
    root = temp_dir("changed")
    File.write!(Path.join(root, "mix.exs"), "changed")

    repository = %Repository{
      identity: "fixture",
      base: root,
      fingerprint: "abc123",
      inputs: [%{path: "mix.exs", content: "original", kind: :INPUT_KIND_SOURCE}]
    }

    assert {:error, "mix.exs changed after the immutable snapshot was created"} =
             Compiler.run(repository, temp_dir("unused-cache"))
  end

  test "rejects dependency context that no longer matches the snapshot" do
    target_root = temp_dir("context-target")
    context_root = temp_dir("context-dependency")
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

    assert {:error, "mix.exs changed after the immutable snapshot was created"} =
             Compiler.run(target, [context], temp_dir("unused-context-cache"))
  end

  test "rejects a checkout changed while the compiler is running" do
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

      assert {:error, "mix.exs changed after the immutable snapshot was created"} =
               Task.await(task, 5_000)
    end)
  end

  test "terminates an overdue compiler process and bounds its captured output" do
    root = temp_dir("timeout")
    cache = temp_dir("timeout-cache")
    child_pid = Path.join(root, "child-pid")

    fake_mix =
      fake_mix(
        root,
        "yes compiler-output | head -c 4096\n(trap '' TERM; sleep 30) &\necho $! > #{shell_quote(child_pid)}\nwait"
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
        "BEHOLDER_ELIXIR_COMPILER_TIMEOUT_MS" => "200",
        "BEHOLDER_ELIXIR_MAX_OUTPUT_BYTES" => "128"
      },
      fn ->
        assert {:error, reason} = Compiler.run(repository, cache)
        assert reason =~ "exceeded 200ms and was terminated"
        assert reason =~ "[compiler output truncated]"
        assert byte_size(reason) < 512
        wait_for_file(child_pid)

        pid = child_pid |> File.read!() |> String.trim()
        wait_for_process_exit(pid)
      end
    )
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

  test "isolates Mix build directories by dependency context identity" do
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

    assert 2 ==
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
