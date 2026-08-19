use crate::v1;
use beholder_domain::{
    LogicalRepository, ProtobufDescriptorSource, Workspace as DomainWorkspace,
    WorkspaceRepository as DomainRepository,
};
use std::path::PathBuf;

impl From<DomainWorkspace> for v1::Workspace {
    fn from(workspace: DomainWorkspace) -> Self {
        Self {
            name: workspace.name,
            repositories: workspace
                .repositories
                .into_iter()
                .map(|repository| v1::WorkspaceRepository {
                    identity: repository.repository.identity,
                    display_name: repository.display_name,
                    base: repository.base.to_string_lossy().into_owned(),
                    alternatives: repository
                        .alternatives
                        .into_iter()
                        .map(|path| path.to_string_lossy().into_owned())
                        .collect(),
                })
                .collect(),
            protobuf_descriptors: workspace
                .protobuf_descriptors
                .into_iter()
                .map(|descriptor| v1::ProtobufDescriptorSource {
                    repository: descriptor.repository.identity,
                    path: descriptor.path.to_string_lossy().into_owned(),
                })
                .collect(),
        }
    }
}

impl TryFrom<v1::Workspace> for DomainWorkspace {
    type Error = String;

    fn try_from(workspace: v1::Workspace) -> Result<Self, Self::Error> {
        Self::new(
            workspace.name,
            workspace
                .repositories
                .into_iter()
                .map(|repository| DomainRepository {
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
                })
                .collect(),
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
        )
    }
}
