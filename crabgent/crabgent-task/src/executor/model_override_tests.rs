use super::spawn::{build_run_request, build_task_record};

use std::sync::{Arc, OnceLock};

use crabgent_core::model::ModelId;
use crabgent_store::Owner;
use tokio_util::sync::CancellationToken;

use crate::TaskRequest;

#[test]
fn build_run_request_forwards_default_model_without_explicit_layer() {
    let req = TaskRequest::new_default(Owner::new("u"), "default", "do it");
    let task = build_task_record(&req);
    let run_req = build_run_request(
        &req,
        &task,
        CancellationToken::new(),
        Arc::new(OnceLock::new()),
    );

    assert_eq!(run_req.model.as_str(), "default");
    assert!(run_req.explicit_model.is_none());
}

#[test]
fn build_run_request_forwards_session_model_override() {
    let req = TaskRequest::new_default(Owner::new("u"), "default", "do it")
        .with_session_model_override(ModelId::new("session-model"));
    let task = build_task_record(&req);
    let run_req = build_run_request(
        &req,
        &task,
        CancellationToken::new(),
        Arc::new(OnceLock::new()),
    );

    assert_eq!(
        run_req.session_model_override.as_ref().map(ModelId::as_str),
        Some("session-model")
    );
}
