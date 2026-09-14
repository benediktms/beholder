defmodule Beholder.Worker.Elixir.AnalyzerTest do
  use ExUnit.Case, async: true

  alias Beholder.Worker.Elixir.Analyzer
  alias Beholder.Worker.Elixir.Snapshot
  alias Beholder.Worker.Elixir.Snapshot.Repository
  alias Beholder.Worker.V1.{FactShard, Observation, RepositoryContribution}

  test "chunks large repository contributions at the protocol boundary" do
    observations =
      Enum.map(1..2_049, fn index ->
        %Observation{
          from: "from-#{index}",
          relation: :RELATION_KIND_CALLS,
          to: "to-#{index}",
          evidence: "lib/example.ex:#{index}",
          confidence: :CONFIDENCE_EXACT,
          provenance: :PROVENANCE_COMPILER
        }
      end)

    contribution = %RepositoryContribution{
      repository: "example",
      completeness: :ANALYSIS_COMPLETENESS_COMPLETE,
      fact_shards: [
        %FactShard{
          repository: "example",
          producer: "elixir",
          owner: "repo://example/elixir-source/lib/example.ex",
          version: "semantic-1",
          observations: observations
        }
      ],
      replaced_diagnostic_codes: ["elixir.macro_expansion_incomplete"]
    }

    assert [first, second] = contribution |> Analyzer.contribution_chunks() |> Enum.to_list()
    assert [%{observations: first_observations}] = first.fact_shards
    assert [%{observations: second_observations}] = second.fact_shards
    assert length(first_observations) == 2_048
    assert length(second_observations) == 1
    assert first.repository == second.repository
    assert first.completeness == second.completeness
    assert first.replaced_diagnostic_codes == ["elixir.macro_expansion_incomplete"]
    assert second.replaced_diagnostic_codes == []
  end

  test "analyzer code identity is independent of declared runtime inputs" do
    assert Analyzer.metadata_version({"1.20.3", "29"}) == "25:14:elixir-compiler:26"
  end

  test "turns a toolchain preflight failure into an incomplete diagnostic" do
    source =
      "defmodule Dynamic.MixProject do\n  use Mix.Project\n  def project, do: [app: :dynamic, version: \"0.1.0\", elixir: System.get_env(\"ELIXIR_VERSION\")]\nend\n"

    root = System.tmp_dir!()

    repository = %Repository{
      identity: "fixture",
      base: root,
      fingerprint: "dynamic",
      inputs: [%{path: "mix.exs", content: source, kind: :INPUT_KIND_SOURCE}]
    }

    snapshot = %Snapshot{
      repositories: %{repository.identity => repository},
      target_repository: repository.identity
    }

    assert {:ok, events} = Analyzer.analyze(snapshot, Path.join(root, "beholder-analyzer-test"))
    [%{event: {:repository, contribution}} | _] = Enum.to_list(events)
    assert contribution.completeness == :ANALYSIS_COMPLETENESS_INCOMPLETE
    assert [%{code: "elixir.compiler.unavailable", detail: detail}] = contribution.diagnostics
    assert detail =~ "non-literal elixir requirement"
  end
end
