defmodule Beholder.Worker.Elixir.SnapshotTest do
  use ExUnit.Case, async: true

  alias Beholder.Worker.Elixir.Snapshot

  alias Beholder.Worker.V1.{
    AnalysisFinish,
    AnalysisStart,
    AnalyzeRequest,
    RepositoryInput,
    RepositoryStart
  }

  test "builds a validated snapshot" do
    requests = [
      %AnalyzeRequest{request: {:start, %AnalysisStart{workspace: "example"}}},
      %AnalyzeRequest{
        request:
          {:repository,
           %RepositoryStart{
             identity: "repo",
             base: "/tmp/repo",
             fingerprint: "abc",
             target: true
           }}
      },
      %AnalyzeRequest{
        request:
          {:input,
           %RepositoryInput{
             repository: "repo",
             path: "lib/example.ex",
             content: "defmodule Example, do: nil",
             kind: :INPUT_KIND_SOURCE
           }}
      },
      %AnalyzeRequest{request: {:finish, %AnalysisFinish{}}}
    ]

    assert {:ok, snapshot} = Snapshot.from_requests(requests)
    assert snapshot.name == "example"
    assert snapshot.target_repository == "repo"

    assert [%{identity: "repo", inputs: [%{path: "lib/example.ex"}]}] =
             Snapshot.repositories(snapshot)
  end

  test "rejects input before its repository" do
    requests = [
      %AnalyzeRequest{request: {:start, %AnalysisStart{workspace: "example"}}},
      %AnalyzeRequest{
        request:
          {:input,
           %RepositoryInput{
             repository: "missing",
             path: "lib/example.ex",
             content: "",
             kind: :INPUT_KIND_SOURCE
           }}
      }
    ]

    assert {:error, "worker input references an unknown repository"} =
             Snapshot.from_requests(requests)
  end
end
