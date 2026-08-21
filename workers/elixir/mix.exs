defmodule BeholderWorkerElixir.MixProject do
  use Mix.Project

  def project do
    [
      app: :beholder_worker_elixir,
      version: "0.1.0",
      elixir: "~> 1.15",
      start_permanent: Mix.env() == :prod,
      escript: [main_module: Beholder.Worker.Elixir.CLI, name: "beholder-worker-elixir"],
      deps: deps()
    ]
  end

  def application do
    [
      extra_applications: [:logger],
      included_applications: [:opentelemetry, :opentelemetry_exporter]
    ]
  end

  defp deps do
    [
      {:grpc, "~> 1.0"},
      {:grpc_server, "~> 1.0"},
      {:opentelemetry, "~> 1.5"},
      {:opentelemetry_api, "~> 1.4"},
      {:opentelemetry_exporter, "~> 1.9"},
      {:protobuf, "~> 0.14"},
      {:protobuf_generate, "~> 0.2.1", only: [:dev, :test], runtime: false}
    ]
  end
end
