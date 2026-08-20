use opentelemetry::{KeyValue, trace::TracerProvider as _};
use opentelemetry_appender_tracing::layer::OpenTelemetryTracingBridge;
use opentelemetry_otlp::{LogExporter, Protocol, SpanExporter, WithExportConfig};
use opentelemetry_sdk::{Resource, logs::SdkLoggerProvider, trace::SdkTracerProvider};
use std::{error::Error, ffi::OsStr, path::Path};
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::{
    EnvFilter,
    fmt::{format::FmtSpan, writer::BoxMakeWriter},
    prelude::*,
};

const OTLP_ENDPOINT: &str = "OTEL_EXPORTER_OTLP_ENDPOINT";
const OTLP_TRACES_ENDPOINT: &str = "OTEL_EXPORTER_OTLP_TRACES_ENDPOINT";
const OTLP_LOGS_ENDPOINT: &str = "OTEL_EXPORTER_OTLP_LOGS_ENDPOINT";

pub struct LoggingGuard {
    _writer: Option<WorkerGuard>,
    tracer_provider: Option<SdkTracerProvider>,
    logger_provider: Option<SdkLoggerProvider>,
}

impl Drop for LoggingGuard {
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

pub fn init(log_dir: &Path) -> LoggingGuard {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let (writer, writer_guard) = log_writer(log_dir);
    let fmt_layer = tracing_subscriber::fmt::layer()
        .json()
        .with_ansi(false)
        .with_span_events(FmtSpan::CLOSE)
        .with_writer(writer)
        .with_filter(filter);

    let (tracer_provider, logger_provider) = match telemetry_providers() {
        Ok(providers) => providers,
        Err(error) => {
            eprintln!("could not initialize OpenTelemetry export ({error}); using local logs only");
            (None, None)
        }
    };
    let trace_layer = tracer_provider.as_ref().map(|provider| {
        tracing_opentelemetry::layer()
            .with_tracer(provider.tracer("beholderd"))
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

    LoggingGuard {
        _writer: writer_guard,
        tracer_provider,
        logger_provider,
    }
}

fn log_writer(log_dir: &Path) -> (BoxMakeWriter, Option<WorkerGuard>) {
    match tracing_appender::rolling::RollingFileAppender::builder()
        .rotation(tracing_appender::rolling::Rotation::DAILY)
        .filename_prefix("beholderd")
        .filename_suffix("log")
        .max_log_files(7)
        .build(log_dir)
    {
        Ok(appender) => {
            let (writer, guard) = tracing_appender::non_blocking(appender);
            (BoxMakeWriter::new(writer), Some(guard))
        }
        Err(error) => {
            eprintln!(
                "could not initialize rolling logs in {} ({error}); falling back to stderr",
                log_dir.display()
            );
            (BoxMakeWriter::new(std::io::stderr), None)
        }
    }
}

fn telemetry_providers()
-> Result<(Option<SdkTracerProvider>, Option<SdkLoggerProvider>), Box<dyn Error + Send + Sync>> {
    if sdk_disabled() {
        return Ok((None, None));
    }
    let shared_endpoint = env_value(OTLP_ENDPOINT).is_some();
    let traces_enabled = shared_endpoint || env_value(OTLP_TRACES_ENDPOINT).is_some();
    let logs_enabled = shared_endpoint || env_value(OTLP_LOGS_ENDPOINT).is_some();
    if !traces_enabled && !logs_enabled {
        return Ok((None, None));
    }

    let resource = telemetry_resource();
    let tracer_provider = traces_enabled
        .then(|| {
            let exporter = SpanExporter::builder()
                .with_http()
                .with_protocol(Protocol::HttpBinary)
                .build()?;
            Ok::<_, opentelemetry_otlp::ExporterBuildError>(
                SdkTracerProvider::builder()
                    .with_batch_exporter(exporter)
                    .with_resource(resource.clone())
                    .build(),
            )
        })
        .transpose()?;
    let logger_provider = logs_enabled
        .then(|| {
            let exporter = LogExporter::builder()
                .with_http()
                .with_protocol(Protocol::HttpBinary)
                .build()?;
            Ok::<_, opentelemetry_otlp::ExporterBuildError>(
                SdkLoggerProvider::builder()
                    .with_batch_exporter(exporter)
                    .with_resource(resource)
                    .build(),
            )
        })
        .transpose()?;
    Ok((tracer_provider, logger_provider))
}

fn telemetry_resource() -> Resource {
    let service_name = std::env::var("OTEL_SERVICE_NAME")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "beholderd".into());
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
