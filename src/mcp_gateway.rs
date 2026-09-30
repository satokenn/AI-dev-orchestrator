//! Stdio MCP transport for the supervisor-facing Operation Service tools.
//!
//! The transport owns only JSON-RPC framing and MCP protocol envelopes. Tool
//! execution is delegated to [`McpToolHandler`], keeping workflow decisions
//! and domain state out of this module.

use std::{
    io::{self, BufRead, Write},
    time::Duration,
};

use serde_json::{Value, json};

use crate::{
    ArtifactInput, AttemptRunRequest, BaseInput, CancellationToken, ModelChoice,
    OperationGetResult, OperationId, OperationService, ProviderRef, ProviderResolver, ServiceError,
    TaskCreateRequest, TaskId, TaskIssueSnapshot, TaskRole, TaskSource,
};

const PROTOCOL_VERSION: &str = "2026-07-28";

/// A handler for one validated MCP tool call.
pub trait McpToolHandler {
    /// Dispatches a tool call to the Operation Service adapter.
    fn call_tool(&self, name: &str, arguments: &Value) -> Result<Value, ToolError>;
}

/// Thin adapter from MCP tool arguments to the existing Operation Service.
/// The caller identity is injected at the stdio trust boundary; it is never
/// accepted from a tool's JSON arguments.
pub struct OperationServiceHandler<P: 'static> {
    service: &'static OperationService<'static, P>,
    caller: String,
}

impl<P> OperationServiceHandler<P> {
    /// Creates an adapter with a caller identity supplied by trusted host wiring.
    pub fn new(service: &'static OperationService<'static, P>, caller: impl Into<String>) -> Self {
        Self {
            service,
            caller: caller.into(),
        }
    }
}

impl<P: ProviderResolver + Sync + 'static> McpToolHandler for OperationServiceHandler<P> {
    fn call_tool(&self, name: &str, arguments: &Value) -> Result<Value, ToolError> {
        match name {
            "task.create" => self.create_task(arguments),
            "task.get_context" => self.get_context(arguments),
            "attempt.run" => self.accept_attempt(arguments),
            "operation.get" => self.get_operation(arguments),
            "operation.list_logs" => self.list_logs(arguments),
            "operation.cancel" => self.accept_cancellation(arguments, "operation.cancel"),
            "task.cancel" => self.accept_cancellation(arguments, "task.cancel"),
            "ci.get" => self.get_ci(arguments),
            "ci.wait" => self.accept_ci_wait(arguments),
            "validation.run" => self.accept_validation(arguments),
            "decision.record" => self.record_decision(arguments),
            "publication.publish" => self.publish(arguments),
            "task.finish" => self.finish_task(arguments),
            _ => Err(ToolError {
                code: "internal_error".into(),
                message: format!("Tool {name} is not available in this Service adapter."),
                retryable: false,
                current_task_revision: None,
                operation_id: None,
                details_ref: None,
            }),
        }
    }
}

impl<P: ProviderResolver + Sync + 'static> OperationServiceHandler<P> {
    fn create_task(&self, args: &Value) -> Result<Value, ToolError> {
        let source = match string(args, "source")? {
            "issue" => TaskSource::Issue,
            "manual" => TaskSource::Manual,
            _ => return Err(invalid("source must be issue or manual")),
        };
        let issue = args
            .get("issue")
            .map(|value| {
                Ok(TaskIssueSnapshot {
                    url: value["url"]
                        .as_str()
                        .ok_or_else(|| invalid("issue.url is required"))?
                        .to_owned(),
                    number: value["number"]
                        .as_u64()
                        .ok_or_else(|| invalid("issue.number must be an integer"))?,
                    title: value["title"]
                        .as_str()
                        .ok_or_else(|| invalid("issue.title is required"))?
                        .to_owned(),
                    body: value["body"]
                        .as_str()
                        .ok_or_else(|| invalid("issue.body is required"))?
                        .to_owned(),
                })
            })
            .transpose()?;
        let constraints = args["constraints"]
            .as_array()
            .ok_or_else(|| invalid("constraints must be an array"))?
            .iter()
            .map(|value| {
                value
                    .as_str()
                    .map(ToOwned::to_owned)
                    .ok_or_else(|| invalid("constraints must contain strings"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let request = TaskCreateRequest::new(
            string(args, "request_id")?,
            source,
            string(args, "title")?,
            string(args, "description")?,
            constraints,
            issue,
        );
        let result = self
            .service
            .create_task(&self.caller, &request)
            .map_err(map_service_error)?;
        Ok(json!({
            "schema_version":"v2", "request_id":result.request_id(),
            "task_id":result.task_id().as_str(), "revision":result.revision(),
            "state":"pending", "request": {
                "source": match result.source() { TaskSource::Issue => "issue", TaskSource::Manual => "manual" },
                "title":result.title(), "description":result.description(),
                "constraints":result.constraints(), "issue": result.issue().map(|issue| json!({
                    "url":issue.url,"number":issue.number,"title":issue.title,"body":issue.body
                }))
            }
        }))
    }

    fn accept_attempt(&self, args: &Value) -> Result<Value, ToolError> {
        let provider = ProviderRef::new(string(args, "provider_id")?);
        let model = match args["model_id"]["kind"].as_str() {
            Some("provider_default") => ModelChoice::ProviderDefault,
            Some("named") => {
                return Err(ToolError {
                    code: "unknown_model".into(),
                    message: "Named models require a configured trusted model catalog.".into(),
                    retryable: false,
                    current_task_revision: None,
                    operation_id: None,
                    details_ref: None,
                });
            }
            _ => return Err(invalid("model_id.kind must be named or provider_default")),
        };
        let task_id = TaskId::new(string(args, "task_id")?);
        let revision = integer(args, "expected_revision")?;
        let role = TaskRole::new(string(args, "role")?);
        let request_id = string(args, "request_id")?;
        let instruction = string(args, "instruction")?;
        let input = &args["input"];
        let request = match input["kind"].as_str() {
            Some("artifact") => AttemptRunRequest::with_artifact(
                request_id,
                task_id,
                revision,
                provider,
                model,
                instruction,
                role,
                ArtifactInput::new(string(input, "artifact_id")?),
            ),
            Some("base") => AttemptRunRequest::new(
                request_id,
                task_id,
                revision,
                provider,
                model,
                instruction,
                role,
                BaseInput::new(string(input, "repository")?, string(input, "commit")?),
            ),
            _ => return Err(invalid("input.kind must be artifact or base")),
        };
        let request = if let Some(timeout) = args.get("timeout_ms").and_then(Value::as_u64) {
            request.with_timeout(Duration::from_millis(timeout))
        } else {
            request
        };
        let accepted = self
            .service
            .submit_attempt(&request)
            .map_err(map_service_error)?;
        let accepted_at_ms = self
            .service
            .get_operation(accepted.operation_id())
            .map_err(map_service_error)?
            .accepted_at_ms();
        let service = self.service;
        let operation_id = accepted.operation_id().clone();
        std::thread::Builder::new()
            .name(format!("mcp-{}", operation_id.as_str()))
            .spawn(move || {
                let _ = service.run(&operation_id, CancellationToken::new());
            })
            .map_err(|_| ToolError {
                code: "internal_error".into(),
                message: "The accepted operation could not be scheduled.".into(),
                retryable: false,
                current_task_revision: None,
                operation_id: Some(accepted.operation_id().as_str().to_owned()),
                details_ref: None,
            })?;
        Ok(acceptance_json(
            accepted.operation_id().as_str(),
            "attempt.run",
            accepted.status(),
            accepted.revision(),
            Some(accepted.attempt_id().as_str()),
            string(args, "request_id")?,
            string(args, "task_id")?,
            accepted_at_ms,
        ))
    }

    fn get_operation(&self, args: &Value) -> Result<Value, ToolError> {
        let id = OperationId::new(string(args, "operation_id")?);
        let result = self
            .service
            .get_operation_result(&id)
            .map_err(map_service_error)?;
        match result {
            OperationGetResult::Attempt(operation) => {
                let state = status_name(operation.status());
                let attempt_state = match operation.attempt_state() {
                    crate::AttemptState::Succeeded => Some("succeeded"),
                    crate::AttemptState::Failed => Some("failed"),
                    crate::AttemptState::Cancelled => Some("cancelled"),
                    crate::AttemptState::Validating
                    | crate::AttemptState::Queued
                    | crate::AttemptState::Running => None,
                };
                let model = match operation.requested_model() {
                    ModelChoice::ProviderDefault => json!({"kind":"provider_default"}),
                    ModelChoice::Named(name) => json!({"kind":"named","model":name.as_str()}),
                };
                Ok(json!({ "schema_version":"v2", "operation": {
                    "operation_id":operation.operation_id().as_str(), "task_id":operation.task_id().as_str(),
                    "kind":"attempt.run", "state":state,
                    "submitted_at":rfc3339_from_millis(operation.accepted_at_ms()),
                    "started_at":operation.started_at_ms().map(rfc3339_from_millis),
                    "completed_at":operation.finished_at_ms().map(rfc3339_from_millis),
                    "result": if matches!(state,"completed"|"failed"|"cancelled") && attempt_state.is_some() { Some(json!({
                        "attempt_id":operation.attempt_id().as_str(), "attempt_state":attempt_state,
                        "requested_provider_id":operation.requested_provider().as_str(), "model_id":model,
                        "observed_provider_id":operation.observed_provider().map(ProviderRef::as_str),
                        "observed_model_id":operation.observed_model().map(crate::ModelRef::as_str),
                        "output_artifact_id":operation.output_artifact_id(),
                        "usage":operation.usage().iter().map(|m| json!({"name":m.name(),"value":m.value().parse::<f64>().ok(),"unit":m.unit(),"basis":"unknown"})).collect::<Vec<_>>(),
                        "diagnostic_ref":operation.diagnostic_code()
                    })) } else { None },
                    "error": if state == "failed" || state == "recovery_required" { Some(json!({
                        "code":canonical_error_code(operation.diagnostic_code().unwrap_or("internal_error")),
                        "message":"Provider operation failed.","retryable":false,
                        "current_task_revision":null,"operation_id":operation.operation_id().as_str(),"details_ref":null
                    })) } else { None }
                }}))
            }
            OperationGetResult::CiWait(operation) => {
                let status = status_name(operation.status());
                let observation = operation.observation();
                Ok(json!({"schema_version":"v2","operation":{
                    "operation_id":id.as_str(),"task_id":operation.task_id().as_str(),"kind":"ci.wait","state":status,
                    "submitted_at":rfc3339_from_millis(operation.accepted_at_ms()),
                    "started_at":operation.started_at_ms().map(rfc3339_from_millis),
                    "completed_at":operation.finished_at_ms().map(rfc3339_from_millis),
                    "result":observation.map(|value| json!({"observation_id":value.id(),"target":ci_target_json(value.target()),
                        "observed_at":rfc3339_from_millis(value.observed_at_ms()),"state":value.state().as_str(),"checks":ci_checks_json(value.checks())})),
                    "error":if status=="failed"||status=="recovery_required"||status=="cancelled" {Some(json!({
                        "code":operation.error_code().unwrap_or("internal_error"),"message":"CI wait did not complete successfully.",
                        "retryable":false,"current_task_revision":null,"operation_id":id.as_str(),"details_ref":operation.details_ref()
                    }))} else {None}
                }}))
            }
            OperationGetResult::ArtifactPublication(operation) => {
                let status = status_name(operation.state());
                let pull_request = operation.pull_request();
                if status == "completed" && pull_request.is_none() {
                    return Err(ToolError::internal());
                }
                let repository =
                    pull_request.and_then(|pr| repository_from_github_pr_url(pr.url()));
                if status == "completed" && repository.is_none() {
                    return Err(ToolError::internal());
                }
                Ok(json!({"schema_version":"v2","operation":{
                    "operation_id":id.as_str(),"task_id":operation.task_id().as_str(),"kind":"publication.publish","state":status,
                    "submitted_at":rfc3339_from_millis(operation.accepted_at_ms()),"started_at":null,
                    "completed_at":operation.finished_at_ms().map(rfc3339_from_millis),
                    "result":if status=="completed" {Some(json!({"publication_id":operation.publication_id().as_str(),
                        "repository":repository,"head_sha":operation.commit_sha().unwrap_or(""),
                        "pull_request_number":pull_request.map(|pr| pr.number()),"pull_request_url":pull_request.map(|pr| pr.url())}))} else {None},
                    "error":if status=="failed"||status=="recovery_required" {Some(json!({
                        "code":operation.error_code().unwrap_or("internal_error"),"message":"Artifact publication did not complete successfully.",
                        "retryable":false,"current_task_revision":null,"operation_id":id.as_str(),"details_ref":null
                    }))} else {None}
                }}))
            }
            OperationGetResult::Validation(operation) => {
                let state = status_name(operation.status());
                Ok(json!({"schema_version":"v2","operation":{
                    "operation_id":operation.operation_id().as_str(),"task_id":operation.task_id().as_str(),"kind":"validation.run","state":state,
                    "submitted_at":rfc3339_from_millis(operation.accepted_at_ms()),
                    "started_at":operation.started_at_ms().map(rfc3339_from_millis),
                    "completed_at":operation.finished_at_ms().map(rfc3339_from_millis),
                    "result":if state=="completed" { operation.result().cloned() } else { None },
                    "error":if state=="failed"||state=="recovery_required" {Some(json!({"code":canonical_error_code(operation.error_code().unwrap_or("internal_error")),"message":"Validation operation did not complete successfully.","retryable":false,"current_task_revision":null,"operation_id":operation.operation_id().as_str(),"details_ref":null}))} else {None}
                }}))
            }
            OperationGetResult::Cancellation(operation) => {
                let state = status_name(operation.status());
                let error = if state == "failed" || state == "recovery_required" {
                    Some(
                        json!({"code":canonical_error_code(operation.error_code().unwrap_or("internal_error")),"message":"Cancellation operation did not complete successfully.","retryable":false,"current_task_revision":null,"operation_id":operation.operation_id().as_str(),"details_ref":null}),
                    )
                } else {
                    None
                };
                Ok(
                    json!({"schema_version":"v2","operation":{"operation_id":operation.operation_id().as_str(),"task_id":operation.task_id().as_str(),"kind":operation.kind(),"state":state,"submitted_at":rfc3339_from_millis(operation.accepted_at_ms()),"started_at":operation.started_at_ms().map(rfc3339_from_millis),"completed_at":operation.finished_at_ms().map(rfc3339_from_millis),"result":if state=="completed"{operation.result().cloned()}else{None},"error":error}}),
                )
            }
        }
    }

    fn list_logs(&self, args: &Value) -> Result<Value, ToolError> {
        let operation_id = OperationId::new(string(args, "operation_id")?);
        self.service
            .list_operation_logs(&operation_id)
            .map_err(map_service_error)?;
        Err(ToolError {
            code: "forbidden".into(),
            message: "Safe redacted operation logs are not configured.".into(),
            retryable: false,
            current_task_revision: None,
            operation_id: Some(operation_id.as_str().to_owned()),
            details_ref: None,
        })
    }

    fn get_context(&self, args: &Value) -> Result<Value, ToolError> {
        let sections = args["sections"]
            .as_array()
            .ok_or_else(|| invalid("sections must be an array"))?
            .iter()
            .map(|v| {
                v.as_str()
                    .map(ToOwned::to_owned)
                    .ok_or_else(|| invalid("section must be a string"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let cursors = args
            .get("cursors")
            .and_then(Value::as_object)
            .map(|map| {
                map.iter()
                    .map(|(k, v)| {
                        Ok((
                            k.clone(),
                            v.as_str()
                                .ok_or_else(|| invalid("cursor must be a string"))?
                                .to_owned(),
                        ))
                    })
                    .collect::<Result<std::collections::BTreeMap<_, _>, ToolError>>()
            })
            .transpose()?
            .unwrap_or_default();
        self.service
            .get_task_context(
                &TaskId::new(string(args, "task_id")?),
                &sections,
                args.get("page_size").and_then(Value::as_u64).unwrap_or(20) as usize,
                &cursors,
            )
            .map_err(map_service_error)
    }

    fn accept_cancellation(&self, args: &Value, kind: &'static str) -> Result<Value, ToolError> {
        let task_id = TaskId::new(string(args, "task_id")?);
        let target = if kind == "operation.cancel" {
            Some(OperationId::new(string(args, "operation_id")?))
        } else {
            None
        };
        let accepted = self
            .service
            .accept_cancellation(
                &self.caller,
                string(args, "request_id")?,
                &task_id,
                integer(args, "expected_revision")?,
                kind,
                target.as_ref(),
                args.get("reason").and_then(Value::as_str),
            )
            .map_err(map_service_error)?;
        let service = self.service;
        let operation_id = accepted.operation_id().clone();
        std::thread::Builder::new()
            .name(format!("mcp-{}", operation_id.as_str()))
            .spawn(move || {
                let _ = service.run_cancellation_operation(&operation_id);
            })
            .map_err(|_| ToolError {
                code: "internal_error".into(),
                message: "The accepted cancellation could not be scheduled.".into(),
                retryable: false,
                current_task_revision: None,
                operation_id: Some(accepted.operation_id().as_str().to_owned()),
                details_ref: None,
            })?;
        Ok(acceptance_json(
            accepted.operation_id().as_str(),
            kind,
            crate::ServiceOperationStatus::Accepted,
            accepted.revision(),
            None,
            string(args, "request_id")?,
            task_id.as_str(),
            accepted.accepted_at_ms(),
        ))
    }

    fn get_ci(&self, args: &Value) -> Result<Value, ToolError> {
        let target = parse_ci_target(args)?;
        let observation = self.service.get_ci(&target).map_err(map_service_error)?;
        Ok(
            json!({"schema_version":"v2","observation_id":observation.id(),
            "target":ci_target_json(observation.target()),"observed_at":rfc3339_from_millis(observation.observed_at_ms()),
            "checks":ci_checks_json(observation.checks()),"state":observation.state().as_str()}),
        )
    }

    fn accept_ci_wait(&self, args: &Value) -> Result<Value, ToolError> {
        let target = parse_ci_target(args)?;
        let deadline = parse_rfc3339_utc(string(args, "deadline")?)?;
        let task_id = TaskId::new(string(args, "task_id")?);
        let request_id = string(args, "request_id")?;
        let accepted = self
            .service
            .accept_ci_wait(
                &self.caller,
                &crate::CiWaitRequest::new(
                    request_id,
                    task_id.clone(),
                    integer(args, "expected_revision")?,
                    target,
                    deadline,
                ),
            )
            .map_err(map_service_error)?;
        let snapshot = self
            .service
            .get_ci_wait_operation(accepted.operation_id())
            .map_err(map_service_error)?;
        let service = self.service;
        let operation_id = accepted.operation_id().clone();
        std::thread::Builder::new()
            .name(format!("mcp-{}", operation_id.as_str()))
            .spawn(move || {
                let _ = service.run_ci_wait_operation(&operation_id);
            })
            .map_err(|_| ToolError {
                code: "internal_error".into(),
                message: "The accepted operation could not be scheduled.".into(),
                retryable: false,
                current_task_revision: None,
                operation_id: Some(accepted.operation_id().as_str().to_owned()),
                details_ref: None,
            })?;
        Ok(acceptance_json(
            accepted.operation_id().as_str(),
            "ci.wait",
            accepted.status(),
            accepted.revision(),
            None,
            request_id,
            task_id.as_str(),
            snapshot.accepted_at_ms(),
        ))
    }

    fn accept_validation(&self, args: &Value) -> Result<Value, ToolError> {
        let checks = args
            .get("checks")
            .map(parse_validation_checks)
            .transpose()?
            .unwrap_or_default();
        let request = crate::ValidationRunRequest::new(
            string(args, "request_id")?,
            TaskId::new(string(args, "task_id")?),
            integer(args, "expected_revision")?,
            string(args, "artifact_id")?,
            args.get("check_profile_id")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned),
            checks,
        );
        let accepted = self
            .service
            .accept_validation(&self.caller, &request)
            .map_err(map_service_error)?;
        let service = self.service;
        let operation_id = accepted.operation_id().clone();
        std::thread::Builder::new()
            .name(format!("mcp-{}", operation_id.as_str()))
            .spawn(move || {
                let _ = service.run_validation_operation(&operation_id);
            })
            .map_err(|_| ToolError {
                code: "internal_error".into(),
                message: "The accepted validation could not be scheduled.".into(),
                retryable: false,
                current_task_revision: None,
                operation_id: Some(accepted.operation_id().as_str().to_owned()),
                details_ref: None,
            })?;
        let mut output = acceptance_json(
            accepted.operation_id().as_str(),
            "validation.run",
            accepted.status(),
            accepted.revision(),
            None,
            string(args, "request_id")?,
            string(args, "task_id")?,
            accepted.accepted_at_ms(),
        );
        output["artifact_id"] = json!(accepted.artifact_id());
        Ok(output)
    }

    fn record_decision(&self, args: &Value) -> Result<Value, ToolError> {
        let decision = match string(args, "decision")? {
            "accepted" => crate::CodexDecisionKind::Accepted,
            "rejected" => crate::CodexDecisionKind::Rejected,
            "changes_requested" => crate::CodexDecisionKind::ChangesRequested,
            _ => {
                return Err(invalid(
                    "decision must be accepted, rejected, or changes_requested",
                ));
            }
        };
        let evidence = parse_evidence(args.get("evidence"))?;
        let record = self
            .service
            .record_artifact_decision_idempotent(
                &self.caller,
                string(args, "request_id")?,
                &TaskId::new(string(args, "task_id")?),
                string(args, "artifact_id")?,
                integer(args, "expected_revision")?,
                decision,
                string(args, "reason")?,
                &evidence,
            )
            .map_err(map_service_error)?;
        Ok(
            json!({"schema_version":"v2","request_id":string(args,"request_id")?,
            "task_id":record.task_id().as_str(),"revision":record.revision(),"decision":{
                "decision_id":record.id(),"artifact_id":record.artifact_id(),
                "decision":match record.decision() {crate::CodexDecisionKind::Accepted=>"accepted",crate::CodexDecisionKind::Rejected=>"rejected",crate::CodexDecisionKind::ChangesRequested=>"changes_requested"},
                "reason":record.reason(),"evidence":record.evidence().iter().map(|(kind,id)|json!({"kind":kind,"id":id})).collect::<Vec<_>>()
            }}),
        )
    }

    fn finish_task(&self, args: &Value) -> Result<Value, ToolError> {
        let evidence = parse_evidence(args.get("evidence"))?;
        let result = self
            .service
            .finish_task_idempotent(
                &self.caller,
                string(args, "request_id")?,
                &TaskId::new(string(args, "task_id")?),
                integer(args, "expected_revision")?,
                string(args, "artifact_id")?,
                string(args, "decision_id")?,
                &evidence,
            )
            .map_err(map_service_error)?;
        Ok(json!({
        "schema_version":"v2", "request_id":result.request_id(),
        "task_id":result.task_id().as_str(), "revision":result.revision(),
        "state":"completed", "artifact_id":result.artifact_id(),
        "evidence":result.evidence().iter().map(|(kind,id)| json!({"kind":kind,"id":id})).collect::<Vec<_>>()
            }))
    }

    fn publish(&self, args: &Value) -> Result<Value, ToolError> {
        let evidence = parse_evidence(args.get("evidence"))?;
        let validation_id = evidence.iter().find(|(kind,_)|kind=="validation").map(|(_,id)|id.as_str())
            .ok_or_else(|| ToolError {code:"policy_denied".into(),message:"A saved passing Validation reference is required by the configured Service policy.".into(),retryable:false,current_task_revision:None,operation_id:None,details_ref:None})?;
        let request = crate::ArtifactPublicationRequest::new(
            string(args, "request_id")?,
            TaskId::new(string(args, "task_id")?),
            integer(args, "expected_revision")?,
            string(args, "artifact_id")?,
            validation_id,
            string(args, "decision_id")?,
            crate::ArtifactPublicationPayload::new(
                string(args, "base_branch")?,
                string(args, "head_branch")?,
                string(args, "title")?,
                string(args, "body")?,
            ),
        );
        let accepted = self
            .service
            .publish_artifact(&request)
            .map_err(map_service_error)?;
        let snapshot = self
            .service
            .get_artifact_publication_operation(accepted.operation_id())
            .map_err(map_service_error)?;
        let service = self.service;
        let operation_id = accepted.operation_id().clone();
        let run_request = request.clone();
        let run_acceptance = accepted.clone();
        std::thread::Builder::new()
            .name(format!("mcp-{}", operation_id.as_str()))
            .spawn(move || {
                let _ = service.run_artifact_publication(&run_acceptance, &run_request);
            })
            .map_err(|_| ToolError {
                code: "internal_error".into(),
                message: "The accepted operation could not be scheduled.".into(),
                retryable: false,
                current_task_revision: None,
                operation_id: Some(operation_id.as_str().to_owned()),
                details_ref: None,
            })?;
        let mut output = acceptance_json(
            accepted.operation_id().as_str(),
            "publication.publish",
            snapshot.state(),
            accepted.revision(),
            None,
            accepted.request_id(),
            accepted.task_id().as_str(),
            snapshot.accepted_at_ms(),
        );
        output["artifact_id"] = json!(string(args, "artifact_id")?);
        Ok(output)
    }
}

fn ci_target_json(target: &crate::CiTarget) -> Value {
    json!({"repository":target.repository(),"pull_request_number":target.pull_request_number(),"head_sha":target.head_sha()})
}
fn ci_checks_json(checks: &[crate::CiCheck]) -> Vec<Value> {
    checks.iter().map(|check| json!({"name":check.name(),"state":check.state().as_str(),"url":check.url(),"completed_at":check.completed_at()})).collect()
}
fn parse_ci_target(args: &Value) -> Result<crate::CiServiceTarget, ToolError> {
    let publication = args.get("publication_id");
    let target = args.get("target");
    match (publication, target) {
        (Some(_), Some(_)) => Err(invalid("publication_id and target are mutually exclusive")),
        (Some(value), None) => value
            .as_str()
            .map(|id| crate::CiServiceTarget::Publication(crate::PublicationId::new(id)))
            .ok_or_else(|| invalid("publication_id must be a string")),
        (None, Some(value)) if value.get("number").is_some() => {
            Ok(crate::CiServiceTarget::PullRequest {
                repository: value
                    .get("repository")
                    .and_then(Value::as_str)
                    .ok_or_else(|| invalid("target.repository is required"))?
                    .to_owned(),
                number: value
                    .get("number")
                    .and_then(Value::as_u64)
                    .filter(|number| *number > 0)
                    .ok_or_else(|| invalid("target.number must be positive"))?,
            })
        }
        (None, Some(value)) if value.get("sha").is_some() => Ok(crate::CiServiceTarget::Commit {
            repository: value
                .get("repository")
                .and_then(Value::as_str)
                .ok_or_else(|| invalid("target.repository is required"))?
                .to_owned(),
            sha: value
                .get("sha")
                .and_then(Value::as_str)
                .ok_or_else(|| invalid("target.sha is required"))?
                .to_owned(),
        }),
        (None, Some(_)) => Err(invalid("target must be a pull request or commit target")),
        (None, None) => Err(invalid("one of publication_id or target is required")),
    }
}

fn canonical_error_code(value: &str) -> &'static str {
    match value {
        "invalid_request" => "invalid_request",
        "unsupported_schema_version" => "unsupported_schema_version",
        "task_not_found" => "task_not_found",
        "stale_revision" => "stale_revision",
        "idempotency_conflict" => "idempotency_conflict",
        "busy" => "busy",
        "unknown_provider" => "unknown_provider",
        "unknown_model" => "unknown_model",
        "policy_denied" => "policy_denied",
        "budget_exhausted" => "budget_exhausted",
        "workspace_boundary_violation" => "workspace_boundary_violation",
        "artifact_not_found" => "artifact_not_found",
        "artifact_task_mismatch" => "artifact_task_mismatch",
        "evidence_artifact_mismatch" => "evidence_artifact_mismatch",
        "invalid_state_transition" => "invalid_state_transition",
        "operation_not_found" => "operation_not_found",
        "not_cancellable" => "not_cancellable",
        "timeout" => "timeout",
        "cancelled" => "cancelled",
        "interrupted" => "interrupted",
        "recovery_required" => "recovery_required",
        "invalid_cursor" => "invalid_cursor",
        "forbidden" => "forbidden",
        _ => "internal_error",
    }
}
fn parse_evidence(evidence: Option<&Value>) -> Result<Vec<(String, String)>, ToolError> {
    evidence.map_or_else(
        || Ok(Vec::new()),
        |items| {
            items
                .as_array()
                .ok_or_else(|| invalid("evidence must be an array"))?
                .iter()
                .map(|item| {
                    let kind = item
                        .get("kind")
                        .and_then(Value::as_str)
                        .ok_or_else(|| invalid("evidence.kind is required"))?;
                    let id = item
                        .get("id")
                        .and_then(Value::as_str)
                        .ok_or_else(|| invalid("evidence.id is required"))?;
                    Ok((kind.to_owned(), id.to_owned()))
                })
                .collect()
        },
    )
}

fn parse_validation_checks(value: &Value) -> Result<Vec<crate::ValidationCheckSpec>, ToolError> {
    value
        .as_array()
        .ok_or_else(|| invalid("checks must be an array"))?
        .iter()
        .map(|check| {
            let name = string(check, "name")?.to_owned();
            let command = string(check, "command")?.to_owned();
            let args = check["args"]
                .as_array()
                .ok_or_else(|| invalid("checks.args must be an array"))?
                .iter()
                .map(|arg| {
                    arg.as_str()
                        .map(ToOwned::to_owned)
                        .ok_or_else(|| invalid("checks.args must contain strings"))
                })
                .collect::<Result<Vec<_>, _>>()?;
            Ok(crate::ValidationCheckSpec::new(
                name,
                command,
                args,
                integer(check, "timeout_ms")?,
            ))
        })
        .collect()
}

fn parse_rfc3339_utc(value: &str) -> Result<std::time::SystemTime, ToolError> {
    let invalid_deadline = || invalid("deadline must be a valid RFC 3339 UTC timestamp");
    let value = value.strip_suffix('Z').ok_or_else(invalid_deadline)?;
    let (date, time) = value.split_once('T').ok_or_else(invalid_deadline)?;
    let mut date_parts = date.split('-');
    let year: i64 = date_parts
        .next()
        .ok_or_else(invalid_deadline)?
        .parse()
        .map_err(|_| invalid_deadline())?;
    let month: i64 = date_parts
        .next()
        .ok_or_else(invalid_deadline)?
        .parse()
        .map_err(|_| invalid_deadline())?;
    let day: i64 = date_parts
        .next()
        .ok_or_else(invalid_deadline)?
        .parse()
        .map_err(|_| invalid_deadline())?;
    if date_parts.next().is_some() || !(1..=12).contains(&month) {
        return Err(invalid_deadline());
    }
    let (clock, fraction) = time
        .split_once('.')
        .map_or((time, None), |(clock, fraction)| (clock, Some(fraction)));
    let mut clock_parts = clock.split(':');
    let hour: i64 = clock_parts
        .next()
        .ok_or_else(invalid_deadline)?
        .parse()
        .map_err(|_| invalid_deadline())?;
    let minute: i64 = clock_parts
        .next()
        .ok_or_else(invalid_deadline)?
        .parse()
        .map_err(|_| invalid_deadline())?;
    let second: i64 = clock_parts
        .next()
        .ok_or_else(invalid_deadline)?
        .parse()
        .map_err(|_| invalid_deadline())?;
    if clock_parts.next().is_some()
        || !(0..=23).contains(&hour)
        || !(0..=59).contains(&minute)
        || !(0..=59).contains(&second)
    {
        return Err(invalid_deadline());
    }
    let nanos = match fraction {
        None => 0,
        Some(fraction)
            if !fraction.is_empty()
                && fraction.len() <= 9
                && fraction.bytes().all(|b| b.is_ascii_digit()) =>
        {
            let padded = format!("{fraction:0<9}");
            padded.parse::<u32>().map_err(|_| invalid_deadline())?
        }
        Some(_) => return Err(invalid_deadline()),
    };
    let max_day = match month {
        2 if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    };
    if day < 1 || day > max_day {
        return Err(invalid_deadline());
    }
    let adjusted_year = year - i64::from(month <= 2);
    let era = adjusted_year.div_euclid(400);
    let yoe = adjusted_year - era * 400;
    let adjusted_month = month + if month > 2 { -3 } else { 9 };
    let doy = (153 * adjusted_month + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    let seconds = days
        .checked_mul(86_400)
        .and_then(|value| value.checked_add(hour * 3600 + minute * 60 + second))
        .ok_or_else(invalid_deadline)?;
    Ok(std::time::UNIX_EPOCH
        + Duration::new(
            u64::try_from(seconds).map_err(|_| invalid_deadline())?,
            nanos,
        ))
}
fn repository_from_github_pr_url(url: &str) -> Option<String> {
    let path = url.strip_prefix("https://github.com/")?;
    let mut segments = path.split('/');
    let owner = segments.next()?;
    let repository = segments.next()?;
    (segments.next()? == "pull" && !owner.is_empty() && !repository.is_empty())
        .then(|| format!("{owner}/{repository}"))
}

fn string<'a>(value: &'a Value, field: &str) -> Result<&'a str, ToolError> {
    value
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| invalid("required string field is missing"))
}
fn integer(value: &Value, field: &str) -> Result<u64, ToolError> {
    value
        .get(field)
        .and_then(Value::as_u64)
        .ok_or_else(|| invalid("required integer field is missing"))
}
fn invalid(message: &str) -> ToolError {
    ToolError {
        code: "invalid_request".into(),
        message: message.into(),
        retryable: false,
        current_task_revision: None,
        operation_id: None,
        details_ref: None,
    }
}
#[allow(clippy::too_many_arguments)] // Serializes the shared OperationAcceptance fields.
fn acceptance_json(
    operation_id: &str,
    kind: &str,
    status: crate::ServiceOperationStatus,
    revision: u64,
    attempt_id: Option<&str>,
    request_id: &str,
    task_id: &str,
    accepted_at_ms: i64,
) -> Value {
    let mut value = json!({"schema_version":"v2","request_id":request_id,"task_id":task_id,"revision":revision,
        "operation":{"operation_id":operation_id,"kind":kind,"state":status_name(status),"submitted_at":rfc3339_from_millis(accepted_at_ms)}});
    if let Some(attempt_id) = attempt_id {
        value["attempt_id"] = json!(attempt_id);
    }
    value
}
fn status_name(status: crate::ServiceOperationStatus) -> &'static str {
    match status {
        crate::ServiceOperationStatus::Accepted => "accepted",
        crate::ServiceOperationStatus::Running => "running",
        crate::ServiceOperationStatus::Cancelling => "cancelling",
        crate::ServiceOperationStatus::Completed => "completed",
        crate::ServiceOperationStatus::Failed => "failed",
        crate::ServiceOperationStatus::Cancelled => "cancelled",
        crate::ServiceOperationStatus::RecoveryRequired => "recovery_required",
    }
}
fn rfc3339_from_millis(ms: i64) -> String {
    // Convert epoch days to Gregorian calendar without adding a date/time dependency.
    let seconds = ms.div_euclid(1000);
    let days = seconds.div_euclid(86_400);
    let day_seconds = seconds.rem_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let mut year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = mp + if mp < 10 { 3 } else { -9 };
    year += i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{:03}Z",
        day_seconds / 3600,
        (day_seconds % 3600) / 60,
        day_seconds % 60,
        ms.rem_euclid(1000)
    )
}

fn map_service_error(error: ServiceError) -> ToolError {
    let (code, retryable, revision, operation_id) = match &error {
        ServiceError::InvalidRequest(_) => ("invalid_request", false, None, None),
        ServiceError::UnknownProvider => ("unknown_provider", false, None, None),
        ServiceError::NamedModelRequiresCatalog => ("unknown_model", false, None, None),
        ServiceError::TaskNotFound => ("task_not_found", false, None, None),
        ServiceError::StaleRevision { actual, .. } => ("stale_revision", true, Some(*actual), None),
        ServiceError::Busy(id) => ("busy", true, None, Some(id.as_str().to_owned())),
        ServiceError::PolicyDenied(_) => ("policy_denied", false, None, None),
        ServiceError::Forbidden(_) => ("forbidden", false, None, None),
        ServiceError::Artifact(crate::artifact::ArtifactError::NotFound) => {
            ("artifact_not_found", false, None, None)
        }
        ServiceError::Artifact(crate::artifact::ArtifactError::TaskNotFound) => {
            ("task_not_found", false, None, None)
        }
        ServiceError::Artifact(crate::artifact::ArtifactError::StaleRevision {
            actual, ..
        }) => ("stale_revision", true, Some(*actual), None),
        ServiceError::Artifact(crate::artifact::ArtifactError::IdempotencyConflict) => {
            ("idempotency_conflict", false, None, None)
        }
        ServiceError::Artifact(
            crate::artifact::ArtifactError::RecoveryRequired
            | crate::artifact::ArtifactError::WorkspaceRetained { .. },
        ) => ("recovery_required", false, None, None),
        ServiceError::Artifact(
            crate::artifact::ArtifactError::IgnoredFiles
            | crate::artifact::ArtifactError::WorkspaceChanged { .. }
            | crate::artifact::ArtifactError::Workspace(_),
        ) => ("workspace_boundary_violation", false, None, None),
        ServiceError::Artifact(crate::artifact::ArtifactError::Invalid(_)) => {
            ("invalid_request", false, None, None)
        }
        ServiceError::Artifact(_) => ("internal_error", false, None, None),
        ServiceError::IdempotencyConflict => ("idempotency_conflict", false, None, None),
        ServiceError::EvidenceArtifactMismatch => ("evidence_artifact_mismatch", false, None, None),
        ServiceError::OperationNotFound => ("operation_not_found", false, None, None),
        ServiceError::InvalidCursor => ("invalid_cursor", false, None, None),
        ServiceError::NotCancellable => ("not_cancellable", false, None, None),
        ServiceError::ValidationFailed | ServiceError::PublicationFailed => {
            ("internal_error", false, None, None)
        }
        ServiceError::PublicationRecoveryRequired => ("recovery_required", false, None, None),
        ServiceError::InvalidStateTransition(_) => ("invalid_state_transition", false, None, None),
        _ => ("internal_error", false, None, None),
    };
    ToolError {
        code: code.into(),
        message: format!("{error}"),
        retryable,
        current_task_revision: revision,
        operation_id,
        details_ref: None,
    }
}

/// A typed business failure returned by a tool handler.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ToolError {
    pub code: String,
    pub message: String,
    pub retryable: bool,
    pub current_task_revision: Option<u64>,
    pub operation_id: Option<String>,
    pub details_ref: Option<String>,
}

impl ToolError {
    /// Creates a fixed, safe internal error without including raw diagnostics.
    pub fn internal() -> Self {
        Self {
            code: "internal_error".to_owned(),
            message: "The operation could not be completed safely.".to_owned(),
            retryable: false,
            current_task_revision: None,
            operation_id: None,
            details_ref: None,
        }
    }

    fn payload(&self, request_id: Option<&str>) -> Value {
        json!({
            "schema_version": "v2",
            "request_id": request_id,
            "error": {
                "code": self.code,
                "message": self.message,
                "retryable": self.retryable,
                "current_task_revision": self.current_task_revision,
                "operation_id": self.operation_id,
                "details_ref": self.details_ref
            }
        })
    }
}

/// Serves newline-delimited JSON-RPC messages over MCP stdio.
///
/// Each input line is handled independently. Notifications receive no response;
/// malformed protocol messages receive JSON-RPC errors, while business failures
/// are MCP tool results with `isError: true` and a typed `structuredContent`.
pub fn serve_stdio<H: McpToolHandler>(handler: &H) -> io::Result<()> {
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut output = stdout.lock();
    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let response = match serde_json::from_str::<Value>(&line) {
            Ok(message) => handle_message(handler, &message),
            Err(_) => Some(json!({
                "jsonrpc": "2.0",
                "id": null,
                "error": { "code": -32700, "message": "Parse error" }
            })),
        };
        if let Some(response) = response {
            serde_json::to_writer(&mut output, &response)?;
            output.write_all(b"\n")?;
            output.flush()?;
        }
    }
    Ok(())
}

fn handle_message<H: McpToolHandler>(handler: &H, message: &Value) -> Option<Value> {
    let invalid_id = message
        .get("id")
        .filter(|id| !(id.is_string() || id.is_number() || id.is_null()))
        .cloned();
    if invalid_id.is_some() {
        return Some(rpc_error(Value::Null, -32600, "Invalid Request"));
    }
    let id = message.get("id").cloned();
    let Some(object) = message.as_object() else {
        return Some(rpc_error(
            id.unwrap_or(Value::Null),
            -32600,
            "Invalid Request",
        ));
    };
    let method = object.get("method").and_then(Value::as_str);
    let Some(method) = method else {
        return Some(rpc_error(
            id.unwrap_or(Value::Null),
            -32600,
            "Invalid Request",
        ));
    };
    if object.get("jsonrpc") != Some(&Value::String("2.0".into())) {
        return Some(rpc_error(
            id.unwrap_or(Value::Null),
            -32600,
            "Invalid Request",
        ));
    }
    if object
        .get("params")
        .is_some_and(|params| !params.is_object())
    {
        return Some(rpc_error(
            id.unwrap_or(Value::Null),
            -32600,
            "Invalid Request",
        ));
    }
    let params = object.get("params").cloned().unwrap_or_else(|| json!({}));
    if id.is_none() {
        if !method.starts_with("notifications/") {
            return Some(rpc_error(Value::Null, -32600, "Invalid Request"));
        }
        return None;
    }
    let id = id.expect("checked request id");
    {
        let meta = &params["_meta"];
        let version = meta["io.modelcontextprotocol/protocolVersion"].as_str();
        if version.is_none() || !meta["io.modelcontextprotocol/clientCapabilities"].is_object() {
            return Some(rpc_error(id, -32602, "Invalid params"));
        }
        if version != Some(PROTOCOL_VERSION) {
            let requested = version.unwrap_or("");
            return Some(
                json!({"jsonrpc":"2.0","id":id,"error":{"code":-32022,"message":"Unsupported protocol version","data":{"requested":requested,"supported":[PROTOCOL_VERSION]}}}),
            );
        }
    }
    let result = match method {
        "server/discover" => Ok(json!({
            "supportedVersions":[PROTOCOL_VERSION],
            "capabilities":{"tools":{"listChanged":false}},
            "ttlMs":0,"cacheScope":"private",
            "_meta":{"io.modelcontextprotocol/serverInfo":{"name":"ai-dev-orchestrator","version":env!("CARGO_PKG_VERSION")}}
        })),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({ "tools": tool_definitions(),"ttlMs":0,"cacheScope":"private" })),
        "tools/call" => call_tool(handler, &params),
        _ => return Some(rpc_error(id, -32601, "Method not found")),
    };
    Some(match result {
        Ok(mut result) => {
            if let Some(object) = result.as_object_mut() {
                object.insert("resultType".into(), json!("complete"));
                object.entry("_meta").or_insert_with(|| json!({}));
                if let Some(meta) = object.get_mut("_meta").and_then(Value::as_object_mut) {
                    meta.entry("io.modelcontextprotocol/serverInfo").or_insert_with(||json!({"name":"ai-dev-orchestrator","version":env!("CARGO_PKG_VERSION")}));
                }
            }
            json!({ "jsonrpc": "2.0", "id": id, "result": result })
        }
        Err(error) => rpc_error(id, error.0, &error.1),
    })
}

fn call_tool<H: McpToolHandler>(handler: &H, params: &Value) -> Result<Value, RpcError> {
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| RpcError(-32602, "Invalid params".to_owned()))?;
    let arguments = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));
    let schema = tool_definitions()
        .into_iter()
        .find(|tool| tool["name"] == name)
        .ok_or_else(|| RpcError(-32602, "Unknown tool".to_owned()))?;
    let request_id = arguments.get("request_id").and_then(Value::as_str);
    // Version negotiation and application-level argument validation are tool
    // errors in the wire contract, not JSON-RPC framing errors. In particular,
    // callers must be able to distinguish an unsupported contract version
    // from malformed JSON-RPC params.
    let version_error = match arguments.get("schema_version").and_then(Value::as_str) {
        Some("v2") => None,
        Some(_) => Some((
            "unsupported_schema_version",
            "Only schema_version v2 is supported.",
        )),
        None => Some((
            "invalid_request",
            "schema_version is required and must be a string.",
        )),
    };
    if let Some((code, message)) = version_error {
        return Ok(tool_error_result(
            ToolError {
                code: code.to_owned(),
                message: message.to_owned(),
                retryable: false,
                current_task_revision: None,
                operation_id: None,
                details_ref: None,
            },
            request_id,
        ));
    }
    if validate_arguments(&arguments, &schema["inputSchema"]).is_err() {
        return Ok(tool_error_result(
            ToolError {
                code: "invalid_request".to_owned(),
                message: "Tool arguments do not match the declared input schema.".to_owned(),
                retryable: false,
                current_task_revision: None,
                operation_id: None,
                details_ref: None,
            },
            request_id,
        ));
    }
    match handler.call_tool(name, &arguments) {
        Ok(output) => Ok(json!({
            "content": [{ "type": "text", "text": output.to_string() }],
            "structuredContent": output,
            "isError": false
        })),
        Err(error) => Ok(tool_error_result(error, request_id)),
    }
}

fn tool_error_result(error: ToolError, request_id: Option<&str>) -> Value {
    let payload = error.payload(request_id);
    json!({
        "content": [{ "type": "text", "text": payload.to_string() }],
        "structuredContent": payload,
        "isError": true
    })
}

#[derive(Debug)]
struct RpcError(i64, String);

fn rpc_error(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

fn validate_arguments(arguments: &Value, schema: &Value) -> Result<(), ()> {
    validate_value(arguments, schema)
}

fn validate_value(value: &Value, schema: &Value) -> Result<(), ()> {
    if let Some(expected) = schema["const"].as_str()
        && value != expected
    {
        return Err(());
    }
    if let Some(allowed) = schema["enum"].as_array()
        && !allowed.contains(value)
    {
        return Err(());
    }
    if let Some(alternatives) = schema["oneOf"].as_array() {
        let matches = alternatives
            .iter()
            .filter(|variant| validate_value(value, variant).is_ok())
            .count();
        if matches != 1 {
            return Err(());
        }
    }
    if let Some(alternatives) = schema["anyOf"].as_array()
        && !alternatives
            .iter()
            .any(|variant| validate_value(value, variant).is_ok())
    {
        return Err(());
    }
    if let Some(negated) = schema.get("not")
        && validate_value(value, negated).is_ok()
    {
        return Err(());
    }
    if let Some(conditions) = schema["allOf"].as_array() {
        for condition in conditions {
            validate_value(value, condition)?;
        }
    }
    match schema["type"].as_str() {
        Some("string") => {
            let string = value.as_str().ok_or(())?;
            if schema["minLength"]
                .as_u64()
                .is_some_and(|min| string.chars().count() < min as usize)
            {
                return Err(());
            }
            if schema["maxLength"]
                .as_u64()
                .is_some_and(|max| string.chars().count() > max as usize)
            {
                return Err(());
            }
            if schema["format"] == "uri"
                && !(string.starts_with("https://") || string.starts_with("http://"))
            {
                return Err(());
            }
        }
        Some("integer") => {
            let number = value.as_i64().ok_or(())?;
            if schema["minimum"].as_i64().is_some_and(|min| number < min)
                || schema["maximum"].as_i64().is_some_and(|max| number > max)
            {
                return Err(());
            }
        }
        Some("number") => {
            if !value.is_number() {
                return Err(());
            }
        }
        Some("boolean") => {
            if !value.is_boolean() {
                return Err(());
            }
        }
        Some("array") => {
            let items = value.as_array().ok_or(())?;
            if schema["minItems"]
                .as_u64()
                .is_some_and(|min| items.len() < min as usize)
                || schema["maxItems"]
                    .as_u64()
                    .is_some_and(|max| items.len() > max as usize)
            {
                return Err(());
            }
            if schema["uniqueItems"] == true
                && (0..items.len()).any(|i| items[i + 1..].contains(&items[i]))
            {
                return Err(());
            }
            if let Some(item_schema) = schema.get("items") {
                for item in items {
                    validate_value(item, item_schema)?;
                }
            }
        }
        Some("object") => {
            if !value.is_object() {
                return Err(());
            }
        }
        None => {}
        _ => return Err(()),
    }
    if let Some(required) = schema["required"].as_array() {
        let object = value.as_object().ok_or(())?;
        if required
            .iter()
            .any(|key| key.as_str().is_none_or(|key| !object.contains_key(key)))
        {
            return Err(());
        }
    }
    if let Some(properties) = schema["properties"].as_object() {
        let object = value.as_object().ok_or(())?;
        for (key, value) in object {
            match properties.get(key) {
                Some(property) => validate_value(value, property)?,
                None if schema["additionalProperties"] == false => return Err(()),
                None if schema["additionalProperties"].is_object() => {
                    validate_value(value, &schema["additionalProperties"])?
                }
                None => {}
            }
        }
    }
    if let Some(condition) = schema.get("if") {
        if validate_value(value, condition).is_ok() {
            if let Some(then_schema) = schema.get("then") {
                validate_value(value, then_schema)?;
            }
        } else if let Some(else_schema) = schema.get("else") {
            validate_value(value, else_schema)?;
        }
    }
    Ok(())
}

#[allow(clippy::type_complexity)] // Compact table drives the 13 exact public tool schemas.
fn tool_definitions() -> Vec<Value> {
    // The object schemas are the executable subset of the normative wire
    // reference. Nested schemas are expanded with the same strict object rule
    // by the adapter as each tool family is wired in.
    let specs: [(&str, &[(&str, &str, bool)]); 13] = [
        (
            "task.create",
            &[
                ("schema_version", "v2", true),
                ("request_id", "string", true),
                ("source", "string", true),
                ("title", "string", true),
                ("description", "string", true),
                ("constraints", "array", true),
                ("issue", "object", false),
            ],
        ),
        (
            "task.get_context",
            &[
                ("schema_version", "v2", true),
                ("task_id", "string", true),
                ("sections", "array", true),
                ("page_size", "integer", false),
                ("cursors", "object", false),
            ],
        ),
        (
            "attempt.run",
            &[
                ("schema_version", "v2", true),
                ("request_id", "string", true),
                ("task_id", "string", true),
                ("expected_revision", "integer", true),
                ("provider_id", "string", true),
                ("model_id", "object", true),
                ("instruction", "string", true),
                ("role", "string", true),
                ("input", "object", true),
                ("timeout_ms", "integer", false),
            ],
        ),
        (
            "operation.get",
            &[
                ("schema_version", "v2", true),
                ("operation_id", "string", true),
            ],
        ),
        (
            "operation.list_logs",
            &[
                ("schema_version", "v2", true),
                ("operation_id", "string", true),
                ("stream", "string", true),
                ("cursor", "string", false),
                ("limit", "integer", false),
            ],
        ),
        (
            "operation.cancel",
            &[
                ("schema_version", "v2", true),
                ("request_id", "string", true),
                ("task_id", "string", true),
                ("expected_revision", "integer", true),
                ("operation_id", "string", true),
            ],
        ),
        (
            "task.cancel",
            &[
                ("schema_version", "v2", true),
                ("request_id", "string", true),
                ("task_id", "string", true),
                ("expected_revision", "integer", true),
                ("reason", "string", false),
            ],
        ),
        (
            "validation.run",
            &[
                ("schema_version", "v2", true),
                ("request_id", "string", true),
                ("task_id", "string", true),
                ("expected_revision", "integer", true),
                ("artifact_id", "string", true),
                ("check_profile_id", "string", false),
                ("checks", "array", false),
            ],
        ),
        (
            "decision.record",
            &[
                ("schema_version", "v2", true),
                ("request_id", "string", true),
                ("task_id", "string", true),
                ("expected_revision", "integer", true),
                ("artifact_id", "string", true),
                ("decision", "string", true),
                ("reason", "string", true),
                ("evidence", "array", false),
            ],
        ),
        (
            "publication.publish",
            &[
                ("schema_version", "v2", true),
                ("request_id", "string", true),
                ("task_id", "string", true),
                ("expected_revision", "integer", true),
                ("artifact_id", "string", true),
                ("decision_id", "string", true),
                ("base_branch", "string", true),
                ("head_branch", "string", true),
                ("title", "string", true),
                ("body", "string", true),
                ("evidence", "array", false),
            ],
        ),
        (
            "ci.get",
            &[
                ("schema_version", "v2", true),
                ("publication_id", "string", false),
                ("target", "object", false),
            ],
        ),
        (
            "ci.wait",
            &[
                ("schema_version", "v2", true),
                ("request_id", "string", true),
                ("task_id", "string", true),
                ("expected_revision", "integer", true),
                ("publication_id", "string", false),
                ("target", "object", false),
                ("deadline", "string", true),
            ],
        ),
        (
            "task.finish",
            &[
                ("schema_version", "v2", true),
                ("request_id", "string", true),
                ("task_id", "string", true),
                ("expected_revision", "integer", true),
                ("artifact_id", "string", true),
                ("decision_id", "string", true),
                ("evidence", "array", false),
            ],
        ),
    ];
    specs.into_iter().map(|(name, fields)| {
        let mut properties = serde_json::Map::new();
        let mut required = Vec::new();
        for (field, ty, is_required) in fields {
            let property = if *field == "schema_version" {
                json!({ "type": "string", "const": "v2" })
            } else {
                json!({ "type": ty })
            };
            properties.insert((*field).to_owned(), property);
            if *is_required { required.push(Value::String((*field).to_owned())); }
        }
        let mut schema = json!({ "type": "object", "properties": properties, "required": required, "additionalProperties": false });
        match name {
            "task.create" => {
                schema["properties"]["source"] = enum_schema(&["issue","manual"]);
                schema["properties"]["constraints"] = json!({"type":"array","items":{"type":"string"}});
                schema["properties"]["issue"] = issue_snapshot_schema();
                schema["allOf"] = json!([{"if":{"properties":{"source":{"const":"issue"}}},"then":{"required":["issue"]}},
                    {"if":{"properties":{"source":{"const":"manual"}}},"then":{"not":{"required":["issue"]}}}]);
            }
            "task.get_context" => {
                schema["properties"]["sections"] = json!({"type":"array","minItems":1,"uniqueItems":true,"items":enum_schema(&["providers","usage","attempts","artifacts","validations","reviews","decisions","publication","ci"])});
                schema["properties"]["page_size"] = json!({"type":"integer","minimum":1,"maximum":100});
                schema["properties"]["cursors"] = json!({"type":"object","additionalProperties":{"type":"string"}});
            }
            "attempt.run" => {
                schema["properties"]["model_id"] = json!({"oneOf":[
                    strict_object(json!({"kind":{"type":"string","const":"named"},"model":{"type":"string","minLength":1}}), &["kind","model"]),
                    strict_object(json!({"kind":{"type":"string","const":"provider_default"}}), &["kind"])]});
                schema["properties"]["role"] = enum_schema(&["implementer","reviewer","explorer"]);
                schema["properties"]["input"] = json!({"oneOf":[
                    strict_object(json!({"kind":{"type":"string","const":"artifact"},"artifact_id":{"type":"string","minLength":1}}), &["kind","artifact_id"]),
                    strict_object(json!({"kind":{"type":"string","const":"base"},"repository":{"type":"string","minLength":1},"commit":{"type":"string","minLength":1}}), &["kind","repository","commit"])]});
                schema["properties"]["timeout_ms"] = json!({"type":"integer","minimum":1});
            }
            "operation.list_logs" => {
                schema["properties"]["stream"] = enum_schema(&["stdout","stderr","diagnostic"]);
                schema["properties"]["limit"] = json!({"type":"integer","minimum":1,"maximum":1000});
            }
            "validation.run" => {
                schema["properties"]["checks"] = json!({"type":"array","minItems":1,"items":strict_object(json!({"name":{"type":"string","minLength":1},"command":{"type":"string","minLength":1},"args":{"type":"array","items":{"type":"string"}},"timeout_ms":{"type":"integer","minimum":1}}), &["name","command","args","timeout_ms"])});
                schema["oneOf"] = json!([
                    {"required":["check_profile_id"],"not":{"required":["checks"]}},
                    {"required":["checks"],"not":{"required":["check_profile_id"]}}
                ]);
            }
            "decision.record" => {
                schema["properties"]["decision"] = enum_schema(&["accepted","rejected","changes_requested"]);
                schema["properties"]["evidence"] = evidence_array_schema();
            }
            "publication.publish" | "task.finish" => schema["properties"]["evidence"] = evidence_array_schema(),
            "ci.get" | "ci.wait" => {
                schema["properties"]["target"] = json!({"oneOf":[
                    strict_object(json!({"repository":{"type":"string","minLength":1},"number":{"type":"integer","minimum":1}}), &["repository","number"]),
                    strict_object(json!({"repository":{"type":"string","minLength":1},"sha":{"type":"string","minLength":1}}), &["repository","sha"])]});
                schema["oneOf"] = json!([
                    {"required":["publication_id"],"not":{"required":["target"]}},
                    {"required":["target"],"not":{"required":["publication_id"]}}
                ]);
            }
            "operation.cancel" | "task.cancel" => {}
            _ => {}
        }
        json!({ "name": name, "description": format!("AI Dev Orchestrator {name} operation"), "inputSchema": schema })
    }).collect()
}

fn enum_schema(values: &[&str]) -> Value {
    json!({"type":"string","enum":values})
}
fn strict_object(properties: Value, required: &[&str]) -> Value {
    json!({"type":"object","properties":properties,"required":required,"additionalProperties":false})
}
fn issue_snapshot_schema() -> Value {
    strict_object(
        json!({"url":{"type":"string","format":"uri"},"number":{"type":"integer","minimum":1},"title":{"type":"string","minLength":1},"body":{"type":"string"}}),
        &["url", "number", "title", "body"],
    )
}
fn evidence_array_schema() -> Value {
    json!({"type":"array","items":strict_object(json!({"kind":enum_schema(&["validation","review","decision","publication","ci"]),"id":{"type":"string","minLength":1}}), &["kind","id"])})
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn request_meta() -> Value {
        json!({"io.modelcontextprotocol/protocolVersion":PROTOCOL_VERSION,"io.modelcontextprotocol/clientCapabilities":{}})
    }

    struct FakeHandler;
    impl McpToolHandler for FakeHandler {
        fn call_tool(&self, name: &str, _arguments: &Value) -> Result<Value, ToolError> {
            assert_eq!(name, "operation.get");
            Ok(json!({ "schema_version": "v2", "operation": { "operation_id": "op-1" } }))
        }
    }

    #[test]
    fn advertises_contract_tools_and_protocol_capabilities() {
        let response = handle_message(&FakeHandler,&json!({ "jsonrpc":"2.0", "id":1, "method":"server/discover", "params":{"_meta":request_meta()} })).unwrap();
        assert_eq!(response["result"]["supportedVersions"][0], PROTOCOL_VERSION);
        let listed = handle_message(
            &FakeHandler,
            &json!({ "jsonrpc":"2.0", "id":2, "method":"tools/list", "params":{"_meta":request_meta()} }),
        )
        .unwrap();
        assert_eq!(listed["result"]["tools"].as_array().unwrap().len(), 13);
        assert_eq!(listed["result"]["resultType"], "complete");
        assert_eq!(listed["result"]["ttlMs"], 0);
    }

    #[test]
    fn routes_valid_calls_and_returns_typed_errors_for_bad_contract_arguments() {
        let response = handle_message(&FakeHandler, &json!({ "jsonrpc":"2.0", "id":3, "method":"tools/call", "params":{"name":"operation.get", "arguments":{"schema_version":"v2", "operation_id":"op-1"},"_meta":request_meta()} })).unwrap();
        assert_eq!(
            response["result"]["structuredContent"]["operation"]["operation_id"],
            "op-1"
        );
        let unsupported = handle_message(&FakeHandler, &json!({ "jsonrpc":"2.0", "id":4, "method":"tools/call", "params":{"name":"operation.get", "arguments":{"schema_version":"v1", "operation_id":"op-1"},"_meta":request_meta()} })).unwrap();
        assert_eq!(unsupported["result"]["isError"], true);
        assert_eq!(
            unsupported["result"]["structuredContent"]["error"]["code"],
            "unsupported_schema_version"
        );
        let malformed = handle_message(&FakeHandler, &json!({ "jsonrpc":"2.0", "id":5, "method":"tools/call", "params":{"name":"operation.get", "arguments":{"schema_version":"v2", "operation_id":"op-1", "unexpected":true},"_meta":request_meta()} })).unwrap();
        assert_eq!(malformed["result"]["isError"], true);
        assert_eq!(
            malformed["result"]["structuredContent"]["error"]["code"],
            "invalid_request"
        );
    }

    #[test]
    fn notifications_have_no_response_and_stdio_frames_one_json_value_per_line() {
        assert!(
            handle_message(
                &FakeHandler,
                &json!({ "jsonrpc":"2.0", "method":"notifications/cancelled", "params":{"requestId":1,"reason":"cancelled"} })
            )
            .is_none()
        );
        let mut output = Vec::new();
        serde_json::to_writer(&mut output, &json!({"id":7})).unwrap();
        output.push(b'\n');
        let mut cursor = Cursor::new(output);
        let mut line = String::new();
        cursor.read_line(&mut line).unwrap();
        assert_eq!(serde_json::from_str::<Value>(&line).unwrap()["id"], 7);
    }

    #[test]
    fn rejects_malformed_notifications_and_negotiates_protocol_version_per_request() {
        let malformed =
            handle_message(&FakeHandler, &json!({"method":"notifications/initialized"})).unwrap();
        assert_eq!(malformed["error"]["code"], -32600);
        let bad_params = handle_message(
            &FakeHandler,
            &json!({"jsonrpc":"2.0","id":8,"method":"tools/list","params":[]}),
        )
        .unwrap();
        assert_eq!(bad_params["error"]["code"], -32600);
        let bad_version = handle_message(
            &FakeHandler,
            &json!({"jsonrpc":"1.0","id":9,"method":"ping"}),
        )
        .unwrap();
        assert_eq!(bad_version["error"]["code"], -32600);
        let unsupported_version=handle_message(&FakeHandler,&json!({"jsonrpc":"2.0","id":10,"method":"ping","params":{"_meta":{"io.modelcontextprotocol/protocolVersion":"2025-03-26","io.modelcontextprotocol/clientCapabilities":{}}}})).unwrap();
        assert_eq!(unsupported_version["error"]["code"], -32022);
        let missing_caps=handle_message(&FakeHandler,&json!({"jsonrpc":"2.0","id":11,"method":"ping","params":{"_meta":{"io.modelcontextprotocol/protocolVersion":PROTOCOL_VERSION}}})).unwrap();
        assert_eq!(missing_caps["error"]["code"], -32602);
    }

    #[test]
    fn service_adapter_creates_an_idempotent_task_and_reads_operation_errors_safely() {
        let root = std::env::temp_dir().join(format!(
            "mcp-gateway-{}-{}",
            std::process::id(),
            rfc3339_from_millis(1)
        ));
        std::fs::create_dir_all(&root).unwrap();
        assert!(
            std::process::Command::new("git")
                .args(["init", "--quiet"])
                .current_dir(&root)
                .status()
                .unwrap()
                .success()
        );
        let ledger = Box::leak(Box::new(
            crate::SqliteExecutionLedger::open_in_memory().unwrap(),
        ));
        let workspaces = Box::leak(Box::new(crate::WorkspaceManager::new(&root).unwrap()));
        let providers = Box::leak(Box::new(crate::ProviderRegistry::new()));
        let service = Box::leak(Box::new(
            OperationService::new(ledger, workspaces, providers, 1, Duration::from_secs(30))
                .unwrap(),
        ));
        let handler = OperationServiceHandler::new(service, "trusted-host");
        let request = json!({"schema_version":"v2","request_id":"create-1","source":"manual","title":"Task","description":"Description","constraints":[]});
        let first = handler.call_tool("task.create", &request).unwrap();
        let second = handler.call_tool("task.create", &request).unwrap();
        assert_eq!(first, second);
        assert_eq!(first["revision"], 0);
        assert_eq!(first["state"], "pending");
        assert!(
            first["task_id"]
                .as_str()
                .unwrap()
                .starts_with("task-create-")
        );

        let context = handler
            .call_tool(
                "task.get_context",
                &json!({
                    "schema_version":"v2", "task_id":first["task_id"],
                    "sections":["attempts","artifacts"], "page_size":1
                }),
            )
            .unwrap();
        assert_eq!(context["task"]["request"]["title"], "Task");
        assert_eq!(context["task"]["revision"], 0);
        assert_eq!(
            context["sections"]["attempts"]["items"]
                .as_array()
                .unwrap()
                .len(),
            0
        );

        let cancel_request = json!({"schema_version":"v2","request_id":"cancel-1","task_id":first["task_id"],"expected_revision":0,"reason":"done"});
        let cancel = handler.call_tool("task.cancel", &cancel_request).unwrap();
        assert_eq!(cancel["operation"]["kind"], "task.cancel");
        assert_eq!(
            handler.call_tool("task.cancel", &cancel_request).unwrap(),
            cancel
        );
        let cancel_id = cancel["operation"]["operation_id"].as_str().unwrap();
        let mut cancel_result = handler
            .call_tool(
                "operation.get",
                &json!({"schema_version":"v2","operation_id":cancel_id}),
            )
            .unwrap();
        for _ in 0..50 {
            if cancel_result["operation"]["state"] == "completed" {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
            cancel_result = handler
                .call_tool(
                    "operation.get",
                    &json!({"schema_version":"v2","operation_id":cancel_id}),
                )
                .unwrap();
        }
        assert_eq!(cancel_result["operation"]["state"], "completed");
        assert_eq!(
            cancel_result["operation"]["result"]["task_state"],
            "cancelled"
        );
        let logs = handler
            .call_tool(
                "operation.list_logs",
                &json!({"schema_version":"v2","operation_id":cancel_id,"stream":"stdout"}),
            )
            .unwrap_err();
        assert_eq!(logs.code, "forbidden");

        let missing = handler
            .call_tool("operation.get", &json!({"operation_id":"absent"}))
            .unwrap_err();
        assert_eq!(missing.code, "operation_not_found");
        assert_eq!(missing.payload(None)["schema_version"], "v2");
        let _ = std::fs::remove_dir_all(root);
    }

    struct NeverRunValidationPolicy;

    impl crate::ValidationPolicy for NeverRunValidationPolicy {
        fn validate(
            &self,
            _repository_root: &std::path::Path,
            _workspace: &std::path::Path,
            _profile_id: Option<&str>,
            _checks: &[crate::ValidationCheckSpec],
        ) -> Result<crate::ValidationResult, crate::ValidatorError> {
            Err(crate::ValidatorError::NoChecksConfigured)
        }
    }

    #[test]
    fn missing_artifact_is_typed_and_log_refusal_is_forbidden() {
        let root = std::env::temp_dir().join(format!(
            "mcp-missing-artifact-{}-{}",
            std::process::id(),
            rfc3339_from_millis(2)
        ));
        std::fs::create_dir_all(&root).unwrap();
        assert!(
            std::process::Command::new("git")
                .args(["init", "--quiet"])
                .current_dir(&root)
                .status()
                .unwrap()
                .success()
        );
        let ledger = Box::leak(Box::new(
            crate::SqliteExecutionLedger::open_in_memory().unwrap(),
        ));
        let workspaces = Box::leak(Box::new(crate::WorkspaceManager::new(&root).unwrap()));
        let providers = Box::leak(Box::new(crate::ProviderRegistry::new()));
        let policy = Box::leak(Box::new(NeverRunValidationPolicy));
        let service = Box::leak(Box::new(
            OperationService::new(ledger, workspaces, providers, 1, Duration::from_secs(30))
                .unwrap()
                .with_validation_policy(policy),
        ));
        let handler = OperationServiceHandler::new(service, "trusted-host");
        let task = handler.call_tool("task.create", &json!({"schema_version":"v2","request_id":"create-missing","source":"manual","title":"Task","description":"Description","constraints":[]})).unwrap();
        let missing = handler.call_tool("validation.run", &json!({"schema_version":"v2","request_id":"validate-missing","task_id":task["task_id"],"expected_revision":0,"artifact_id":"missing-artifact","checks":[{"name":"never-run","command":"true","args":[],"timeout_ms":1000}]})).unwrap_err();
        assert_eq!(missing.code, "artifact_not_found");
        let missing_log = handler.call_tool("operation.list_logs", &json!({"schema_version":"v2","operation_id":"missing-operation","stream":"stdout"})).unwrap_err();
        assert_eq!(missing_log.code, "operation_not_found");
        let missing_artifact_source = handler
            .call_tool(
                "operation.list_logs",
                &json!({"schema_version":"v2","operation_id":task["task_id"],"stream":"stdout"}),
            )
            .unwrap_err();
        assert_eq!(missing_artifact_source.code, "operation_not_found");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn epoch_conversion_is_utc_and_keeps_milliseconds() {
        assert_eq!(rfc3339_from_millis(0), "1970-01-01T00:00:00.000Z");
        assert_eq!(rfc3339_from_millis(1), "1970-01-01T00:00:00.001Z");
    }
}
