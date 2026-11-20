use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

struct TestDir(PathBuf);
impl TestDir {
    fn new() -> Self {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../corpus")
            .join(format!(
                "catalog-test-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
        fs::create_dir(&dir).unwrap();
        Self(dir)
    }

    fn bench(&self) -> std::process::Output {
        Command::new(env!("CARGO_BIN_EXE_vrp"))
            .args(["bench", "--corpus"])
            .arg(&self.0)
            .args(["--moves", "4"])
            .output()
            .unwrap()
    }
}
impl Drop for TestDir {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

#[test]
fn catalogue_selects_requests_without_loading_archived_results() {
    let dir = TestDir::new();
    fs::write(
        dir.0.join("request.json"),
        r#"{
        "planning_date":"2026-10-31", "plan": {
            "pois":[{"id":"p","location":"121,30"}],
            "agents":[{"id":"a","date":"2026-10-31","start_loc":"p"}],
            "tickets":[]
        }
    }"#,
    )
    .unwrap();
    fs::write(dir.0.join("broken-historical-result.json"), "broken").unwrap();
    fs::write(
        dir.0.join("bench.txt"),
        "# requests only\nrequest.json\nrequest.json\n",
    )
    .unwrap();
    let output = dir.bench();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("共 1 个 JSON：评测 1"));
    assert!(!stdout.contains("broken-historical-result"));
}

#[test]
fn failed_imports_and_empty_selections_fail_the_benchmark() {
    let dir = TestDir::new();
    fs::write(dir.0.join("bad.json"), "not JSON").unwrap();
    assert!(!dir.bench().status.success());
    fs::write(dir.0.join("bench.txt"), "bad.json\n").unwrap();
    assert!(!dir.bench().status.success());
    fs::write(dir.0.join("bench.txt"), "# nothing selected\n").unwrap();
    assert!(!dir.bench().status.success());
    fs::write(dir.0.join("bad.json"), "{}").unwrap();
    fs::write(dir.0.join("bench.txt"), "bad.json\n").unwrap();
    assert!(!dir.bench().status.success());
}

#[test]
fn catalogue_cannot_read_outside_its_root() {
    let dir = TestDir::new();
    let nested = dir.0.join("nested");
    fs::create_dir(&nested).unwrap();
    fs::write(dir.0.join("outside.json"), "{}").unwrap();
    fs::write(nested.join("bench.txt"), "../outside.json\n").unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_vrp"))
        .args(["bench", "--corpus"])
        .arg(nested)
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("不在语料目录内"));
}
