use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;
use vrp_model::ThreadBudget;
use vrp_proto::app::v1::ScenarioRef;
use vrp_proto::engine::v1::{SearchStrategy, SolveOptions};
use vrp_server::{Config, Runtime};

fn main() {
    if let Err(error) = run() {
        eprintln!("{error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let first = args.next();
    if first.as_deref() == Some("worker-child") {
        let limit = args.next().ok_or("missing frame limit")?.parse()?;
        vrp_worker::serve_child(limit)?;
        return Ok(());
    }
    if first.as_deref() == Some("geo-child") {
        let limit = args.next().ok_or("missing frame limit")?.parse()?;
        vrp_server::serve_geo_child(limit)?;
        return Ok(());
    }
    if first.as_deref() == Some("basemap-child") {
        let limit = args.next().ok_or("missing frame limit")?.parse()?;
        vrp_server::serve_basemap_child(limit)?;
        return Ok(());
    }
    let mut config = Config::default();
    let mut bind: SocketAddr = "127.0.0.1:8080".parse()?;
    let mut multiplier = std::env::var("VRP_CPU_MULTIPLIER")
        .unwrap_or_else(|_| "1".into())
        .parse::<f64>()?;
    let mut explicit_threads = None;
    let mut mcp_origin = None;
    let mut mcp_threads = None;
    let mut mcp_default_scenario = None;
    let mut mcp_options = SolveOptions {
        time_limit_ms: 30_000,
        seed: 42,
        search_strategy: SearchStrategy::Baseline as i32,
        ..Default::default()
    };
    let mut args = first.into_iter().chain(args);
    while let Some(flag) = args.next() {
        if flag == "--help" || flag == "-h" {
            println!(
                "vrp-server [--bind 127.0.0.1:8080] [--data-dir var] [--cpu-multiplier 1] [--total-threads N]\n  [--mcp-origin http://127.0.0.1:8080] [--mcp-threads 4] [--mcp-search-ms 30000]\n  [--mcp-search-strategy baseline|nearest] [--mcp-default-scenario ID@VERSION]\n  [--map-config <ProtoJSON file>] [--max-running 4] [--max-queued 128] [--worker-memory-mib 1024]\n  [--memory-budget-mib 4096] [--max-storage-mib 4096] [--max-records 10000]\n  [--basemap-auto true] [--basemap-download-mib 500] [--basemap-storage-mib 10240]\n  [--basemap-catalog-url <builds.json or archive.pmtiles>] [--basemap-archive-base-url <URL>]\nSet VRP_API_TOKEN for Bearer authentication (required for non-loopback bind).\nBundled workbench: / ; API: /v1/scenarios, /v1/jobs, /v1/operations, /v1/basemap-packages, /healthz."
            );
            return Ok(());
        }
        let value = args
            .next()
            .ok_or_else(|| format!("missing value for {flag}"))?;
        let mib = |v: &str| -> Result<u64, Box<dyn std::error::Error>> {
            Ok(v.parse::<u64>()?
                .checked_mul(1024 * 1024)
                .ok_or("MiB value overflows")?)
        };
        match flag.as_str() {
            "--bind" => bind = value.parse()?,
            "--mcp-origin" => mcp_origin = Some(value),
            "--mcp-threads" => mcp_threads = Some(value.parse::<u32>()?),
            "--mcp-search-ms" => mcp_options.time_limit_ms = value.parse()?,
            "--mcp-default-scenario" => {
                let (id, version) = value
                    .rsplit_once('@')
                    .ok_or("--mcp-default-scenario expects ID@VERSION")?;
                let version = version.parse::<u64>()?;
                if id.is_empty() || version == 0 {
                    return Err("--mcp-default-scenario requires an ID and positive version".into());
                }
                mcp_default_scenario = Some(ScenarioRef {
                    scenario_id: id.into(),
                    version,
                });
            }
            "--mcp-search-strategy" => {
                mcp_options.search_strategy = match value.as_str() {
                    "baseline" => SearchStrategy::Baseline,
                    "nearest" => SearchStrategy::Nearest,
                    _ => return Err("--mcp-search-strategy expects baseline or nearest".into()),
                } as i32;
            }
            "--data-dir" => config.data_dir = PathBuf::from(value),
            "--map-config" => config.map_config = serde_json::from_slice(&std::fs::read(value)?)?,
            "--cpu-multiplier" => multiplier = value.parse()?,
            "--total-threads" => explicit_threads = Some(value.parse::<u32>()?),
            "--max-running" => config.max_running = value.parse()?,
            "--max-queued" => config.max_queued = value.parse()?,
            "--worker-memory-mib" => config.isolation.memory_limit_bytes = Some(mib(&value)?),
            "--memory-budget-mib" => config.memory_budget_bytes = mib(&value)?,
            "--max-storage-mib" => config.max_payload_bytes = mib(&value)?,
            "--max-records" => config.max_records = value.parse()?,
            "--geo-prefetch" => config.geo.auto_prefetch = value.parse()?,
            "--basemap-auto" => config.basemap.automatic = value.parse()?,
            "--basemap-catalog-url" => config.basemap.catalog_url = value,
            "--basemap-archive-base-url" => config.basemap.archive_base_url = value,
            "--basemap-download-mib" => config.basemap.download_budget_bytes = mib(&value)?,
            "--basemap-storage-mib" => config.basemap.disk_budget_bytes = mib(&value)?,
            "--geo-catalog-url" => config.geo.catalog_url = value,
            "--geo-local-dir" => config.geo.local_import_dir = Some(PathBuf::from(value)),
            "--geo-download-mib" => config.geo.download_budget_bytes = mib(&value)?,
            "--geo-memory-mib" => config.geo.worker_memory_bytes = mib(&value)?,
            "--geo-road-wait-ms" => config.geo.road_wait = Duration::from_millis(value.parse()?),
            _ => return Err(format!("unknown option {flag}").into()),
        }
    }
    config.budget = match explicit_threads {
        Some(total_threads) => ThreadBudget { total_threads },
        None => ThreadBudget::from_cpu_count(
            std::thread::available_parallelism()?.get() as u32,
            multiplier,
        )?,
    };
    mcp_options.threads = mcp_threads.unwrap_or(config.budget.total_threads.min(4));
    if mcp_origin.is_some() {
        if mcp_options.threads == 0 || mcp_options.threads > config.budget.total_threads {
            return Err("--mcp-threads must be within the server's total thread budget".into());
        }
        if mcp_options.time_limit_ms == 0 {
            return Err("--mcp-search-ms must be positive".into());
        }
    }
    let token = std::env::var("VRP_API_TOKEN")
        .ok()
        .filter(|s| !s.is_empty());
    if token
        .as_ref()
        .is_some_and(|s| s.len() < 16 || !s.bytes().all(|b| b.is_ascii_graphic()))
    {
        return Err("VRP_API_TOKEN must be at least 16 visible ASCII characters".into());
    }
    if !bind.ip().is_loopback() && token.is_none() {
        return Err("a non-loopback bind requires VRP_API_TOKEN".into());
    }
    tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build()?.block_on(async move {
        let listener = tokio::net::TcpListener::bind(bind).await?;
        let address = listener.local_addr()?;
        let limit = config.isolation.max_frame_bytes;
        let runtime = Runtime::open(config, std::env::current_exe()?, vec!["worker-child".into(), limit.to_string().into()])?;
        let service = runtime.service.clone();
        let app = match mcp_origin {
            Some(origin) => vrp_server::http::router_with_mcp(service.clone(), token, &origin, mcp_options, mcp_default_scenario)?,
            None => vrp_server::http::router(service.clone(), token),
        };
        eprintln!("vrp-server listening on http://{address}");
        let stop = service.clone();
        let shutdown = async move {
            #[cfg(unix)]
            {
                let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).expect("SIGTERM handler");
                tokio::select! { _ = tokio::signal::ctrl_c() => {}, _ = term.recv() => {} }
            }
            #[cfg(not(unix))]
            { let _ = tokio::signal::ctrl_c().await; }
            stop.stop();
        };
        let server = axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>()).with_graceful_shutdown(shutdown);
        // SSE observes stop and terminates. Bound HTTP draining independently of
        // worker cleanup; Runtime::shutdown always joins the supervisors.
        let served = async { server.await };
        tokio::pin!(served);
        let status = tokio::select! {
            result = &mut served => result,
            _ = async { while !service.is_stopping() { tokio::time::sleep(Duration::from_millis(100)).await; } } => {
                tokio::time::timeout(Duration::from_secs(15), &mut served).await.unwrap_or(Ok(()))
            }
        };
        service.stop();
        tokio::task::spawn_blocking(move || runtime.shutdown()).await?;
        status?;
        Ok::<(), Box<dyn std::error::Error>>(())
    })
}
