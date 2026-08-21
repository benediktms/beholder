defmodule Beholder.Worker.Elixir.AnalyzerTest do
  use ExUnit.Case, async: true

  alias Beholder.Worker.Elixir.Analyzer
  alias Beholder.Worker.V1.{Observation, RepositoryContribution}

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
      observations: observations
    }

    assert [first, second] = Analyzer.contribution_chunks(contribution)
    assert length(first.observations) == 2_048
    assert length(second.observations) == 1
    assert first.repository == second.repository
    assert first.completeness == second.completeness
  end

  test "analyzer identity includes Mix, Elixir, and OTP runtime inputs" do
    previous = System.get_env("BEHOLDER_ELIXIR_MIX_ENV")

    on_exit(fn ->
      if previous,
        do: System.put_env("BEHOLDER_ELIXIR_MIX_ENV", previous),
        else: System.delete_env("BEHOLDER_ELIXIR_MIX_ENV")
    end)

    System.put_env("BEHOLDER_ELIXIR_MIX_ENV", "test")
    version = Analyzer.metadata_version({"1.20.3", "29"})

    assert version =~ ":mix-test:"
    assert version =~ ":elixir-1.20.3:"
    assert version =~ ":otp-29"
  end
end
