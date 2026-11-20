use super::*;
use std::collections::HashSet;

impl Service {
    pub fn map_config(&self) -> pb::MapConfig {
        self.inner.config.map_config.clone()
    }

    pub fn get_geometry(&self, id: &str) -> Result<pb::JobGeometry> {
        let state = self.state()?;
        let job = state.store.job(job_id(id)?)?;
        if !terminal(&job) || !job.has_result {
            return Err(AppError::conflict("geometry requires a terminal result"));
        }
        state.store.geometry(job_id(id)?)
    }

    pub fn save_geometry(&self, geometry: pb::JobGeometry) -> Result<pb::JobGeometry> {
        self.accepting()?;
        if geometry.encoded_len() > self.request_limit() {
            return Err(AppError::new(
                "too_large",
                "geometry exceeds request size limit",
            ));
        }
        if geometry.coordinate_system != "WGS84"
            || geometry.provider.len() > 256
            || geometry.legs.len() > 5000
        {
            return Err(AppError::invalid(
                "geometry requires WGS84, provider <=256 bytes and at most 5000 legs",
            ));
        }
        let id = job_id(&geometry.job_id)?;
        let mut state = self.state()?;
        let job = state.store.job(id)?;
        if !terminal(&job) || !job.has_result {
            return Err(AppError::conflict("geometry requires a terminal result"));
        }
        let input = engine::SolveRequest::decode(state.store.payload(id, false)?.as_slice())?;
        let result = engine::SolveResult::decode(state.store.payload(id, true)?.as_slice())?;
        let p = input
            .problem
            .as_ref()
            .ok_or_else(|| AppError::internal("stored problem missing"))?;
        let solution = result
            .solution
            .as_ref()
            .ok_or_else(|| AppError::conflict("result has no solution"))?;
        let mut expected = HashMap::new();
        for route in &solution.routes {
            let agent = &p.agents[route.agent as usize];
            let mut locations = vec![agent.start_location];
            locations.extend(
                route
                    .tickets
                    .iter()
                    .map(|&i| p.tickets[i as usize].location),
            );
            locations.push(agent.end_location);
            for (index, pair) in locations.windows(2).enumerate() {
                expected.insert(
                    (agent.id.as_str(), index as u32),
                    (
                        &p.locations[pair[0] as usize].id,
                        &p.locations[pair[1] as usize].id,
                    ),
                );
            }
        }
        let mut seen = HashSet::new();
        let mut points = 0usize;
        for leg in &geometry.legs {
            match (leg.road_start_point_index, leg.road_end_point_index) {
                (None, None) => {}
                (Some(start), Some(end)) if start <= end && (end as usize) < leg.points.len() => {}
                _ => return Err(AppError::invalid("invalid geometry road point indices")),
            }
            let key = (leg.agent_id.as_str(), leg.leg_index);
            let Some((from, to)) = expected.get(&key) else {
                return Err(AppError::invalid("geometry leg is not in this job result"));
            };
            if !seen.insert(key) || **from != leg.from_location_id || **to != leg.to_location_id {
                return Err(AppError::invalid(
                    "duplicate leg or geometry endpoints disagree with the result",
                ));
            }
            points += leg.points.len();
            if leg.points.len() < 2
                || points > 250_000
                || leg.points.iter().any(|p| {
                    !p.lng.is_finite()
                        || !p.lat.is_finite()
                        || p.lng.abs() > 180.0
                        || p.lat.abs() > 90.0
                })
            {
                return Err(AppError::invalid(
                    "invalid geometry coordinates or point limit",
                ));
            }
        }
        state.store.save_geometry(id, &geometry)?;
        Ok(geometry)
    }
}
