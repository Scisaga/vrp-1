use vrp_model::{ThreadBudget, compile, resolve_options};
use vrp_proto::engine::v1 as pb;

#[test]
fn search_strategy_and_variable_depth_are_validated() {
    let r = resolve_options(Some(&pb::SolveOptions {
        search_strategy: pb::SearchStrategy::AlphaKOpt as i32,
        k_opt_depth: 7,
        ..Default::default()
    }))
    .unwrap();
    assert_eq!(r.search_strategy, pb::SearchStrategy::AlphaKOpt);
    assert_eq!(r.k_opt_depth, 7);
    assert_eq!(resolve_options(None).unwrap().k_opt_depth, 5);
    for k_opt_depth in [1, 13, u32::MAX] {
        assert!(
            resolve_options(Some(&pb::SolveOptions {
                k_opt_depth,
                ..Default::default()
            }))
            .is_err()
        );
    }
    assert!(
        resolve_options(Some(&pb::SolveOptions {
            search_strategy: 99,
            ..Default::default()
        }))
        .is_err()
    );
}

#[test]
fn presets_distinguish_total_time_from_unimproved_time() {
    let defaults = resolve_options(None).unwrap();
    assert_eq!(defaults.preset, pb::SolvePreset::Fast);
    assert_eq!(defaults.time_limit_ms, Some(30_000));
    assert_eq!(defaults.unimproved_stop_ms, None);
    assert_eq!(defaults.threads, 1);
    for (preset, total, unimproved) in [
        (pb::SolvePreset::Fast, Some(30_000), None),
        (pb::SolvePreset::Standard, Some(120_000), None),
        (pb::SolvePreset::Deep, None, Some(300_000)),
    ] {
        let actual = resolve_options(Some(&pb::SolveOptions {
            preset: preset as i32,
            ..Default::default()
        }))
        .unwrap();
        assert_eq!(actual.time_limit_ms, total);
        assert_eq!(actual.unimproved_stop_ms, unimproved);
    }
}

#[test]
fn overrides_and_multiple_threads_survive_resolution() {
    let actual = resolve_options(Some(&pb::SolveOptions {
        preset: pb::SolvePreset::Deep as i32,
        time_limit_ms: 900_000,
        threads: 4,
        seed: 37,
        ..Default::default()
    }))
    .unwrap();
    assert_eq!(actual.time_limit_ms, Some(900_000));
    assert_eq!(actual.unimproved_stop_ms, Some(300_000));
    assert_eq!(actual.threads, 4);
    assert_eq!(actual.seed, 37);

    let actual = resolve_options(Some(&pb::SolveOptions {
        time_limit_ms: 50_000,
        unimproved_stop_ms: 10_000,
        ..Default::default()
    }))
    .unwrap();
    assert_eq!(actual.time_limit_ms, Some(50_000));
    assert_eq!(actual.unimproved_stop_ms, Some(10_000));
}

#[test]
fn unknown_preset_is_rejected_before_problem_compilation() {
    let request = pb::SolveRequest {
        options: Some(pb::SolveOptions {
            preset: 99,
            ..Default::default()
        }),
        ..Default::default()
    };
    assert!(compile(&request).unwrap_err().0.contains("options.preset"));
}

#[test]
fn machine_budget_is_shared_capacity_not_per_task_parallelism() {
    let budget = ThreadBudget::from_cpu_count(8, 1.8).unwrap();
    assert_eq!(budget.total_threads, 14);
    assert_eq!(
        ThreadBudget::from_cpu_count(8, 1.0).unwrap().total_threads,
        8
    );
    assert_eq!(
        ThreadBudget::from_cpu_count(1, 0.5).unwrap().total_threads,
        1
    );
    let mut request = resolve_options(Some(&pb::SolveOptions {
        threads: 4,
        ..Default::default()
    }))
    .unwrap();
    assert!(budget.validate_request(&request).is_ok());
    request.threads = 15;
    assert!(budget.validate_request(&request).is_err());
    for multiplier in [0.0, -1.0, f64::NAN, f64::INFINITY, 1e30] {
        assert!(ThreadBudget::from_cpu_count(8, multiplier).is_err());
    }
    assert!(ThreadBudget::from_cpu_count(0, 1.0).is_err());
}
