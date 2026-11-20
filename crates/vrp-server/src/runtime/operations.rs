use super::*;
use crate::operations as execution;
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
                pb::Job {
                    scenario: op.scenario.clone(),
                    kind: pb::JobKind::RealtimeReplan as i32,
                    source_job_id: source_job_id.to_owned(),
                    state: pb::JobState::Queued as i32,
                    threads,
                    revision: 1,
                    created_unix_ms: now_ms(),
                    execution_basis: Some(pb::ExecutionBasis {
                        scenario_id: request.scenario_id.clone(),
                        execution_version: op.execution_version,
                        plan_version: op.plan.as_ref().map_or(0, |p| p.version),
                        at,
                    }),
                    ..Default::default()
                },
                &bytes,
                "",
                &Sha256::digest(&bytes),
            )?;
            current.latest_job_id = job.id.clone();
            current.needs_replan = false;
            current.planning_error = None;
            current.revision += 1;
            store.save_operation(&current, "replan", &request.encode_to_vec(), None, &[])?;
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
