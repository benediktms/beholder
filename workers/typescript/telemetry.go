package main

import (
	"context"
	"os"
	"strings"
	"time"

	"go.opentelemetry.io/otel"
	"go.opentelemetry.io/otel/exporters/otlp/otlptrace/otlptracehttp"
	"go.opentelemetry.io/otel/propagation"
	"go.opentelemetry.io/otel/sdk/resource"
	"go.opentelemetry.io/otel/sdk/trace"
	"google.golang.org/grpc/metadata"
)

type telemetryProvider struct {
	shutdown func(context.Context) error
	flush    func(context.Context) error
}

func disabledTelemetry() telemetryProvider {
	return telemetryProvider{
		shutdown: func(context.Context) error { return nil },
		flush:    func(context.Context) error { return nil },
	}
}

func configureTelemetry(ctx context.Context) (telemetryProvider, error) {
	otel.SetTextMapPropagator(propagation.NewCompositeTextMapPropagator(
		propagation.TraceContext{}, propagation.Baggage{},
	))
	if !telemetryEnabled() {
		return disabledTelemetry(), nil
	}
	exporter, err := otlptracehttp.New(ctx)
	if err != nil {
		return disabledTelemetry(), err
	}
	provider := trace.NewTracerProvider(
		trace.WithBatcher(exporter, trace.WithExportTimeout(time.Second)),
		trace.WithResource(resource.Default()),
	)
	otel.SetTracerProvider(provider)
	return telemetryProvider{shutdown: provider.Shutdown, flush: provider.ForceFlush}, nil
}

func telemetryEnabled() bool {
	if strings.EqualFold(os.Getenv("OTEL_SDK_DISABLED"), "true") {
		return false
	}
	return strings.TrimSpace(os.Getenv("OTEL_EXPORTER_OTLP_ENDPOINT")) != "" ||
		strings.TrimSpace(os.Getenv("OTEL_EXPORTER_OTLP_TRACES_ENDPOINT")) != ""
}

func extractTraceContext(ctx context.Context) context.Context {
	metadata, ok := metadata.FromIncomingContext(ctx)
	if !ok {
		return ctx
	}
	return otel.GetTextMapPropagator().Extract(ctx, metadataCarrier(metadata))
}

type metadataCarrier metadata.MD

func (carrier metadataCarrier) Set(key, value string) {
	metadata.MD(carrier).Set(key, value)
}

func (carrier metadataCarrier) Get(key string) string {
	values := metadata.MD(carrier).Get(key)
	if len(values) == 0 {
		return ""
	}
	return values[0]
}

func (carrier metadataCarrier) Keys() []string {
	keys := make([]string, 0, len(carrier))
	for key := range carrier {
		keys = append(keys, key)
	}
	return keys
}
