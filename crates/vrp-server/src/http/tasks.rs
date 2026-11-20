use super::*;
pub(super) async fn submit(State(s): State<HttpState>, r: Request) -> Result<Response> {
    let out = accept(r.headers(), Wire::Json);
    let (w, b) = body(r, s.service.request_limit()).await?;
    let req = decode(w, &b)?;
    blocking(s, out, move |v| v.submit_task(req)).await
}
pub(super) async fn delete(
    State(s): State<HttpState>,
    Path(id): Path<String>,
    r: Request,
) -> Result<Response> {
    blocking(s, accept(r.headers(), Wire::Json), move |v| {
        v.delete_task(&id)
    })
    .await
}

pub(super) async fn list(State(s): State<HttpState>, r: Request) -> Result<Response> {
    let q = query::<pb::ListTasksRequest>(r.uri())?;
    blocking(s, accept(r.headers(), Wire::Json), move |v| v.list_tasks(q)).await
}
pub(super) async fn get_task(
    State(s): State<HttpState>,
    Path(id): Path<String>,
    r: Request,
) -> Result<Response> {
    blocking(s, accept(r.headers(), Wire::Json), move |v| v.get_task(&id)).await
}
pub(super) async fn cancel(
    State(s): State<HttpState>,
    Path(id): Path<String>,
    r: Request,
) -> Result<Response> {
    blocking(s, accept(r.headers(), Wire::Json), move |v| {
        v.cancel_task(&id)
    })
    .await
}
pub(super) async fn retry(
    State(s): State<HttpState>,
    Path(id): Path<String>,
    r: Request,
) -> Result<Response> {
    blocking(s, accept(r.headers(), Wire::Json), move |v| {
        v.retry_task(&id)
    })
    .await
}
pub(super) async fn artifacts(
    State(s): State<HttpState>,
    Path(id): Path<String>,
    r: Request,
) -> Result<Response> {
    blocking(s, accept(r.headers(), Wire::Json), move |v| {
        v.task_artifacts(&id)
    })
    .await
}
pub(super) async fn plan(
    State(s): State<HttpState>,
    Path(id): Path<String>,
    r: Request,
) -> Result<Response> {
    let out = accept(r.headers(), Wire::Json);
    let (w, b) = body(r, s.service.request_limit()).await?;
    let mut req: pb::GeoPlanRequest = decode(w, &b)?;
    if !req.scenario_id.is_empty() && req.scenario_id != id {
        return Err(AppError::invalid("scenario_id disagrees with path"));
    }
    req.scenario_id = id;
    blocking(s, out, move |v| v.plan_with_roads(req)).await
}
pub(super) async fn roads(State(s): State<HttpState>, r: Request) -> Result<Response> {
    let q = query::<pb::ListRoadPackagesRequest>(r.uri())?;
    blocking(s, accept(r.headers(), Wire::Json), move |v| {
        v.list_road_packages(q)
    })
    .await
}
pub(super) async fn road(
    State(s): State<HttpState>,
    Path(id): Path<String>,
    r: Request,
) -> Result<Response> {
    blocking(s, accept(r.headers(), Wire::Json), move |v| {
        v.get_road_package(&id)
    })
    .await
}
pub(super) async fn versions(
    State(s): State<HttpState>,
    Path(id): Path<String>,
    r: Request,
) -> Result<Response> {
    blocking(s, accept(r.headers(), Wire::Json), move |v| {
        v.road_package_versions(&id)
    })
    .await
}
pub(super) async fn catalog(State(s): State<HttpState>, r: Request) -> Result<Response> {
    blocking(s, accept(r.headers(), Wire::Json), move |v| {
        v.road_catalog()
    })
    .await
}
pub(super) async fn import(State(s): State<HttpState>, r: Request) -> Result<Response> {
    let out = accept(r.headers(), Wire::Json);
    let (w, b) = body(r, s.service.request_limit()).await?;
    let req = decode(w, &b)?;
    blocking(s, out, move |v| v.import_road_package(req)).await
}
pub(super) async fn prepare(State(s): State<HttpState>, r: Request) -> Result<Response> {
    let out = accept(r.headers(), Wire::Json);
    let (w, b) = body(r, s.service.request_limit()).await?;
    let req: pb::PrepareRoadPackageRequest = decode(w, &b)?;
    blocking(s, out, move |v| {
        v.prepare_road_package_force(&req.region_id, req.force_download)
    })
    .await
}
pub(super) async fn enable(
    State(s): State<HttpState>,
    Path(id): Path<String>,
    r: Request,
) -> Result<Response> {
    let out = accept(r.headers(), Wire::Json);
    let (w, b) = body(r, s.service.request_limit()).await?;
    let mut req: pb::EnableRoadPackageRequest = decode(w, &b)?;
    req.package_id = id;
    blocking(s, out, move |v| v.enable_road_package(req)).await
}
pub(super) async fn update(
    State(s): State<HttpState>,
    Path(id): Path<String>,
    r: Request,
) -> Result<Response> {
    let out = accept(r.headers(), Wire::Json);
    let (wire, bytes) = body(r, s.service.request_limit()).await?;
    let request: pb::PrepareRoadPackageRequest = if bytes.is_empty() {
        Default::default()
    } else {
        decode(wire, &bytes)?
    };
    blocking(s, out, move |v| {
        v.update_road_package_force(&id, request.force_download)
    })
    .await
}
pub(super) async fn validate_package(
    State(s): State<HttpState>,
    Path(id): Path<String>,
    r: Request,
) -> Result<Response> {
    blocking(s, accept(r.headers(), Wire::Json), move |v| {
        v.validate_road_package(&id)
    })
    .await
}
pub(super) async fn delete_road(
    State(s): State<HttpState>,
    Path(id): Path<String>,
    r: Request,
) -> Result<Response> {
    blocking(s, accept(r.headers(), Wire::Json), move |v| {
        v.delete_road_package(&id)
    })
    .await
}
pub(super) async fn download(
    State(s): State<HttpState>,
    Path((id, artifact)): Path<(String, String)>,
) -> Result<Response> {
    let (a, path) = compute(&s, move |v| v.task_artifact(&id, &artifact)).await?;
    let mut file = tokio::fs::File::open(path).await?;
    let stream = async_stream::try_stream! {use tokio::io::AsyncReadExt;let mut buffer=vec![0;65536];loop{let n=file.read(&mut buffer).await?;if n==0{break;}yield axum::body::Bytes::copy_from_slice(&buffer[..n]);}};
    let stream: std::pin::Pin<
        Box<dyn futures_util::Stream<Item = std::io::Result<axum::body::Bytes>> + Send>,
    > = Box::pin(stream);
    Ok((
        [(header::CONTENT_TYPE, a.media_type)],
        Body::from_stream(stream),
    )
        .into_response())
}
pub(super) async fn events(
    State(s): State<HttpState>,
    Path(id): Path<String>,
    r: Request,
) -> Result<Response> {
    let initial = compute(&s, {
        let id = id.clone();
        move |v| v.get_task(&id)
    })
    .await?;
    if let Some(h) = r.headers().get("last-event-id") {
        let value = h
            .to_str()
            .map_err(|_| AppError::invalid("invalid Last-Event-ID"))?;
        let (t, n) = value
            .rsplit_once(':')
            .ok_or_else(|| AppError::invalid("invalid Last-Event-ID"))?;
        if t != id || n.parse::<u64>().map_or(true, |n| n > initial.revision) {
            return Err(AppError::invalid(
                "Last-Event-ID disagrees with task revision",
            ));
        }
    }
    let stream = async_stream::stream! {
        let mut previous=0;
        while let Ok(task)=s.service.get_task(&id) {
            if task.revision>previous {
                previous=task.revision;
                if let Ok(event)=Event::default().event("snapshot").id(format!("{}:{}",task.id,task.revision)).json_data(&task) { yield Ok::<_,Infallible>(event); }
            }
            if crate::runtime::tasks::task_terminal(&task)||s.service.is_stopping() {break;}
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    };
    Ok(Sse::new(stream)
        .keep_alive(KeepAlive::default())
        .into_response())
}
