use std::sync::Barrier;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};
use vrp_core::oracle;
use vrp_model::{Executor, ThreadBudget, compile};
use vrp_proto::engine::v1 as pb;

fn request() -> pb::SolveRequest {
    pb::SolveRequest {
        problem: Some(pb::Problem {
            locations: vec![pb::Location {
                id: "depot".into(),
                ..Default::default()
            }],
            matrices: vec![pb::TravelMatrix {
                profile: "car".into(),
                durations: vec![0; 4],
                distances: vec![0; 4],
                ..Default::default()
            }],
            agents: (0..2)
                .map(|a| pb::Agent {
                    id: format!("a{a}"),
                    ..Default::default()
                })
                .collect(),
            tickets: (0..16)
                .map(|t| pb::Ticket {
                    id: format!("t{t}"),
                    latest_start: 86400,
                    latest_end: 86400,
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        }),
        options: Some(pb::SolveOptions {
            threads: 2,
            time_limit_ms: 50,
            ..Default::default()
        }),
        initial_solution: None,
    }
}

#[test]
fn concurrent_jobs_share_the_budget_and_each_gets_its_own_search_time() {
    for budget in [
        ThreadBudget::from_cpu_count(2, 1.8).unwrap(),
        ThreadBudget::from_cpu_count(4, 1.0).unwrap(),
    ] {
        let executor = Executor::new(budget, 8).unwrap();
        // 3 个额度只能容纳一个 2 线程任务，4 个额度允许两个任务同时搜索。
        let request = request();
        let barrier = Barrier::new(5);
        let completed = AtomicUsize::new(0);
        let results = std::thread::scope(|scope| {
            let mut jobs = Vec::new();
            for _ in 0..4 {
                jobs.push(scope.spawn(|| {
                    barrier.wait();
                    let result = executor.solve(&request, &AtomicBool::new(false));
                    completed.fetch_add(1, Ordering::Release);
                    result
                }));
            }
            barrier.wait();
            let start = Instant::now();
            while completed.load(Ordering::Acquire) < 4 {
                let status = executor.status();
                assert!(status.reserved_threads <= budget.total_threads);
                assert!(status.running_jobs <= budget.total_threads as usize / 2);
                assert!(start.elapsed() < Duration::from_secs(10));
                std::thread::sleep(Duration::from_millis(1));
            }
            jobs.into_iter()
                .map(|j| j.join().unwrap().unwrap())
                .collect::<Vec<_>>()
        });
        for result in results {
            let stats = result.stats.unwrap();
            assert_eq!(stats.threads, 2);
            assert_eq!(stats.termination_reason, "time_limit");
            assert!(stats.elapsed_ms >= 50);
            assert!(stats.evaluated_moves > 0);
            let compiled = compile(&pb::SolveRequest {
                initial_solution: result.solution,
                ..request.clone()
            })
            .unwrap();
            let expected =
                oracle::evaluate(&compiled.problem, compiled.initial.as_ref().unwrap()).score;
            let score = result.score.unwrap();
            assert_eq!(
                (score.hard, score.medium, score.soft),
                (expected.hard, expected.medium, expected.soft)
            );
        }
        assert_eq!(executor.status().reserved_threads, 0);
        assert_eq!(executor.status().running_jobs, 0);
        assert_eq!(executor.status().queued_jobs, 0);
    }
}
