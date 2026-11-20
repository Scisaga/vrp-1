//! HTTP is a transport over generated protobuf types, including ProtoJSON.
use crate::{AppError, Result, Service, terminal};
use axum::Router;
use axum::body::{Body, to_bytes};
use axum::extract::{ConnectInfo, Path, Query, Request, State};
use axum::http::{HeaderMap, StatusCode, Uri, header};
use axum::middleware::{self, Next};
use axum::response::{
    IntoResponse, Response, Sse,
    sse::{Event, KeepAlive},
};
use axum::routing::{get, post};
use futures_util::StreamExt;
use prost::Message;
use serde::{Serialize, de::DeserializeOwned};
use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Semaphore;
use vrp_proto::{app::v1 as pb, engine::v1 as engine};
mod basemaps;
mod tasks;

#[derive(Clone)]
struct HttpState {
    service: Service,
    token: Option<Arc<str>>,
    requests: Arc<Semaphore>,
    operations: Arc<Semaphore>,
    events: Arc<Semaphore>,
    map_resources: Arc<Semaphore>,
    map_operations: Arc<Semaphore>,
    csp: axum::http::HeaderValue,
}

pub fn router(service: Service, token: Option<String>) -> Router {
    let csp = crate::maps::csp(&service.map_config())
        .parse()
        .expect("validated map CSP");
    let state = HttpState {
        service,
        token: token.map(Into::into),
        requests: Arc::new(Semaphore::new(4)),
        operations: Arc::new(Semaphore::new(4)),
        events: Arc::new(Semaphore::new(128)),
        map_resources: Arc::new(Semaphore::new(8)),
        map_operations: Arc::new(Semaphore::new(8)),
        csp,
    };
    Router::new()
        .route("/v1/basemap-packages", get(basemaps::list))
        .route("/v1/basemap-packages/prepare", post(basemaps::prepare))
        .route("/v1/basemap-packages/estimate", post(basemaps::estimate))
        .route("/v1/basemap-packages/import", post(basemaps::import))
        .route(
            "/v1/basemap-packages/{id}",
            get(basemaps::get_package).delete(basemaps::delete),
        )
        .route("/v1/basemap-packages/{id}/enable", post(basemaps::enable))
        .route("/v1/basemap-packages/{id}/update", post(basemaps::update))
        .route(
            "/v1/basemaps/selection",
            get(basemaps::selection).post(basemaps::set_selection),
        )
        .route("/v1/basemaps/{id}/{file}", get(basemaps::resource))
        .route("/v1/basemap-assets/{version}/{*path}", get(basemaps::asset))
        .route("/v1/tasks", get(tasks::list).post(tasks::submit))
        .route("/v1/tasks/{id}", get(tasks::get_task).delete(tasks::delete))
        .route("/v1/tasks/{id}/events", get(tasks::events))
        .route("/v1/tasks/{id}/cancel", post(tasks::cancel))
        .route("/v1/tasks/{id}/retry", post(tasks::retry))
        .route("/v1/tasks/{id}/artifacts", get(tasks::artifacts))
        .route("/v1/tasks/{id}/artifacts/{artifact}", get(tasks::download))
        .route("/v1/scenarios/{id}/geo-plan", post(tasks::plan))
        .route("/v1/road-packages", get(tasks::roads))
        .route("/v1/road-packages/catalog", get(tasks::catalog))
        .route("/v1/road-packages/import", post(tasks::import))
        .route("/v1/road-packages/prepare", post(tasks::prepare))
        .route(
            "/v1/road-packages/{id}",
            get(tasks::road).delete(tasks::delete_road),
        )
        .route("/v1/road-packages/{id}/versions", get(tasks::versions))
        .route("/v1/road-packages/{id}/enable", post(tasks::enable))
        .route("/v1/road-packages/{id}/update", post(tasks::update))
        .route(
            "/v1/road-packages/{id}/rebuild",
            post(tasks::validate_package),
        )
        .route(
            "/tasks.js",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
                    include_str!("../../../web/tasks.js"),
                )
            }),
        )
        .route(
            "/roads.js",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
                    include_str!("../../../web/roads.js"),
                )
            }),
        )
        .route(
            "/basemaps.js",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
                    include_str!("../../../web/basemaps.js"),
                )
            }),
        )
        .route("/healthz", get(health))
        .route("/v1/map-config", get(map_config))
        .route(
            "/v1/jobs/{id}/geometry",
            get(get_geometry).post(save_geometry),
        )
        .route("/vendor/{file}", get(vendor))
        .route(
            "/map.js",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
                    include_str!("../../../web/map.js"),
                )
            }),
        )
        .route("/v1/scenarios", post(save_scenario).get(list_scenarios))
        .route("/v1/scenarios/import", post(import_scenario))
        .route(
            "/",
            get(|| async { axum::response::Html(include_str!("../../../web/index.html")) }),
        )
        .route(
            "/app.js",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
                    include_str!("../../../web/app.js"),
                )
            }),
        )
        .route(
            "/schedule.js",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
                    include_str!("../../../web/schedule.js"),
                )
            }),
        )
        .route(
            "/forms.js",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
                    include_str!("../../../web/forms.js"),
                )
            }),
        )
        .route(
            "/sheet.js",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
                    include_str!("../../../web/sheet.js"),
                )
            }),
        )
        .route(
            "/matrix.js",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
                    include_str!("../../../web/matrix.js"),
                )
            }),
        )
        .route(
            "/style.css",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
                    include_str!("../../../web/style.css"),
                )
            }),
        )
        .route(
            "/v1/scenarios/{id}",
            get(get_scenario).delete(delete_scenario),
        )
        .route(
            "/progress.js",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
                    include_str!("../../../web/progress.js"),
                )
            }),
        )
        .route(
            "/brand.svg",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "image/svg+xml")],
                    include_str!("../../../web/brand.svg"),
                )
            }),
        )
        .route("/v1/scenarios/{id}/metadata", get(get_scenario_metadata))
        .route("/v1/jobs", post(submit).get(list_jobs))
        .route("/v1/jobs/{id}", get(get_job).delete(delete_job))
        .route("/v1/jobs/{id}/cancel", post(cancel))
        .route("/v1/jobs/{id}/continue", post(continue_job))
        .route("/v1/jobs/{id}/adjust", post(adjust_plan))
        .route("/v1/jobs/{id}/request", get(input))
        .route("/v1/jobs/{id}/result", get(result))
        .route("/v1/jobs/{id}/trend", get(job_trend))
        .route("/v1/jobs/{id}/events", get(events))
        .route("/v1/operations/start", post(start_operation))
        .route("/v1/operations/{id}", get(get_operation))
        .route("/v1/operations/{id}/history", get(operation_history))
        .route("/v1/operations/{id}/events", post(execution_event))
        .route("/v1/operations/{id}/configure", post(configure_operation))
        .route("/v1/operations/{id}/replan", post(replan))
        .route("/v1/operations/{id}/adopt", post(adopt_plan))
        .route("/v1/operations/{id}/ack", post(acknowledge_plan))
        .route("/v1/operations/{id}/close", post(close_operation))
        .fallback(|| async { AppError::new("not_found", "endpoint not found") })
        .method_not_allowed_fallback(|| async {
            AppError::new("method_not_allowed", "HTTP method not supported")
        })
        .layer(middleware::from_fn_with_state(state.clone(), guard))
        .with_state(state)
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let status = match self.code {
            "invalid_request" => StatusCode::BAD_REQUEST,
            "not_found" => StatusCode::NOT_FOUND,
            "conflict" => StatusCode::CONFLICT,
            "matrix_not_ready" => StatusCode::CONFLICT,
            "too_large" => StatusCode::PAYLOAD_TOO_LARGE,
            "capacity" => StatusCode::TOO_MANY_REQUESTS,
            "unavailable" => StatusCode::SERVICE_UNAVAILABLE,
            "unauthorized" => StatusCode::UNAUTHORIZED,
            "forbidden" => StatusCode::FORBIDDEN,
            "method_not_allowed" => StatusCode::METHOD_NOT_ALLOWED,
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        };
        (status, axum::Json(self.envelope())).into_response()
    }
}

async fn guard(State(state): State<HttpState>, request: Request, next: Next) -> Response {
    let checked = {
        let check = || -> Result<()> {
            let host = request
                .headers()
                .get(header::HOST)
                .and_then(|h| h.to_str().ok())
                .ok_or_else(|| AppError::invalid("Host header is required"))?;
            let authority = host
                .parse::<axum::http::uri::Authority>()
                .map_err(|_| AppError::invalid("invalid Host"))?;
            let name = authority.host().trim_matches(['[', ']']);
            let loopback_host = name == "localhost"
                || name
                    .parse::<std::net::IpAddr>()
                    .is_ok_and(|ip| ip.is_loopback());
            // Host and forwarding headers are client-controlled. Only direct
            // loopback connections may use local access without a token.
            let direct_loopback = loopback_host
                && request
                    .extensions()
                    .get::<ConnectInfo<SocketAddr>>()
                    .is_some_and(|ConnectInfo(peer)| peer.ip().is_loopback())
                && !["forwarded", "x-forwarded-for", "x-forwarded-host"]
                    .iter()
                    .any(|name| request.headers().contains_key(*name));
            if let Some(origin) = request.headers().get(header::ORIGIN) {
                let uri: Uri = origin
                    .to_str()
                    .ok()
                    .and_then(|v| v.parse().ok())
                    .ok_or_else(|| AppError::new("forbidden", "invalid Origin"))?;
                if !matches!(uri.scheme_str(), Some("http" | "https"))
                    || uri.authority().map(|v| v.as_str()) != Some(host)
                {
                    return Err(AppError::new(
                        "forbidden",
                        "cross-origin requests are not enabled",
                    ));
                }
            }
            if let Some(token) = &state.token {
                let shell = request.method() == axum::http::Method::GET
                    && matches!(
                        request.uri().path(),
                        "/" | "/app.js"
                            | "/schedule.js"
                            | "/forms.js"
                            | "/style.css"
                            | "/map.js"
                            | "/sheet.js"
                            | "/matrix.js"
                            | "/progress.js"
                            | "/brand.svg"
                            | "/tasks.js"
                            | "/roads.js"
                            | "/basemaps.js"
                    )
                    || request.method() == axum::http::Method::GET
                        && request.uri().path().starts_with("/vendor/");
                let supplied = request
                    .headers()
                    .get(header::AUTHORIZATION)
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.strip_prefix("Bearer "))
                    .unwrap_or("");
                if !shell
                    && !direct_loopback
                    && !constant_time_eq::constant_time_eq(supplied.as_bytes(), token.as_bytes())
                {
                    return Err(AppError::new("unauthorized", "Bearer token required"));
                }
            } else if !direct_loopback {
                return Err(AppError::new(
                    "forbidden",
                    "without a configured token, only direct loopback access is allowed",
                ));
            }
            Ok(())
        };
        check()
    };
    if let Err(error) = checked {
        return error.into_response();
    }
    let is_events =
        request.method() == axum::http::Method::GET && request.uri().path().ends_with("/events");
    let semaphore = if basemaps::resource_path(request.uri().path()) {
        state.map_resources
    } else if is_events {
        state.events
    } else {
        state.requests
    };
    let Ok(permit) = semaphore.try_acquire_owned() else {
        return AppError::new("capacity", "HTTP concurrency limit reached").into_response();
    };
    let response = next.run(request).await;
    let (mut parts, body) = response.into_parts();
    parts
        .headers
        .entry(header::CACHE_CONTROL)
        .or_insert_with(|| "no-store".parse().unwrap());
    parts
        .headers
        .insert("x-content-type-options", "nosniff".parse().unwrap());
    parts.headers.insert("content-security-policy", state.csp);
    parts.headers.insert(
        "referrer-policy",
        "strict-origin-when-cross-origin".parse().unwrap(),
    );
    // Retain admission until the response body is sent/dropped. Slow downloads
    // must not leave an unbounded number of large result buffers in memory.
    let stream = async_stream::stream! {
        let _permit = permit;
        let mut stream = body.into_data_stream();
        while let Some(chunk) = stream.next().await { yield chunk; }
    };
    Response::from_parts(parts, Body::from_stream(stream))
}

#[derive(Clone, Copy)]
enum Wire {
    Json,
    Binary,
}
fn accept(headers: &HeaderMap, default: Wire) -> Wire {
    let value = headers
        .get(header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if value.contains("application/json") {
        Wire::Json
    } else if value.contains("application/protobuf") {
        Wire::Binary
    } else {
        default
    }
}
fn respond<T: Message + Serialize>(value: T, wire: Wire) -> Result<Response> {
    let (content_type, bytes) = match wire {
        Wire::Json => (
            "application/json",
            serde_json::to_vec(&value)
                .map_err(|_| AppError::internal("response serialization failed"))?,
        ),
        Wire::Binary => ("application/protobuf", value.encode_to_vec()),
    };
    Ok(([(header::CONTENT_TYPE, content_type)], bytes).into_response())
}
async fn blocking<T: Message + Serialize + Send + 'static>(
    state: HttpState,
    wire: Wire,
    task: impl FnOnce(Service) -> Result<T> + Send + 'static,
) -> Result<Response> {
    compute(&state, move |service| respond(task(service)?, wire)).await
}
async fn compute<T: Send + 'static>(
    state: &HttpState,
    task: impl FnOnce(Service) -> Result<T> + Send + 'static,
) -> Result<T> {
    let service = state.service.clone();
    offload(state.operations.clone(), move || task(service)).await
}
async fn offload<T: Send + 'static>(
    operations: Arc<Semaphore>,
    task: impl FnOnce() -> Result<T> + Send + 'static,
) -> Result<T> {
    let permit = operations
        .try_acquire_owned()
        .map_err(|_| AppError::new("capacity", "request processing limit reached"))?;
    // A disconnected client cannot cancel spawn_blocking. Keep this separate
    // permit inside the operation until decoding/storage/encoding actually ends.
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        task()
    })
    .await
    .map_err(|_| AppError::internal("request handler failed"))?
}

fn query<T: DeserializeOwned>(uri: &Uri) -> Result<T> {
    Query::<T>::try_from_uri(uri)
        .map(|q| q.0)
        .map_err(|_| AppError::invalid("invalid query parameters"))
}

async fn body(request: Request, max: usize) -> Result<(Wire, axum::body::Bytes)> {
    let content_type = request
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .split(';')
        .next()
        .unwrap_or("")
        .trim();
    let wire = match content_type {
        "application/json" => Wire::Json,
        "application/protobuf" => Wire::Binary,
        _ => {
            return Err(AppError::invalid(
                "Content-Type must be application/json or application/protobuf",
            ));
        }
    };
    let bytes = tokio::time::timeout(Duration::from_secs(10), to_bytes(request.into_body(), max))
        .await
        .map_err(|_| AppError::invalid("request body timed out"))?
        .map_err(|_| {
            AppError::new(
                "too_large",
                "request body exceeds configured limit or could not be read",
            )
        })?;
    Ok((wire, bytes))
}
fn decode<T: Message + Default + DeserializeOwned>(wire: Wire, bytes: &[u8]) -> Result<T> {
    match wire {
        Wire::Json => serde_json::from_slice(bytes).map_err(|e| {
            AppError::invalid(
                format!("invalid ProtoJSON: {e}")
                    .chars()
                    .take(1024)
                    .collect::<String>(),
            )
        }),
        Wire::Binary => T::decode(bytes).map_err(|_| AppError::invalid("invalid protobuf request")),
    }
}

async fn save_scenario(State(state): State<HttpState>, request: Request) -> Result<Response> {
    let output = accept(request.headers(), Wire::Json);
    let (wire, bytes) = body(request, state.service.request_limit()).await?;
    blocking(state, output, move |s| {
        s.save_scenario(decode(wire, &bytes)?)
    })
    .await
}

async fn map_config(State(state): State<HttpState>, request: Request) -> Result<Response> {
    respond(
        state.service.map_config(),
        accept(request.headers(), Wire::Json),
    )
}
async fn get_geometry(
    State(state): State<HttpState>,
    Path(id): Path<String>,
    request: Request,
) -> Result<Response> {
    blocking(state, accept(request.headers(), Wire::Json), move |s| {
        s.get_geometry(&id)
    })
    .await
}
async fn save_geometry(
    State(state): State<HttpState>,
    Path(id): Path<String>,
    request: Request,
) -> Result<Response> {
    let output = accept(request.headers(), Wire::Json);
    let (wire, bytes) = body(request, state.service.request_limit()).await?;
    blocking(state, output, move |s| {
        let mut value: pb::JobGeometry = decode(wire, &bytes)?;
        if !value.job_id.is_empty() && value.job_id != id {
            return Err(AppError::invalid("job_id disagrees with path"));
        }
        value.job_id = id;
        s.save_geometry(value)
    })
    .await
}
async fn vendor(Path(file): Path<String>) -> Response {
    let (mime, bytes): (&str, &[u8]) = match file.as_str() {
        "maplibre-gl.mjs" => (
            "text/javascript",
            include_bytes!("../../../web/vendor/maplibre-gl.mjs"),
        ),
        "maplibre-gl-shared.mjs" => (
            "text/javascript",
            include_bytes!("../../../web/vendor/maplibre-gl-shared.mjs"),
        ),
        "maplibre-gl-worker.mjs" => (
            "text/javascript",
            include_bytes!("../../../web/vendor/maplibre-gl-worker.mjs"),
        ),
        "maplibre-gl.css" => (
            "text/css",
            include_bytes!("../../../web/vendor/maplibre-gl.css"),
        ),
        "pmtiles.js" => (
            "text/javascript",
            include_bytes!("../../../web/vendor/pmtiles.js"),
        ),
        "maplibre-LICENSE.txt" => (
            "text/plain",
            include_bytes!("../../../web/vendor/maplibre-LICENSE.txt"),
        ),
        "pmtiles-LICENSE.txt" => (
            "text/plain",
            include_bytes!("../../../web/vendor/pmtiles-LICENSE.txt"),
        ),
        _ => return AppError::new("not_found", "unknown bundled resource").into_response(),
    };
    ([(header::CONTENT_TYPE, mime)], bytes).into_response()
}
async fn import_scenario(State(state): State<HttpState>, request: Request) -> Result<Response> {
    let output = accept(request.headers(), Wire::Json);
    let (wire, bytes) = body(request, state.service.request_limit()).await?;
    blocking(state, output, move |s| {
        let value: pb::ImportScenarioRequest = decode(wire, &bytes)?;
        let input = engine::SolveRequest::decode(value.request_bytes.as_slice())
            .map_err(|_| AppError::invalid("invalid native SolveRequest protobuf"))?;
        s.save_scenario(pb::SaveScenarioRequest {
            scenario_id: value.scenario_id,
            name: value.name,
            expected_version: value.expected_version,
            request: Some(input),
            display: value.display,
            origin: value.origin,
        })
    })
    .await
}

async fn start_operation(State(state): State<HttpState>, request: Request) -> Result<Response> {
    let output = accept(request.headers(), Wire::Json);
    let (wire, bytes) = body(request, state.service.request_limit()).await?;
    blocking(state, output, move |s| {
        s.start_operation(decode(wire, &bytes)?)
    })
    .await
}
async fn get_operation(
    State(state): State<HttpState>,
    Path(id): Path<String>,
    request: Request,
) -> Result<Response> {
    let query: pb::GetOperationRequest = query(request.uri())?;
    if !query.scenario_id.is_empty() && query.scenario_id != id {
        return Err(AppError::invalid("scenario_id disagrees with path"));
    }
    blocking(state, accept(request.headers(), Wire::Json), move |s| {
        s.get_operation(&id, query.revision)
    })
    .await
}
macro_rules! operation_post {
    ($handler:ident,$ty:ty,$method:ident) => {
        async fn $handler(
            State(state): State<HttpState>,
            Path(id): Path<String>,
            request: Request,
        ) -> Result<Response> {
            let output = accept(request.headers(), Wire::Json);
            let (wire, bytes) = body(request, state.service.request_limit()).await?;
            blocking(state, output, move |s| {
                let mut value: $ty = decode(wire, &bytes)?;
                if !value.scenario_id.is_empty() && value.scenario_id != id {
                    return Err(AppError::invalid("scenario_id disagrees with path"));
                }
                value.scenario_id = id;
                s.$method(value)
            })
            .await
        }
    };
}
operation_post!(execution_event, pb::ExecutionEvent, apply_execution_event);
operation_post!(
    configure_operation,
    pb::ConfigureOperationRequest,
    configure_operation
);
operation_post!(replan, pb::ReplanRequest, replan);
operation_post!(adopt_plan, pb::AdoptPlanRequest, adopt_plan);
operation_post!(
    acknowledge_plan,
    pb::AcknowledgePlanRequest,
    acknowledge_plan
);
operation_post!(close_operation, pb::CloseOperationRequest, close_operation);
async fn submit(State(state): State<HttpState>, request: Request) -> Result<Response> {
    let output = accept(request.headers(), Wire::Json);
    let (wire, bytes) = body(request, state.service.request_limit()).await?;
    let mut response = blocking(state, output, move |s| s.submit(decode(wire, &bytes)?)).await?;
    *response.status_mut() = StatusCode::ACCEPTED;
    Ok(response)
}
macro_rules! job_post {
    ($handler:ident,$ty:ty,$method:ident) => {
        async fn $handler(
            State(state): State<HttpState>,
            Path(id): Path<String>,
            request: Request,
        ) -> Result<Response> {
            let output = accept(request.headers(), Wire::Json);
            let (wire, bytes) = body(request, state.service.request_limit()).await?;
            blocking(state, output, move |s| {
                let mut value: $ty = decode(wire, &bytes)?;
                if !value.job_id.is_empty() && value.job_id != id {
                    return Err(AppError::invalid("job_id disagrees with path"));
                }
                value.job_id = id;
                s.$method(value)
            })
            .await
        }
    };
}
job_post!(continue_job, pb::ContinueJobRequest, continue_job);
job_post!(adjust_plan, pb::AdjustPlanRequest, adjust_plan);
async fn operation_history(
    State(state): State<HttpState>,
    Path(id): Path<String>,
    request: Request,
) -> Result<Response> {
    let mut value: pb::ListOperationHistoryRequest = query(request.uri())?;
    if !value.scenario_id.is_empty() && value.scenario_id != id {
        return Err(AppError::invalid("scenario_id disagrees with path"));
    }
    value.scenario_id = id;
    blocking(state, accept(request.headers(), Wire::Json), move |s| {
        s.list_operation_history(value)
    })
    .await
}
async fn get_scenario(
    State(state): State<HttpState>,
    Path(id): Path<String>,
    request: Request,
) -> Result<Response> {
    let query: pb::GetScenarioRequest = query(request.uri())?;
    if !query.scenario_id.is_empty() && query.scenario_id != id {
        return Err(AppError::invalid("scenario_id disagrees with path"));
    }
    blocking(state, accept(request.headers(), Wire::Json), move |s| {
        s.get_scenario(&id, query.version)
    })
    .await
}
async fn list_scenarios(State(state): State<HttpState>, request: Request) -> Result<Response> {
    let query = query(request.uri())?;
    blocking(state, accept(request.headers(), Wire::Json), move |s| {
        s.list_scenarios(query)
    })
    .await
}
async fn delete_scenario(
    State(state): State<HttpState>,
    Path(id): Path<String>,
    request: Request,
) -> Result<Response> {
    let mut value: pb::DeleteScenarioRequest = query(request.uri())?;
    if !value.scenario_id.is_empty() && value.scenario_id != id {
        return Err(AppError::invalid("scenario_id disagrees with path"));
    }
    value.scenario_id = id;
    blocking(
        state,
        accept(request.headers(), Wire::Json),
        move |service| service.delete_scenario(value),
    )
    .await
}
async fn get_scenario_metadata(
    State(state): State<HttpState>,
    Path(id): Path<String>,
    request: Request,
) -> Result<Response> {
    let query: pb::GetScenarioRequest = query(request.uri())?;
    if !query.scenario_id.is_empty() && query.scenario_id != id {
        return Err(AppError::invalid("scenario_id disagrees with path"));
    }
    blocking(state, accept(request.headers(), Wire::Json), move |s| {
        s.get_scenario_metadata(&id, query.version)
    })
    .await
}
async fn list_jobs(State(state): State<HttpState>, request: Request) -> Result<Response> {
    let query = query(request.uri())?;
    blocking(state, accept(request.headers(), Wire::Json), move |s| {
        s.list_jobs(query)
    })
    .await
}
async fn get_job(
    State(state): State<HttpState>,
    Path(id): Path<String>,
    request: Request,
) -> Result<Response> {
    blocking(state, accept(request.headers(), Wire::Json), move |s| {
        s.get_job(&id)
    })
    .await
}
async fn job_trend(
    State(state): State<HttpState>,
    Path(id): Path<String>,
    request: Request,
) -> Result<Response> {
    blocking(
        state,
        accept(request.headers(), Wire::Json),
        move |service| service.get_job_trend(&id),
    )
    .await
}
async fn cancel(
    State(state): State<HttpState>,
    Path(id): Path<String>,
    request: Request,
) -> Result<Response> {
    blocking(state, accept(request.headers(), Wire::Json), move |s| {
        s.cancel(&id)
    })
    .await
}
async fn delete_job(
    State(state): State<HttpState>,
    Path(id): Path<String>,
    request: Request,
) -> Result<Response> {
    blocking(state, accept(request.headers(), Wire::Json), move |s| {
        s.delete(&id)
    })
    .await
}
async fn input(
    State(state): State<HttpState>,
    Path(id): Path<String>,
    request: Request,
) -> Result<Response> {
    download(state, id, false, accept(request.headers(), Wire::Binary)).await
}
async fn result(
    State(state): State<HttpState>,
    Path(id): Path<String>,
    request: Request,
) -> Result<Response> {
    download(state, id, true, accept(request.headers(), Wire::Binary)).await
}
async fn download(state: HttpState, id: String, result: bool, wire: Wire) -> Result<Response> {
    compute(&state, move |service| {
        let bytes = service.payload(&id, result)?;
        match wire {
            Wire::Binary => {
                Ok(([(header::CONTENT_TYPE, "application/protobuf")], bytes).into_response())
            }
            Wire::Json if result => respond(engine::SolveResult::decode(bytes.as_slice())?, wire),
            Wire::Json => respond(engine::SolveRequest::decode(bytes.as_slice())?, wire),
        }
    })
    .await
}
async fn health(State(state): State<HttpState>, request: Request) -> Result<Response> {
    let wire = accept(request.headers(), Wire::Json);
    compute(&state, move |service| {
        let health = service
            .health()
            .map_err(|_| AppError::new("unavailable", "task storage unavailable"))?;
        let ready = health.ready;
        let mut response = respond(health, wire)?;
        if !ready {
            *response.status_mut() = StatusCode::SERVICE_UNAVAILABLE;
        }
        Ok(response)
    })
    .await
}
async fn events(
    State(state): State<HttpState>,
    Path(id): Path<String>,
    request: Request,
) -> Result<Response> {
    let task_id = id.clone();
    let mut receiver = compute(&state, move |service| service.subscribe(&task_id)).await?;
    if let Some(last) = request.headers().get("last-event-id") {
        let (job, revision) = last
            .to_str()
            .ok()
            .and_then(|s| s.split_once(':'))
            .ok_or_else(|| AppError::invalid("invalid Last-Event-ID"))?;
        if job != id
            || revision
                .parse::<u64>()
                .map_or(true, |n| n > receiver.borrow().revision)
        {
            return Err(AppError::invalid(
                "Last-Event-ID does not match this job revision",
            ));
        }
    }
    let stream = async_stream::stream! {
        let mut last_revision = 0;
        loop {
            let job = receiver.borrow_and_update().clone();
            if job.revision > last_revision {
                last_revision = job.revision;
                if let Ok(event) = Event::default().event("snapshot").id(format!("{}:{}", job.id, job.revision)).json_data(&job) {
                    yield Ok::<_, Infallible>(event);
                }
            }
            if terminal(&job) || state.service.is_stopping() { break; }
            tokio::select! {
                changed = receiver.changed() => { if changed.is_err() { break; } },
                _ = tokio::time::sleep(Duration::from_secs(1)) => {},
            }
        }
    };
    Ok(Sse::new(stream)
        .keep_alive(KeepAlive::default())
        .into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn abandoned_http_work_keeps_its_processing_permit_until_done() {
        let operations = Arc::new(Semaphore::new(1));
        let (started, observed) = tokio::sync::oneshot::channel();
        let (release, blocked) = std::sync::mpsc::channel();
        let pending = tokio::spawn(offload(operations.clone(), move || {
            let _ = started.send(());
            blocked.recv().unwrap();
            Ok(())
        }));
        observed.await.unwrap();
        pending.abort();
        let _ = pending.await;
        assert_eq!(operations.available_permits(), 0);
        assert_eq!(
            offload(operations.clone(), || Ok(()))
                .await
                .unwrap_err()
                .code,
            "capacity"
        );
        release.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(2), async {
            while operations.available_permits() == 0 {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .unwrap();
        offload(operations, || Ok(())).await.unwrap();
    }
}
