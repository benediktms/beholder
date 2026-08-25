defmodule Beholder.Worker.V1.AnalyzerWorker.Server do
  @moduledoc false
  use GRPC.Server, service: Beholder.Worker.V1.AnalyzerWorker.Service

  alias Beholder.Worker.Elixir.{Analyzer, Observability, Snapshot}

  alias Beholder.Worker.V1.{
    AnalysisFailure,
    AnalysisProgress,
    AnalyzeEvent
  }

  @heartbeat_interval_ms 30_000

  def analyze(requests, stream) do
    stream
    |> GRPC.Stream.get_headers()
    |> Observability.with_server_span(fn -> analyze_with_span(requests, stream) end)
  end

  defp analyze_with_span(requests, stream) do
    send_progress(stream, :ANALYSIS_PHASE_RECEIVING_SNAPSHOT)

    result =
      with {:ok, snapshot} <- Snapshot.from_requests(requests) do
        Observability.set_attributes(%{
          "workspace" => snapshot.name,
          "repository.count" => length(Snapshot.repositories(snapshot))
        })

        send_progress(stream, :ANALYSIS_PHASE_ANALYZING, "materializing compiler snapshot")

        heartbeat = Task.async(fn -> heartbeat(stream) end)

        try do
          Analyzer.analyze(snapshot, cache_dir(), fn detail ->
            send_progress(stream, :ANALYSIS_PHASE_ANALYZING, detail)
          end)
        after
          send(heartbeat.pid, :stop)
          Task.await(heartbeat)
        end
      end

    case result do
      {:ok, events} ->
        Enum.each(events, &GRPC.Server.send_reply(stream, &1))

      {:error, reason} ->
        Observability.set_error(reason)
        GRPC.Server.send_reply(stream, failure(reason))
    end
  rescue
    error ->
      Observability.record_exception(error, __STACKTRACE__)
      Observability.set_error(error)
      GRPC.Server.send_reply(stream, failure(Exception.format(:error, error, __STACKTRACE__)))
  end

  defp send_progress(stream, phase, detail \\ nil) do
    GRPC.Server.send_reply(
      stream,
      %AnalyzeEvent{event: {:progress, %AnalysisProgress{phase: phase, detail: detail}}}
    )
  end

  defp heartbeat(stream) do
    receive do
      :stop ->
        :ok
    after
      @heartbeat_interval_ms ->
        send_progress(stream, :ANALYSIS_PHASE_ANALYZING, "analysis still running")
        heartbeat(stream)
    end
  end

  defp failure(reason) do
    %AnalyzeEvent{
      event: {:failure, %AnalysisFailure{code: "elixir.worker_failed", message: reason}}
    }
  end

  defp cache_dir do
    Application.fetch_env!(:beholder_worker_elixir, :cache_dir)
  end
end
