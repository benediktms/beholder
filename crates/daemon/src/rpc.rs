use super::daemon::BeholderDaemon;
use super::rpc_service::operation_status_ref;
use beholder_domain::BeholderError;
use beholder_dto::{Revisioned, SemanticQueryResult};
use std::error::Error;
use tonic::{Response, Status};

impl BeholderDaemon {
    pub(super) fn query_response<T, P>(
        &self,
        workspace: &str,
        enriching: Vec<String>,
        result: Result<Revisioned<T>, Box<dyn Error>>,
    ) -> Result<Response<P>, Status>
    where
        T: SemanticQueryResult,
        P: From<T>,
    {
        let revisioned = result.map_err(|error| {
            error
                .downcast_ref::<BeholderError>()
                .map_or_else(|| Status::internal(error.to_string()), operation_status_ref)
        })?;
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
