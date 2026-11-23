use super::{HttpState, compute};
use crate::{AppError, Result as AppResult, Service, terminal};
use axum::http::Uri;
use prost::Message;
use rmcp::{
    ServerHandler,
    handler::server::wrapper::Parameters,
    model::{CallToolResult, Implementation, ServerCapabilities, ServerConfig},
    schemars, tool, tool_handler, tool_router,
    transport::streamable_http_server::{
        StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
    },
};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::time::Duration;
use vrp_proto::{app::v1 as pb, engine::v1 as engine};

pub(super) const BODY_LIMIT: usize = 64 * 1024;
const INLINE_RESULT_WAIT: Duration = Duration::from_secs(10);

pub(super) struct Endpoint {
    origin: String,
    authority: String,
    options: engine::SolveOptions,
    default_scenario: Option<pb::ScenarioRef>,
}

impl Endpoint {
    pub(super) fn parse(
        value: &str,
        options: engine::SolveOptions,
        default_scenario: Option<pb::ScenarioRef>,
    ) -> AppResult<Self> {
        let uri = value
            .parse::<Uri>()
            .map_err(|_| AppError::invalid("invalid MCP origin"))?;
        let scheme = uri
            .scheme_str()
            .filter(|s| matches!(*s, "http" | "https"))
            .ok_or_else(|| AppError::invalid("MCP origin must use HTTP or HTTPS"))?;
        let authority = uri
            .authority()
            .filter(|a| !a.host().is_empty() && !a.as_str().contains('@'))
            .ok_or_else(|| {
                AppError::invalid("MCP origin requires an authority without user info")
            })?;
        if uri.path() != "/" || uri.query().is_some() {
            return Err(AppError::invalid(
                "MCP origin must not contain a path or query",
            ));
        }
        Ok(Self {
            origin: format!("{scheme}://{authority}"),
            authority: authority.to_string(),
            options,
            default_scenario,
        })
    }

    pub(super) fn service(
        self,
        state: HttpState,
    ) -> StreamableHttpService<Planner, LocalSessionManager> {
        let config = StreamableHttpServerConfig::default()
            .with_legacy_session_mode(false)
            .with_json_response(true)
            .with_max_request_body_bytes(BODY_LIMIT)
            .with_allowed_hosts([self.authority])
            .with_allowed_origins([self.origin.clone()]);
        let planner = Planner {
            state,
            origin: self.origin,
            options: self.options,
            default_scenario: self.default_scenario,
        };
        StreamableHttpService::new(
            move || Ok(planner.clone()),
            LocalSessionManager::default().into(),
            config,
        )
    }
}

#[derive(Clone)]
pub(super) struct Planner {
    state: HttpState,
    origin: String,
    options: engine::SolveOptions,
    default_scenario: Option<pb::ScenarioRef>,
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ListArgs {
    /// Cursor from the previous response, or omit to start.
    after_id: Option<String>,
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SolveArgs {
    /// Omit with expectedVersion to use the server's configured planning scenario.
    scenario_id: Option<String>,
    /// Known immutable version. Omit with scenarioId for the configured planning scenario.
    expected_version: Option<u64>,
    /// Unique per intended run. Use exactly the same arguments when retrying it.
    request_id: String,
    /// Wait briefly for the summary and link. Defaults to true for a configured scene
    /// selected without arguments and to false when an explicit scene is passed.
    wait_for_result: Option<bool>,
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ReplanArgs {
    scenario_id: String,
    /// Known current execution revision. Read the operation only when the revision is unknown.
    expected_revision: u64,
    /// Unique per intended change. Use exactly the same arguments on retry.
    request_id: String,
    /// Stable ID of the vehicle/execution unit to mark unavailable.
    disabled_agent_id: Option<String>,
    urgent_ticket: Option<UrgentTicket>,
    /// Wait briefly for result metrics and urgent-ticket timing. Defaults to false.
    #[serde(default)]
    wait_for_result: bool,
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct UrgentTicket {
    ticket_id: String,
    /// Existing location ID in the scenario road matrix.
    location_id: String,
    duration_seconds: i64,
    /// Deadline to start service, in seconds from scenario horizon start.
    latest_start_seconds: i64,
    /// Exact skill IDs copied verbatim from the scenario, including Chinese literals.
    #[serde(default)]
    skills_required: Vec<String>,
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct GetArgs {
    /// Task ID returned by solve_plan or replan.
    task_id: String,
    /// Optional order ID to retrieve assignment and arrival details.
    ticket_id: Option<String>,
    /// Include ordered unit and ticket IDs for machine-readable evaluation.
    #[serde(default)]
    include_assignments: bool,
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AdoptArgs {
    scenario_id: String,
    /// Exact candidate job ID named in the human's approval.
    job_id: String,
    /// Current execution revision observed with the candidate.
    expected_revision: u64,
    /// Current published plan version observed with the candidate.
    expected_plan_version: u64,
    /// True only after the human explicitly approves this specific candidate.
    confirmed: bool,
    /// Human approver's name or business role, for the execution history.
    actor: String,
    reason: Option<String>,
    /// Optional ticket ID to read from the newly published plan.
    ticket_id: Option<String>,
}

#[tool_router]
impl Planner {
    #[tool(
        description = "List saved scenario IDs and versions, resource/order counts, and current execution revision and clock if running. Use these IDs for planning and replanning."
    )]
    async fn list_scenarios(&self, Parameters(args): Parameters<ListArgs>) -> CallToolResult {
        self.run(move |service| {
            let page = service.list_scenarios(pb::ListScenariosRequest {
                page_size: 20,
                after_id: args.after_id.unwrap_or_default(),
            })?;
            let items = page.scenarios.into_iter().map(|scene| {
                let op = operation(&service, &scene.id)?;
                let summary = scene.summary.unwrap_or_default();
                Ok(json!({
                    "scenarioId": scene.id, "name": scene.name, "version": scene.version,
                    "tickets": summary.ticket_count, "agents": summary.agent_count, "matrixReady": scene.matrix_ready,
                    "operation": op
                }))
            }).collect::<AppResult<Vec<_>>>()?;
            Ok(json!({"scenarios": items, "nextAfterId": page.next_after_id}))
        }).await
    }

    #[tool(
        description = "安排工单，生成车辆路线图和甘特图 (plan work orders and return their route and Gantt view). For the configured scene, call with only a fresh requestId. Otherwise supply scenarioId and expectedVersion. Use the returned viewUrl directly; pending views update in the workbench."
    )]
    async fn solve_plan(&self, Parameters(args): Parameters<SolveArgs>) -> CallToolResult {
        let origin = self.origin.clone();
        let options = self.options;
        let uses_default = args.scenario_id.is_none() && args.expected_version.is_none();
        let wait_for_result = args.wait_for_result.unwrap_or(uses_default);
        let default_scenario = self.default_scenario.clone();
        self.deliver(wait_for_result, None, move |service| {
            if args.request_id.is_empty()
                || args.request_id.len() > 128
                || !args.request_id.bytes().all(|byte| byte.is_ascii_graphic())
            {
                return Err(AppError::invalid(
                    "requestId must be 1..128 visible ASCII characters",
                ));
            }
            let scenario = match (args.scenario_id, args.expected_version) {
                (Some(scenario_id), Some(version)) if version > 0 => pb::ScenarioRef {
                    scenario_id,
                    version,
                },
                (None, None) => default_scenario.ok_or_else(|| {
                    AppError::invalid("no default scenario; supply scenarioId and expectedVersion")
                })?,
                _ => {
                    return Err(AppError::invalid(
                        "supply scenarioId and a positive expectedVersion together",
                    ));
                }
            };
            let metadata =
                service.get_scenario_metadata(&scenario.scenario_id, scenario.version)?;
            let key = format!(
                "mcp-solve-{:x}",
                Sha256::digest(format!("{}:{}", scenario.scenario_id, args.request_id))
            );
            if !metadata.matrix_ready {
                let task = service.plan_with_roads(pb::GeoPlanRequest {
                    scenario_id: scenario.scenario_id,
                    expected_version: scenario.version,
                    idempotency_key: key,
                    options: Some(options),
                    ..Default::default()
                })?;
                return task_status(&service, &origin, &task.id, None, false);
            }
            let job = service.submit(pb::SubmitJobRequest {
                idempotency_key: key,
                source: Some(pb::submit_job_request::Source::Scenario(scenario)),
                options: Some(options),
                ..Default::default()
            })?;
            submission(&service, &origin, &job)
        })
        .await
    }

    #[tool(
        description = "Apply a vehicle outage or urgent service ticket to a known execution revision and queue a candidate retaining completed and committed work. Set waitForResult=true for a short-budget run to receive the candidate link and urgent ticket timing in this call. Use get_plan only when pending or for more details. Adoption requires human confirmation."
    )]
    async fn replan(&self, Parameters(args): Parameters<ReplanArgs>) -> CallToolResult {
        let origin = self.origin.clone();
        let options = self.options;
        let ticket = args
            .urgent_ticket
            .as_ref()
            .map(|ticket| ticket.ticket_id.clone());
        self.deliver(args.wait_for_result, ticket, move |service| {
            let job = service.replan_change(pb::ReplanChangeRequest {
                scenario_id: args.scenario_id,
                expected_revision: args.expected_revision,
                request_id: args.request_id,
                disabled_agent_id: args.disabled_agent_id.unwrap_or_default(),
                urgent_ticket: args.urgent_ticket.map(|ticket| pb::UrgentServiceTicket {
                    ticket_id: ticket.ticket_id,
                    location_id: ticket.location_id,
                    duration_seconds: ticket.duration_seconds,
                    latest_start_seconds: ticket.latest_start_seconds,
                    skills_required: ticket.skills_required,
                }),
                options: Some(options),
            })?;
            submission(&service, &origin, &job)
        })
        .await
    }

    #[tool(
        description = "Read a pending planning task or request extra details. Return summary metrics, a clickable Workbench Gantt/map link, and optionally one ticket's assigned unit and timing. Assignments are optional for machine evaluation and unnecessary to open the Workbench."
    )]
    async fn get_plan(&self, Parameters(args): Parameters<GetArgs>) -> CallToolResult {
        let origin = self.origin.clone();
        self.run(move |service| {
            task_status(
                &service,
                &origin,
                &args.task_id,
                args.ticket_id.as_deref(),
                args.include_assignments,
            )
        })
        .await
    }

    #[tool(
        description = "确认采用候选 (publish one execution replan). Call only after the human explicitly approves this exact jobId. Use the revision and plan version they reviewed. On conflict show the changed state and ask them to review again. Returns the published version, Workbench link and optional ticket timing."
    )]
    async fn adopt_plan(&self, Parameters(args): Parameters<AdoptArgs>) -> CallToolResult {
        let origin = self.origin.clone();
        self.run(move |service| {
            if !args.confirmed || args.actor.trim().is_empty() {
                return Err(AppError::invalid("explicit approval and an approver are required"));
            }
            let request = pb::AdoptPlanRequest {
                scenario_id: args.scenario_id,
                job_id: args.job_id,
                expected_revision: args.expected_revision,
                expected_plan_version: args.expected_plan_version,
                actor: args.actor,
                reason: args.reason.unwrap_or_else(|| "用户确认采用".to_owned()),
                accept_business_violations: false,
            };
            let (op, already_active) = publish(&service, &request)?;
            let plan = op.plan.as_ref().ok_or_else(|| AppError::new("internal", "published plan is missing"))?;
            let ticket = args.ticket_id.as_deref().and_then(|id| {
                plan.routes.iter().find_map(|route| {
                    route.stops.iter().find(|stop| stop.ticket_id == id).map(|stop| json!({
                        "ticketId": id, "agentId": route.agent_id, "arrivalSeconds": stop.arrival,
                        "startSeconds": stop.start, "endSeconds": stop.end
                    }))
                })
            });
            Ok(json!({
                "scenarioId": request.scenario_id, "activeJobId": plan.job_id,
                "planVersion": plan.version, "operationRevision": op.revision,
                "adopted": true, "alreadyActive": already_active, "planStale": op.plan_stale,
                "adoptedUnixMs": plan.adopted_unix_ms, "ticket": ticket,
                "viewUrl": format!("{origin}/?view=panel#scene/{}/operations/{}", request.scenario_id, plan.job_id)
            }))
        }).await
    }

    async fn run(
        &self,
        call: impl FnOnce(Service) -> AppResult<Value> + Send + 'static,
    ) -> CallToolResult {
        tool_result(compute(&self.state, call).await)
    }

    async fn deliver(
        &self,
        wait_for_result: bool,
        ticket: Option<String>,
        submit: impl FnOnce(Service) -> AppResult<Value> + Send + 'static,
    ) -> CallToolResult {
        let receipt = match compute(&self.state, submit).await {
            Ok(receipt) => receipt,
            Err(error) => return tool_result(Err(error)),
        };
        let Some(job_id) = receipt["jobId"].as_str().filter(|_| wait_for_result) else {
            return CallToolResult::structured(receipt);
        };
        let Ok(_slot) = self.state.events.try_acquire() else {
            return CallToolResult::structured(receipt);
        };
        let job_id = job_id.to_owned();
        let task_id = receipt["taskId"].as_str().unwrap_or(&job_id).to_owned();
        let origin = self.origin.clone();
        let result = tokio::time::timeout(INLINE_RESULT_WAIT, async {
            let mut updates = compute(&self.state, move |service| service.subscribe(&job_id))
                .await
                .ok()?;
            updates.wait_for(terminal).await.ok()?;
            compute(&self.state, move |service| {
                task_status(&service, &origin, &task_id, ticket.as_deref(), false)
            })
            .await
            .ok()
        })
        .await;
        // A receipt always remains usable after timeout, including when the job is queued.
        CallToolResult::structured(result.ok().flatten().unwrap_or(receipt))
    }
}

fn tool_result(result: AppResult<Value>) -> CallToolResult {
    match result {
        Ok(value) => CallToolResult::structured(value),
        Err(error) => CallToolResult::structured_error(json!({
            "code": error.code, "message": error.message
        })),
    }
}

#[tool_handler]
impl ServerHandler for Planner {
    fn get_info(&self) -> ServerConfig {
        let instructions = "Use the returned viewUrl exactly as provided and show it as a clickable URL. If the response contains a result or a workbench URL, deliver it directly without get_plan, code execution or local files. Use get_plan for extra details or pending workflows that have no link. A candidate becomes effective only after the human explicitly approves it. Then call adopt_plan for that exact candidate and return the published version and link. On conflict ask for a fresh review.";
        let instructions = match &self.default_scenario {
            Some(scene) => format!(
                "已配置工单场景 {} v{}。对‘排一下今天的工单’，直接调用 solve_plan，只填新的 ASCII requestId。\n{instructions}",
                scene.scenario_id, scene.version
            ),
            None => instructions.to_owned(),
        };
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("vrp-1", env!("CARGO_PKG_VERSION")))
            .with_instructions(instructions)
    }
}

fn publish(service: &Service, request: &pb::AdoptPlanRequest) -> AppResult<(pb::Operation, bool)> {
    let read = || service.get_operation(&request.scenario_id, 0);
    let same = |op: &pb::Operation| {
        !op.closed
            && op.revision > request.expected_revision
            && op.plan.as_ref().is_some_and(|plan| {
                plan.job_id == request.job_id
                    && plan.version.checked_sub(1) == Some(request.expected_plan_version)
            })
    };
    let current = read()?;
    if same(&current) {
        return Ok((current, true));
    }
    if current.closed {
        return Err(AppError::conflict("execution is closed"));
    }
    match service.adopt_plan(request.clone()) {
        Ok(published) => Ok((published, false)),
        Err(error) if error.code == "conflict" => {
            let current = read()?;
            if same(&current) {
                Ok((current, true))
            } else {
                Err(error)
            }
        }
        Err(error) => Err(error),
    }
}

fn operation(service: &Service, scenario_id: &str) -> AppResult<Value> {
    match service.get_operation(scenario_id, 0) {
        Ok(op) => Ok(json!({
            "revision": op.revision, "atSeconds": op.at, "closed": op.closed,
            "planVersion": op.plan.as_ref().map(|p| p.version),
            "activeJobId": op.plan.as_ref().map(|p| &p.job_id),
            "latestJobId": op.latest_job_id
        })),
        Err(error) if error.code == "not_found" => Ok(Value::Null),
        Err(error) => Err(error),
    }
}

fn submission(service: &Service, origin: &str, job: &pb::Job) -> AppResult<Value> {
    let warning = service
        .queue_plan_geometry(job)
        .err()
        .map(|error| error.message);
    let mut response = plan_status(service, origin, job, None, None, false)?;
    if let Some(warning) = warning {
        response["geometryMessage"] = json!(warning);
    }
    Ok(response)
}

fn task_status(
    service: &Service,
    origin: &str,
    id: &str,
    ticket: Option<&str>,
    include_assignments: bool,
) -> AppResult<Value> {
    if !id.starts_with("task-") {
        return plan_status(
            service,
            origin,
            &service.get_job(id)?,
            None,
            ticket,
            include_assignments,
        );
    }
    let task = service.get_task(id)?;
    if task.kind() != pb::TaskKind::GeoPlan {
        return Err(AppError::invalid("task is not a planning workflow"));
    }
    if let Some(job) = task.solver.as_ref() {
        return plan_status(
            service,
            origin,
            job,
            Some(&task),
            ticket,
            include_assignments,
        );
    }
    Ok(json!({
        "taskId": task.id, "solveState": task.state().as_str_name(),
        "geometryState": "waiting", "phase": task.progress.as_ref().map(|p| &p.phase),
        "error": task.error
    }))
}

fn plan_status(
    service: &Service,
    origin: &str,
    job: &pb::Job,
    workflow: Option<&pb::Task>,
    ticket: Option<&str>,
    include_assignments: bool,
) -> AppResult<Value> {
    let geometry_task = service.plan_geometry_task(&job.id)?;
    let source = geometry_task.as_ref().or(workflow);
    let geometry_state = match source.map(pb::Task::state) {
        Some(pb::TaskState::Queued) => "queued",
        Some(pb::TaskState::Running | pb::TaskState::Cancelling) => "running",
        Some(pb::TaskState::Failed | pb::TaskState::Cancelled | pb::TaskState::Interrupted) => {
            "failed"
        }
        Some(pb::TaskState::Partial) => "partial",
        _ => "unavailable",
    };
    let mut response = json!({
        "taskId": workflow.map_or(job.id.as_str(), |task| &task.id),
        "jobId": job.id,
        "solveState": job.state().as_str_name(),
        "geometryState": geometry_state,
        "geometryMessage": source.and_then(|task| task.error.as_ref().map(|e| &e.message).or(task.warnings.first())),
        "scenarioVersion": job.scenario.as_ref().map(|s| s.version),
        "candidate": job.execution_basis.is_some(),
        "threads": job.threads,
        "timing": {
            "submittedUnixMs": job.created_unix_ms,
            "startedUnixMs": (job.started_unix_ms > 0).then_some(job.started_unix_ms),
            "finishedUnixMs": (job.finished_unix_ms > 0).then_some(job.finished_unix_ms)
        },
        "error": job.error
    });
    if let Some(scene) = &job.scenario {
        let tab = if job.execution_basis.is_some() {
            "operations"
        } else {
            "plans"
        };
        response["scenarioId"] = json!(scene.scenario_id);
        response["viewUrl"] = json!(format!(
            "{origin}/?view=panel#scene/{}/{tab}/{}",
            scene.scenario_id, job.id
        ));
        if job.execution_basis.is_some() {
            response["operation"] = operation(service, &scene.scenario_id)?;
        }
    }
    if terminal(job) && job.has_result {
        let input = engine::SolveRequest::decode(service.payload(&job.id, false)?.as_slice())?;
        let result = engine::SolveResult::decode(service.payload(&job.id, true)?.as_slice())?;
        let problem = input.problem.as_ref().unwrap();
        let business = |i: u32| !problem.tickets[i as usize].id.starts_with("@vrp/");
        let geometry = service.get_geometry(&job.id)?;
        let expected_legs = result
            .routes
            .iter()
            .map(|route| route.stops.len() + 1)
            .sum::<usize>();
        let road_legs = geometry.legs.len();
        if road_legs > 0 {
            response["geometryState"] = json!(if road_legs < expected_legs {
                "partial"
            } else {
                "ready"
            });
        }
        response["result"] = json!({
            "score": result.score, "solveElapsedMs": result.stats.as_ref().map(|s| s.elapsed_ms),
            "assigned": result.routes.iter().flat_map(|route| &route.stops).filter(|stop| business(stop.ticket)).count(),
            "unassigned": result.unassigned.iter().filter(|row| business(row.ticket)).count(),
            "workingUnitsUsed": result.routes.len(),
            "travelMeters": result.routes.iter().map(|route| route.travel_distance).sum::<i64>(),
            "roadLegs": road_legs, "totalLegs": expected_legs
        });
        if include_assignments {
            response["assignments"] = json!({"routes": result.solution.iter()
                .flat_map(|solution| &solution.routes).map(|route| json!({
                    "agentId": problem.agents[route.agent as usize].id,
                    "ticketIds": route.tickets.iter().map(|&i| &problem.tickets[i as usize].id).collect::<Vec<_>>()
                })).collect::<Vec<_>>()});
        }
        if let Some(id) = ticket {
            response["ticket"] = result.routes.iter().find_map(|route| {
                route.stops.iter().find(|stop| problem.tickets[stop.ticket as usize].id == id)
                    .map(|stop| json!({
                        "ticketId": id, "agentId": problem.agents[route.agent as usize].id,
                        "arrivalSeconds": stop.arrival, "startSeconds": stop.start, "endSeconds": stop.end
                    }))
            }).unwrap_or(Value::Null);
        }
    }
    Ok(response)
}
