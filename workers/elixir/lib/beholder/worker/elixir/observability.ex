defmodule Beholder.Worker.Elixir.Observability do
  @moduledoc false

  require OpenTelemetry.Tracer, as: Tracer

  @default_service_name "beholder-worker-elixir"
  @endpoint_variables ["OTEL_EXPORTER_OTLP_ENDPOINT", "OTEL_EXPORTER_OTLP_TRACES_ENDPOINT"]

  @spec start() :: :ok | {:error, term()}
  def start do
    ensure_service_name()

    Application.put_env(:opentelemetry, :span_processor, :simple)

    if export_enabled?() do
      with :ok <- start_application(:opentelemetry_exporter),
           :ok <- start_application(:opentelemetry) do
        :ok
      end
    else
      Application.put_env(:opentelemetry, :traces_exporter, :none)
      start_application(:opentelemetry)
    end
  end

  @spec with_server_span(map() | [{binary(), binary()}], (-> result)) :: result
        when result: term()
  def with_server_span(headers, operation) do
    token =
      headers
      |> Enum.map(fn {key, value} -> {to_string(key), to_string(value)} end)
      |> :otel_propagator_text_map.extract()

    try do
      Tracer.with_span "worker.analyze",
                       %{
                         kind: :server,
                         attributes: %{
                           "rpc.system" => "grpc",
                           "rpc.service" => "beholder.worker.v1.AnalyzerWorker",
                           "rpc.method" => "Analyze"
                         }
                       } do
        operation.()
      end
    after
      :otel_ctx.detach(token)
    end
  end

  @spec with_span(binary(), map(), (-> result)) :: result when result: term()
  def with_span(name, attributes, operation) do
    Tracer.with_span name, %{attributes: attributes} do
      operation.()
    end
  end

  @spec set_attributes(map()) :: boolean()
  def set_attributes(attributes), do: Tracer.set_attributes(attributes)

  @spec set_error(term()) :: boolean()
  def set_error(reason), do: Tracer.set_status(:error, format_reason(reason))

  @spec record_exception(Exception.t(), list()) :: boolean()
  def record_exception(error, stacktrace), do: Tracer.record_exception(error, stacktrace)

  @doc false
  def export_enabled?(environment \\ System.get_env()) do
    not sdk_disabled?(environment["OTEL_SDK_DISABLED"]) and
      Enum.any?(@endpoint_variables, &configured?(environment[&1]))
  end

  defp ensure_service_name do
    unless configured?(System.get_env("OTEL_SERVICE_NAME")) do
      System.put_env("OTEL_SERVICE_NAME", @default_service_name)
    end
  end

  defp start_application(application) do
    case Application.ensure_all_started(application) do
      {:ok, _applications} -> :ok
      {:error, reason} -> {:error, reason}
    end
  end

  defp configured?(value), do: is_binary(value) and String.trim(value) != ""

  defp sdk_disabled?(value) when is_binary(value) do
    String.downcase(String.trim(value)) in ["true", "1", "yes"]
  end

  defp sdk_disabled?(_value), do: false

  defp format_reason(reason) when is_binary(reason), do: reason
  defp format_reason(reason), do: inspect(reason)
end
