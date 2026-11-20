use super::*;
use crate::operations as execution;

// Recheck under the Store lock immediately before persisting a derived task.
fn current_source(store: &Store, source: &pb::Job) -> Result<Option<pb::Operation>> {
    // Compilation happens outside the store lock; a source could be deleted in
    // that interval. Recheck existence before creating a lineage reference.
    store.job(job_id(&source.id)?)?;
    let Some(reference) = &source.scenario else {
        return Ok(None);
    };
    let (latest, _) = store.scenario(&reference.scenario_id, 0)?;
    if latest.version != reference.version {
        return Err(AppError::conflict(
            "scenario changed; replan from the latest saved data",
        ));
    }
    let operation = store.operation(&reference.scenario_id, 0)?;
    if let Some(basis) = &source.execution_basis {
        let op = operation
            .as_ref()
            .ok_or_else(|| AppError::conflict("execution no longer exists"))?;
        if op.closed
            || op.execution_version != basis.execution_version
            || op.plan.as_ref().map_or(0, |p| p.version) != basis.plan_version
        {
            return Err(AppError::conflict(
                "execution or published plan changed; use real-time replan",
            ));
        }
    } else if operation.is_some() {
        return Err(AppError::conflict(
            "execution has started; use a current real-time candidate",
        ));
    }
    Ok(operation)
}

impl Service {
    fn completed_source(
        &self,
        id: &str,
    ) -> Result<(pb::Job, engine::SolveRequest, engine::SolveResult)> {
        let state = self.state()?;
        let key = job_id(id)?;
        let job = state.store.job(key)?;
        if !terminal(&job) || !job.has_result {
            return Err(AppError::conflict(
                "a terminal task with a final result is required",
            ));
        }
        current_source(&state.store, &job)?;
        let input = engine::SolveRequest::decode(state.store.payload(key, false)?.as_slice())?;
        let result = engine::SolveResult::decode(state.store.payload(key, true)?.as_slice())?;
        Ok((job, input, result))
    }

    pub fn continue_job(&self, request: pb::ContinueJobRequest) -> Result<pb::Job> {
        let (source, mut input, result) = self.completed_source(&request.job_id)?;
        input.initial_solution = Some(
            result
                .solution
                .ok_or_else(|| AppError::conflict("source has no solution"))?,
        );
        if request.options.is_some() {
            input.options = request.options;
        }
        // Compatibility normalization is explicit for old frozen job inputs.
        vrp_model::materialize_matrices(&mut input).map_err(|e| AppError::invalid(e.0))?;
        vrp_model::validate_ready_matrices(&input).map_err(|e| AppError::invalid(e.0))?;
        let threads = self.validate(&input)?;
        vrp_model::compile(&input).map_err(|e| AppError::invalid(e.0))?;
        let bytes = input.encode_to_vec();
        let mut state = self.state()?;
        self.accepting()?;
        let mut operation = current_source(&state.store, &source)?;
        if state.store.queued()? >= self.inner.config.max_queued {
            return Err(AppError::new("capacity", "task queue is full"));
        }
        let job = state.store.atomic(|store| {
            let job = store.insert_job(
                pb::Job {
                    scenario: source.scenario.clone(),
                    execution_basis: source.execution_basis.clone(),
                    kind: pb::JobKind::Optimization as i32,
                    source_job_id: source.id.clone(),
                    threads,
                    revision: 1,
                    created_unix_ms: now_ms(),
                    ..Default::default()
                },
                &bytes,
                "",
                &Sha256::digest(&bytes),
            )?;
            if let Some(op) = &mut operation {
                op.latest_job_id = job.id.clone();
                op.needs_replan = false;
                op.planning_error = None;
                op.revision += 1;
                store.save_operation(op, "optimize", &request.encode_to_vec(), None, &[])?;
            }
            Ok(job)
        })?;
        self.inner.changed.notify_all();
        Ok(job)
    }

    pub fn adjust_plan(&self, request: pb::AdjustPlanRequest) -> Result<pb::Job> {
        if request.actor.trim().is_empty()
            || request.reason.trim().is_empty()
            || request.actor.len() > 256
            || request.reason.len() > 2048
        {
            return Err(AppError::invalid(
                "manual adjustment requires actor and reason",
            ));
        }
        let (source, mut input, _) = self.completed_source(&request.job_id)?;
        input.initial_solution = Some(
            request
                .solution
                .clone()
                .ok_or_else(|| AppError::invalid("solution is required"))?,
        );
        vrp_model::materialize_matrices(&mut input).map_err(|e| AppError::conflict(e.0))?;
        vrp_model::validate_ready_matrices(&input).map_err(|e| AppError::conflict(e.0))?;
        self.validate(&input)?;
        let compiled = vrp_model::compile(&input).map_err(|e| AppError::conflict(e.0))?;
        let solution = compiled
            .initial
            .as_ref()
            .ok_or_else(|| AppError::invalid("solution is required"))?;
        let evaluation = vrp_core::oracle::evaluate(&compiled.problem, solution);
        let result = vrp_model::build_result(
            &compiled,
            solution,
            &evaluation,
            engine::SolveStats {
                termination_reason: "manual_evaluation".into(),
                engine_version: env!("CARGO_PKG_VERSION").into(),
                ..Default::default()
            },
        );
        let bytes = input.encode_to_vec();
        let mut state = self.state()?;
        self.accepting()?;
        let mut operation = current_source(&state.store, &source)?;
        let job = state.store.atomic(|store| {
            let mut job = store.insert_job(
                pb::Job {
                    scenario: source.scenario.clone(),
                    execution_basis: source.execution_basis.clone(),
                    kind: pb::JobKind::ManualAdjustment as i32,
                    source_job_id: source.id.clone(),
                    actor: request.actor.clone(),
                    reason: request.reason.clone(),
                    revision: 1,
                    created_unix_ms: now_ms(),
                    ..Default::default()
                },
                &bytes,
                "",
                &Sha256::digest(&bytes),
            )?;
            job.state = pb::JobState::Succeeded as i32;
            job.has_result = true;
            job.started_unix_ms = job.created_unix_ms;
            job.finished_unix_ms = now_ms();
            job.score = result.score;
            job.stats = result.stats.clone();
            store.update(&job, Some(&result.encode_to_vec()))?;
            if let Some(op) = &mut operation {
                op.latest_job_id = job.id.clone();
                op.needs_replan = false;
                op.planning_error = None;
                op.revision += 1;
                store.save_operation(
                    op,
                    "manual_adjustment",
                    &request.encode_to_vec(),
                    None,
                    &[],
                )?;
            }
            Ok(job)
        })?;
        // Manual drafts always require explicit adoption, including auto-adopt scenes.
        Ok(job)
    }

    pub fn list_operation_history(
        &self,
        request: pb::ListOperationHistoryRequest,
    ) -> Result<pb::ListOperationHistoryResponse> {
        scenario_id(&request.scenario_id)?;
        if request.before_revision > i64::MAX as u64 {
            return Err(AppError::invalid("revision out of range"));
        }
        let limit = page_size(request.page_size)?;
        let mut entries = self.state()?.store.operation_history(
            &request.scenario_id,
            request.before_revision,
            limit + 1,
        )?;
        let more = entries.len() > limit;
        entries.truncate(limit);
        let next_before_revision = if more {
            entries.last().unwrap().revision
        } else {
            0
        };
        Ok(pb::ListOperationHistoryResponse {
            entries,
            next_before_revision,
        })
    }

    pub(super) fn initial_plan(
        &self,
        request: &pb::StartOperationRequest,
        input: &engine::SolveRequest,
        op: &mut pb::Operation,
    ) -> Result<()> {
        if request.initial_job_id.is_empty() {
            return Ok(());
        }
        let (job, previous, result) = self.completed_source(&request.initial_job_id)?;
        if job.scenario != request.scenario || job.execution_basis.is_some() {
            return Err(AppError::conflict(
                "initial plan must use this exact static scenario version",
            ));
        }
        let mut current = execution::project(input, op, op.at, None)?;
        vrp_model::prepare_matrices(&mut current).map_err(|e| AppError::conflict(e.0))?;
        vrp_model::validate_ready_matrices(&current).map_err(|e| AppError::conflict(e.0))?;
        let mut solution = execution::remap_solution(
            &previous,
            result
                .solution
                .as_ref()
                .ok_or_else(|| AppError::conflict("missing solution"))?,
            &current,
        )?;
        // Static delivery assumes a loaded departure; execution starts with warehouse
        // stock. Add required collection visits, without inventing physical loading.
        let p = current.problem.as_ref().unwrap();
        let context = p.execution.as_ref().unwrap();
        for route in &mut solution.routes {
            let mut ordered = Vec::new();
            for &ticket in route.iter() {
                if let Some(load) = context
                    .tickets
                    .iter()
                    .find(|r| r.ticket == ticket)
                    .and_then(|r| r.load_before)
                    && !ordered.contains(&load)
                {
                    ordered.push(load);
                }
                if !ordered.contains(&ticket) {
                    ordered.push(ticket);
                }
            }
            *route = ordered;
        }
        let compiled = vrp_model::compile(&current).map_err(|e| AppError::conflict(e.0))?;
        solution
            .validate(&compiled.problem)
            .map_err(|e| AppError::conflict(e.0))?;
        let evaluated = vrp_core::oracle::evaluate(&compiled.problem, &solution);
        use vrp_core::ConstraintId as C;
        if evaluated.score.hard < 0
            || [
                C::ExecutionConsistency,
                C::AgentCapacity,
                C::UnreachableLeg,
                C::RefTicketAfterDepTicket,
            ]
            .into_iter()
            .any(|c| evaluated.total(c).amount > 0)
        {
            return Err(AppError::conflict(
                "initial plan conflicts with execution start, cargo, capacity or commitments; revise the plan",
            ));
        }
        op.plan = Some(execution::published(
            &current,
            &evaluated,
            op,
            &job.id,
            &request.actor,
            now_ms(),
        )?);
        op.latest_job_id = job.id;
        op.needs_replan = false;
        op.plan_stale = false;
        Ok(())
    }
}
