package main

import (
	"context"
	"net/http"
	"net/http/httptest"
	"testing"
	"time"

	"go.opentelemetry.io/otel"
	"go.opentelemetry.io/otel/propagation"
	"go.opentelemetry.io/otel/trace"
	"google.golang.org/grpc/metadata"
)

func TestExtractTraceContextUsesGRPCTraceparent(t *testing.T) {
	otel.SetTextMapPropagator(propagation.TraceContext{})
	ctx := metadata.NewIncomingContext(context.Background(), metadata.Pairs(
		"traceparent", "00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01",
	))
	parent := trace.SpanContextFromContext(extractTraceContext(ctx))
	if parent.TraceID().String() != "0af7651916cd43dd8448eb211c80319c" || !parent.IsRemote() {
		t.Fatalf("unexpected extracted trace context: %+v", parent)
	}
}

func TestConfigureTelemetryExportsOTLPTraces(t *testing.T) {
	received := make(chan struct{}, 1)
	collector := httptest.NewServer(http.HandlerFunc(func(writer http.ResponseWriter, request *http.Request) {
		if request.URL.Path != "/v1/traces" {
			t.Errorf("unexpected OTLP path: %s", request.URL.Path)
		}
		received <- struct{}{}
		writer.WriteHeader(http.StatusOK)
	}))
	defer collector.Close()
	t.Setenv("OTEL_SDK_DISABLED", "false")
	t.Setenv("OTEL_EXPORTER_OTLP_ENDPOINT", "")
	t.Setenv("OTEL_EXPORTER_OTLP_TRACES_ENDPOINT", collector.URL+"/v1/traces")

	shutdown, err := configureTelemetry(context.Background())
	if err != nil {
		t.Fatal(err)
	}
	_, span := otel.Tracer("test").Start(context.Background(), "worker.test")
	span.End()
	if err := shutdown(context.Background()); err != nil {
		t.Fatal(err)
	}
	select {
	case <-received:
	case <-time.After(time.Second):
		t.Fatal("OTLP trace was not exported")
	}
}
