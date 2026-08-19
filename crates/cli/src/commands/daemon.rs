use super::DaemonCommand;
use crate::{service, stdout};
use beholder_daemon_client::{get_status, state_dir, stop};
use std::{
    error::Error,
    fs::{self, OpenOptions},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::Duration,
};

const STOP_TIMEOUT: Duration = Duration::from_secs(300);

pub(super) async fn run(command: DaemonCommand) -> Result<(), Box<dyn Error>> {
    match command {
        DaemonCommand::Install => install_service().await?,
        DaemonCommand::Uninstall => uninstall_service().await?,
        DaemonCommand::Start => start().await?,
        DaemonCommand::Run => foreground()?,
        DaemonCommand::Status => {
            let status = get_status().await?;
            stdout(format_args!(
                "{} (pid {}, protocol v{})",
                status.status, status.pid, status.protocol_version
            ))?;
        }
        DaemonCommand::Stop => stdout(format_args!(
            "{}",
            if stop_daemon().await? {
                "stopped"
            } else {
                "not running"
            }
        ))?,
    }
    Ok(())
}

fn binary() -> Result<PathBuf, Box<dyn Error>> {
    let binary = std::env::current_exe()?.with_file_name(if cfg!(windows) {
        "beholderd.exe"
    } else {
        "beholderd"
    });
    if binary.is_file() {
        Ok(binary)
    } else {
        Err(format!("beholderd not found next to CLI at {}", binary.display()).into())
    }
}

async fn start() -> Result<(), Box<dyn Error>> {
    if let Ok(status) = get_status().await {
        stdout(format_args!("already running (pid {})", status.pid))?;
        return Ok(());
    }
    wait_for_lock().await?;
    if let Ok(status) = get_status().await {
        stdout(format_args!("already running (pid {})", status.pid))?;
        return Ok(());
    }
    let state = state_dir()?;
    fs::create_dir_all(&state)?;
    let log_path = state.join("beholderd.log");
    let log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)?;
    let mut child = Command::new(binary()?)
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log)
        .spawn()?;
    for _ in 0..50 {
        if let Some(status) = child.try_wait()? {
            return Err(
                format!("beholderd exited with {status}; see {}", log_path.display()).into(),
            );
        }
        if let Ok(status) = get_status().await
            && status.pid == child.id()
        {
            stdout(format_args!("started (pid {})", status.pid))?;
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    Err(format!("beholderd did not become ready; see {}", log_path.display()).into())
}

fn foreground() -> Result<(), Box<dyn Error>> {
    let binary = binary()?;
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        Err(Command::new(binary).exec().into())
    }
    #[cfg(not(unix))]
    {
        let status = Command::new(binary).status()?;
        if status.success() {
            Ok(())
        } else {
            Err(format!("beholderd exited with {status}").into())
        }
    }
}

async fn wait_for_lock() -> Result<(), Box<dyn Error>> {
    let path = state_dir()?.join("beholderd.pid");
    wait_for_lock_at(&path, STOP_TIMEOUT).await
}

async fn wait_for_lock_at(path: &Path, timeout: Duration) -> Result<(), Box<dyn Error>> {
    match tokio::time::timeout(timeout, async {
        let mut reported = false;
        loop {
            if !path.exists() {
                return Ok(());
            }
            let file = fs::File::options().read(true).write(true).open(path)?;
            if file.try_lock().is_ok() {
                return Ok(());
            }
            if !reported {
                eprintln!("waiting for beholderd to finish active work...");
                reported = true;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    {
        Ok(result) => result,
        Err(_) => Err(format!(
            "timed out waiting for beholderd to stop after {timeout:?}; retry once active work completes"
        )
        .into()),
    }
}

async fn stop_for_service_change() -> Result<(), Box<dyn Error>> {
    if matches!(
        tokio::time::timeout(Duration::from_millis(500), get_status()).await,
        Ok(Ok(_))
    ) {
        tokio::time::timeout(Duration::from_secs(2), stop())
            .await
            .map_err(|_| "timed out stopping beholderd")??;
    }
    wait_for_lock().await
}

async fn stop_daemon() -> Result<bool, Box<dyn Error>> {
    let running = matches!(
        tokio::time::timeout(Duration::from_millis(500), get_status()).await,
        Ok(Ok(_))
    );
    if std::env::var_os("BEHOLDER_STATE_DIR").is_none() {
        service::stop()?;
    }
    if running {
        let _ = stop().await;
    }
    wait_for_lock().await?;
    Ok(running)
}

async fn install_service() -> Result<(), Box<dyn Error>> {
    stop_for_service_change().await?;
    let state = state_dir()?;
    let outcome = service::install(&service::installed_daemon_path()?, &state)?;
    if std::env::var("BEHOLDER_LAUNCHER").as_deref() != Ok("fake") {
        for _ in 0..50 {
            if get_status().await.is_ok() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        get_status().await.map_err(|_| {
            format!(
                "installed beholderd did not become ready; see {}",
                state.join("beholderd.log").display()
            )
        })?;
    }
    stdout(format_args!(
        "installed {} ({})",
        outcome.manifest_path.display(),
        if outcome.manifest_changed {
            "updated"
        } else {
            "unchanged"
        }
    ))?;
    Ok(())
}

async fn uninstall_service() -> Result<(), Box<dyn Error>> {
    stop_for_service_change().await?;
    let outcome = service::uninstall()?;
    stdout(format_args!(
        "{} {}",
        if outcome.manifest_existed {
            "removed"
        } else {
            "already absent"
        },
        outcome.manifest_path.display()
    ))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn waiting_for_daemon_lock_times_out() {
        let path = std::env::temp_dir().join(format!(
            "beholder-cli-daemon-lock-test-{}",
            std::process::id()
        ));
        let file = fs::File::create(&path).unwrap();
        file.try_lock().unwrap();

        let error = wait_for_lock_at(&path, Duration::from_millis(10))
            .await
            .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("timed out waiting for beholderd")
        );
        fs::remove_file(path).unwrap();
    }
}
