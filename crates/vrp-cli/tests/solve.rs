use prost::Message;
use std::path::PathBuf;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};
use std::{fs, thread};
use vrp_core::oracle;
use vrp_proto::engine::v1 as pb;

struct TestDir(PathBuf);
impl TestDir {
    fn new() -> Self {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../corpus")
            .canonicalize()
            .unwrap();
        let name = format!(
            "test-{}-{}-{}",
            std::process::id(),
            thread::current().name().unwrap_or("solve"),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let dir = root.join(name);
        fs::create_dir(&dir).unwrap();
        Self(dir)
    }
}
impl Drop for TestDir {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

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
            agents: vec![pb::Agent {
                id: "a".into(),
                ..Default::default()
            }],
            tickets: vec![pb::Ticket {
                id: "fixed".into(),
                pinned: true,
                original_agent: Some(0),
                latest_start: 86400,
                latest_end: 86400,
                ..Default::default()
            }],
            ..Default::default()
        }),
        options: Some(pb::SolveOptions {
            time_limit_ms: 1,
            seed: 42,
            ..Default::default()
        }),
        initial_solution: None,
    }
}

#[test]
fn solve_roundtrips_protobuf_and_preset_resets_input_deadlines() {
    for command in ["solve", "solve-isolated"] {
        let dir = TestDir::new();
        let input = dir.0.join("request.pb");
        let output = dir.0.join("result.pb");
        let request = request();
        fs::write(&input, request.encode_to_vec()).unwrap();
        let run = Command::new(env!("CARGO_BIN_EXE_vrp"))
            .arg(command)
            .arg(&input)
            .arg(&output)
            .args([
                "--preset",
                "deep",
                "--unimproved-ms",
                "30",
                "--threads",
                "2",
                "--search",
                "alpha-kopt",
                "--k-opt-depth",
                "3",
                "--cpu-multiplier",
                "2",
            ])
            .output()
            .unwrap();
        assert!(
            run.status.success(),
            "{}",
            String::from_utf8_lossy(&run.stderr)
        );
        let result = pb::SolveResult::decode(fs::read(&output).unwrap().as_slice()).unwrap();
        let stats = result.stats.unwrap();
        assert_eq!(stats.termination_reason, "unimproved");
        assert_eq!(stats.threads, 2);
        assert_eq!(stats.seed, 42);
        assert_eq!(stats.best_found_ms, 0);
        assert!(stats.elapsed_ms >= 30);
        assert!(result.unassigned.is_empty());
        let checked = vrp_model::compile(&pb::SolveRequest {
            initial_solution: result.solution,
            ..request
        })
        .unwrap();
        let actual = oracle::evaluate(&checked.problem, checked.initial.as_ref().unwrap()).score;
        let reported = result.score.unwrap();
        assert_eq!(
            (reported.hard, reported.medium, reported.soft),
            (actual.hard, actual.medium, actual.soft)
        );
    }
}

#[test]
fn impossible_thread_budget_is_rejected_without_creating_a_result() {
    let dir = TestDir::new();
    let input = dir.0.join("request.pb");
    let output = dir.0.join("result.pb");
    fs::write(&input, request().encode_to_vec()).unwrap();
    let run = Command::new(env!("CARGO_BIN_EXE_vrp"))
        .arg("solve")
        .arg(&input)
        .arg(&output)
        .args(["--threads", "4294967295", "--cpu-multiplier", "1"])
        .output()
        .unwrap();
    assert!(!run.status.success());
    assert!(String::from_utf8_lossy(&run.stderr).contains("exceeds total thread budget"));
    assert!(!output.exists());
}

#[test]
fn an_existing_input_or_result_cannot_be_overwritten_through_an_alias() {
    let dir = TestDir::new();
    let input = dir.0.join("request.pb");
    let original = request().encode_to_vec();
    fs::write(&input, &original).unwrap();
    let run = Command::new(env!("CARGO_BIN_EXE_vrp"))
        .arg("solve")
        .arg(&input)
        .arg(dir.0.join(".").join("request.pb"))
        .output()
        .unwrap();
    assert!(!run.status.success());
    assert_eq!(fs::read(&input).unwrap(), original);
}
