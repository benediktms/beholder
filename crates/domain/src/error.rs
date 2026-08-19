use std::{error::Error, fmt};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BeholderErrorCode {
    DaemonUnavailable,
    TransportGrpc,
    WorkspaceIndexFailed,
    WorkspaceIndexWorkerFailed,
    WorkspaceNotRegistered,
    WorkspaceObservationCountOverflow,
    WorkspaceRegistryFailed,
}

impl BeholderErrorCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::DaemonUnavailable => "beholder.daemon.unavailable",
            Self::TransportGrpc => "beholder.transport.grpc",
            Self::WorkspaceIndexFailed => "beholder.workspace.index_failed",
            Self::WorkspaceIndexWorkerFailed => "beholder.workspace.index_worker_failed",
            Self::WorkspaceNotRegistered => "beholder.workspace.not_registered",
            Self::WorkspaceObservationCountOverflow => {
                "beholder.workspace.observation_count_overflow"
            }
            Self::WorkspaceRegistryFailed => "beholder.workspace.registry_failed",
        }
    }
}

impl std::str::FromStr for BeholderErrorCode {
    type Err = ();

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "beholder.daemon.unavailable" => Ok(Self::DaemonUnavailable),
            "beholder.transport.grpc" => Ok(Self::TransportGrpc),
            "beholder.workspace.index_failed" => Ok(Self::WorkspaceIndexFailed),
            "beholder.workspace.index_worker_failed" => Ok(Self::WorkspaceIndexWorkerFailed),
            "beholder.workspace.not_registered" => Ok(Self::WorkspaceNotRegistered),
            "beholder.workspace.observation_count_overflow" => {
                Ok(Self::WorkspaceObservationCountOverflow)
            }
            "beholder.workspace.registry_failed" => Ok(Self::WorkspaceRegistryFailed),
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
            BeholderErrorCode::TransportGrpc,
            BeholderErrorCode::WorkspaceIndexFailed,
            BeholderErrorCode::WorkspaceIndexWorkerFailed,
            BeholderErrorCode::WorkspaceNotRegistered,
            BeholderErrorCode::WorkspaceObservationCountOverflow,
            BeholderErrorCode::WorkspaceRegistryFailed,
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
}
