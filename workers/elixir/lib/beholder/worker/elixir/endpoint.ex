defmodule Beholder.Worker.Elixir.Endpoint do
  use GRPC.Endpoint

  run(Beholder.Worker.V1.AnalyzerWorker.Server)
end
