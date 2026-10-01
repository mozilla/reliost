use std::sync::Arc;

use actix_web::{web, HttpResponse};
use serde_json::json;

use crate::proguard::api::{resolve_job, unresolved_job_result, Job, JobResult, Request, Response};
use crate::proguard::{MappingFileId, MappingFileStore};

#[tracing::instrument(
    name = "Deobfuscate Java v1",
    skip(request_json, store),
    fields(
        jobs = tracing::field::Empty,
        frames = tracing::field::Empty,
        mapping_files_found = tracing::field::Empty,
        mapping_files_failed = tracing::field::Empty,
    )
)]
pub async fn deobfuscate_java_v1(
    request_json: String,
    store: web::Data<Arc<MappingFileStore>>,
) -> HttpResponse {
    let request: Request = match serde_json::from_str(&request_json) {
        Ok(request) => request,
        Err(e) => {
            return HttpResponse::BadRequest().json(json!({ "error": e.to_string() }));
        }
    };

    let span = tracing::Span::current();
    span.record("jobs", request.jobs.len());
    span.record(
        "frames",
        request
            .jobs
            .iter()
            .flat_map(|job| &job.stacks)
            .map(|stack| stack.frames.len())
            .sum::<usize>(),
    );

    let mut results = Vec::with_capacity(request.jobs.len());
    for job in request.jobs {
        results.push(process_job(job, &store).await);
    }

    let found = results.iter().filter(|r| r.mapping_file.found).count();
    span.record("mapping_files_found", found);
    span.record("mapping_files_failed", results.len() - found);

    HttpResponse::Ok().json(Response { results })
}

async fn process_job(job: Job, store: &MappingFileStore) -> JobResult {
    let mapping_file = match MappingFileId::new(&job.mapping_file.uuid) {
        Ok(id) => store.get(&id).await,
        Err(e) => Err(e),
    };
    let mapping_file = match mapping_file {
        Ok(mapping_file) => mapping_file,
        Err(e) => return unresolved_job_result(job, e.to_string()),
    };

    // Lookups touch pages of the memory-mapped cache file, which can block on
    // disk I/O, so they run on the blocking thread pool.
    let result = tokio::task::spawn_blocking(move || match mapping_file.cache() {
        Ok(cache) => resolve_job(&job, &cache),
        Err(e) => unresolved_job_result(job, e.to_string()),
    })
    .await;
    result.expect("deobfuscation task panicked")
}
