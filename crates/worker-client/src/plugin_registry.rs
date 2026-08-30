use crate::{ELIXIR_WORKER_ID, RUST_WORKER_ID, TYPESCRIPT_WORKER_ID};
use beholder_indexing::PluginDescriptor;
use beholder_protocol::{
    descriptor_from_wire,
    worker_v1::{DescribeRequest, analyzer_plugin_client::AnalyzerPluginClient},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    error::Error,
    fs::{self, File},
    path::{Path, PathBuf},
    process::Stdio,
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};
use tokio::process::Command;
use tonic::Request;

const REGISTRY_SCHEMA: u32 = 1;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const DESCRIBE_TIMEOUT: Duration = Duration::from_secs(5);
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(2);
static DISCOVERY_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct InstalledPlugin {
    pub descriptor: PluginDescriptor,
    pub digest: String,
}

#[derive(Default, Deserialize, Serialize)]
struct StoredRegistry {
    schema: u32,
    plugins: Vec<InstalledPlugin>,
}

pub struct PluginRegistry {
    state_dir: PathBuf,
    plugins: BTreeMap<String, InstalledPlugin>,
}

impl PluginRegistry {
    pub fn open(state_dir: impl Into<PathBuf>) -> Result<Self, Box<dyn Error>> {
        let state_dir = state_dir.into();
        let path = state_dir.join("plugins.json");
        let stored = if path.exists() {
            serde_json::from_reader::<_, StoredRegistry>(File::open(&path)?)?
        } else {
            StoredRegistry {
                schema: REGISTRY_SCHEMA,
                plugins: Vec::new(),
            }
        };
        if stored.schema != REGISTRY_SCHEMA {
            return Err(format!("unsupported plugin registry schema {}", stored.schema).into());
        }
        let mut plugins = BTreeMap::new();
        for plugin in stored.plugins {
            plugin.descriptor.validate()?;
            if !is_digest(&plugin.digest) {
                return Err(format!(
                    "plugin {} has an invalid executable digest",
                    plugin.descriptor.id
                )
                .into());
            }
            if plugins
                .insert(plugin.descriptor.id.clone(), plugin)
                .is_some()
            {
                return Err("plugin registry contains a duplicate ID".into());
            }
        }
        Ok(Self { state_dir, plugins })
    }

    pub fn plugins(&self) -> impl Iterator<Item = &InstalledPlugin> {
        self.plugins.values()
    }

    pub fn executable(&self, plugin: &InstalledPlugin) -> PathBuf {
        self.state_dir
            .join("plugins")
            .join(&plugin.descriptor.id)
            .join(&plugin.digest)
            .join("plugin")
    }

    pub fn install(
        &mut self,
        executable: &Path,
        descriptor: PluginDescriptor,
        replace: bool,
    ) -> Result<InstalledPlugin, Box<dyn Error>> {
        descriptor.validate()?;
        if [RUST_WORKER_ID, ELIXIR_WORKER_ID, TYPESCRIPT_WORKER_ID]
            .contains(&descriptor.id.as_str())
        {
            return Err(format!(
                "plugin ID {} is reserved for a built-in worker",
                descriptor.id
            )
            .into());
        }
        if self.plugins.contains_key(&descriptor.id) && !replace {
            return Err(
                format!("plugin {} is already installed; use replace", descriptor.id).into(),
            );
        }
        if !executable.is_file() {
            return Err(format!("plugin executable not found: {}", executable.display()).into());
        }
        let digest = file_digest(executable)?;
        let plugin = InstalledPlugin { descriptor, digest };
        let target = self.executable(&plugin);
        if !target.exists() {
            let parent = target.parent().ok_or("plugin target has no parent")?;
            fs::create_dir_all(parent)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(parent, fs::Permissions::from_mode(0o700))?;
            }
            let temporary = parent.join("plugin.tmp");
            fs::copy(executable, &temporary)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&temporary, fs::Permissions::from_mode(0o500))?;
            }
            fs::rename(temporary, &target)?;
        }
        self.plugins
            .insert(plugin.descriptor.id.clone(), plugin.clone());
        self.persist()?;
        Ok(plugin)
    }

    pub fn remove(&mut self, id: &str) -> Result<bool, Box<dyn Error>> {
        let removed = self.plugins.remove(id).is_some();
        if removed {
            self.persist()?;
        }
        Ok(removed)
    }

    fn persist(&self) -> Result<(), Box<dyn Error>> {
        fs::create_dir_all(&self.state_dir)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&self.state_dir, fs::Permissions::from_mode(0o700))?;
        }
        let path = self.state_dir.join("plugins.json");
        let temporary = self.state_dir.join("plugins.json.tmp");
        let file = File::create(&temporary)?;
        serde_json::to_writer_pretty(
            &file,
            &StoredRegistry {
                schema: REGISTRY_SCHEMA,
                plugins: self.plugins.values().cloned().collect(),
            },
        )?;
        file.sync_all()?;
        fs::rename(temporary, path)?;
        Ok(())
    }
}

pub async fn describe_plugin(
    executable: &Path,
    socket_dir: &Path,
) -> Result<PluginDescriptor, Box<dyn Error>> {
    if !executable.is_file() {
        return Err(format!("plugin executable not found: {}", executable.display()).into());
    }
    fs::create_dir_all(socket_dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(socket_dir, fs::Permissions::from_mode(0o700))?;
    }
    let socket = socket_dir.join(format!(
        "describe-{}-{}.sock",
        std::process::id(),
        DISCOVERY_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    let _socket = SocketFile(socket.clone());
    let mut child = Command::new(executable)
        .arg("--socket")
        .arg(&socket)
        .stdin(Stdio::null())
        .kill_on_drop(true)
        .spawn()?;
    let endpoint = format!("unix:{}", socket.display());
    let started = tokio::time::Instant::now();
    let mut client = loop {
        match AnalyzerPluginClient::connect(endpoint.clone()).await {
            Ok(client) => break client,
            Err(_) if started.elapsed() < CONNECT_TIMEOUT => {
                if let Some(status) = child.try_wait()? {
                    return Err(format!("plugin exited before discovery: {status}").into());
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            Err(error) => return Err(error.into()),
        }
    };
    let response = tokio::time::timeout(
        DESCRIBE_TIMEOUT,
        client.describe(Request::new(DescribeRequest {})),
    )
    .await
    .map_err(|_| "plugin descriptor request timed out")??
    .into_inner();
    let descriptor = descriptor_from_wire(
        response
            .descriptor
            .ok_or("plugin descriptor response is empty")?,
    )?;
    if tokio::time::timeout(SHUTDOWN_TIMEOUT, child.wait())
        .await
        .is_err()
    {
        child.kill().await?;
    }
    Ok(descriptor)
}

fn file_digest(path: &Path) -> Result<String, Box<dyn Error>> {
    let mut file = File::open(path)?;
    let mut hash = Sha256::new();
    std::io::copy(&mut file, &mut hash)?;
    Ok(format!("{:x}", hash.finalize()))
}

fn is_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

struct SocketFile(PathBuf);

impl Drop for SocketFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use beholder_indexing::{
        AnalysisInputKind, PLUGIN_API_VERSION, PluginInputScope, PluginInputSelector,
        PluginPathMatcher,
    };
    use std::{collections::BTreeSet, time::SystemTime};

    fn descriptor(id: &str) -> PluginDescriptor {
        PluginDescriptor {
            id: id.into(),
            api_version: PLUGIN_API_VERSION,
            inputs: vec![PluginInputSelector {
                scope: PluginInputScope::Target,
                matcher: PluginPathMatcher::Extension("rs".into()),
                kind: AnalysisInputKind::Source,
            }],
            semantic_entities: BTreeSet::new(),
            semantic_relations: BTreeSet::new(),
            produces_entities: BTreeSet::new(),
            produces_relations: BTreeSet::new(),
        }
    }

    #[test]
    fn registry_installs_immutable_executables_and_persists_selection() {
        let unique = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let state = std::env::temp_dir().join(format!("beholder-plugin-registry-{unique}"));
        fs::create_dir_all(&state).unwrap();
        let executable = state.join("candidate");
        fs::write(&executable, b"one").unwrap();

        let mut registry = PluginRegistry::open(&state).unwrap();
        for id in [RUST_WORKER_ID, ELIXIR_WORKER_ID, TYPESCRIPT_WORKER_ID] {
            let error = registry
                .install(&executable, descriptor(id), false)
                .unwrap_err();
            assert_eq!(
                error.to_string(),
                format!("plugin ID {id} is reserved for a built-in worker")
            );
            assert!(registry.install(&executable, descriptor(id), true).is_err());
        }

        let first = registry
            .install(&executable, descriptor("example"), false)
            .unwrap();
        let first_path = registry.executable(&first);
        assert_eq!(fs::read(&first_path).unwrap(), b"one");
        fs::write(&executable, b"collision").unwrap();
        assert_eq!(
            registry
                .install(&executable, descriptor("example"), false)
                .unwrap_err()
                .to_string(),
            "plugin example is already installed; use replace"
        );
        assert_eq!(registry.plugins().next(), Some(&first));
        assert_eq!(fs::read(&first_path).unwrap(), b"one");

        fs::write(&executable, b"two").unwrap();
        let second = registry
            .install(&executable, descriptor("example"), true)
            .unwrap();
        assert_ne!(first.digest, second.digest);
        assert_eq!(fs::read(&first_path).unwrap(), b"one");
        assert_eq!(fs::read(registry.executable(&second)).unwrap(), b"two");

        let mut reloaded = PluginRegistry::open(&state).unwrap();
        assert_eq!(
            reloaded
                .plugins()
                .map(|plugin| plugin.descriptor.id.as_str())
                .collect::<Vec<_>>(),
            ["example"]
        );
        assert_eq!(reloaded.plugins().next(), Some(&second));
        assert!(reloaded.remove("example").unwrap());
        assert!(
            PluginRegistry::open(&state)
                .unwrap()
                .plugins()
                .next()
                .is_none()
        );
        fs::remove_dir_all(state).unwrap();
    }
}
