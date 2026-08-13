use std::path::Path;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::{EnvFilter, fmt::format::FmtSpan, prelude::*};

pub fn init(log_dir: &Path) -> Option<WorkerGuard> {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let appender = match tracing_appender::rolling::RollingFileAppender::builder()
        .rotation(tracing_appender::rolling::Rotation::DAILY)
        .filename_prefix("beholderd")
        .filename_suffix("log")
        .max_log_files(7)
        .build(log_dir)
    {
        Ok(appender) => appender,
        Err(error) => {
            eprintln!(
                "could not initialize rolling logs in {} ({error}); falling back to stderr",
                log_dir.display()
            );
            tracing_subscriber::registry()
                .with(filter)
                .with(tracing_subscriber::fmt::layer().json().with_ansi(false))
                .init();
            return None;
        }
    };
    let (writer, guard) = tracing_appender::non_blocking(appender);
    tracing_subscriber::registry()
        .with(filter)
        .with(
            tracing_subscriber::fmt::layer()
                .json()
                .with_ansi(false)
                .with_span_events(FmtSpan::CLOSE)
                .with_writer(writer),
        )
        .init();
    Some(guard)
}
