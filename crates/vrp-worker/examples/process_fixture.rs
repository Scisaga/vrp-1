//! Deterministic local child used by isolation integration tests.
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};
use vrp_proto::worker::v1::{ChildFailure, GeoRunRequest, GeoRunResponse};

fn main() {
    vrp_worker::serve_process_child::<GeoRunRequest, GeoRunResponse>(
        4 * 1024 * 1024,
        |request, cancelled, progress| {
            if request.output_path == "panic" {
                panic!("fixture panic");
            }
            let started = Instant::now();
            while started.elapsed() < Duration::from_millis(request.threads as u64) {
                progress(
                    "working",
                    (started.elapsed().as_millis() as u64).min(request.threads as u64),
                    request.threads as u64,
                );
                if cancelled.load(Ordering::Acquire) && request.output_path != "ignore-cancel" {
                    return Err(ChildFailure {
                        code: "cancelled".into(),
                        message: "fixture cancelled".into(),
                    });
                }
                std::thread::sleep(Duration::from_millis(2));
            }
            Ok(GeoRunResponse {
                output_path: request.output_path,
                model_edges: 7,
                ..Default::default()
            })
        },
    )
    .unwrap();
}
