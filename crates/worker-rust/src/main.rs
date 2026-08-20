use std::{env, error::Error, path::PathBuf};

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error + Send + Sync>> {
    let mut args = env::args_os().skip(1);
    let mut socket = None;
    let mut cache_dir = None;
    while let Some(argument) = args.next() {
        match argument.to_str() {
            Some("--socket") => socket = args.next().map(PathBuf::from),
            Some("--cache-dir") => cache_dir = args.next().map(PathBuf::from),
            _ => return Err(format!("unknown argument: {}", argument.to_string_lossy()).into()),
        }
    }
    beholder_worker_rust::serve(
        &socket.ok_or("missing --socket")?,
        cache_dir.ok_or("missing --cache-dir")?,
    )
    .await
}
