//! All map fixtures and HTTP sources are local; no public tile requests.
use axum::{
    Router,
    body::{Body, to_bytes},
    extract::ConnectInfo,
    http::{Request, StatusCode},
};
use prost::Message;
use std::{
    fs::{self, File},
    path::PathBuf,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tower::ServiceExt;
use vrp_basemap::pmtiles;
use vrp_model::ThreadBudget;
use vrp_proto::{app::v1 as app, engine::v1 as engine, maps::v1 as maps};
use vrp_server::{Config, Runtime, Service};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        static SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../corpus")
            .join(format!(
                "basemap-test-{}-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_nanos(),
                SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            ));
        fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
    fn start(&self) -> Runtime {
        self.start_with_disk(10 * 1024 * 1024 * 1024)
    }
    fn start_with_disk(&self, disk: u64) -> Runtime {
        let mut config = Config {
            data_dir: self.0.join("data"),
            budget: ThreadBudget { total_threads: 2 },
            max_running: 2,
            memory_budget_bytes: 1024 * 1024 * 1024,
            host_reserve_bytes: 64 * 1024 * 1024,
            ..Default::default()
        };
        config.isolation.memory_limit_bytes = Some(128 * 1024 * 1024);
        config.geo.auto_prefetch = false;
        config.geo.catalog_url.clear();
        config.basemap.catalog_url = "http://127.0.0.1:1/no-network".into();
        config.basemap.disk_budget_bytes = disk;
        Runtime::open(
            config,
            env!("CARGO_BIN_EXE_vrp-server").into(),
            vec!["worker-child".into(), "67108864".into()],
        )
        .unwrap()
    }
    fn package(&self, name: &str, kind: pmtiles::TileType) -> PathBuf {
        let p = self.0.join(name);
        let metadata = serde_json::json!({"vector_layers":[{"id":"earth"},{"id":"water"},{"id":"roads"},{"id":"places"}]});
        let mut w = pmtiles::PmTilesWriter::new(kind)
            .precompressed(pmtiles::Compression::None)
            .bounds(-180., -85., 180., 85.)
            .metadata(metadata.to_string())
            .create(File::create(&p).unwrap())
            .unwrap();
        for z in 0..=14 {
            w.add_tile(pmtiles::TileCoord::new(z, 0, 0).unwrap(), &[z, 1, 2, 3])
                .unwrap();
        }
        w.finalize().unwrap();
        p
    }
}

#[test]
fn same_content_import_at_full_disk_reuses_the_verified_file() {
    let f = Fixture::new();
    let first = f.package("one.pmtiles", pmtiles::TileType::Mvt);
    let second = f.0.join("two.pmtiles");
    fs::copy(&first, &second).unwrap();
    let limit = first.metadata().unwrap().len();
    let runtime = f.start_with_disk(limit);
    let a = import(&runtime.service, first);
    let b = import(&runtime.service, second);
    assert_ne!(a.id, b.id);
    assert_eq!(a.basemap_package_id, b.basemap_package_id);
    let page = runtime
        .service
        .list_basemaps(maps::ListBasemapsRequest::default())
        .unwrap();
    assert_eq!(page.used_bytes, limit);
    assert_eq!(page.packages.len(), 1);
    runtime.shutdown();
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}
fn wait(s: &Service, id: &str) -> app::Task {
    let start = Instant::now();
    loop {
        let task = s.get_task(id).unwrap();
        if matches!(
            task.state(),
            app::TaskState::Succeeded
                | app::TaskState::Failed
                | app::TaskState::Cancelled
                | app::TaskState::Interrupted
        ) {
            return task;
        }
        assert!(start.elapsed() < Duration::from_secs(30), "{task:?}");
        std::thread::sleep(Duration::from_millis(10));
    }
}
fn save(s: &Service) -> app::Scenario {
    s.save_scenario(app::SaveScenarioRequest {
        scenario_id: "offline-scene".into(),
        name: "offline-scene".into(),
        request: Some(engine::SolveRequest {
            problem: Some(engine::Problem {
                locations: vec![engine::Location {
                    id: "location".into(),
                    lng: 10.,
                    lat: 10.,
                }],
                ..Default::default()
            }),
            ..Default::default()
        }),
        ..Default::default()
    })
    .unwrap()
}
fn import(s: &Service, path: PathBuf) -> app::Task {
    let task = s
        .import_basemap(maps::ImportBasemapRequest {
            path: path.to_string_lossy().into(),
            name: "Synthetic basemap".into(),
        })
        .unwrap();
    let result = wait(s, &task.id);
    assert_eq!(result.state(), app::TaskState::Succeeded, "{result:?}");
    result
}
fn context() -> maps::BasemapContext {
    maps::BasemapContext {
        scenario_id: "offline-scene".into(),
        version: 1,
        ..Default::default()
    }
}
async fn call(
    router: &Router,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    local: bool,
) -> axum::response::Response {
    let mut r = Request::builder()
        .method(method)
        .uri(path)
        .header("host", if local { "127.0.0.1" } else { "workbench.lan" })
        .extension(ConnectInfo(
            if local {
                "127.0.0.1:4000"
            } else {
                "192.168.1.20:4000"
            }
            .parse::<std::net::SocketAddr>()
            .unwrap(),
        ));
    for (k, v) in headers {
        r = r.header(*k, *v);
    }
    router
        .clone()
        .oneshot(r.body(Body::empty()).unwrap())
        .await
        .unwrap()
}

#[test]
fn imports_are_offline_deduplicated_and_selections_do_not_change_business_versions() {
    let f = Fixture::new();
    let runtime = f.start();
    let s = &runtime.service;
    let scene = save(s);
    let original = s
        .get_scenario(&scene.id, 1)
        .unwrap()
        .request
        .unwrap()
        .encode_to_vec();
    let selection = s.select_basemap(context()).unwrap();
    assert_eq!(selection.status, "missing");
    assert!(selection.package.is_none());
    assert!(
        s.list_tasks(app::ListTasksRequest::default())
            .unwrap()
            .tasks
            .is_empty()
    );
    let path = f.package("first.pmtiles", pmtiles::TileType::Mvt);
    let first = import(s, path.clone());
    let second = import(s, path);
    assert_eq!(first.basemap_package_id, second.basemap_package_id);
    let selected = s.select_basemap(context()).unwrap();
    assert_eq!(selected.status, "ready");
    assert!(!selected.selected);
    assert_eq!(
        s.get_basemap(&first.basemap_package_id)
            .unwrap()
            .reference_count,
        0
    );
    let selected = s
        .set_basemap_selection(maps::SetBasemapSelectionRequest {
            context: Some(context()),
            package_id: first.basemap_package_id.clone(),
            expected_package_id: String::new(),
        })
        .unwrap();
    assert!(selected.selected);
    assert_eq!(
        s.get_basemap(&first.basemap_package_id)
            .unwrap()
            .reference_count,
        1
    );
    assert_eq!(
        s.delete_basemap(&first.basemap_package_id)
            .unwrap_err()
            .code,
        "conflict"
    );
    assert_eq!(
        s.prepare_basemap(maps::PrepareBasemapRequest {
            context: Some(context()),
            automatic: true,
            native_max_zoom: 14,
            ..Default::default()
        })
        .unwrap()
        .basemap_package_id,
        first.basemap_package_id
    );
    assert!(
        s.prepare_basemap(maps::PrepareBasemapRequest {
            context: Some(context()),
            automatic: true,
            native_max_zoom: 15,
            ..Default::default()
        })
        .is_err()
    );
    let p = s.get_basemap(&first.basemap_package_id).unwrap();
    assert!(
        s.enable_basemap(maps::EnableBasemapRequest {
            package_id: p.id.clone(),
            expected_version: 99,
            enabled: false
        })
        .is_err()
    );
    s.enable_basemap(maps::EnableBasemapRequest {
        package_id: p.id.clone(),
        expected_version: p.version,
        enabled: false,
    })
    .unwrap();
    let off = s.select_basemap(context()).unwrap();
    assert!(off.package.is_none());
    assert_eq!(off.bound_package_id, p.id);
    s.enable_basemap(maps::EnableBasemapRequest {
        package_id: p.id.clone(),
        expected_version: p.version + 1,
        enabled: true,
    })
    .unwrap();
    assert!(s.select_basemap(context()).unwrap().selected);
    assert_eq!(
        original,
        s.get_scenario(&scene.id, 1)
            .unwrap()
            .request
            .unwrap()
            .encode_to_vec()
    );
    let imported = import(s, f.package("raster.pmtiles", pmtiles::TileType::Png));
    s.delete_basemap(&imported.basemap_package_id).unwrap();
    assert!(s.get_basemap(&imported.basemap_package_id).is_err());
    runtime.shutdown();
    let db = rusqlite::Connection::open(f.0.join("data/tasks.sqlite")).unwrap();
    db.execute_batch(
        "DROP TABLE basemap_refs; DROP TABLE basemap_packages; PRAGMA user_version=5;",
    )
    .unwrap();
    drop(db);
    let migrated = f.start();
    assert_eq!(
        original,
        migrated
            .service
            .get_scenario(&scene.id, 1)
            .unwrap()
            .request
            .unwrap()
            .encode_to_vec()
    );
    assert!(
        migrated
            .service
            .list_basemaps(maps::ListBasemapsRequest::default())
            .unwrap()
            .packages
            .is_empty()
    );
    migrated.shutdown();
    let db = rusqlite::Connection::open(f.0.join("data/tasks.sqlite")).unwrap();
    assert_eq!(
        db.pragma_query_value(None, "user_version", |r| r.get::<_, u32>(0))
            .unwrap(),
        6
    );
}

#[tokio::test]
async fn resources_have_ranges_private_cache_local_auth_and_a_separate_admission_pool() {
    let f = Fixture::new();
    let runtime = f.start();
    let imported = import(
        &runtime.service,
        f.package("map.pmtiles", pmtiles::TileType::Mvt),
    );
    let path = format!(
        "/v1/basemaps/{}/archive.pmtiles",
        imported.basemap_package_id
    );
    let router = vrp_server::http::router(runtime.service.clone(), Some("test-token".into()));
    let local_only = vrp_server::http::router(runtime.service.clone(), None);
    assert_eq!(
        call(&local_only, "GET", &path, &[], false).await.status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        call(
            &local_only,
            "GET",
            &path,
            &[("forwarded", "for=192.168.1.20")],
            true
        )
        .await
        .status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        call(&router, "GET", &path, &[], false).await.status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        call(
            &router,
            "GET",
            &path,
            &[("authorization", "Bearer wrong")],
            false
        )
        .await
        .status(),
        StatusCode::UNAUTHORIZED
    );
    let r = call(&router, "GET", &path, &[("range", "bytes=0-126")], true).await;
    assert_eq!(r.status(), StatusCode::PARTIAL_CONTENT);
    assert!(
        r.headers()["cache-control"]
            .to_str()
            .unwrap()
            .contains("private")
    );
    let etag = r.headers()["etag"].to_str().unwrap().to_string();
    assert_eq!(to_bytes(r.into_body(), 1024).await.unwrap().len(), 127);
    let head = call(
        &router,
        "HEAD",
        &path,
        &[
            ("range", "bytes=2-15"),
            ("authorization", "Bearer test-token"),
        ],
        false,
    )
    .await;
    assert_eq!(head.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(head.headers()["content-length"], "14");
    assert!(to_bytes(head.into_body(), 16).await.unwrap().is_empty());
    assert_eq!(
        call(&router, "GET", &path, &[("range", "bytes=9999999-")], true)
            .await
            .status(),
        StatusCode::RANGE_NOT_SATISFIABLE
    );
    assert_eq!(
        call(&router, "GET", &path, &[("range", "bytes=0-5,10-20")], true)
            .await
            .status(),
        StatusCode::RANGE_NOT_SATISFIABLE
    );
    assert_eq!(
        call(&router, "GET", &path, &[("if-none-match", &etag)], true)
            .await
            .status(),
        StatusCode::NOT_MODIFIED
    );
    let asset = format!(
        "/v1/basemap-assets/{}/fonts/Noto%20Sans%20Regular/19968-20223.pbf",
        vrp_basemap::ASSETS_VERSION
    );
    assert_eq!(
        call(&router, "GET", &asset, &[], false).await.status(),
        StatusCode::UNAUTHORIZED
    );
    let font = call(
        &router,
        "GET",
        &asset,
        &[("authorization", "Bearer test-token")],
        false,
    )
    .await;
    assert_eq!(font.status(), StatusCode::OK);
    assert!(
        !to_bytes(font.into_body(), 1024 * 1024)
            .await
            .unwrap()
            .is_empty()
    );
    let style = path.replace("archive.pmtiles", "style-light.json");
    let r = call(
        &router,
        "GET",
        &style,
        &[("authorization", "Bearer test-token")],
        false,
    )
    .await;
    let bytes = to_bytes(r.into_body(), 1024 * 1024).await.unwrap();
    let text = std::str::from_utf8(&bytes).unwrap();
    assert!(text.contains("/v1/basemap-assets/"));
    assert!(!text.contains("https://"));
    assert!(!text.contains("test-token"));
    assert_eq!(
        call(
            &router,
            "GET",
            &path.replace("archive.pmtiles", "tasks.sqlite"),
            &[],
            true
        )
        .await
        .status(),
        StatusCode::NOT_FOUND
    );
    let mut held = Vec::new();
    for _ in 0..8 {
        let r = call(&router, "GET", &path, &[], true).await;
        assert_eq!(r.status(), StatusCode::OK);
        held.push(r);
    }
    assert_eq!(
        call(&router, "GET", &path, &[], true).await.status(),
        StatusCode::TOO_MANY_REQUESTS
    );
    let business = call(&router, "GET", "/v1/basemap-packages", &[], true).await;
    assert_eq!(business.status(), StatusCode::OK);
    assert_eq!(business.headers()["cache-control"], "no-store");
    drop(business);
    drop(held);
    runtime.shutdown();
}
