mod commands;
mod service;

use std::{
    error::Error,
    fmt,
    io::{self, Write},
};

pub(crate) fn stdout(arguments: fmt::Arguments<'_>) -> io::Result<()> {
    writeln!(io::stdout().lock(), "{arguments}")
}

fn is_broken_pipe(mut error: &(dyn Error + 'static)) -> bool {
    loop {
        if error
            .downcast_ref::<io::Error>()
            .is_some_and(|error| error.kind() == io::ErrorKind::BrokenPipe)
        {
            return true;
        }
        let Some(source) = error.source() else {
            return false;
        };
        error = source;
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let _observability_guard = beholder_observability::init(
        "beholder-cli",
        beholder_observability::LogOutput::Stderr,
        beholder_observability::ExportMode::Simple,
    );
    run().await
}

#[tracing::instrument(name = "cli.run", err)]
async fn run() -> Result<(), Box<dyn Error>> {
    match commands::run().await {
        Err(error) if is_broken_pipe(error.as_ref()) => Ok(()),
        result => result,
    }
}
