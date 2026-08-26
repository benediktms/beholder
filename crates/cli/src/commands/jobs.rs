use super::{JobCommand, JobsCommand};
use beholder_protocol::v1::{
    IndexJobOutcome, JobStatus, JobSummary, JobTrigger, JobType, JobWaitReason, index_destination,
    job_target,
};
use std::{error::Error, fmt::Debug};

pub(super) async fn list(command: JobsCommand) -> Result<(), Box<dyn Error>> {
    let JobsCommand::List { page_token } = command;
    let response = beholder_daemon_client::list_jobs(page_token).await?;
    for job in response.jobs {
        println!(
            "{}\t{}\t{}\t{}\t{}",
            job.id,
            enum_name::<JobStatus>(job.status),
            enum_name::<JobType>(job.r#type),
            target(&job),
            enum_name::<JobTrigger>(job.trigger),
        );
    }
    if let Some(token) = response.next_page_token {
        println!("next page: --page-token {token}");
    }
    Ok(())
}

pub(super) async fn get(command: JobCommand) -> Result<(), Box<dyn Error>> {
    let JobCommand::Get { id } = command;
    let job = beholder_daemon_client::get_job(id)
        .await?
        .job
        .ok_or("daemon returned no job")?;
    let summary = job
        .summary
        .as_ref()
        .ok_or("daemon returned no job summary")?;
    println!("id: {}", summary.id);
    println!("status: {}", enum_name::<JobStatus>(summary.status));
    println!("type: {}", enum_name::<JobType>(summary.r#type));
    println!("target: {}", target(summary));
    println!("trigger: {}", enum_name::<JobTrigger>(summary.trigger));
    println!("submitted_at_ms: {}", summary.submitted_at_ms);
    if let Some(timestamp) = job.run_at_ms {
        println!("eligible_at_ms: {timestamp}");
    }
    if let Some(timestamp) = job.started_at_ms {
        println!("started_at_ms: {timestamp}");
    }
    if let Some(timestamp) = job.completed_at_ms {
        println!("completed_at_ms: {timestamp}");
    }
    println!("attempts: {}/{}", job.attempts, job.max_attempts);
    if let Some(reason) = job.wait_reason {
        println!("waiting: {}", enum_name::<JobWaitReason>(reason));
    }
    if !job.prerequisite_job_ids.is_empty() {
        println!("prerequisites: {}", job.prerequisite_job_ids.join(", "));
    }
    if let Some(error) = job.last_error {
        println!("error: {error}");
    }
    if let Some(result) = job.index_result {
        for result in result.destinations {
            let destination = result
                .destination
                .and_then(|destination| destination.destination)
                .map_or_else(
                    || "unknown".into(),
                    |destination| match destination {
                        index_destination::Destination::Workspace(workspace) => {
                            format!("workspace:{workspace}")
                        }
                        index_destination::Destination::StandaloneRepository(repository) => {
                            format!("standalone-repository:{repository}")
                        }
                    },
                );
            println!(
                "result: {destination} {} ({} observations, published={})",
                enum_name::<IndexJobOutcome>(result.outcome),
                result.observation_count,
                result.published,
            );
        }
    }
    Ok(())
}

pub(super) fn target(job: &JobSummary) -> String {
    let Some(target) = &job.target else {
        return "unknown".into();
    };
    match &target.target {
        Some(job_target::Target::Workspace(workspace)) => format!("workspace:{workspace}"),
        Some(job_target::Target::Repository(repository)) => {
            let mut value = format!("repository:{repository}");
            if let Some(workspace) = &target.workspace_scope {
                value.push_str(&format!("@workspace:{workspace}"));
            }
            if let Some(worker) = &target.worker_id {
                value.push_str(&format!("/worker:{worker}"));
            }
            value
        }
        None => "unknown".into(),
    }
}

pub(super) fn enum_name<T>(value: i32) -> String
where
    T: TryFrom<i32> + Debug,
{
    T::try_from(value).map_or_else(|_| "Unspecified".into(), |value| format!("{value:?}"))
}
