//! 可重复生成的合成场景，同预算比较基础搜索和候选搜索，不代表生产数据表现。
//! cargo run --release -p vrp-core --example search_bench -- [单数=100] [车数=20] [毫秒=1000] [线程=1] [种子数=3]
use std::sync::atomic::AtomicBool;
use std::time::Duration;
use vrp_core::rng::Rng;
use vrp_core::{
    AgentSpec, Matrix, Problem, ProblemSpec, SearchOptions, SearchStrategy, TicketSpec, Weights,
    oracle, solve,
};

fn main() {
    let args: Vec<usize> = std::env::args()
        .skip(1)
        .map(|v| v.parse().expect("expected positive integer"))
        .collect();
    let arg = |i, default| args.get(i).copied().unwrap_or(default);
    let (n, vehicles, millis, threads, seeds) =
        (arg(0, 100), arg(1, 20), arg(2, 1000), arg(3, 1), arg(4, 3));
    assert!((1..=1000).contains(&n) && (1..=200).contains(&vehicles));
    assert!(millis > 0 && threads > 0 && seeds > 0);
    let mut rng = Rng::new(741);
    // 一个公共起终点，加 n 个服务地点；不引入超出需求的大场景。
    let points: Vec<(i32, i32)> = (0..=n)
        .map(|_| (rng.below(10000) as i32, rng.below(10000) as i32))
        .collect();
    let distance: Vec<u32> = points
        .iter()
        .flat_map(|&(x, y)| {
            points
                .iter()
                .map(move |&(a, b)| x.abs_diff(a) + y.abs_diff(b))
        })
        .collect();
    let duration = distance.iter().map(|d| d.div_ceil(10)).collect();
    let p = Problem::new(ProblemSpec {
        matrices: vec![Matrix {
            n: n + 1,
            duration,
            distance,
            buffer_permille: vec![],
        }],
        agents: (0..vehicles)
            .map(|i| AgentSpec {
                id: format!("a{i}"),
                ..AgentSpec::default()
            })
            .collect(),
        tickets: (0..n)
            .map(|i| TicketSpec {
                id: format!("t{i}"),
                location: (i + 1) as u32,
                duration: 300,
                ..TicketSpec::default()
            })
            .collect(),
        weights: Weights::default(),
        ..ProblemSpec::default()
    })
    .unwrap();
    println!(
        "strategy,tickets,agents,threads,seed,elapsed_ms,moves,hard,medium,soft,unassigned,alpha_profiles,ascent_iterations,preprocessing_ms,k_opt_improvements,patching_attempts,patched_improvements"
    );
    for seed in 0..seeds as u64 {
        // 交替先后顺序，减小固定执行顺序的偏差。
        let mut strategies = [
            SearchStrategy::Baseline,
            SearchStrategy::Candidate,
            SearchStrategy::Alpha,
            SearchStrategy::AlphaKOpt,
        ];
        strategies.rotate_left(seed as usize % 4);
        for strategy in strategies {
            let r = solve(
                &p,
                None,
                &SearchOptions {
                    strategy,
                    threads,
                    seed,
                    time_limit: Some(Duration::from_millis(millis as u64)),
                    ..SearchOptions::default()
                },
                &AtomicBool::new(false),
            )
            .unwrap();
            r.solution.validate(&p).unwrap();
            assert_eq!(r.score, oracle::evaluate(&p, &r.solution).score);
            println!(
                "{strategy:?},{n},{vehicles},{},{seed},{},{},{},{},{},{},{},{},{},{},{},{}",
                r.threads,
                r.elapsed.as_millis(),
                r.evaluated_moves,
                r.score.hard,
                r.score.medium,
                r.score.soft,
                n - r.solution.assigned_count(),
                r.alpha_profiles,
                r.ascent_iterations,
                r.preprocessing.as_millis(),
                r.k_opt_improvements,
                r.patching_attempts,
                r.patched_improvements
            );
        }
    }
}
