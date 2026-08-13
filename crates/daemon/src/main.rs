use beholder_adapters_mnestic::SemanticStore;
use beholder_daemon_client::{ADDRESS, state_dir};
use beholder_protocol::v1::{
    GetStatusRequest, GetStatusResponse,
    daemon_server::{Daemon, DaemonServer},
};
use std::{error::Error, net::SocketAddr};
use tonic::{Request, Response, Status, transport::Server};

mod single_instance;

struct BeholderDaemon {
    _store: SemanticStore,
}

#[tonic::async_trait]
impl Daemon for BeholderDaemon {
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
    let service = BeholderDaemon {
        _store: SemanticStore::persistent(&state_dir.join("beholder.db"), true)?,
    };
    Server::builder()
        .add_service(DaemonServer::new(service))
        .serve(ADDRESS.parse::<SocketAddr>()?)
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use beholder_protocol::v1::{GetStatusRequest, daemon_client::DaemonClient};
    use std::{env, fs, net::TcpListener, time::Duration};
    use tokio::sync::oneshot;

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
        let service = BeholderDaemon {
            _store: SemanticStore::persistent(&database, true).unwrap(),
        };
        let (shutdown, stopped) = oneshot::channel();
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

        shutdown.send(()).unwrap();
        server.await.unwrap();
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
