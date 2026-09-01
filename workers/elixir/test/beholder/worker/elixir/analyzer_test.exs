defmodule Beholder.Worker.Elixir.AnalyzerTest do
  use ExUnit.Case, async: true

  alias Beholder.Worker.Elixir.Analyzer
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
    assert Analyzer.metadata_version({"1.20.3", "29"}) == "21:11:elixir-compiler:17"
  end
end
