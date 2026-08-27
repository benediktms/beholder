use super::job::{enum_name, target};
use crate::stdout;
use beholder_daemon_client::{get_repository, submit_enrichment};
use beholder_protocol::v1::{EnrichmentSubmissionDisposition, JobStatus};
use std::error::Error;

pub(super) async fn submit(
    repository: String,
    workspace: Option<String>,
    only: Vec<String>,
) -> Result<(), Box<dyn Error>> {
    get_repository(repository.clone()).await?;
    let response = submit_enrichment(repository, workspace, only).await?;
    for prerequisite in response.prerequisite_jobs {
        stdout(format_args!(
            "prerequisite {}\t{}\t{}",
            prerequisite.id,
            enum_name::<JobStatus>(prerequisite.status),
            target(&prerequisite),
        ))?;
    }
    for result in response.results {
        let disposition = disposition_name(result.disposition);
        if let Some(job) = result.job {
            stdout(format_args!("{} {}\t{}", disposition, job.id, target(&job),))?;
        } else if let Some(target) = result.target {
            let summary = beholder_protocol::v1::JobSummary {
                target: Some(target),
                ..Default::default()
            };
            stdout(format_args!(
                "{}\t{}",
                disposition,
                super::job::target(&summary),
            ))?;
        }
    }
    Ok(())
}

fn disposition_name(value: i32) -> &'static str {
    match EnrichmentSubmissionDisposition::try_from(value) {
        Ok(EnrichmentSubmissionDisposition::Enqueued) => "enqueued",
        Ok(EnrichmentSubmissionDisposition::AlreadyCurrent) => "already-current",
        Ok(EnrichmentSubmissionDisposition::InProgress) => "in-progress",
        _ => "unspecified",
    }
}

#[cfg(test)]
mod tests {
    use super::disposition_name;
    use beholder_protocol::v1::EnrichmentSubmissionDisposition;

    #[test]
    fn formats_submission_dispositions_for_cli_output() {
        assert_eq!(
            disposition_name(EnrichmentSubmissionDisposition::AlreadyCurrent.into()),
            "already-current"
        );
        assert_eq!(
            disposition_name(EnrichmentSubmissionDisposition::InProgress.into()),
            "in-progress"
        );
    }
}
