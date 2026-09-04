use super::daemon;
use std::{error::Error, path::PathBuf, process::Command};

fn binary() -> Result<PathBuf, Box<dyn Error>> {
    let binary = std::env::current_exe()?.with_file_name(if cfg!(windows) {
        "beholder-graph-ui.exe"
    } else {
        "beholder-graph-ui"
    });
    binary.is_file().then_some(binary.clone()).ok_or_else(|| {
        format!(
            "beholder-graph-ui not found next to CLI at {}",
            binary.display()
        )
        .into()
    })
}

pub(super) async fn run() -> Result<(), Box<dyn Error>> {
    daemon::start().await?;
    let binary = binary()?;
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        Err(Command::new(binary).exec().into())
    }
    #[cfg(not(unix))]
    {
        let status = Command::new(binary).status()?;
        status
            .success()
            .then_some(())
            .ok_or_else(|| format!("graph UI exited with {status}").into())
    }
}
