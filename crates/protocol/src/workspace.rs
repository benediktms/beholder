use crate::v1;
use beholder_domain::{
    LogicalRepository, ProtobufDescriptorSource, Workspace as DomainWorkspace,
    WorkspaceRepository as DomainRepository,
};
use beholder_dto::{AnalysisCompleteness, AnalysisMetadata, RepositoryRevision, RepositoryStatus};
use std::path::PathBuf;

impl From<DomainRepository> for v1::WorkspaceRepository {
    fn from(repository: DomainRepository) -> Self {
        Self {
            identity: repository.repository.identity,
            display_name: repository.display_name,
            base: repository.base.to_string_lossy().into_owned(),
            alternatives: repository
                .alternatives
                .into_iter()
                .map(|path| path.to_string_lossy().into_owned())
                .collect(),
        }
    }
}

impl From<v1::WorkspaceRepository> for DomainRepository {
    fn from(repository: v1::WorkspaceRepository) -> Self {
        Self {
            repository: LogicalRepository {
                identity: repository.identity,
            },
            display_name: repository.display_name,
            base: PathBuf::from(repository.base),
            alternatives: repository
                .alternatives
                .into_iter()
                .map(PathBuf::from)
                .collect(),
        }
    }
}

impl From<DomainWorkspace> for v1::Workspace {
    fn from(workspace: DomainWorkspace) -> Self {
        Self {
            name: workspace.name,
            repositories: workspace.repositories.into_iter().map(Into::into).collect(),
            protobuf_descriptors: workspace
                .protobuf_descriptors
                .into_iter()
                .map(|descriptor| v1::ProtobufDescriptorSource {
                    repository: descriptor.repository.identity,
                    path: descriptor.path.to_string_lossy().into_owned(),
                })
                .collect(),
            enabled_plugins: workspace.enabled_plugins.into_iter().collect(),
        }
    }
}

impl TryFrom<v1::Workspace> for DomainWorkspace {
    type Error = String;

    fn try_from(workspace: v1::Workspace) -> Result<Self, Self::Error> {
        Self::new(
            workspace.name,
            workspace.repositories.into_iter().map(Into::into).collect(),
        )?
        .with_protobuf_descriptors(
            workspace
                .protobuf_descriptors
                .into_iter()
                .map(|descriptor| ProtobufDescriptorSource {
                    repository: LogicalRepository {
                        identity: descriptor.repository,
                    },
                    path: PathBuf::from(descriptor.path),
                })
                .collect(),
        )?
        .with_enabled_plugins(workspace.enabled_plugins)
    }
}

impl From<RepositoryStatus> for v1::RepositoryStatus {
    fn from(status: RepositoryStatus) -> Self {
        Self {
            repository: Some(v1::WorkspaceRepository {
                identity: status.identity,
                display_name: status.display_name,
                base: status.base.to_string_lossy().into_owned(),
                alternatives: status
                    .alternatives
                    .into_iter()
                    .map(|path| path.to_string_lossy().into_owned())
                    .collect(),
            }),
            revision: status.revision.map(|revision| v1::RepositoryRevision {
                source_state: revision.source_state,
                head: revision.head,
                analysis_identity: revision.analysis_identity,
                incomplete: revision.analysis.completeness == AnalysisCompleteness::Incomplete,
                diagnostics: revision
                    .analysis
                    .diagnostics
                    .into_iter()
                    .map(Into::into)
                    .collect(),
            }),
            indexing: status.indexing,
        }
    }
}

impl TryFrom<v1::RepositoryStatus> for RepositoryStatus {
    type Error = &'static str;

    fn try_from(status: v1::RepositoryStatus) -> Result<Self, Self::Error> {
        let repository = status.repository.ok_or("repository status is missing")?;
        let revision = status
            .revision
            .map(|revision| -> Result<RepositoryRevision, &'static str> {
                Ok(RepositoryRevision {
                    source_state: revision.source_state,
                    head: revision.head,
                    analysis_identity: revision.analysis_identity,
                    analysis: AnalysisMetadata {
                        completeness: if revision.incomplete {
                            AnalysisCompleteness::Incomplete
                        } else {
                            AnalysisCompleteness::Complete
                        },
                        diagnostics: revision
                            .diagnostics
                            .into_iter()
                            .map(TryInto::try_into)
                            .collect::<Result<_, _>>()?,
                    },
                })
            })
            .transpose()?;
        Ok(Self {
            identity: repository.identity,
            display_name: repository.display_name,
            base: repository.base.into(),
            alternatives: repository
                .alternatives
                .into_iter()
                .map(Into::into)
                .collect(),
            revision,
            indexing: status.indexing,
        })
    }
}
