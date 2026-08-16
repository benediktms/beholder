use std::{
    error::Error,
    path::{Path, PathBuf},
};

#[cfg(unix)]
use tokio::net::UnixListener;

#[cfg(unix)]
pub(super) struct SocketFile(pub(super) PathBuf);

#[cfg(unix)]
impl Drop for SocketFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

#[cfg(unix)]
pub(super) fn bind_socket(path: &Path) -> Result<(UnixListener, SocketFile), Box<dyn Error>> {
    match std::fs::remove_file(path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    let listener = UnixListener::bind(path)?;
    let socket = SocketFile(path.to_path_buf());
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok((listener, socket))
}

#[cfg(unix)]
pub(super) async fn shutdown_signal(stopped: tokio::sync::oneshot::Receiver<()>) {
    match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
        Ok(mut terminate) => {
            tokio::select! {
                _ = stopped => {}
                _ = tokio::signal::ctrl_c() => {}
                _ = terminate.recv() => {}
            }
        }
        Err(_) => {
            tokio::select! {
                _ = stopped => {}
                _ = tokio::signal::ctrl_c() => {}
            }
        }
    }
}

#[cfg(not(unix))]
pub(super) async fn shutdown_signal(stopped: tokio::sync::oneshot::Receiver<()>) {
    tokio::select! {
        _ = stopped => {}
        _ = tokio::signal::ctrl_c() => {}
    }
}
