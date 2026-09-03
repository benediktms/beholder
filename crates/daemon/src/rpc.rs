use super::daemon::BeholderDaemon;
use super::rpc_service::operation_status_ref;
use beholder_domain::BeholderError;
use beholder_dto::{Revisioned, SemanticQueryResult};
use std::error::Error;
use tonic::{Response, Status};

pub(super) async fn semantic_query<T>(
    query: impl FnOnce() -> Result<Revisioned<T>, Box<dyn Error>> + Send + 'static,
) -> Result<Revisioned<T>, Status>
where
    T: Send + 'static,
{
    let span = tracing::Span::current();
    tokio::task::spawn_blocking(move || {
        let _entered = span.enter();
        query().map_err(query_status)
    })
    .await
    .map_err(|error| Status::internal(format!("semantic query worker failed: {error}")))?
}

impl BeholderDaemon {
    pub(super) fn query_response<T, P>(
        &self,
        workspace: &str,
        enriching: Vec<String>,
        revisioned: Revisioned<T>,
    ) -> Result<Response<P>, Status>
    where
        T: SemanticQueryResult,
        P: From<T>,
    {
        let mut result = revisioned.result;
        *result.metadata_mut() = self.scheduler.query_metadata_with_enrichments(
            workspace,
            revisioned.analysis_revision,
            revisioned.analysis,
            Some(enriching),
        );
        Ok(Response::new(result.into()))
    }
}

fn query_status(error: Box<dyn Error>) -> Status {
    if error
        .downcast_ref::<std::io::Error>()
        .is_some_and(|error| error.kind() == std::io::ErrorKind::TimedOut)
    {
        return Status::deadline_exceeded(error.to_string());
    }
    error
        .downcast_ref::<BeholderError>()
        .map_or_else(|| Status::internal(error.to_string()), operation_status_ref)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    use std::time::Duration;
    use tokio::sync::oneshot;
    use tonic::Code;

    #[test]
    fn query_timeout_is_a_deadline_error() {
        let error = std::io::Error::new(std::io::ErrorKind::TimedOut, "query timed out");

        assert_eq!(query_status(Box::new(error)).code(), Code::DeadlineExceeded);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn cancelled_query_handler_releases_the_async_runtime() {
        let active = Arc::new(AtomicBool::new(false));
        let worker_active = active.clone();
        let (started, running) = oneshot::channel();
        let handler = tokio::spawn(semantic_query::<()>(move || {
            worker_active.store(true, Ordering::SeqCst);
            started.send(()).unwrap();
            std::thread::sleep(Duration::from_millis(50));
            worker_active.store(false, Ordering::SeqCst);
            Err(std::io::Error::other("finished").into())
        }));

        running.await.unwrap();
        assert!(active.load(Ordering::SeqCst));
        handler.abort();
        tokio::time::timeout(Duration::from_secs(1), async {
            while active.load(Ordering::SeqCst) {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .unwrap();
    }
}
