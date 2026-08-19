use super::daemon::BeholderDaemon;
use beholder_dto::{Revisioned, SemanticQueryResult};
use std::error::Error;
use tonic::{Response, Status};

impl BeholderDaemon {
    pub(super) fn query_response<T, P>(
        &self,
        workspace: &str,
        result: Result<Revisioned<T>, Box<dyn Error>>,
    ) -> Result<Response<P>, Status>
    where
        T: SemanticQueryResult,
        P: From<T>,
    {
        let revisioned = result.map_err(|error| Status::internal(error.to_string()))?;
        let mut result = revisioned.result;
        *result.metadata_mut() = self.scheduler.query_metadata(
            workspace,
            revisioned.analysis_revision,
            revisioned.analysis,
        );
        Ok(Response::new(result.into()))
    }
}
