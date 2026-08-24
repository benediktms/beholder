use std::{error::Error, fmt};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BeholderErrorCode {
    DaemonUnavailable,
    GarbageCollectionFailed,
    GarbageCollectionWorkerFailed,
    SchedulerUnavailable,
    RepositoryIndexFailed,
    RepositoryIndexWorkerFailed,
    RepositoryNotRegistered,
    RepositoryObservationCountOverflow,
    RepositoryRegistryFailed,
    SourceRecoveryUnsafe,
    TransportGrpc,
    WorkspaceIndexFailed,
    WorkspaceIndexWorkerFailed,
    WorkspaceNotRegistered,
    WorkspaceObservationCountOverflow,
    WorkspaceRegistryFailed,
    WorkspaceRevisionUnavailable,
}

impl BeholderErrorCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::DaemonUnavailable => "beholder.daemon.unavailable",
            Self::GarbageCollectionFailed => "beholder.garbage_collection.failed",
            Self::GarbageCollectionWorkerFailed => "beholder.garbage_collection.worker_failed",
            Self::SchedulerUnavailable => "beholder.scheduler.unavailable",
            Self::RepositoryIndexFailed => "beholder.repository.index_failed",
            Self::RepositoryIndexWorkerFailed => "beholder.repository.index_worker_failed",
            Self::RepositoryNotRegistered => "beholder.repository.not_registered",
            Self::RepositoryObservationCountOverflow => {
                "beholder.repository.observation_count_overflow"
            }
            Self::RepositoryRegistryFailed => "beholder.repository.registry_failed",
            Self::SourceRecoveryUnsafe => "beholder.source.recovery_unsafe",
            Self::TransportGrpc => "beholder.transport.grpc",
            Self::WorkspaceIndexFailed => "beholder.workspace.index_failed",
            Self::WorkspaceIndexWorkerFailed => "beholder.workspace.index_worker_failed",
            Self::WorkspaceNotRegistered => "beholder.workspace.not_registered",
            Self::WorkspaceObservationCountOverflow => {
                "beholder.workspace.observation_count_overflow"
            }
            Self::WorkspaceRegistryFailed => "beholder.workspace.registry_failed",
            Self::WorkspaceRevisionUnavailable => "beholder.workspace.revision_unavailable",
        }
    }
}

impl std::str::FromStr for BeholderErrorCode {
    type Err = ();

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "beholder.daemon.unavailable" => Ok(Self::DaemonUnavailable),
            "beholder.garbage_collection.failed" => Ok(Self::GarbageCollectionFailed),
            "beholder.garbage_collection.worker_failed" => Ok(Self::GarbageCollectionWorkerFailed),
            "beholder.scheduler.unavailable" => Ok(Self::SchedulerUnavailable),
            "beholder.repository.index_failed" => Ok(Self::RepositoryIndexFailed),
            "beholder.repository.index_worker_failed" => Ok(Self::RepositoryIndexWorkerFailed),
            "beholder.repository.not_registered" => Ok(Self::RepositoryNotRegistered),
            "beholder.repository.observation_count_overflow" => {
                Ok(Self::RepositoryObservationCountOverflow)
            }
            "beholder.repository.registry_failed" => Ok(Self::RepositoryRegistryFailed),
            "beholder.source.recovery_unsafe" => Ok(Self::SourceRecoveryUnsafe),
            "beholder.transport.grpc" => Ok(Self::TransportGrpc),
            "beholder.workspace.index_failed" => Ok(Self::WorkspaceIndexFailed),
            "beholder.workspace.index_worker_failed" => Ok(Self::WorkspaceIndexWorkerFailed),
            "beholder.workspace.not_registered" => Ok(Self::WorkspaceNotRegistered),
            "beholder.workspace.observation_count_overflow" => {
                Ok(Self::WorkspaceObservationCountOverflow)
            }
            "beholder.workspace.registry_failed" => Ok(Self::WorkspaceRegistryFailed),
            "beholder.workspace.revision_unavailable" => Ok(Self::WorkspaceRevisionUnavailable),
            _ => Err(()),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BeholderErrorKind {
    InvalidInput,
    NotFound,
    FailedPrecondition,
    Unavailable,
    Internal,
}

pub struct BeholderError {
    kind: BeholderErrorKind,
    code: BeholderErrorCode,
    message: String,
    source: Option<Box<dyn Error + Send + Sync>>,
}

/// A source cannot satisfy Beholder's conservative tree-recovery policy.
///
/// Adapters discard a complete top-level unit containing an `ERROR` node and
/// may publish unaffected sibling units. Any `MISSING` node aborts the source
/// because a missing delimiter can change the apparent nesting of later code.
#[derive(Debug)]
pub struct UnsafeTreeRecovery {
    language: &'static str,
    reason: &'static str,
}

impl UnsafeTreeRecovery {
    pub fn new(language: &'static str, reason: &'static str) -> Self {
        Self { language, reason }
    }
}

impl fmt::Display for UnsafeTreeRecovery {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{} source cannot be recovered safely: {}",
            self.language, self.reason
        )
    }
}

impl Error for UnsafeTreeRecovery {}

#[derive(Debug)]
pub struct SourceAnalysisError {
    unsafe_recovery: bool,
    message: String,
}

impl SourceAnalysisError {
    pub fn from_source(path: &std::path::Path, error: Box<dyn Error>) -> Self {
        let message = format!("failed to analyze {}: {error}", path.display());
        Self {
            unsafe_recovery: error.downcast_ref::<UnsafeTreeRecovery>().is_some(),
            message,
        }
    }

    pub fn is_unsafe_recovery(&self) -> bool {
        self.unsafe_recovery
    }
}

impl fmt::Display for SourceAnalysisError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl Error for SourceAnalysisError {}

impl BeholderError {
    pub fn new(
        kind: BeholderErrorKind,
        code: BeholderErrorCode,
        message: impl Into<String>,
    ) -> Self {
        Self {
            kind,
            code,
            message: message.into(),
            source: None,
        }
    }

    pub fn with_source(mut self, source: impl Error + Send + Sync + 'static) -> Self {
        self.source = Some(Box::new(source));
        self
    }

    pub fn kind(&self) -> BeholderErrorKind {
        self.kind
    }

    pub fn code(&self) -> BeholderErrorCode {
        self.code
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for BeholderError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "[{}] {}", self.code.as_str(), self.message)
    }
}

impl fmt::Debug for BeholderError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, formatter)
    }
}

impl Error for BeholderError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        self.source.as_deref().map(|source| source as _)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exposes_stable_public_diagnostics_without_printing_internal_causes() {
        for code in [
            BeholderErrorCode::DaemonUnavailable,
            BeholderErrorCode::GarbageCollectionFailed,
            BeholderErrorCode::GarbageCollectionWorkerFailed,
            BeholderErrorCode::SchedulerUnavailable,
            BeholderErrorCode::RepositoryIndexFailed,
            BeholderErrorCode::RepositoryIndexWorkerFailed,
            BeholderErrorCode::RepositoryNotRegistered,
            BeholderErrorCode::RepositoryObservationCountOverflow,
            BeholderErrorCode::RepositoryRegistryFailed,
            BeholderErrorCode::SourceRecoveryUnsafe,
            BeholderErrorCode::TransportGrpc,
            BeholderErrorCode::WorkspaceIndexFailed,
            BeholderErrorCode::WorkspaceIndexWorkerFailed,
            BeholderErrorCode::WorkspaceNotRegistered,
            BeholderErrorCode::WorkspaceObservationCountOverflow,
            BeholderErrorCode::WorkspaceRegistryFailed,
            BeholderErrorCode::WorkspaceRevisionUnavailable,
        ] {
            assert_eq!(code.as_str().parse(), Ok(code));
        }

        let error = BeholderError::new(
            BeholderErrorKind::Internal,
            BeholderErrorCode::WorkspaceIndexFailed,
            "workspace indexing failed",
        )
        .with_source(std::io::Error::other("database password leaked here"));

        assert_eq!(error.kind(), BeholderErrorKind::Internal);
        assert_eq!(error.code(), BeholderErrorCode::WorkspaceIndexFailed);
        assert_eq!(error.code().as_str(), "beholder.workspace.index_failed");
        assert_eq!(
            error.to_string(),
            "[beholder.workspace.index_failed] workspace indexing failed"
        );
        assert!(!format!("{error:?}").contains("password"));
        assert!(error.source().unwrap().to_string().contains("password"));
    }

    #[test]
    fn classifies_unsafe_tree_recovery_without_matching_error_text() {
        let error = SourceAnalysisError::from_source(
            std::path::Path::new("src/broken.rs"),
            Box::new(UnsafeTreeRecovery::new(
                "Rust",
                "missing syntax may change nesting",
            )),
        );

        assert!(error.is_unsafe_recovery());
        assert!(error.to_string().contains("src/broken.rs"));
    }
}
