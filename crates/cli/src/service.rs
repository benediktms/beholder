use std::{
    collections::BTreeMap,
    error::Error,
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    process::Command,
};

const LABEL: &str = "dev.beholder.daemon";
const UNIT: &str = "beholder.service";
const SERVICE_ENVIRONMENT_VARIABLES: [&str; 9] = [
    "BEHOLDER_TYPESCRIPT_WORKER_MEMORY_LIMIT_BYTES",
    "BEHOLDER_TYPESCRIPT_WORKER_PATH",
    "MIX_HOME",
    "OTEL_EXPORTER_OTLP_ENDPOINT",
    "OTEL_EXPORTER_OTLP_TRACES_ENDPOINT",
    "OTEL_EXPORTER_OTLP_LOGS_ENDPOINT",
    "OTEL_SERVICE_NAME",
    "OTEL_SDK_DISABLED",
    "RUST_LOG",
];

pub struct InstallOutcome {
    pub manifest_path: PathBuf,
    pub manifest_changed: bool,
}

pub struct UninstallOutcome {
    pub manifest_path: PathBuf,
    pub manifest_existed: bool,
}

pub fn installed_daemon_path() -> Result<PathBuf, Box<dyn Error>> {
    let path = std::env::var_os("BEHOLDER_DAEMON_PATH")
        .map(PathBuf::from)
        .unwrap_or(home_dir()?.join(".local/bin/beholderd"));
    if path.is_file() {
        Ok(path)
    } else {
        Err(format!(
            "beholderd not found at {}; run `just install` first",
            path.display()
        )
        .into())
    }
}

pub fn install(binary: &Path, state_dir: &Path) -> Result<InstallOutcome, Box<dyn Error>> {
    fs::create_dir_all(state_dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(state_dir, fs::Permissions::from_mode(0o700))?;
    }

    let environment = service_environment(&home_dir()?);
    if cfg!(target_os = "macos") {
        install_macos(binary, state_dir, &environment)
    } else if cfg!(target_os = "linux") {
        install_linux(binary, &environment)
    } else {
        Err("daemon installation is supported on macOS and Linux".into())
    }
}

fn service_environment(home: &Path) -> BTreeMap<String, String> {
    let mut environment: BTreeMap<_, _> = SERVICE_ENVIRONMENT_VARIABLES
        .into_iter()
        .filter_map(|name| {
            std::env::var(name)
                .ok()
                .filter(|value| !value.is_empty())
                .map(|value| (name.into(), value))
        })
        .collect();
    environment.insert(
        "PATH".into(),
        format!(
            "{}/.local/share/mise/shims:{}/.cargo/bin:{}/.local/bin:/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin",
            home.display(),
            home.display(),
            home.display()
        ),
    );
    environment
}

pub fn uninstall() -> Result<UninstallOutcome, Box<dyn Error>> {
    if cfg!(target_os = "macos") {
        uninstall_macos()
    } else if cfg!(target_os = "linux") {
        uninstall_linux()
    } else {
        Err("daemon installation is supported on macOS and Linux".into())
    }
}

pub fn stop() -> Result<(), Box<dyn Error>> {
    if cfg!(target_os = "macos") {
        let manifest = launch_agent_path()?;
        launch(
            vec![
                "launchctl".into(),
                "bootout".into(),
                format!("gui/{}", current_uid()?),
                path_text(&manifest)?.into(),
            ],
            true,
        )
    } else if cfg!(target_os = "linux") {
        launch(
            vec![
                "systemctl".into(),
                "--user".into(),
                "stop".into(),
                UNIT.into(),
            ],
            true,
        )
    } else {
        Err("daemon installation is supported on macOS and Linux".into())
    }
}

fn home_dir() -> Result<PathBuf, Box<dyn Error>> {
    std::env::var_os("BEHOLDER_HOME")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
        .ok_or_else(|| "cannot locate home directory: set HOME".into())
}

fn write_if_changed(path: &Path, desired: &str) -> Result<bool, Box<dyn Error>> {
    if fs::read(path).is_ok_and(|current| current == desired.as_bytes()) {
        return Ok(false);
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let temporary = path.with_extension("tmp");
    let mut file = fs::File::create(&temporary)?;
    file.write_all(desired.as_bytes())?;
    file.sync_all()?;
    fs::rename(temporary, path)?;
    Ok(true)
}

fn remove_if_present(path: &Path) -> Result<bool, Box<dyn Error>> {
    match fs::remove_file(path) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

fn launch(arguments: Vec<String>, missing_ok: bool) -> Result<(), Box<dyn Error>> {
    if std::env::var("BEHOLDER_LAUNCHER").as_deref() == Ok("fake") {
        let log = std::env::var_os("BEHOLDER_LAUNCHER_LOG")
            .map(PathBuf::from)
            .ok_or("BEHOLDER_LAUNCHER_LOG is required for the fake launcher")?;
        if let Some(parent) = log.parent() {
            fs::create_dir_all(parent)?;
        }
        writeln!(
            OpenOptions::new().create(true).append(true).open(log)?,
            "{}",
            arguments.join("\t")
        )?;
        return Ok(());
    }

    let (program, arguments) = arguments.split_first().ok_or("empty launcher command")?;
    let output = Command::new(program).args(arguments).output()?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let missing = matches!(output.status.code(), Some(4 | 5 | 113))
        || stderr.contains("Could not find service")
        || stderr.contains("not loaded")
        || stderr.contains("not found")
        || stderr.contains("does not exist");
    if missing_ok && missing {
        return Ok(());
    }
    Err(format!(
        "{} failed with {}: {}",
        program,
        output.status,
        stderr.trim()
    )
    .into())
}

fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn path_text(path: &Path) -> Result<&str, Box<dyn Error>> {
    path.to_str()
        .ok_or_else(|| format!("path is not UTF-8: {}", path.display()).into())
}

fn launch_agent_path() -> Result<PathBuf, Box<dyn Error>> {
    Ok(home_dir()?.join(format!("Library/LaunchAgents/{LABEL}.plist")))
}

fn current_uid() -> Result<String, Box<dyn Error>> {
    if let Ok(uid) = std::env::var("BEHOLDER_UID") {
        return Ok(uid);
    }
    let output = Command::new("id").arg("-u").output()?;
    if !output.status.success() {
        return Err("failed to determine current uid".into());
    }
    Ok(String::from_utf8(output.stdout)?.trim().to_owned())
}

fn render_plist(
    binary: &Path,
    log: &Path,
    environment: &BTreeMap<String, String>,
) -> Result<String, Box<dyn Error>> {
    let environment = environment
        .iter()
        .map(|(name, value)| {
            format!(
                "    <key>{}</key><string>{}</string>",
                xml_escape(name),
                xml_escape(value)
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    Ok(format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>{LABEL}</string>
  <key>ProgramArguments</key><array><string>{}</string></array>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><dict><key>SuccessfulExit</key><false/></dict>
  <key>ProcessType</key><string>Background</string>
  <key>EnvironmentVariables</key><dict>
{environment}
  </dict>
  <key>StandardOutPath</key><string>{}</string>
  <key>StandardErrorPath</key><string>{}</string>
</dict>
</plist>
"#,
        xml_escape(path_text(binary)?),
        xml_escape(path_text(log)?),
        xml_escape(path_text(log)?),
    ))
}

fn install_macos(
    binary: &Path,
    state_dir: &Path,
    environment: &BTreeMap<String, String>,
) -> Result<InstallOutcome, Box<dyn Error>> {
    let manifest_path = launch_agent_path()?;
    let changed = write_if_changed(
        &manifest_path,
        &render_plist(binary, &state_dir.join("beholderd.log"), environment)?,
    )?;
    let uid = current_uid()?;
    let domain = format!("gui/{uid}");
    let target = format!("{domain}/{LABEL}");
    launch(
        vec![
            "launchctl".into(),
            "bootout".into(),
            domain.clone(),
            path_text(&manifest_path)?.into(),
        ],
        true,
    )?;
    launch(vec!["launchctl".into(), "enable".into(), target], false)?;
    launch(
        vec![
            "launchctl".into(),
            "bootstrap".into(),
            domain,
            path_text(&manifest_path)?.into(),
        ],
        false,
    )?;
    Ok(InstallOutcome {
        manifest_path,
        manifest_changed: changed,
    })
}

fn uninstall_macos() -> Result<UninstallOutcome, Box<dyn Error>> {
    let manifest_path = launch_agent_path()?;
    let domain = format!("gui/{}", current_uid()?);
    launch(
        vec![
            "launchctl".into(),
            "bootout".into(),
            domain,
            path_text(&manifest_path)?.into(),
        ],
        true,
    )?;
    let existed = remove_if_present(&manifest_path)?;
    Ok(UninstallOutcome {
        manifest_path,
        manifest_existed: existed,
    })
}

fn systemd_unit_path() -> Result<PathBuf, Box<dyn Error>> {
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or(home_dir()?.join(".config"));
    Ok(config.join(format!("systemd/user/{UNIT}")))
}

fn render_systemd_unit(
    binary: &Path,
    environment: &BTreeMap<String, String>,
) -> Result<String, Box<dyn Error>> {
    let binary = path_text(binary)?
        .replace('\\', "\\\\")
        .replace('"', "\\\"");
    let environment = environment
        .iter()
        .map(|(name, value)| {
            let value = value.replace('\\', "\\\\").replace('"', "\\\"");
            format!("Environment=\"{name}={value}\"\n")
        })
        .collect::<String>();
    Ok(format!(
        "[Unit]\nDescription=Beholder architecture intelligence daemon\n\n\
         [Service]\nExecStart=\"{binary}\"\n{environment}Restart=on-failure\nRestartSec=1\n\n\
         [Install]\nWantedBy=default.target\n"
    ))
}

fn install_linux(
    binary: &Path,
    environment: &BTreeMap<String, String>,
) -> Result<InstallOutcome, Box<dyn Error>> {
    let manifest_path = systemd_unit_path()?;
    let changed = write_if_changed(&manifest_path, &render_systemd_unit(binary, environment)?)?;
    launch(
        vec!["systemctl".into(), "--user".into(), "daemon-reload".into()],
        false,
    )?;
    launch(
        vec![
            "systemctl".into(),
            "--user".into(),
            "enable".into(),
            "--now".into(),
            UNIT.into(),
        ],
        false,
    )?;
    Ok(InstallOutcome {
        manifest_path,
        manifest_changed: changed,
    })
}

fn uninstall_linux() -> Result<UninstallOutcome, Box<dyn Error>> {
    let manifest_path = systemd_unit_path()?;
    launch(
        vec![
            "systemctl".into(),
            "--user".into(),
            "disable".into(),
            "--now".into(),
            UNIT.into(),
        ],
        true,
    )?;
    let existed = remove_if_present(&manifest_path)?;
    launch(
        vec!["systemctl".into(), "--user".into(), "daemon-reload".into()],
        false,
    )?;
    Ok(UninstallOutcome {
        manifest_path,
        manifest_existed: existed,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn service_environment_includes_stable_tool_paths() {
        let environment = service_environment(Path::new("/home/test"));

        assert_eq!(
            environment["PATH"],
            "/home/test/.local/share/mise/shims:/home/test/.cargo/bin:/home/test/.local/bin:/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin"
        );
    }

    #[test]
    fn launchd_manifest_escapes_paths_and_restarts_only_failures() {
        let environment = BTreeMap::from([
            (
                "OTEL_EXPORTER_OTLP_ENDPOINT".into(),
                "http://localhost:4318?a=1&b=2".into(),
            ),
            ("PATH".into(), "/opt/toolchain/bin:/usr/bin".into()),
        ]);
        let manifest = render_plist(
            Path::new("/tmp/Beholder & tools/beholderd"),
            Path::new("/tmp/Beholder & tools/beholderd.log"),
            &environment,
        )
        .unwrap();
        assert!(manifest.contains("/tmp/Beholder &amp; tools/beholderd"));
        assert!(manifest.contains("http://localhost:4318?a=1&amp;b=2"));
        assert!(manifest.contains("<key>PATH</key><string>/opt/toolchain/bin:/usr/bin</string>"));
        assert!(manifest.contains("<key>SuccessfulExit</key><false/>"));
    }

    #[test]
    fn systemd_manifest_restarts_only_failures() {
        let environment = BTreeMap::from([
            (
                "OTEL_EXPORTER_OTLP_ENDPOINT".into(),
                "http://localhost:4318".into(),
            ),
            ("PATH".into(), "/opt/toolchain/bin:/usr/bin".into()),
        ]);
        let unit =
            render_systemd_unit(Path::new("/tmp/Beholder tools/beholderd"), &environment).unwrap();
        assert!(unit.contains("ExecStart=\"/tmp/Beholder tools/beholderd\""));
        assert!(unit.contains("Environment=\"OTEL_EXPORTER_OTLP_ENDPOINT=http://localhost:4318\""));
        assert!(unit.contains("Environment=\"PATH=/opt/toolchain/bin:/usr/bin\""));
        assert!(unit.contains("Restart=on-failure"));
    }

    #[test]
    fn unchanged_manifest_is_not_rewritten() {
        let directory =
            std::env::temp_dir().join(format!("beholder-service-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&directory);
        let manifest = directory.join("manifest");
        assert!(write_if_changed(&manifest, "desired").unwrap());
        assert!(!write_if_changed(&manifest, "desired").unwrap());
        fs::remove_dir_all(directory).unwrap();
    }
}
