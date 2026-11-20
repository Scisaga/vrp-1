#![allow(dead_code)]

use vrp_core::{AgentSpec, Matrix, ProblemSpec, TicketSpec, Weights};

/// n 个地点排在一条直线上：相邻两点行驶 60 秒、1000 米。
pub fn line_matrix(n: usize) -> Matrix {
    let mut duration = vec![0; n * n];
    let mut distance = vec![0; n * n];
    for i in 0..n {
        for j in 0..n {
            let gap = i.abs_diff(j) as u32;
            duration[i * n + j] = gap * 60;
            distance[i * n + j] = gap * 1000;
        }
    }
    Matrix {
        n,
        duration,
        distance,
        buffer_permille: Vec::new(),
    }
}

pub fn agent(id: &str, start_location: u32, start_time: i64) -> AgentSpec {
    AgentSpec {
        id: id.to_string(),
        start_location,
        end_location: start_location,
        start_time,
        ..AgentSpec::default()
    }
}

pub fn ticket(id: &str, location: u32, duration: i64) -> TicketSpec {
    TicketSpec {
        id: id.to_string(),
        location,
        duration,
        ..TicketSpec::default()
    }
}

pub fn spec(locations: usize, weights: Weights) -> ProblemSpec {
    ProblemSpec {
        matrices: vec![line_matrix(locations)],
        weights,
        ..ProblemSpec::default()
    }
}
