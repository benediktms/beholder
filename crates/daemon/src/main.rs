use beholder_adapters_git::repository_state;
use beholder_adapters_mnestic::SemanticStore;
use beholder_adapters_treesitter_rust::{observations, resolve_repository_calls, source_files};
use beholder_daemon_client::{ADDRESS, state_dir};
use beholder_domain::{RepositoryState, WorkspaceView};
use beholder_protocol::v1::{
    EntityRequest, GetStatusRequest, GetStatusResponse, IndexRustWorkspaceRequest,
    IndexRustWorkspaceResponse, PathRequest, QueryResult, StopRequest, StopResponse,
    daemon_server::{Daemon, DaemonServer},
};
use std::{error::Error, fs, net::SocketAddr, path::Path, path::PathBuf, sync::Mutex};
use tokio::sync::oneshot;
use tonic::{Request, Response, Status, transport::Server};

mod single_instance;

struct BeholderDaemon {
    store: SemanticStore,
    shutdown: Mutex<Option<oneshot::Sender<()>>>,
}

fn daemon(store: SemanticStore) -> (BeholderDaemon, oneshot::Receiver<()>) {
    let (shutdown, stopped) = oneshot::channel();
    (
        BeholderDaemon {
            store,
            shutdown: Mutex::new(Some(shutdown)),
        },
        stopped,
    )
}

type RustSources = Vec<(PathBuf, String)>;

fn rust_repository_sources(root: &Path) -> Result<(RepositoryState, RustSources), Box<dyn Error>> {
    if !root.is_dir() {
        return Err(format!("repository does not exist: {}", root.display()).into());
    }
    let mut files = Vec::new();
    source_files(root, &mut files)?;
    files.sort();
    let sources = files
        .into_iter()
        .map(|path| {
            let relative_path = path.strip_prefix(root)?.to_path_buf();
            Ok((relative_path, fs::read_to_string(path)?))
        })
        .collect::<Result<Vec<_>, Box<dyn Error>>>()?;
    Ok((repository_state(root, &sources)?, sources))
}

fn index_rust_workspace(
    store: &SemanticStore,
    roots: &[PathBuf],
) -> Result<(usize, bool), Box<dyn Error>> {
    if roots.is_empty() {
        return Err("workspace must contain a repository".into());
    }
    let repositories = roots
        .iter()
        .map(|root| rust_repository_sources(root))
        .collect::<Result<Vec<_>, _>>()?;
    let view = WorkspaceView::new(
        "main",
        repositories
            .iter()
            .map(|(state, _)| state.clone())
            .collect(),
    )?;
    if store.view_matches(&view)? {
        return Ok((0, false));
    }

    let mut all_observations = Vec::new();
    for (state, sources) in repositories {
        for (path, source) in sources {
            all_observations.extend(observations(&state.repository.identity, &source, &path)?);
        }
    }
    resolve_repository_calls(&mut all_observations);
    store.publish(&view, &all_observations)?;
    Ok((all_observations.len(), true))
}

#[tonic::async_trait]
impl Daemon for BeholderDaemon {
    async fn context(
        &self,
        request: Request<EntityRequest>,
    ) -> Result<Response<QueryResult>, Status> {
        query_response(self.store.context(&request.into_inner().entity))
    }

    async fn dependencies(
        &self,
        request: Request<EntityRequest>,
    ) -> Result<Response<QueryResult>, Status> {
        query_response(self.store.dependencies(&request.into_inner().entity))
    }

    async fn get_status(
        &self,
        _request: Request<GetStatusRequest>,
    ) -> Result<Response<GetStatusResponse>, Status> {
        Ok(Response::new(GetStatusResponse {
            status: "ready".into(),
            protocol_version: 1,
            pid: std::process::id(),
        }))
    }

    async fn impact(
        &self,
        request: Request<EntityRequest>,
    ) -> Result<Response<QueryResult>, Status> {
        query_response(self.store.impact(&request.into_inner().entity))
    }

    async fn index_rust_workspace(
        &self,
        request: Request<IndexRustWorkspaceRequest>,
    ) -> Result<Response<IndexRustWorkspaceResponse>, Status> {
        // ponytail: this blocks one Tokio worker; add a bounded job queue when indexing competes with RPC latency.
        let roots = request
            .into_inner()
            .repositories
            .into_iter()
            .map(PathBuf::from)
            .collect::<Vec<_>>();
        let (observation_count, published) = index_rust_workspace(&self.store, &roots)
            .map_err(|error| Status::invalid_argument(error.to_string()))?;
        Ok(Response::new(IndexRustWorkspaceResponse {
            observation_count: observation_count
                .try_into()
                .map_err(|_| Status::internal("observation count exceeds protocol capacity"))?,
            published,
        }))
    }

    async fn stop(&self, _request: Request<StopRequest>) -> Result<Response<StopResponse>, Status> {
        let accepted = self
            .shutdown
            .lock()
            .map_err(|_| Status::internal("shutdown lock poisoned"))?
            .take()
            .is_some_and(|shutdown| shutdown.send(()).is_ok());
        Ok(Response::new(StopResponse { accepted }))
    }

    async fn trace(&self, request: Request<PathRequest>) -> Result<Response<QueryResult>, Status> {
        let request = request.into_inner();
        query_response(self.store.trace(&request.from, &request.to))
    }

    async fn why(&self, request: Request<PathRequest>) -> Result<Response<QueryResult>, Status> {
        let request = request.into_inner();
        query_response(self.store.trace(&request.from, &request.to))
    }
}

fn query_response(
    result: Result<beholder_dto::QueryResult, Box<dyn Error>>,
) -> Result<Response<QueryResult>, Status> {
    result
        .map(|result| Response::new(result.into()))
        .map_err(|error| Status::internal(error.to_string()))
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let state_dir = state_dir()?;
    std::fs::create_dir_all(&state_dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&state_dir, std::fs::Permissions::from_mode(0o700))?;
    }
    let _lock = single_instance::acquire(&state_dir)?;
    let (service, stopped) = daemon(SemanticStore::persistent(
        &state_dir.join("beholder.db"),
        true,
    )?);
    Server::builder()
        .add_service(DaemonServer::new(service))
        .serve_with_shutdown(ADDRESS.parse::<SocketAddr>()?, async {
            tokio::select! {
                _ = stopped => {}
                _ = tokio::signal::ctrl_c() => {}
            }
        })
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use beholder_protocol::v1::{
        EntityRequest, GetStatusRequest, IndexRustWorkspaceRequest, PathRequest, StopRequest,
        daemon_client::DaemonClient,
    };
    use std::{env, fs, net::TcpListener, time::Duration};

    #[tokio::test]
    async fn workspace_smoke() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        drop(listener);
        let database = env::temp_dir().join(format!("beholderd-{}.db", std::process::id()));
        let state = env::temp_dir().join(format!("beholderd-state-{}", std::process::id()));
        let _ = fs::remove_dir_all(&state);
        fs::create_dir_all(&state).unwrap();
        let lock = single_instance::acquire(&state).unwrap();
        assert_eq!(
            fs::read_to_string(state.join("beholderd.pid"))
                .unwrap()
                .trim(),
            std::process::id().to_string()
        );
        assert!(
            single_instance::acquire(&state)
                .unwrap_err()
                .to_string()
                .contains(&std::process::id().to_string())
        );
        let _ = fs::remove_file(&database);
        let (service, stopped) = daemon(SemanticStore::persistent(&database, true).unwrap());
        let server = tokio::spawn(async move {
            Server::builder()
                .add_service(DaemonServer::new(service))
                .serve_with_shutdown(address, async {
                    let _ = stopped.await;
                })
                .await
                .unwrap();
        });

        let endpoint = format!("http://{address}");
        let mut client = loop {
            match DaemonClient::connect(endpoint.clone()).await {
                Ok(client) => break client,
                Err(_) => tokio::time::sleep(Duration::from_millis(10)).await,
            }
        };
        let status = client
            .get_status(GetStatusRequest {})
            .await
            .unwrap()
            .into_inner();
        assert_eq!(status.status, "ready");
        assert_eq!(status.protocol_version, 1);
        assert_eq!(status.pid, std::process::id());

        let first = state.join("repo-a");
        let second = state.join("repo-b");
        fs::create_dir_all(first.join("src")).unwrap();
        fs::create_dir_all(second.join("src")).unwrap();
        fs::write(first.join("src/lib.rs"), "fn caller() { helper(); }").unwrap();
        fs::write(second.join("src/lib.rs"), "fn helper() {}").unwrap();
        let repository = |path: &Path| path.to_str().unwrap().to_owned();
        let indexed = client
            .index_rust_workspace(IndexRustWorkspaceRequest {
                repositories: vec![repository(&first), repository(&second)],
            })
            .await
            .unwrap()
            .into_inner();
        assert!(indexed.published && indexed.observation_count > 0);
        let unchanged = client
            .index_rust_workspace(IndexRustWorkspaceRequest {
                repositories: vec![repository(&second), repository(&first)],
            })
            .await
            .unwrap()
            .into_inner();
        assert!(!unchanged.published);
        assert_eq!(unchanged.observation_count, 0);

        let caller = "repo://repo-a/rust/lib/caller";
        let helper = "repo://repo-b/rust/lib/helper";
        assert!(
            format!(
                "{:?}",
                client
                    .context(EntityRequest {
                        entity: caller.into()
                    })
                    .await
                    .unwrap()
                    .into_inner()
            )
            .contains(helper)
        );
        assert!(
            !client
                .dependencies(EntityRequest {
                    entity: caller.into()
                })
                .await
                .unwrap()
                .into_inner()
                .rows
                .is_empty()
        );
        assert!(
            !client
                .impact(EntityRequest {
                    entity: caller.into()
                })
                .await
                .unwrap()
                .into_inner()
                .rows
                .is_empty()
        );
        let path = || PathRequest {
            from: caller.into(),
            to: helper.into(),
        };
        assert!(
            !client
                .trace(path())
                .await
                .unwrap()
                .into_inner()
                .rows
                .is_empty()
        );
        assert!(
            !client
                .why(path())
                .await
                .unwrap()
                .into_inner()
                .rows
                .is_empty()
        );

        assert!(
            client
                .stop(StopRequest {})
                .await
                .unwrap()
                .into_inner()
                .accepted
        );
        server.await.unwrap();
        let indexed = SemanticStore::persistent(&database, false).unwrap();
        assert_eq!(indexed.inspect_revisions().unwrap().rows.len(), 2);
        assert!(
            format!(
                "{:?}",
                indexed.context("repo://repo-a/rust/lib/caller").unwrap()
            )
            .contains("repo://repo-b/rust/lib/helper")
        );
        drop(indexed);
        drop(lock);
        assert!(
            fs::read_to_string(state.join("beholderd.pid"))
                .unwrap()
                .is_empty()
        );
        fs::remove_file(database).unwrap();
        fs::remove_dir_all(state).unwrap();
    }
}
