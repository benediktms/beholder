defmodule Beholder.Worker.Elixir.ObservabilityTest do
  use ExUnit.Case, async: false

  alias Beholder.Worker.Elixir.Observability

  setup_all do
    assert :ok = Observability.start()
    :ok
  end

  test "enables trace export only for configured trace endpoints" do
    refute Observability.export_enabled?(%{})

    assert Observability.export_enabled?(%{
             "OTEL_EXPORTER_OTLP_ENDPOINT" => "http://localhost:4318"
           })

    assert Observability.export_enabled?(%{
             "OTEL_EXPORTER_OTLP_TRACES_ENDPOINT" => "http://localhost:4318/v1/traces"
           })

    refute Observability.export_enabled?(%{
             "OTEL_EXPORTER_OTLP_ENDPOINT" => "http://localhost:4318",
             "OTEL_SDK_DISABLED" => "true"
           })
  end

  test "extracts W3C trace context and restores the previous context" do
    previous = OpenTelemetry.Ctx.get_current()
    trace_id = "0af7651916cd43dd8448eb211c80319c"
    parent_span_id = "b7ad6b7169203331"

    child =
      Observability.with_server_span(
        %{"traceparent" => "00-#{trace_id}-#{parent_span_id}-01"},
        fn ->
          OpenTelemetry.Tracer.current_span_ctx()
          |> OpenTelemetry.Span.hex_span_ctx()
        end
      )

    assert child.otel_trace_id == trace_id
    refute child.otel_span_id == parent_span_id
    assert OpenTelemetry.Ctx.get_current() == previous
  end
end
