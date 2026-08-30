use super::sources::accepted_files;
use beholder_adapters_git::repository_state_from_content_hashes;
use beholder_domain::{BeholderError, BeholderErrorCode, BeholderErrorKind, Workspace};
use beholder_indexing::{
    Indexer, InputKind, RepositoryInput, RepositorySnapshot, WorkspaceSnapshot,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::UNIX_EPOCH,
};

const INVENTORY_VERSION: u32 = 1;
const CHECKPOINT_VERSION: u32 = 1;
static TEMPORARY_FILE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

pub(super) enum RefreshMode<'a> {
    Hinted,
    Dirty(&'a BTreeSet<PathBuf>),
    Authoritative,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) struct InventoryStatistics {
    pub(super) discovered_inputs: usize,
    pub(super) watcher_inputs: usize,
    pub(super) content_hashes: usize,
    pub(super) authoritative_hashes: usize,
    pub(super) watcher_hashes: usize,
    pub(super) membership_or_metadata_hashes: usize,
    pub(super) cache_recovery_hashes: usize,
    pub(super) repository_bytes_read: u64,
    pub(super) cached_bytes_reused: u64,
    pub(super) repositories_changed: usize,
    pub(super) repositories_reused: usize,
}

impl std::ops::AddAssign for InventoryStatistics {
    fn add_assign(&mut self, other: Self) {
        self.discovered_inputs += other.discovered_inputs;
        self.watcher_inputs += other.watcher_inputs;
        self.content_hashes += other.content_hashes;
        self.authoritative_hashes += other.authoritative_hashes;
        self.watcher_hashes += other.watcher_hashes;
        self.membership_or_metadata_hashes += other.membership_or_metadata_hashes;
        self.cache_recovery_hashes += other.cache_recovery_hashes;
        self.repository_bytes_read += other.repository_bytes_read;
        self.cached_bytes_reused += other.cached_bytes_reused;
        self.repositories_changed += other.repositories_changed;
        self.repositories_reused += other.repositories_reused;
    }
}

pub(super) struct InventoryRefresh {
    pub(super) snapshot: RepositorySnapshot,
    pub(super) statistics: InventoryStatistics,
}

pub(super) struct InventoryStore {
    root: PathBuf,
    verified: Mutex<BTreeSet<String>>,
    content: Mutex<BTreeMap<String, Arc<[u8]>>>,
}

pub(super) struct WorkspaceCheckpoint<'a> {
    pub(super) workspace: &'a Workspace,
    pub(super) snapshot: &'a WorkspaceSnapshot,
    pub(super) indexer: &'a Indexer,
    pub(super) verification_fingerprint: &'a str,
}

pub(super) struct CheckpointVerification {
    pub(super) verification_fingerprint: String,
    inventory_keys: Vec<String>,
}

struct ManifestVerification<'a> {
    repository: &'a str,
    base: &'a Path,
    descriptors: &'a [PathBuf],
    indexer: &'a Indexer,
    key: &'a str,
    expected_fingerprint: &'a str,
}

impl InventoryStore {
    pub(super) fn new(cache_dir: &Path) -> Self {
        let cache_dir = if cache_dir.as_os_str().is_empty() {
            std::env::temp_dir().join(format!(
                "beholder-inventory-ephemeral-{}",
                std::process::id()
            ))
        } else {
            cache_dir.to_path_buf()
        };
        Self {
            root: cache_dir.join("repository-inventory-v1"),
            verified: Mutex::new(BTreeSet::new()),
            content: Mutex::new(BTreeMap::new()),
        }
    }

    pub(super) fn clear(&self) {
        if let Ok(mut verified) = self.verified.lock() {
            verified.clear();
        }
        if let Ok(mut content) = self.content.lock() {
            content.clear();
        }
    }

    pub(super) fn verify_checkpoint(
        &self,
        workspace: &Workspace,
        indexer: &Indexer,
    ) -> Result<Option<CheckpointVerification>, BeholderError> {
        let path = self.checkpoint_path(&workspace.name);
        let Some(checkpoint) = load_checkpoint(&path) else {
            tracing::debug!(workspace = %workspace.name, "reconciliation checkpoint unavailable");
            return Ok(None);
        };
        let reconciliation_identity = indexer.reconciliation_identity();
        if checkpoint.reconciliation_identity != reconciliation_identity
            || checkpoint.enabled_plugins != workspace.enabled_plugins
            || checkpoint.repositories.len() != workspace.repositories.len()
        {
            tracing::debug!(
                workspace = %workspace.name,
                catalog_matches = checkpoint.reconciliation_identity == reconciliation_identity,
                plugin_configuration_matches = checkpoint.enabled_plugins == workspace.enabled_plugins,
                repository_count_matches = checkpoint.repositories.len() == workspace.repositories.len(),
                "reconciliation checkpoint configuration changed"
            );
            return Ok(None);
        }
        let mut inventory_keys = Vec::with_capacity(workspace.repositories.len());
        for (configured, persisted) in workspace.repositories.iter().zip(&checkpoint.repositories) {
            let descriptors = workspace
                .protobuf_descriptors
                .iter()
                .filter(|descriptor| descriptor.repository == configured.repository)
                .map(|descriptor| descriptor.path.clone())
                .collect::<Vec<_>>();
            let canonical = configured
                .base
                .canonicalize()
                .map_err(|error| input_error(&configured.base, error))?;
            let key = inventory_key(&configured.repository.identity, &canonical, &descriptors);
            let verified = self.verify_manifest(ManifestVerification {
                repository: &configured.repository.identity,
                base: &configured.base,
                descriptors: &descriptors,
                indexer,
                key: &key,
                expected_fingerprint: &persisted.fingerprint,
            })?;
            if persisted.identity != configured.repository.identity
                || persisted.inventory_key != key
                || !verified
            {
                tracing::debug!(
                    workspace = %workspace.name,
                    repository = %configured.repository.identity,
                    identity_matches = persisted.identity == configured.repository.identity,
                    inventory_matches = persisted.inventory_key == key,
                    manifest_matches = verified,
                    "reconciliation checkpoint repository changed"
                );
                return Ok(None);
            }
            inventory_keys.push(key);
        }
        Ok(Some(CheckpointVerification {
            verification_fingerprint: checkpoint.verification_fingerprint,
            inventory_keys,
        }))
    }

    pub(super) fn accept_checkpoint(
        &self,
        checkpoint: &CheckpointVerification,
        base: &Path,
    ) -> Result<(), BeholderError> {
        self.verified
            .lock()
            .map_err(|_| input_error(base, "inventory verification lock poisoned"))?
            .extend(checkpoint.inventory_keys.iter().cloned());
        Ok(())
    }

    pub(super) fn store_checkpoint(
        &self,
        checkpoint: WorkspaceCheckpoint<'_>,
    ) -> Result<(), BeholderError> {
        let repositories = checkpoint
            .workspace
            .repositories
            .iter()
            .zip(&checkpoint.snapshot.repositories)
            .map(|(configured, snapshot)| {
                let descriptors = checkpoint
                    .workspace
                    .protobuf_descriptors
                    .iter()
                    .filter(|descriptor| descriptor.repository == configured.repository)
                    .map(|descriptor| descriptor.path.clone())
                    .collect::<Vec<_>>();
                let canonical = configured
                    .base
                    .canonicalize()
                    .map_err(|error| input_error(&configured.base, error))?;
                Ok(PersistedCheckpointRepository {
                    identity: configured.repository.identity.clone(),
                    inventory_key: inventory_key(
                        &configured.repository.identity,
                        &canonical,
                        &descriptors,
                    ),
                    fingerprint: snapshot.state.fingerprint.clone(),
                })
            })
            .collect::<Result<Vec<_>, BeholderError>>()?;
        let persisted = PersistedWorkspaceCheckpoint {
            version: CHECKPOINT_VERSION,
            reconciliation_identity: checkpoint.indexer.reconciliation_identity(),
            enabled_plugins: checkpoint.workspace.enabled_plugins.clone(),
            verification_fingerprint: checkpoint.verification_fingerprint.to_owned(),
            repositories,
        };
        let path = self.checkpoint_path(&checkpoint.workspace.name);
        let parent = path.parent().expect("checkpoint path has a parent");
        fs::create_dir_all(parent).map_err(|error| input_error(parent, error))?;
        let bytes = serde_json::to_vec(&persisted)
            .map_err(|error| input_error(&checkpoint.snapshot.repositories[0].base, error))?;
        atomic_write(&path, &bytes).map_err(|error| input_error(&path, error))
    }

    fn verify_manifest(&self, input: ManifestVerification<'_>) -> Result<bool, BeholderError> {
        let path = self
            .root
            .join("manifests")
            .join(format!("{}.json", input.key));
        let Some(manifest) = load_manifest(&path) else {
            return Ok(false);
        };
        let fingerprint = manifest.repository_fingerprint.clone();
        let previous = manifest
            .entries
            .into_iter()
            .map(|entry| ((entry.path.clone(), entry.kind), entry))
            .collect::<BTreeMap<_, _>>();
        let candidates = inventory_candidates(input.base, input.descriptors, input.indexer)?;
        if candidates.len() != previous.len() {
            return Ok(false);
        }
        let mut inputs = Vec::with_capacity(candidates.len());
        for (path, kind) in candidates {
            let relative = path
                .strip_prefix(input.base)
                .map_err(|error| input_error(&path, error))?
                .to_path_buf();
            let Some(entry) = previous.get(&(StoredPath::from_path(&relative), kind)) else {
                return Ok(false);
            };
            let Ok(metadata) = fs::metadata(&path) else {
                return Ok(false);
            };
            if entry.metadata != MetadataHint::from(&metadata) {
                return Ok(false);
            }
            inputs.push((relative, kind, entry.content_hash.as_str()));
        }
        let mut state = repository_state_from_content_hashes(
            input.base,
            inputs
                .iter()
                .map(|(path, kind, hash)| (path.as_path(), u8::from(*kind), *hash)),
        )
        .map_err(|error| input_error(input.base, error))?;
        state.repository.identity = input.repository.to_owned();
        Ok(fingerprint.as_deref() == Some(state.fingerprint.as_str())
            && state.fingerprint == input.expected_fingerprint)
    }

    fn checkpoint_path(&self, workspace: &str) -> PathBuf {
        self.root
            .join("checkpoints")
            .join(format!("{:x}.json", Sha256::digest(workspace.as_bytes())))
    }

    pub(super) fn refresh(
        &self,
        repository: &str,
        base: &Path,
        descriptors: &[PathBuf],
        indexer: &Indexer,
        mode: RefreshMode<'_>,
    ) -> Result<InventoryRefresh, BeholderError> {
        if !base.is_dir() {
            return Err(input_error(
                base,
                format!("repository does not exist: {}", base.display()),
            ));
        }
        let canonical = base
            .canonicalize()
            .map_err(|error| input_error(base, error))?;
        let key = inventory_key(repository, &canonical, descriptors);
        let runtime_verified = self
            .verified
            .lock()
            .map_err(|_| input_error(base, "inventory verification lock poisoned"))?
            .contains(&key);
        let authoritative = matches!(mode, RefreshMode::Authoritative) || !runtime_verified;
        let dirty = match mode {
            RefreshMode::Dirty(paths) if runtime_verified => Some(paths),
            _ => None,
        };
        let manifest_path = self.root.join("manifests").join(format!("{key}.json"));
        let previous = load_manifest(&manifest_path).unwrap_or_default();
        let previous_fingerprint = previous.repository_fingerprint;
        let previous = previous
            .entries
            .into_iter()
            .map(|entry| ((entry.path.clone(), entry.kind), entry))
            .collect::<BTreeMap<_, _>>();

        let candidates = inventory_candidates(base, descriptors, indexer)?;

        let mut statistics = InventoryStatistics {
            discovered_inputs: candidates.len(),
            watcher_inputs: dirty.map_or(0, BTreeSet::len),
            ..InventoryStatistics::default()
        };
        let mut entries = Vec::with_capacity(candidates.len());
        let mut inputs = Vec::with_capacity(candidates.len());
        for (path, kind) in candidates {
            let relative = path
                .strip_prefix(base)
                .map_err(|error| input_error(&path, error))?
                .to_path_buf();
            let stored_path = StoredPath::from_path(&relative);
            let metadata = fs::metadata(&path).map_err(|error| input_error(&path, error))?;
            let hint = MetadataHint::from(&metadata);
            let old = previous.get(&(stored_path.clone(), kind));
            let watcher_selected = dirty.is_some_and(|paths| paths.contains(&relative));
            let selected =
                authoritative || watcher_selected || old.is_none_or(|entry| entry.metadata != hint);
            let (content, content_hash) = if selected {
                let bytes = fs::read(&path).map_err(|error| input_error(&path, error))?;
                statistics.content_hashes += 1;
                if authoritative {
                    statistics.authoritative_hashes += 1;
                } else if watcher_selected {
                    statistics.watcher_hashes += 1;
                } else {
                    statistics.membership_or_metadata_hashes += 1;
                }
                statistics.repository_bytes_read += bytes.len() as u64;
                let hash = format!("{:x}", Sha256::digest(&bytes));
                let content = Arc::<[u8]>::from(bytes);
                self.remember(&hash, content.clone(), base)?;
                self.store_blob(&hash, &content, base)?;
                (content, hash)
            } else {
                let hash = old
                    .expect("unselected entries have a previous value")
                    .content_hash
                    .clone();
                match self.load_content(&hash, base) {
                    Ok(content) => {
                        statistics.cached_bytes_reused += content.len() as u64;
                        (content, hash)
                    }
                    Err(_) => {
                        let bytes = fs::read(&path).map_err(|error| input_error(&path, error))?;
                        statistics.content_hashes += 1;
                        statistics.cache_recovery_hashes += 1;
                        statistics.repository_bytes_read += bytes.len() as u64;
                        let hash = format!("{:x}", Sha256::digest(&bytes));
                        let content = Arc::<[u8]>::from(bytes);
                        self.remember(&hash, content.clone(), base)?;
                        self.store_blob(&hash, &content, base)?;
                        (content, hash)
                    }
                }
            };
            entries.push(PersistedEntry {
                path: stored_path,
                kind,
                metadata: hint,
                content_hash,
            });
            inputs.push(RepositoryInput {
                path: relative,
                content,
                kind: kind.into(),
            });
        }

        let mut state = repository_state_from_content_hashes(
            base,
            inputs.iter().zip(&entries).map(|(input, entry)| {
                (
                    input.path.as_path(),
                    match input.kind {
                        InputKind::Source => 0,
                        InputKind::ProtobufDescriptor => 1,
                    },
                    entry.content_hash.as_str(),
                )
            }),
        )
        .map_err(|error| input_error(base, error))?;
        state.repository.identity = repository.to_owned();
        if previous_fingerprint.as_deref() == Some(state.fingerprint.as_str()) {
            statistics.repositories_reused = 1;
        } else {
            statistics.repositories_changed = 1;
        }
        store_manifest(
            &manifest_path,
            &PersistedInventory {
                version: INVENTORY_VERSION,
                repository_fingerprint: Some(state.fingerprint.clone()),
                entries,
            },
            base,
        )?;
        self.verified
            .lock()
            .map_err(|_| input_error(base, "inventory verification lock poisoned"))?
            .insert(key);
        Ok(InventoryRefresh {
            snapshot: RepositorySnapshot {
                base: base.to_path_buf(),
                state,
                inputs,
            },
            statistics,
        })
    }

    fn remember(&self, hash: &str, content: Arc<[u8]>, base: &Path) -> Result<(), BeholderError> {
        self.content
            .lock()
            .map_err(|_| input_error(base, "inventory content lock poisoned"))?
            .insert(hash.to_owned(), content);
        Ok(())
    }

    fn load_content(&self, hash: &str, base: &Path) -> Result<Arc<[u8]>, BeholderError> {
        if let Some(content) = self
            .content
            .lock()
            .map_err(|_| input_error(base, "inventory content lock poisoned"))?
            .get(hash)
            .cloned()
        {
            return Ok(content);
        }
        let path = blob_path(&self.root, hash)
            .ok_or_else(|| input_error(base, "invalid inventory content digest"))?;
        let bytes = fs::read(&path).map_err(|error| input_error(&path, error))?;
        if format!("{:x}", Sha256::digest(&bytes)) != hash {
            return Err(input_error(&path, "inventory content digest mismatch"));
        }
        let content = Arc::<[u8]>::from(bytes);
        self.remember(hash, content.clone(), base)?;
        Ok(content)
    }

    fn store_blob(&self, hash: &str, content: &[u8], base: &Path) -> Result<(), BeholderError> {
        let path = blob_path(&self.root, hash)
            .expect("new inventory content digests are valid SHA-256 values");
        if fs::metadata(&path).is_ok_and(|metadata| metadata.len() == content.len() as u64) {
            return Ok(());
        }
        let parent = path.parent().expect("blob path has a parent");
        fs::create_dir_all(parent).map_err(|error| input_error(parent, error))?;
        atomic_write(&path, content).map_err(|error| input_error(base, error))
    }
}

#[derive(Default, Deserialize, Serialize)]
struct PersistedInventory {
    version: u32,
    #[serde(default)]
    repository_fingerprint: Option<String>,
    entries: Vec<PersistedEntry>,
}

#[derive(Deserialize, Serialize)]
struct PersistedEntry {
    path: StoredPath,
    kind: StoredInputKind,
    metadata: MetadataHint,
    content_hash: String,
}

#[derive(Deserialize, Serialize)]
struct PersistedWorkspaceCheckpoint {
    version: u32,
    reconciliation_identity: String,
    enabled_plugins: BTreeSet<String>,
    verification_fingerprint: String,
    repositories: Vec<PersistedCheckpointRepository>,
}

#[derive(Deserialize, Serialize)]
struct PersistedCheckpointRepository {
    identity: String,
    inventory_key: String,
    fingerprint: String,
}

#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
struct StoredPath(Vec<u8>);

impl StoredPath {
    fn from_path(path: &Path) -> Self {
        Self(path_bytes(path))
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
enum StoredInputKind {
    Source,
    ProtobufDescriptor,
}

impl From<StoredInputKind> for u8 {
    fn from(value: StoredInputKind) -> Self {
        match value {
            StoredInputKind::Source => 0,
            StoredInputKind::ProtobufDescriptor => 1,
        }
    }
}

impl From<StoredInputKind> for InputKind {
    fn from(value: StoredInputKind) -> Self {
        match value {
            StoredInputKind::Source => Self::Source,
            StoredInputKind::ProtobufDescriptor => Self::ProtobufDescriptor,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct MetadataHint {
    len: u64,
    modified_nanos: Option<u128>,
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
    #[cfg(unix)]
    changed_seconds: i64,
    #[cfg(unix)]
    changed_nanos: i64,
}

impl From<&fs::Metadata> for MetadataHint {
    fn from(metadata: &fs::Metadata) -> Self {
        Self {
            len: metadata.len(),
            modified_nanos: metadata
                .modified()
                .ok()
                .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
                .map(|duration| duration.as_nanos()),
            #[cfg(unix)]
            device: {
                use std::os::unix::fs::MetadataExt;
                metadata.dev()
            },
            #[cfg(unix)]
            inode: {
                use std::os::unix::fs::MetadataExt;
                metadata.ino()
            },
            #[cfg(unix)]
            changed_seconds: {
                use std::os::unix::fs::MetadataExt;
                metadata.ctime()
            },
            #[cfg(unix)]
            changed_nanos: {
                use std::os::unix::fs::MetadataExt;
                metadata.ctime_nsec()
            },
        }
    }
}

fn load_manifest(path: &Path) -> Option<PersistedInventory> {
    let bytes = fs::read(path).ok()?;
    let manifest = serde_json::from_slice::<PersistedInventory>(&bytes).ok()?;
    (manifest.version == INVENTORY_VERSION).then_some(manifest)
}

fn inventory_candidates(
    base: &Path,
    descriptors: &[PathBuf],
    indexer: &Indexer,
) -> Result<Vec<(PathBuf, StoredInputKind)>, BeholderError> {
    let mut files = Vec::new();
    accepted_files(base, indexer, &mut files).map_err(|error| input_error(base, error))?;
    let mut candidates = files
        .into_iter()
        .map(|path| (path, StoredInputKind::Source))
        .chain(
            descriptors
                .iter()
                .cloned()
                .map(|path| (path, StoredInputKind::ProtobufDescriptor)),
        )
        .collect::<Vec<_>>();
    candidates.sort_by(|left, right| {
        let left = (left.0.strip_prefix(base).unwrap_or(&left.0), left.1);
        let right = (right.0.strip_prefix(base).unwrap_or(&right.0), right.1);
        left.cmp(&right)
    });
    Ok(candidates)
}

fn load_checkpoint(path: &Path) -> Option<PersistedWorkspaceCheckpoint> {
    let bytes = fs::read(path).ok()?;
    let checkpoint = serde_json::from_slice::<PersistedWorkspaceCheckpoint>(&bytes).ok()?;
    (checkpoint.version == CHECKPOINT_VERSION).then_some(checkpoint)
}

fn store_manifest(
    path: &Path,
    manifest: &PersistedInventory,
    base: &Path,
) -> Result<(), BeholderError> {
    let parent = path.parent().expect("manifest path has a parent");
    fs::create_dir_all(parent).map_err(|error| input_error(parent, error))?;
    let bytes = serde_json::to_vec(manifest).map_err(|error| input_error(base, error))?;
    atomic_write(path, &bytes).map_err(|error| input_error(path, error))
}

fn atomic_write(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let temporary = path.with_extension(format!(
        "tmp-{}-{}",
        std::process::id(),
        TEMPORARY_FILE_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    fs::write(&temporary, bytes)?;
    fs::rename(temporary, path)
}

fn blob_path(root: &Path, hash: &str) -> Option<PathBuf> {
    (hash.len() == 64 && hash.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .then(|| root.join("blobs").join(&hash[..2]).join(hash))
}

fn inventory_key(repository: &str, canonical: &Path, descriptors: &[PathBuf]) -> String {
    let mut digest = Sha256::new();
    framed(&mut digest, repository.as_bytes());
    framed(&mut digest, &path_bytes(canonical));
    let mut descriptors = descriptors
        .iter()
        .map(|path| path_bytes(path))
        .collect::<Vec<_>>();
    descriptors.sort();
    for descriptor in descriptors {
        framed(&mut digest, &descriptor);
    }
    format!("{:x}", digest.finalize())
}

fn framed(digest: &mut Sha256, value: &[u8]) {
    digest.update((value.len() as u64).to_le_bytes());
    digest.update(value);
}

#[cfg(unix)]
fn path_bytes(path: &Path) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt;
    path.as_os_str().as_bytes().to_vec()
}

#[cfg(windows)]
fn path_bytes(path: &Path) -> Vec<u8> {
    use std::os::windows::ffi::OsStrExt;
    path.as_os_str()
        .encode_wide()
        .flat_map(u16::to_le_bytes)
        .collect()
}

#[cfg(not(any(unix, windows)))]
fn path_bytes(path: &Path) -> Vec<u8> {
    path.to_string_lossy().into_owned().into_bytes()
}

fn input_error(path: &Path, error: impl std::fmt::Display) -> BeholderError {
    BeholderError::new(
        BeholderErrorKind::FailedPrecondition,
        BeholderErrorCode::WorkspaceIndexFailed,
        format!("failed to load indexing inputs from {}", path.display()),
    )
    .with_source(std::io::Error::other(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use beholder_adapters_graphql::GraphqlAnalyzer;
    use beholder_indexing::IndexerBuilder;
    use std::{process::Command, time::SystemTime};

    fn fixture(name: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("beholder-inventory-{name}-{unique}"))
    }

    fn indexer(cache: &Path) -> Indexer {
        IndexerBuilder::new(cache.to_path_buf(), 1)
            .add_analyzer(GraphqlAnalyzer)
            .build()
            .unwrap()
    }

    #[test]
    fn watcher_refresh_hashes_only_the_named_input() {
        let root = fixture("dirty");
        let cache = root.join("cache");
        let repository = root.join("repository");
        fs::create_dir_all(&repository).unwrap();
        fs::write(repository.join("one.graphql"), "type One { id: ID! }").unwrap();
        fs::write(repository.join("two.graphql"), "type Two { id: ID! }").unwrap();
        let indexer = indexer(&cache);
        let store = InventoryStore::new(&cache);

        let initial = store
            .refresh("repo", &repository, &[], &indexer, RefreshMode::Hinted)
            .unwrap();
        assert_eq!(initial.statistics.content_hashes, 2);
        let unchanged = store
            .refresh("repo", &repository, &[], &indexer, RefreshMode::Hinted)
            .unwrap();
        assert_eq!(unchanged.statistics.content_hashes, 0);
        assert_eq!(unchanged.statistics.cached_bytes_reused, 40);

        fs::write(repository.join("one.graphql"), "type One { ok: ID! }").unwrap();
        let dirty = BTreeSet::from([PathBuf::from("one.graphql")]);
        let changed = store
            .refresh(
                "repo",
                &repository,
                &[],
                &indexer,
                RefreshMode::Dirty(&dirty),
            )
            .unwrap();
        assert_eq!(changed.statistics.content_hashes, 1);
        assert_ne!(
            initial.snapshot.state.fingerprint,
            changed.snapshot.state.fingerprint
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn restart_and_reconciliation_are_content_authoritative() {
        let root = fixture("reconcile");
        let cache = root.join("cache");
        let repository = root.join("repository");
        fs::create_dir_all(&repository).unwrap();
        fs::write(repository.join("schema.graphql"), "type Query { a: ID! }").unwrap();
        let indexer = indexer(&cache);
        let store = InventoryStore::new(&cache);
        store
            .refresh("repo", &repository, &[], &indexer, RefreshMode::Hinted)
            .unwrap();

        let restarted = InventoryStore::new(&cache)
            .refresh("repo", &repository, &[], &indexer, RefreshMode::Hinted)
            .unwrap();
        assert_eq!(restarted.statistics.content_hashes, 1);

        fs::write(repository.join("schema.graphql"), "type Query { b: ID! }").unwrap();
        let canonical = repository.canonicalize().unwrap();
        let key = inventory_key("repo", &canonical, &[]);
        let manifest_path = cache
            .join("repository-inventory-v1/manifests")
            .join(format!("{key}.json"));
        let mut manifest = load_manifest(&manifest_path).unwrap();
        manifest.entries[0].metadata =
            MetadataHint::from(&fs::metadata(repository.join("schema.graphql")).unwrap());
        store_manifest(&manifest_path, &manifest, &repository).unwrap();
        let metadata_deceived = store
            .refresh("repo", &repository, &[], &indexer, RefreshMode::Hinted)
            .unwrap();
        assert_eq!(metadata_deceived.statistics.content_hashes, 0);
        assert_eq!(
            restarted.snapshot.state.fingerprint,
            metadata_deceived.snapshot.state.fingerprint
        );

        let reconciled = store
            .refresh(
                "repo",
                &repository,
                &[],
                &indexer,
                RefreshMode::Authoritative,
            )
            .unwrap();
        assert_eq!(reconciled.statistics.content_hashes, 1);
        assert_ne!(
            restarted.snapshot.state.fingerprint,
            reconciled.snapshot.state.fingerprint
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn membership_and_git_head_participate_in_identity() {
        let root = fixture("git");
        let cache = root.join("cache");
        let repository = root.join("repository");
        fs::create_dir_all(&repository).unwrap();
        fs::write(repository.join("schema.graphql"), "type Query { id: ID! }").unwrap();
        fs::write(repository.join("README.md"), "first").unwrap();
        for args in [
            vec!["init"],
            vec!["config", "user.email", "test@example.com"],
            vec!["config", "user.name", "Beholder Test"],
            vec!["config", "commit.gpgsign", "false"],
            vec!["add", "."],
            vec!["commit", "-m", "first"],
        ] {
            assert!(
                Command::new("git")
                    .arg("-C")
                    .arg(&repository)
                    .args(args)
                    .status()
                    .unwrap()
                    .success()
            );
        }
        let indexer = indexer(&cache);
        let store = InventoryStore::new(&cache);
        let initial = store
            .refresh("repo", &repository, &[], &indexer, RefreshMode::Hinted)
            .unwrap();

        fs::write(repository.join("README.md"), "second").unwrap();
        assert!(
            Command::new("git")
                .arg("-C")
                .arg(&repository)
                .args(["add", "README.md"])
                .status()
                .unwrap()
                .success()
        );
        assert!(
            Command::new("git")
                .arg("-C")
                .arg(&repository)
                .args(["commit", "-m", "second"])
                .status()
                .unwrap()
                .success()
        );
        let head_changed = store
            .refresh("repo", &repository, &[], &indexer, RefreshMode::Hinted)
            .unwrap();
        assert_eq!(head_changed.statistics.content_hashes, 0);
        assert_ne!(
            initial.snapshot.state.head,
            head_changed.snapshot.state.head
        );
        assert_ne!(
            initial.snapshot.state.fingerprint,
            head_changed.snapshot.state.fingerprint
        );

        fs::rename(
            repository.join("schema.graphql"),
            repository.join("renamed.graphql"),
        )
        .unwrap();
        let renamed = store
            .refresh("repo", &repository, &[], &indexer, RefreshMode::Hinted)
            .unwrap();
        assert_eq!(
            renamed.snapshot.inputs[0].path,
            Path::new("renamed.graphql")
        );
        assert_ne!(
            head_changed.snapshot.state.fingerprint,
            renamed.snapshot.state.fingerprint
        );

        fs::remove_dir_all(root).unwrap();
    }
}
