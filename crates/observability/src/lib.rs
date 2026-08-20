use opentelemetry::{
    KeyValue, global,
    propagation::{Extractor, Injector},
    trace::TracerProvider as _,
};
use opentelemetry_appender_tracing::layer::OpenTelemetryTracingBridge;
use opentelemetry_otlp::{LogExporter, Protocol, SpanExporter, WithExportConfig};
use opentelemetry_sdk::{
    Resource, logs::SdkLoggerProvider, propagation::TraceContextPropagator,
    trace::SdkTracerProvider,
};
use std::{error::Error, ffi::OsStr, path::PathBuf};
use tonic::metadata::{Ascii, MetadataKey, MetadataMap, MetadataValue};
use tracing_appender::non_blocking::WorkerGuard;
use tracing_opentelemetry::OpenTelemetrySpanExt;
use tracing_subscriber::{
    EnvFilter,
    fmt::{format::FmtSpan, writer::BoxMakeWriter},
    prelude::*,
};

const OTLP_ENDPOINT: &str = "OTEL_EXPORTER_OTLP_ENDPOINT";
const OTLP_TRACES_ENDPOINT: &str = "OTEL_EXPORTER_OTLP_TRACES_ENDPOINT";
const OTLP_LOGS_ENDPOINT: &str = "OTEL_EXPORTER_OTLP_LOGS_ENDPOINT";

pub enum LogOutput {
    Rolling { directory: PathBuf, prefix: String },
    Stderr,
}

#[derive(Clone, Copy)]
pub enum ExportMode {
    Batch,
    Simple,
}

pub struct ObservabilityGuard {
    _writer: Option<WorkerGuard>,
    tracer_provider: Option<SdkTracerProvider>,
    logger_provider: Option<SdkLoggerProvider>,
}

impl Drop for ObservabilityGuard {
    fn drop(&mut self) {
        if let Some(provider) = self.tracer_provider.take()
            && let Err(error) = provider.shutdown()
        {
            eprintln!("could not flush OpenTelemetry traces: {error}");
        }
        if let Some(provider) = self.logger_provider.take()
            && let Err(error) = provider.shutdown()
        {
            eprintln!("could not flush OpenTelemetry logs: {error}");
        }
    }
}

pub fn init(
    default_service_name: &str,
    output: LogOutput,
    export_mode: ExportMode,
) -> ObservabilityGuard {
    global::set_text_map_propagator(TraceContextPropagator::new());
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let (writer, writer_guard) = log_writer(output);
    let fmt_layer = tracing_subscriber::fmt::layer()
        .json()
        .with_ansi(false)
        .with_span_events(FmtSpan::CLOSE)
        .with_writer(writer)
        .with_filter(filter);

    let (tracer_provider, logger_provider) =
        match telemetry_providers(default_service_name, export_mode) {
            Ok(providers) => providers,
            Err(error) => {
                eprintln!(
                    "could not initialize OpenTelemetry export ({error}); using local logs only"
                );
                (None, None)
            }
        };
    let trace_layer = tracer_provider.as_ref().map(|provider| {
        tracing_opentelemetry::layer()
            .with_tracer(provider.tracer(default_service_name.to_owned()))
            .with_filter(telemetry_filter())
    });
    let log_layer = logger_provider
        .as_ref()
        .map(|provider| OpenTelemetryTracingBridge::new(provider).with_filter(telemetry_filter()));

    tracing_subscriber::registry()
        .with(fmt_layer)
        .with(trace_layer)
        .with(log_layer)
        .init();

    if tracer_provider.is_some() || logger_provider.is_some() {
        tracing::info!(
            traces = tracer_provider.is_some(),
            logs = logger_provider.is_some(),
            protocol = "otlp/http-protobuf",
            "OpenTelemetry export enabled"
        );
    }

    ObservabilityGuard {
        _writer: writer_guard,
        tracer_provider,
        logger_provider,
    }
}

pub fn inject_current_context(metadata: &mut MetadataMap) {
    global::get_text_map_propagator(|propagator| {
        propagator.inject_context(
            &tracing::Span::current().context(),
            &mut MetadataInjector(metadata),
        );
    });
}

pub fn set_parent_from_metadata(span: &tracing::Span, metadata: &MetadataMap) {
    let parent = global::get_text_map_propagator(|propagator| {
        propagator.extract(&MetadataExtractor(metadata))
    });
    let _ = span.set_parent(parent);
}

fn log_writer(output: LogOutput) -> (BoxMakeWriter, Option<WorkerGuard>) {
    match output {
        LogOutput::Rolling { directory, prefix } => {
            match tracing_appender::rolling::RollingFileAppender::builder()
                .rotation(tracing_appender::rolling::Rotation::DAILY)
                .filename_prefix(prefix)
                .filename_suffix("log")
                .max_log_files(7)
                .build(&directory)
            {
                Ok(appender) => {
                    let (writer, guard) = tracing_appender::non_blocking(appender);
                    (BoxMakeWriter::new(writer), Some(guard))
                }
                Err(error) => {
                    eprintln!(
                        "could not initialize rolling logs in {} ({error}); falling back to stderr",
                        directory.display()
                    );
                    (BoxMakeWriter::new(std::io::stderr), None)
                }
            }
        }
        LogOutput::Stderr => (BoxMakeWriter::new(std::io::stderr), None),
    }
}

fn telemetry_providers(
    default_service_name: &str,
    export_mode: ExportMode,
) -> Result<(Option<SdkTracerProvider>, Option<SdkLoggerProvider>), Box<dyn Error + Send + Sync>> {
    if sdk_disabled() {
        return Ok((None, None));
    }
    let shared_endpoint = env_value(OTLP_ENDPOINT).is_some();
    let traces_enabled = shared_endpoint || env_value(OTLP_TRACES_ENDPOINT).is_some();
    let logs_enabled = shared_endpoint || env_value(OTLP_LOGS_ENDPOINT).is_some();
    if !traces_enabled && !logs_enabled {
        return Ok((None, None));
    }

    let resource = telemetry_resource(default_service_name);
    let tracer_provider = traces_enabled
        .then(|| {
            let exporter = SpanExporter::builder()
                .with_http()
                .with_protocol(Protocol::HttpBinary)
                .build()?;
            let builder = SdkTracerProvider::builder().with_resource(resource.clone());
            Ok::<_, opentelemetry_otlp::ExporterBuildError>(match export_mode {
                ExportMode::Batch => builder.with_batch_exporter(exporter).build(),
                ExportMode::Simple => builder.with_simple_exporter(exporter).build(),
            })
        })
        .transpose()?;
    let logger_provider = logs_enabled
        .then(|| {
            let exporter = LogExporter::builder()
                .with_http()
                .with_protocol(Protocol::HttpBinary)
                .build()?;
            let builder = SdkLoggerProvider::builder().with_resource(resource);
            Ok::<_, opentelemetry_otlp::ExporterBuildError>(match export_mode {
                ExportMode::Batch => builder.with_batch_exporter(exporter).build(),
                ExportMode::Simple => builder.with_simple_exporter(exporter).build(),
            })
        })
        .transpose()?;
    Ok((tracer_provider, logger_provider))
}

fn telemetry_resource(default_service_name: &str) -> Resource {
    let service_name = std::env::var("OTEL_SERVICE_NAME")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| default_service_name.into());
    Resource::builder()
        .with_service_name(service_name)
        .with_attributes([
            KeyValue::new("service.version", env!("CARGO_PKG_VERSION")),
            KeyValue::new("process.pid", i64::from(std::process::id())),
        ])
        .build()
}

fn telemetry_filter() -> EnvFilter {
    EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info"))
        .add_directive("opentelemetry=off".parse().expect("valid directive"))
        .add_directive("reqwest=off".parse().expect("valid directive"))
        .add_directive("hyper=off".parse().expect("valid directive"))
        .add_directive("h2=off".parse().expect("valid directive"))
}

fn env_value(name: &str) -> Option<std::ffi::OsString> {
    std::env::var_os(name).filter(|value| !value.is_empty())
}

fn sdk_disabled() -> bool {
    std::env::var_os("OTEL_SDK_DISABLED")
        .as_deref()
        .and_then(OsStr::to_str)
        .is_some_and(|value| value.eq_ignore_ascii_case("true"))
}

struct MetadataInjector<'a>(&'a mut MetadataMap);

impl Injector for MetadataInjector<'_> {
    fn set(&mut self, key: &str, value: String) {
        let Ok(key) = MetadataKey::<Ascii>::from_bytes(key.as_bytes()) else {
            return;
        };
        let Ok(value) = MetadataValue::try_from(value) else {
            return;
        };
        self.0.insert(key, value);
    }
}

struct MetadataExtractor<'a>(&'a MetadataMap);

impl Extractor for MetadataExtractor<'_> {
    fn get(&self, key: &str) -> Option<&str> {
        self.0.get(key).and_then(|value| value.to_str().ok())
    }

    fn keys(&self) -> Vec<&str> {
        self.0
            .keys()
            .filter_map(|key| match key {
                tonic::metadata::KeyRef::Ascii(key) => Some(key.as_str()),
                tonic::metadata::KeyRef::Binary(_) => None,
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use opentelemetry::propagation::TextMapPropagator;
    use opentelemetry::trace::{
        SpanContext, SpanId, TraceContextExt, TraceFlags, TraceId, TraceState,
    };

    #[test]
    fn tonic_metadata_round_trips_w3c_trace_context() {
        let expected = SpanContext::new(
            TraceId::from_hex("4bf92f3577b34da6a3ce929d0e0e4736").unwrap(),
            SpanId::from_hex("00f067aa0ba902b7").unwrap(),
            TraceFlags::SAMPLED,
            true,
            TraceState::default(),
        );
        let context = opentelemetry::Context::new().with_remote_span_context(expected.clone());
        let propagator = TraceContextPropagator::new();
        let mut metadata = MetadataMap::new();

        propagator.inject_context(&context, &mut MetadataInjector(&mut metadata));
        let extracted = propagator.extract(&MetadataExtractor(&metadata));

        assert_eq!(extracted.span().span_context(), &expected);
    }
}
