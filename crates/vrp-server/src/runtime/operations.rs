use super::*;
use crate::operations as execution;
use std::collections::BTreeSet;
use vrp_core::ConstraintId as C;

impl Service {
    pub fn get_operation(&self, id: &str, revision: u64) -> Result<pb::Operation> {
        scenario_id(id)?;
        if revision > i64::MAX as u64 {
            return Err(AppError::invalid("revision out of range"));
        }
        self.state()?
            .store
            .operation(id, revision)?
            .ok_or_else(|| AppError::new("not_found", "operation not found"))
    }
    pub fn start_operation(&self, request: pb::StartOperationRequest) -> Result<pb::Operation> {
        let reference = request
            .scenario
            .clone()
            .ok_or_else(|| AppError::invalid("scenario reference required"))?;
        if reference.version == 0 {
            return Err(AppError::invalid("explicit scenario version required"));
        }
        let saved = self.get_scenario(&reference.scenario_id, reference.version)?;
        let input = saved.request.unwrap();
        let mut op = execution::initial(
            reference.clone(),
            &input,
            request.at,
            request.policy.clone(),
            now_ms(),
        )?;
        let mut validation = input.clone();
        validation.options = op.policy.as_ref().and_then(|p| p.options);
        self.validate(&validation)?;
        self.initial_plan(&request, &input, &mut op)?;
        let resources = execution::problem(&input)?
            .agents
            .iter()
            .map(|a| a.resource_id.clone())
            .collect::<Vec<_>>();
        let mut state = self.state()?;
        self.accepting()?;
        if state.store.operation(&reference.scenario_id, 0)?.is_some() {
            return Err(AppError::conflict(
                "scenario already has an operation; create a new scenario for a new execution",
            ));
        }
        let (latest, _) = state.store.scenario(&reference.scenario_id, 0)?;
        if latest.version != reference.version {
            return Err(AppError::conflict(
                "start execution from the latest business version",
            ));
        }
        if !request.initial_job_id.is_empty() {
            state.store.job(job_id(&request.initial_job_id)?)?;
        }
        state.store.atomic(|s| {
            s.claim_resources(&reference.scenario_id, &resources)?;
            s.save_operation(&op, "start", &request.encode_to_vec(), None, &[])
        })?;
        self.inner.changed.notify_all();
        Ok(op)
    }
    pub fn apply_execution_event(&self, request: pb::ExecutionEvent) -> Result<pb::Operation> {
        scenario_id(&request.scenario_id)?;
        if request.event_id.is_empty()
            || request.event_id.len() > 128
            || !request.event_id.bytes().all(|b| b.is_ascii_graphic())
        {
            return Err(AppError::invalid(
                "event_id must be 1..128 visible ASCII characters",
            ));
        }
        let bytes = request.encode_to_vec();
        let hash = Sha256::digest(&bytes);
        let mut state = self.state()?;
        if let Some(old) =
            state
                .store
                .operation_event(&request.scenario_id, &request.event_id, &hash)?
        {
            return Ok(old);
        }
        self.accepting()?;
        let mut op = state
            .store
            .operation(&request.scenario_id, 0)?
            .ok_or_else(|| AppError::new("not_found", "operation not found"))?;
        execution::require_revision(&op, request.expected_revision)?;
        let reference = op.scenario.as_ref().unwrap();
        let (_, input) = state
            .store
            .scenario(&reference.scenario_id, reference.version)?;
        let input = engine::SolveRequest::decode(input.as_slice())?;
        execution::apply_event(&mut op, execution::problem(&input)?, &request, now_ms())?;
        state
            .store
            .save_operation(&op, "event", &bytes, Some(&request.event_id), &hash)?;
        self.inner.changed.notify_all();
        Ok(op)
    }
    pub fn configure_operation(
        &self,
        request: pb::ConfigureOperationRequest,
    ) -> Result<pb::Operation> {
        let mut state = self.state()?;
        self.accepting()?;
        let mut op = state
            .store
            .operation(&request.scenario_id, 0)?
            .ok_or_else(|| AppError::new("not_found", "operation not found"))?;
        execution::require_revision(&op, request.expected_revision)?;
        let reference = op.scenario.as_ref().unwrap();
        let (_, input) = state
            .store
            .scenario(&reference.scenario_id, reference.version)?;
        let mut input = engine::SolveRequest::decode(input.as_slice())?;
        let policy = execution::policy(
            request.policy.clone(),
            op.policy.as_ref().and_then(|p| p.options.as_ref()),
        )?;
        if policy.clock != op.policy.as_ref().unwrap().clock {
            return Err(AppError::conflict(
                "execution clock cannot change during an operation",
            ));
        }
        input.options = policy.options;
        self.validate(&input)?;
        op.policy = Some(policy);
        op.route_commitments = request.route_commitments.clone();
        op.time_commitments = request.time_commitments.clone();
        execution::validate_commitments(&op, execution::problem(&input)?)?;
        if !op.policy.as_ref().unwrap().auto_replan {
            op.needs_replan = false;
        }
        execution::mark_dirty(&mut op, now_ms(), true);
        op.revision += 1;
        state
            .store
            .save_operation(&op, "configure", &request.encode_to_vec(), None, &[])?;
        self.inner.changed.notify_all();
        Ok(op)
    }
    pub fn replan(&self, request: pb::ReplanRequest) -> Result<pb::Job> {
        self.replan_from(request, "")
    }

    pub(crate) fn replan_change(&self, change: pb::ReplanChangeRequest) -> Result<pb::Job> {
        scenario_id(&change.scenario_id)?;
        if change.request_id.is_empty()
            || change.request_id.len() > 128
            || !change.request_id.bytes().all(|b| b.is_ascii_graphic())
            || (change.urgent_ticket.is_none() && change.disabled_agent_id.is_empty())
        {
            return Err(AppError::invalid(
                "a request ID (1..128 visible ASCII) and at least one change are required",
            ));
        }
        let key = format!(
            "mcp-replan-{:x}",
            Sha256::digest(format!("{}:{}", change.scenario_id, change.request_id))
        );
        let fingerprint = Sha256::digest(change.encode_to_vec());
        let mut state = self.state()?;
        if let Some(job) = state.store.idempotent(&key, &fingerprint)? {
            return Ok(job);
        }
        self.accepting()?;
        if state.store.queued()? >= self.inner.config.max_queued {
            return Err(AppError::new("capacity", "task queue is full"));
        }
        let mut op = state
            .store
            .operation(&change.scenario_id, 0)?
            .ok_or_else(|| AppError::new("not_found", "operation not found"))?;
        execution::require_revision(&op, change.expected_revision)?;
        if op.closed || op.plan.is_none() || op.policy.as_ref().is_some_and(|p| p.auto_adopt) {
            return Err(AppError::conflict(
                "MCP replan requires an active published plan and manual adoption",
            ));
        }
        let reference = op.scenario.as_ref().unwrap();
        let (scene, bytes) = state
            .store
            .scenario(&reference.scenario_id, reference.version)?;
        let mut business = engine::SolveRequest::decode(bytes.as_slice())?;
        let now = now_ms();
        let at = execution::at(&op, now);
        let update = change
            .urgent_ticket
            .as_ref()
            .map(|ticket| {
                self.insert_service_ticket(&mut op, &scene, &mut business, ticket, at, now)
            })
            .transpose()?;
        let event = if change.disabled_agent_id.is_empty() {
            None
        } else {
            let event = pb::ExecutionEvent {
                scenario_id: change.scenario_id.clone(),
                expected_revision: op.revision,
                event_id: key.clone(),
                at,
                actor: "MCP".into(),
                vehicles: vec![pb::VehicleUpdate {
                    agent_id: change.disabled_agent_id,
                    phase: Some(pb::VehiclePhase::Unavailable as i32),
                    ..Default::default()
                }],
                ..Default::default()
            };
            execution::apply_event(&mut op, execution::problem(&business)?, &event, now)?;
            Some((event, op.clone()))
        };
        let mut projected = execution::project(&business, &op, at, change.options)?;
        vrp_model::prepare_matrices(&mut projected).map_err(|e| AppError::invalid(e.0))?;
        vrp_model::validate_ready_matrices(&projected).map_err(|e| AppError::invalid(e.0))?;
        let threads = self.validate(&projected)?;
        let command = pb::ReplanRequest {
            scenario_id: change.scenario_id,
            expected_revision: op.revision,
            options: change.options,
        };

        let job = state.store.atomic(|store| {
            if let Some((request, business_op)) = update {
                let saved = store.save_scenario(&request, &business.encode_to_vec())?;
                store.save_operation(
                    &business_op,
                    "business_update",
                    &saved.encode_to_vec(),
                    None,
                    &[],
                )?;
            }
            if let Some((event, event_op)) = event {
                let bytes = event.encode_to_vec();
                store.save_operation(
                    &event_op,
                    "event",
                    &bytes,
                    Some(&key),
                    &Sha256::digest(&bytes),
                )?;
            }
            let job = store.insert_job(
                queued_replan(&op, threads, at, ""),
                &projected.encode_to_vec(),
                &key,
                &fingerprint,
            )?;
            record_replan(store, &mut op, &job, &command)?;
            Ok(job)
        })?;
        self.inner.changed.notify_all();
        Ok(job)
    }

    fn insert_service_ticket(
        &self,
        op: &mut pb::Operation,
        scene: &pb::Scenario,
        business: &mut engine::SolveRequest,
        ticket: &pb::UrgentServiceTicket,
        at: i64,
        now: i64,
    ) -> Result<(pb::SaveScenarioRequest, pb::Operation)> {
        if ticket.ticket_id.is_empty()
            || ticket.ticket_id.len() > 128
            || ticket.ticket_id.starts_with("@vrp/")
            || ticket.duration_seconds <= 0
            || ticket.latest_start_seconds < at
        {
            return Err(AppError::invalid(
                "invalid urgent ticket ID, duration or deadline",
            ));
        }
        let previous = business.clone();
        let problem = business.problem.as_mut().unwrap();
        if problem.tickets.iter().any(|t| t.id == ticket.ticket_id) {
            return Err(AppError::conflict("urgent ticket ID already exists"));
        }
        if !ticket.skills_required.is_empty() {
            let skills = problem
                .agents
                .iter()
                .flat_map(|agent| agent.skills.iter().map(String::as_str))
                .collect::<BTreeSet<_>>();
            if let Some(skill) = ticket
                .skills_required
                .iter()
                .find(|skill| !skills.contains(skill.as_str()))
            {
                return Err(AppError::invalid(format!(
                    "Unknown skill ID {skill:?}. Use an exact skill ID from the scenario: {}",
                    skills.into_iter().collect::<Vec<_>>().join(", ")
                )));
            }
        }
        let location = problem
            .locations
            .iter()
            .position(|l| l.id == ticket.location_id)
            .ok_or_else(|| {
                AppError::invalid("urgent ticket must use an existing matrix location")
            })?;
        let day = problem
            .horizon
            .as_ref()
            .filter(|h| !h.day_starts.is_empty())
            .map_or(at.div_euclid(86_400), |h| {
                h.day_starts
                    .partition_point(|start| *start <= at)
                    .saturating_sub(1) as i64
            });
        problem.tickets.push(engine::Ticket {
            id: ticket.ticket_id.clone(),
            location: location as u32,
            duration: ticket.duration_seconds,
            earliest_start: at,
            latest_start: ticket.latest_start_seconds,
            day: i32::try_from(day)
                .map_err(|_| AppError::invalid("urgent ticket day out of range"))?,
            skills_required: ticket.skills_required.clone(),
            ..Default::default()
        });
        self.validate(business)?;
        op.at = at;
        op.clock_unix_ms = now;
        execution::synchronize(op, Some(&previous), business)?;
        let next = scene
            .version
            .checked_add(1)
            .filter(|v| *v <= i64::MAX as u64)
            .ok_or_else(|| AppError::conflict("scenario version exhausted"))?;
        op.scenario.as_mut().unwrap().version = next;
        execution::mark_dirty(op, now, true);
        op.revision += 1;
        Ok((
            pb::SaveScenarioRequest {
                scenario_id: scene.id.clone(),
                name: scene.name.clone(),
                expected_version: scene.version,
                request: Some(business.clone()),
                ..Default::default()
            },
            op.clone(),
        ))
    }

    pub(super) fn replan_from(
        &self,
        request: pb::ReplanRequest,
        source_job_id: &str,
    ) -> Result<pb::Job> {
        let op = self.get_operation(&request.scenario_id, 0)?;
        execution::require_revision(&op, request.expected_revision)?;
        let reference = op.scenario.as_ref().unwrap();
        let business = self
            .get_scenario(&reference.scenario_id, reference.version)?
            .request
            .unwrap();
        let at = execution::at(&op, now_ms());
        let mut projected = execution::project(&business, &op, at, request.options)?;
        vrp_model::prepare_matrices(&mut projected).map_err(|e| AppError::invalid(e.0))?;
        vrp_model::validate_ready_matrices(&projected).map_err(|e| AppError::invalid(e.0))?;
        let threads = self.validate(&projected)?;
        let bytes = projected.encode_to_vec();
        let mut state = self.state()?;
        self.accepting()?;
        let mut current = state
            .store
            .operation(&request.scenario_id, 0)?
            .ok_or_else(|| AppError::new("not_found", "operation not found"))?;
        execution::require_revision(&current, op.revision)?;
        if state.store.queued()? >= self.inner.config.max_queued {
            return Err(AppError::new("capacity", "task queue is full"));
        }
        let job = state.store.atomic(|store| {
            let job = store.insert_job(
                queued_replan(&op, threads, at, source_job_id),
                &bytes,
                "",
                &Sha256::digest(&bytes),
            )?;
            record_replan(store, &mut current, &job, &request)?;
            Ok(job)
        })?;
        self.inner.changed.notify_all();
        Ok(job)
    }
    pub fn adopt_plan(&self, request: pb::AdoptPlanRequest) -> Result<pb::Operation> {
        self.adopt_checked(request, false)
    }
    fn adopt_checked(
        &self,
        request: pb::AdoptPlanRequest,
        automatic: bool,
    ) -> Result<pb::Operation> {
        let op = self.get_operation(&request.scenario_id, 0)?;
        execution::require_revision(&op, request.expected_revision)?;
        if op.plan.as_ref().map_or(0, |p| p.version) != request.expected_plan_version {
            return Err(AppError::conflict("published plan changed"));
        }
        if request.accept_business_violations && (automatic || request.reason.trim().is_empty()) {
            return Err(AppError::invalid(
                "manual business override requires a reason",
            ));
        }
        let job = self.get_job(&request.job_id)?;
        let basis = job.execution_basis.as_ref().ok_or_else(|| {
            AppError::conflict("only an execution replan can become a published plan")
        })?;
        if !job.has_result
            || basis.scenario_id != request.scenario_id
            || job.scenario != op.scenario
            || basis.execution_version != op.execution_version
            || basis.plan_version != request.expected_plan_version
        {
            return Err(AppError::conflict(
                "candidate is missing or stale; replan against current business, execution and published-plan versions",
            ));
        }
        let reference = op.scenario.as_ref().unwrap();
        let business = self
            .get_scenario(&reference.scenario_id, reference.version)?
            .request
            .unwrap();
        let previous = engine::SolveRequest::decode(self.payload(&job.id, false)?.as_slice())?;
        let result = engine::SolveResult::decode(self.payload(&job.id, true)?.as_slice())?;
        let mut current = execution::project(
            &business,
            &op,
            execution::at(&op, now_ms()),
            previous.options,
        )?;
        vrp_model::prepare_matrices(&mut current).map_err(|e| AppError::conflict(e.0))?;
        vrp_model::validate_ready_matrices(&current).map_err(|e| AppError::conflict(e.0))?;
        let solution = execution::remap_solution(
            &previous,
            result
                .solution
                .as_ref()
                .ok_or_else(|| AppError::conflict("candidate has no solution"))?,
            &current,
        )?;
        let compiled = vrp_model::compile(&current).map_err(|e| AppError::conflict(e.0))?;
        solution
            .validate(&compiled.problem)
            .map_err(|e| AppError::conflict(e.0))?;
        let evaluation = vrp_core::oracle::evaluate(&compiled.problem, &solution);
        if [
            C::ExecutionConsistency,
            C::AgentCapacity,
            C::UnreachableLeg,
            C::RefTicketAfterDepTicket,
        ]
        .into_iter()
        .any(|c| evaluation.total(c).amount > 0)
        {
            return Err(AppError::conflict(
                "candidate violates physical execution, cargo/capacity, commitment times, reachability or predecessor completion",
            ));
        }
        if evaluation.score.hard < 0 && !request.accept_business_violations {
            return Err(AppError::conflict(
                "candidate has business hard violations; inspect the result before a manual override",
            ));
        }
        if automatic && let Some(plan) = &op.plan {
            let mut baseline = vrp_core::Solution::empty(compiled.problem.agents.len());
            let p = current.problem.as_ref().unwrap();
            for route in &plan.routes {
                if let Some(a) = p.agents.iter().position(|a| a.id == route.agent_id) {
                    for stop in &route.stops {
                        if let Some(t) = p.tickets.iter().position(|t| t.id == stop.ticket_id) {
                            baseline.routes[a].push(t as u32);
                        }
                    }
                }
            }
            if baseline.validate(&compiled.problem).is_ok() {
                let baseline_score = vrp_core::oracle::evaluate(&compiled.problem, &baseline).score;
                // Equal scores may refresh an obsolete ETA; a valid, better
                // published remainder remains authoritative without an error.
                if evaluation.score < baseline_score
                    || (evaluation.score == baseline_score && !op.plan_stale)
                {
                    return Ok(op);
                }
            }
        }
        let mut next = op.clone();
        next.plan = Some(execution::published(
            &current,
            &evaluation,
            &op,
            &job.id,
            &request.actor,
            now_ms(),
        )?);
        next.plan_stale = false;
        next.planning_error = None;
        next.revision += 1;
        let mut state = self.state()?;
        self.accepting()?;
        execution::require_revision(
            &state
                .store
                .operation(&request.scenario_id, 0)?
                .ok_or_else(|| AppError::new("not_found", "operation not found"))?,
            op.revision,
        )?;
        state.store.save_operation(
            &next,
            if automatic { "auto_adopt" } else { "adopt" },
            &request.encode_to_vec(),
            None,
            &[],
        )?;
        Ok(next)
    }
    pub fn acknowledge_plan(&self, request: pb::AcknowledgePlanRequest) -> Result<pb::Operation> {
        let mut state = self.state()?;
        self.accepting()?;
        let mut op = state
            .store
            .operation(&request.scenario_id, 0)?
            .ok_or_else(|| AppError::new("not_found", "operation not found"))?;
        if op.plan.as_ref().map_or(0, |p| p.version) != request.plan_version
            || request.plan_version == 0
        {
            return Err(AppError::conflict(
                "acknowledgment belongs to an old or absent plan",
            ));
        }
        let vehicle = op
            .vehicles
            .iter()
            .position(|v| v.agent_id == request.agent_id)
            .ok_or_else(|| AppError::invalid("unknown vehicle"))?;
        if op.vehicles[vehicle].acknowledged_plan == request.plan_version {
            return Ok(op);
        }
        execution::require_revision(&op, request.expected_revision)?;
        op.vehicles[vehicle].acknowledged_plan = request.plan_version;
        op.revision += 1;
        state
            .store
            .save_operation(&op, "acknowledge", &request.encode_to_vec(), None, &[])?;
        Ok(op)
    }
    pub fn close_operation(&self, request: pb::CloseOperationRequest) -> Result<pb::Operation> {
        let mut state = self.state()?;
        self.accepting()?;
        let mut op = state
            .store
            .operation(&request.scenario_id, 0)?
            .ok_or_else(|| AppError::new("not_found", "operation not found"))?;
        execution::require_revision(&op, request.expected_revision)?;
        if op.tickets.iter().any(|t| {
            matches!(
                t.phase(),
                pb::TicketPhase::EnRoute | pb::TicketPhase::Arrived | pb::TicketPhase::Serving
            )
        }) || op
            .cargo
            .iter()
            .any(|c| !c.agent_id.is_empty() && !c.delivered)
        {
            return Err(AppError::conflict(
                "finish or resolve active work and unload cargo before closing",
            ));
        }
        op.closed = true;
        op.needs_replan = false;
        op.revision += 1;
        state.store.atomic(|s| {
            s.claim_resources(&request.scenario_id, &[])?;
            s.save_operation(&op, "close", &request.encode_to_vec(), None, &[])
        })?;
        drop(state);
        if !op.latest_job_id.is_empty() {
            let _ = self.cancel(&op.latest_job_id);
        }
        Ok(op)
    }

    pub(super) fn save_with_execution(
        &self,
        state: &mut State,
        request: &pb::SaveScenarioRequest,
        bytes: &[u8],
    ) -> Result<pb::Scenario> {
        let Some(mut op) = state.store.operation(&request.scenario_id, 0)? else {
            return state.store.save_scenario(request, bytes);
        };
        if op.closed {
            return Err(AppError::conflict(
                "closed operation is immutable; copy it into a new scenario",
            ));
        }
        let reference = op.scenario.as_ref().unwrap();
        let (_, old) = state
            .store
            .scenario(&reference.scenario_id, reference.version)?;
        let old = engine::SolveRequest::decode(old.as_slice())?;
        op.at = execution::at(&op, now_ms());
        op.clock_unix_ms = now_ms();
        execution::synchronize(&mut op, Some(&old), request.request.as_ref().unwrap())?;
        let resources = execution::problem(request.request.as_ref().unwrap())?
            .agents
            .iter()
            .map(|a| a.resource_id.clone())
            .collect::<Vec<_>>();
        state.store.atomic(|s| {
            s.claim_resources(&request.scenario_id, &resources)?;
            let saved = s.save_scenario(request, bytes)?;
            op.scenario.as_mut().unwrap().version = saved.version;
            execution::mark_dirty(&mut op, now_ms(), true);
            op.revision += 1;
            s.save_operation(&op, "business_update", &saved.encode_to_vec(), None, &[])?;
            Ok(saved)
        })
    }
    pub(super) fn recover_operations(&self) -> Result<()> {
        let mut state = self.state()?;
        let mut completed = Vec::new();
        for id in state.store.operation_ids()? {
            let mut op = state.store.operation(&id, 0)?.unwrap();
            if !op.latest_job_id.is_empty() {
                let job = state.store.job(job_id(&op.latest_job_id)?)?;
                if matches!(
                    job.state(),
                    pb::JobState::Interrupted | pb::JobState::Failed
                ) {
                    execution::mark_dirty(&mut op, now_ms(), false);
                    op.planning_error = job.error;
                    op.revision += 1;
                    state
                        .store
                        .save_operation(&op, "recovery", &[], None, &[])?;
                } else if terminal(&job)
                    && job.has_result
                    && op.plan.as_ref().is_none_or(|p| p.job_id != job.id)
                {
                    completed.push(job);
                }
            }
        }
        drop(state);
        for job in completed {
            self.complete_replan(&job);
        }
        Ok(())
    }
    pub(super) fn dispatch_replan(&self) {
        let action = || -> Result<()> {
            let selected = {
                let state = self.state()?;
                if state.store.queued()? >= self.inner.config.max_queued {
                    return Ok(());
                }
                let mut selected = None;
                for id in state.store.due_operations(now_ms())? {
                    let op = state.store.operation(&id, 0)?.unwrap();
                    let previous = if op.latest_job_id.is_empty() {
                        None
                    } else {
                        Some(state.store.job(job_id(&op.latest_job_id)?)?)
                    };
                    if previous.as_ref().is_some_and(|job| {
                        !terminal(job)
                            && job
                                .execution_basis
                                .as_ref()
                                .is_some_and(|b| b.execution_version == op.execution_version)
                    }) {
                        continue;
                    }
                    selected = Some((id, op, previous));
                    break;
                }
                selected
            };
            let Some((id, op, previous)) = selected else {
                return Ok(());
            };
            if let Some(job) = previous
                && !terminal(&job)
            {
                if job
                    .execution_basis
                    .as_ref()
                    .is_some_and(|v| v.execution_version == op.execution_version)
                {
                    return Ok(());
                }
                self.cancel(&job.id)?;
            }
            match self.replan(pb::ReplanRequest {
                scenario_id: id.clone(),
                expected_revision: op.revision,
                options: None,
            }) {
                Ok(_) => {}
                Err(error) if error.code == "capacity" => {}
                Err(error) => {
                    let mut state = self.state()?;
                    let mut latest = state.store.operation(&id, 0)?.unwrap();
                    if latest.revision == op.revision {
                        latest.needs_replan = false;
                        latest.planning_error = Some(error.envelope());
                        latest.revision += 1;
                        state
                            .store
                            .save_operation(&latest, "planning_error", &[], None, &[])?;
                    }
                }
            }
            Ok(())
        };
        if let Err(error) = action() {
            eprintln!("execution dispatcher: {}", error.code);
        }
    }
    pub(super) fn complete_replan(&self, job: &pb::Job) {
        // An evaluated manual draft must not become live during recovery either.
        if job.kind() == pb::JobKind::ManualAdjustment {
            return;
        }
        let Some(basis) = &job.execution_basis else {
            return;
        };
        let completion = || -> Result<()> {
            let mut op = self.get_operation(&basis.scenario_id, 0)?;
            if op.closed || op.latest_job_id != job.id {
                return Ok(());
            }
            if !job.has_result {
                let mut state = self.state()?;
                if state
                    .store
                    .operation(&basis.scenario_id, 0)?
                    .is_none_or(|latest| latest.revision != op.revision)
                {
                    return Ok(());
                }
                op.planning_error = job.error.clone();
                op.revision += 1;
                state
                    .store
                    .save_operation(&op, "planning_error", &[], None, &[])?;
                return Ok(());
            }
            if op.policy.as_ref().is_some_and(|p| p.auto_adopt) {
                let request = pb::AdoptPlanRequest {
                    scenario_id: basis.scenario_id.clone(),
                    expected_revision: op.revision,
                    expected_plan_version: op.plan.as_ref().map_or(0, |p| p.version),
                    job_id: job.id.clone(),
                    actor: "automatic".into(),
                    ..Default::default()
                };
                if let Err(error) = self.adopt_checked(request, true) {
                    let mut state = self.state()?;
                    let mut latest = state.store.operation(&basis.scenario_id, 0)?.unwrap();
                    if latest.revision == op.revision {
                        latest.planning_error = Some(error.envelope());
                        latest.revision += 1;
                        state
                            .store
                            .save_operation(&latest, "adoption_blocked", &[], None, &[])?;
                    }
                }
            }
            Ok(())
        };
        if let Err(error) = completion() {
            eprintln!("execution completion: {}", error.code);
        }
    }
}

fn queued_replan(op: &pb::Operation, threads: u32, at: i64, source: &str) -> pb::Job {
    pb::Job {
        scenario: op.scenario.clone(),
        kind: pb::JobKind::RealtimeReplan as i32,
        source_job_id: source.to_owned(),
        state: pb::JobState::Queued as i32,
        threads,
        revision: 1,
        created_unix_ms: now_ms(),
        execution_basis: Some(pb::ExecutionBasis {
            scenario_id: op.scenario.as_ref().unwrap().scenario_id.clone(),
            execution_version: op.execution_version,
            plan_version: op.plan.as_ref().map_or(0, |p| p.version),
            at,
        }),
        ..Default::default()
    }
}

fn record_replan(
    store: &mut crate::store::Store,
    op: &mut pb::Operation,
    job: &pb::Job,
    request: &pb::ReplanRequest,
) -> Result<()> {
    op.latest_job_id.clone_from(&job.id);
    op.needs_replan = false;
    op.planning_error = None;
    op.revision += 1;
    store.save_operation(op, "replan", &request.encode_to_vec(), None, &[])
}
