// Handlers return a ready `Response` as their error, which is how axum expects it.
#![allow(clippy::result_large_err)]

use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::{
    Json, Router,
    body::Bytes,
    extract::{DefaultBodyLimit, Path, Query, Request, State, rejection::BytesRejection},
    http::{HeaderMap, HeaderValue, Method, StatusCode, Uri, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post, put},
};
use rust_embed::Embed;
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use crate::config::{Format, Workspace};
use crate::gates::Actor;
use crate::lint;
use crate::model::{
    AcceptTemplate, AcceptType, Approve, CreateDocument, CreateRecord, Decide, Outcome,
    SelectSection, SetReviewed, UpdateDocument, UpdateRecord, VersionOnly,
};
use crate::render::{self, RenderError, Typst};
use crate::store::{Resolved, Store, StoreError};

/// The header naming the agent behind a CLI request. Browsers can't send it
/// cross-origin without a preflight hoja never answers.
pub const AGENT_HEADER: &str = "x-hoja-agent";
const MAX_WAIT: u64 = 45;

#[derive(Clone)]
pub struct App {
    pub store: Arc<Store>,
    pub workspace: Arc<Workspace>,
    pub typst: Arc<Typst>,
    pub origin: String,
}

#[derive(Embed)]
#[folder = "spa/dist"]
struct Assets;

type ApiResult<T = Value> = Result<Json<T>, Response>;

fn error(status: StatusCode, code: &str, message: &str) -> Response {
    (status, Json(json!({"error": code, "message": message}))).into_response()
}

fn internal() -> Response {
    error(
        StatusCode::INTERNAL_SERVER_ERROR,
        "internal_error",
        "Local data is unavailable.",
    )
}

fn store_error(err: StoreError) -> Response {
    let message = err.to_string();
    match err {
        StoreError::Invalid(_) => error(StatusCode::BAD_REQUEST, "invalid_request", &message),
        StoreError::Refused(_) => error(StatusCode::FORBIDDEN, "refused", &message),
        StoreError::NotFound => error(StatusCode::NOT_FOUND, "not_found", "Not found."),
        StoreError::Conflict => error(
            StatusCode::CONFLICT,
            "conflict",
            "This changed elsewhere. Reload it before saving.",
        ),
        StoreError::Stale => error(StatusCode::CONFLICT, "stale", &message),
        StoreError::Internal(m) => {
            eprintln!("hoja: {m}");
            internal()
        }
    }
}

fn render_error(err: RenderError) -> Response {
    match err {
        RenderError::Io(m) => {
            eprintln!("hoja: render: {m}");
            error(StatusCode::INTERNAL_SERVER_ERROR, "render_failed", &m)
        }
        other => error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "render_failed",
            &other.to_string(),
        ),
    }
}

fn parse<T: DeserializeOwned>(bytes: Result<Bytes, BytesRejection>) -> Result<T, Response> {
    let bytes = bytes.map_err(|rejection| {
        error(
            rejection.status(),
            "invalid_request",
            "Request body could not be read or exceeds 64 KiB.",
        )
    })?;
    serde_json::from_slice(&bytes).map_err(|e| {
        error(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            &format!("Invalid JSON request: {e}"),
        )
    })
}

async fn blocking<T: Send + 'static>(
    job: impl FnOnce() -> T + Send + 'static,
) -> Result<T, Response> {
    tokio::task::spawn_blocking(job)
        .await
        .map_err(|_| internal())
}

async fn run<T: serde::Serialize + Send + 'static>(
    job: impl FnOnce() -> Result<T, StoreError> + Send + 'static,
) -> ApiResult {
    let value = blocking(job).await?.map_err(store_error)?;
    Ok(Json(serde_json::to_value(value).map_err(|_| internal())?))
}

fn outcome<T: serde::Serialize>(outcome: Outcome<T>) -> Response {
    let status = match outcome {
        Outcome::Applied { .. } => StatusCode::OK,
        Outcome::AwaitingPerson { .. } => StatusCode::ACCEPTED,
    };
    (status, Json(outcome)).into_response()
}

async fn run_outcome<T: serde::Serialize + Send + 'static>(
    job: impl FnOnce() -> Result<Outcome<T>, StoreError> + Send + 'static,
) -> Response {
    match blocking(job).await {
        Ok(Ok(o)) => outcome(o),
        Ok(Err(e)) => store_error(e),
        Err(response) => response,
    }
}

fn agent_actor(headers: &HeaderMap) -> Result<Actor, Response> {
    let name = headers
        .get(AGENT_HEADER)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    let valid = !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
    if !valid {
        return Err(error(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "Agent requests need an X-Hoja-Agent header: 1 to 64 letters, digits, dots, dashes or underscores.",
        ));
    }
    Ok(Actor::Agent(name.to_string()))
}

fn today() -> String {
    chrono::Local::now().date_naive().to_string()
}

#[derive(Deserialize)]
struct ListQuery {
    #[serde(rename = "type")]
    type_name: Option<String>,
    kind: Option<String>,
    state: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ExportInput {
    format: Format,
}

#[derive(Deserialize)]
struct WaitQuery {
    wait: Option<u64>,
}

#[derive(Deserialize)]
struct PathQuery {
    path: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TemplatePreview {
    path: String,
    hash: String,
    #[serde(rename = "type")]
    type_name: String,
    /// Render this document instead of the type's sample data.
    #[serde(default)]
    document: Option<String>,
}

// ---- shared reads ---------------------------------------------------------

fn type_files(app: &App) -> Result<Vec<crate::config::TypeFile>, StoreError> {
    app.workspace
        .type_files()
        .map_err(|e| StoreError::Invalid(format!("{e:#}")))
}

fn template_files(app: &App) -> Result<Vec<crate::templates::DiskTemplate>, StoreError> {
    app.workspace
        .template_files()
        .map_err(|e| StoreError::Invalid(format!("{e:#}")))
}

async fn records(State(app): State<App>, Query(q): Query<ListQuery>) -> ApiResult {
    run(move || app.store.records(q.type_name.as_deref(), q.kind.as_deref())).await
}

async fn record(State(app): State<App>, Path(id): Path<String>) -> ApiResult {
    run(move || {
        let record = app.store.record(&id)?;
        let revisions = app.store.revisions("record", &id)?;
        Ok(json!({"record": record, "revisions": revisions}))
    })
    .await
}

async fn documents(State(app): State<App>, Query(q): Query<ListQuery>) -> ApiResult {
    run(move || app.store.documents(q.type_name.as_deref())).await
}

fn document_view(app: &App, resolved: &Resolved) -> Value {
    let owner = app.workspace.config.owner.as_deref();
    let findings = lint::lint(resolved);
    let files: Vec<Value> = resolved
        .config
        .exports
        .iter()
        .map(|f| json!({"format": f, "filename": render::filename(resolved, owner, &today(), *f)}))
        .collect();
    json!({
        "document": resolved.document,
        "content_hash": resolved.content_hash(),
        "lints": findings,
        "blocking": lint::errors(&findings),
        "exports": files,
        "has_template": resolved.config.template.is_some(),
    })
}

async fn document(State(app): State<App>, Path(id): Path<String>) -> ApiResult {
    run(move || {
        let (resolved, provenance) = app.store.inspect(&id)?;
        let mut view = document_view(&app, &resolved);
        view["provenance"] = serde_json::to_value(provenance).unwrap_or_default();
        view["revisions"] =
            serde_json::to_value(app.store.revisions("document", &id)?).unwrap_or_default();
        Ok(view)
    })
    .await
}

async fn rendered(app: &App, id: String, format: Format) -> Result<(Resolved, Vec<u8>), Response> {
    let store = app.store.clone();
    let resolved = blocking(move || store.resolve(&id))
        .await?
        .map_err(store_error)?;
    if !resolved.config.exports.contains(&format) {
        return Err(error(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            &format!("this type does not export {}", format.extension()),
        ));
    }
    let bytes = match format {
        Format::Pdf => {
            if resolved.config.template.is_none() {
                return Err(render_error(RenderError::NoTemplate));
            }
            let store = app.store.clone();
            let config = resolved.config.clone();
            let (entry, files) = blocking(move || store.template_closure(&config))
                .await?
                .map_err(store_error)?;
            let data = render::data(&resolved, app.workspace.config.owner.as_deref(), &today());
            app.typst
                .pdf(&files, &entry, &data)
                .await
                .map_err(render_error)?
        }
        Format::Md => render::markdown(&resolved).into_bytes(),
        Format::Html => render::html(&resolved).into_bytes(),
    };
    Ok((resolved, bytes))
}

async fn preview(State(app): State<App>, Path(id): Path<String>) -> Response {
    match rendered(&app, id, Format::Pdf).await {
        Ok((_, bytes)) => ([(header::CONTENT_TYPE, "application/pdf")], bytes).into_response(),
        Err(response) => response,
    }
}

/// Writes the export into `<workspace>/out/`, named by the type's pattern.
async fn export(app: &App, id: String, format: Format) -> ApiResult {
    let (resolved, bytes) = rendered(app, id, format).await?;
    let filename = render::filename(
        &resolved,
        app.workspace.config.owner.as_deref(),
        &today(),
        format,
    );
    let out = app.workspace.out();
    let path = out.join(&filename);
    blocking(move || -> std::io::Result<()> {
        std::fs::create_dir_all(&out)?;
        std::fs::write(&path, bytes)
    })
    .await?
    .map_err(|e| {
        eprintln!("hoja: export: {e}");
        error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "export_failed",
            "Could not write the export.",
        )
    })?;
    Ok(Json(json!({
        "path": app.workspace.out().join(&filename),
        "filename": filename,
        "approved": resolved.document.approved,
    })))
}

// ---- person ---------------------------------------------------------------

async fn state(State(app): State<App>) -> ApiResult {
    run(move || {
        let files = type_files(&app);
        let (types, files_error) = match files {
            Ok(files) => (app.store.type_statuses(&files)?, None),
            Err(e) => (app.store.type_statuses(&[])?, Some(e.to_string())),
        };
        let (templates, templates_error) = match template_files(&app) {
            Ok(files) => (app.store.template_statuses(&files)?, None),
            Err(e) => (app.store.template_statuses(&[])?, Some(e.to_string())),
        };
        let pending = app.store.approvals(Some("pending"))?.len();
        Ok(json!({
            "owner": app.workspace.config.owner,
            "workspace": app.workspace.root,
            "types": types,
            "types_error": files_error,
            "templates": templates,
            "templates_error": templates_error,
            "pending_approvals": pending,
        }))
    })
    .await
}

async fn type_change(State(app): State<App>, Path(name): Path<String>) -> ApiResult {
    run(move || app.store.type_change(&name, &type_files(&app)?)).await
}

async fn accept_type(
    State(app): State<App>,
    Path(name): Path<String>,
    bytes: Result<Bytes, BytesRejection>,
) -> ApiResult {
    let input: AcceptType = parse(bytes)?;
    run(move || {
        app.store
            .accept_type(&Actor::Person, &name, &input.hash, &type_files(&app)?)?;
        Ok(json!({"accepted": name}))
    })
    .await
}

async fn template_change(State(app): State<App>, Query(q): Query<PathQuery>) -> ApiResult {
    run(move || app.store.template_change(&q.path, &template_files(&app)?)).await
}

async fn accept_template(
    State(app): State<App>,
    bytes: Result<Bytes, BytesRejection>,
) -> ApiResult {
    let input: AcceptTemplate = parse(bytes)?;
    run(move || {
        app.store.accept_template(
            &Actor::Person,
            &input.path,
            &input.hash,
            &template_files(&app)?,
        )?;
        Ok(json!({"accepted": input.path}))
    })
    .await
}

/// Renders a type with one template file replaced by the copy on disk, so
/// the person sees what accepting it would produce. The type is read from
/// disk too when it parses, since a new template usually arrives with a new
/// or changed type file.
async fn template_preview(
    State(app): State<App>,
    bytes: Result<Bytes, BytesRejection>,
) -> Response {
    let input: TemplatePreview = match parse(bytes) {
        Ok(input) => input,
        Err(response) => return response,
    };
    let job_app = app.clone();
    let prepared = blocking(move || -> Result<_, StoreError> {
        let app = job_app;
        let files = template_files(&app)?;
        let disk = files
            .iter()
            .find(|f| f.path == input.path)
            .ok_or(StoreError::NotFound)?;
        if disk.file.hash != input.hash {
            return Err(StoreError::Conflict);
        }
        let set = app
            .store
            .template_set()?
            .with(&input.path, disk.file.clone());
        let on_disk = type_files(&app)?
            .into_iter()
            .find(|f| f.name == input.type_name)
            .and_then(|f| crate::config::TypeConfig::parse(&f.body).ok());
        let config = match on_disk {
            Some(config) => Arc::new(config),
            None => app.store.config(&input.type_name)?,
        };
        let entry = config.template.clone().ok_or_else(|| {
            StoreError::Invalid("this type has no template, so it has no PDF".into())
        })?;
        let closure = set.closure(&entry).map_err(StoreError::Invalid)?;
        let owner = app.workspace.config.owner.as_deref();
        let data = match &input.document {
            Some(id) => {
                let resolved = app.store.resolve(id)?;
                if resolved.document.type_name != input.type_name {
                    return Err(StoreError::Invalid(format!(
                        "that document is not a {}",
                        input.type_name
                    )));
                }
                render::data(&resolved, owner, &today())
            }
            None => {
                let sample = app
                    .workspace
                    .sample(&input.type_name)
                    .map_err(|e| StoreError::Invalid(format!("{e:#}")))?;
                let mut configs = std::collections::HashMap::new();
                for (name, c) in app.store.accepted_types()? {
                    configs.insert(name, c);
                }
                configs.insert(input.type_name.clone(), config.clone());
                let resolved = render::sample(&input.type_name, config, configs, sample.as_ref());
                render::data(&resolved, owner, &today())
            }
        };
        Ok((closure, entry, data))
    })
    .await;
    let (closure, entry, data) = match prepared {
        Ok(Ok(v)) => v,
        Ok(Err(e)) => return store_error(e),
        Err(response) => return response,
    };
    match app.typst.pdf(&closure, &entry, &data).await {
        Ok(bytes) => ([(header::CONTENT_TYPE, "application/pdf")], bytes).into_response(),
        Err(e) => render_error(e),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MarkdownInput {
    text: String,
}

/// The editor's live preview, rendered by the same parser that feeds templates.
async fn markdown_preview(bytes: Result<Bytes, BytesRejection>) -> ApiResult {
    let input: MarkdownInput = parse(bytes)?;
    Ok(Json(json!({"html": crate::markdown::html(&input.text, 1)})))
}

async fn create_record(State(app): State<App>, bytes: Result<Bytes, BytesRejection>) -> Response {
    let input: CreateRecord = match parse(bytes) {
        Ok(input) => input,
        Err(response) => return response,
    };
    run_outcome(move || app.store.create_record(&Actor::Person, input)).await
}

async fn update_record(
    State(app): State<App>,
    Path(id): Path<String>,
    bytes: Result<Bytes, BytesRejection>,
) -> Response {
    let input: UpdateRecord = match parse(bytes) {
        Ok(input) => input,
        Err(response) => return response,
    };
    run_outcome(move || app.store.update_record(&Actor::Person, &id, input)).await
}

async fn set_reviewed(
    State(app): State<App>,
    Path(id): Path<String>,
    bytes: Result<Bytes, BytesRejection>,
) -> ApiResult {
    let input: SetReviewed = parse(bytes)?;
    run(move || {
        app.store
            .set_reviewed(&Actor::Person, &id, input.version, input.reviewed)
    })
    .await
}

async fn delete_record(
    State(app): State<App>,
    Path(id): Path<String>,
    bytes: Result<Bytes, BytesRejection>,
) -> ApiResult {
    let input: VersionOnly = parse(bytes)?;
    run(move || {
        app.store
            .delete_record(&Actor::Person, &id, input.version)?;
        Ok(json!({"deleted": id}))
    })
    .await
}

async fn create_document(State(app): State<App>, bytes: Result<Bytes, BytesRejection>) -> Response {
    let input: CreateDocument = match parse(bytes) {
        Ok(input) => input,
        Err(response) => return response,
    };
    run_outcome(move || app.store.create_document(&Actor::Person, input)).await
}

async fn update_document(
    State(app): State<App>,
    Path(id): Path<String>,
    bytes: Result<Bytes, BytesRejection>,
) -> Response {
    let input: UpdateDocument = match parse(bytes) {
        Ok(input) => input,
        Err(response) => return response,
    };
    run_outcome(move || app.store.update_document(&Actor::Person, &id, input)).await
}

async fn select(
    State(app): State<App>,
    Path((id, section)): Path<(String, String)>,
    bytes: Result<Bytes, BytesRejection>,
) -> Response {
    let input: SelectSection = match parse(bytes) {
        Ok(input) => input,
        Err(response) => return response,
    };
    run_outcome(move || app.store.select(&Actor::Person, &id, &section, input)).await
}

async fn approve(
    State(app): State<App>,
    Path(id): Path<String>,
    bytes: Result<Bytes, BytesRejection>,
) -> ApiResult {
    let input: Approve = parse(bytes)?;
    run(move || {
        app.store
            .approve_document(&Actor::Person, &id, input.version, &input.hash)
    })
    .await
}

async fn person_export(
    State(app): State<App>,
    Path(id): Path<String>,
    bytes: Result<Bytes, BytesRejection>,
) -> ApiResult {
    let input: ExportInput = parse(bytes)?;
    export(&app, id, input.format).await
}

async fn approvals(State(app): State<App>, Query(q): Query<ListQuery>) -> ApiResult {
    run(move || app.store.approvals(q.state.as_deref())).await
}

async fn decide(
    State(app): State<App>,
    Path(id): Path<String>,
    bytes: Result<Bytes, BytesRejection>,
) -> ApiResult {
    let input: Decide = parse(bytes)?;
    run(move || app.store.decide(&Actor::Person, &id, input)).await
}

// ---- agents ---------------------------------------------------------------

async fn agent_types(State(app): State<App>) -> ApiResult {
    run(move || {
        let types = app.store.accepted_types()?;
        Ok(types
            .into_iter()
            .map(|(name, config)| json!({"name": name, "config": *config}))
            .collect::<Vec<_>>())
    })
    .await
}

async fn agent_document(State(app): State<App>, Path(id): Path<String>) -> ApiResult {
    run(move || Ok(document_view(&app, &app.store.resolve(&id)?))).await
}

async fn agent_create_record(
    State(app): State<App>,
    headers: HeaderMap,
    bytes: Result<Bytes, BytesRejection>,
) -> Response {
    let (actor, input) =
        match agent_actor(&headers).and_then(|a| Ok((a, parse::<CreateRecord>(bytes)?))) {
            Ok(v) => v,
            Err(response) => return response,
        };
    run_outcome(move || app.store.create_record(&actor, input)).await
}

async fn agent_update_record(
    State(app): State<App>,
    headers: HeaderMap,
    Path(id): Path<String>,
    bytes: Result<Bytes, BytesRejection>,
) -> Response {
    let (actor, input) =
        match agent_actor(&headers).and_then(|a| Ok((a, parse::<UpdateRecord>(bytes)?))) {
            Ok(v) => v,
            Err(response) => return response,
        };
    run_outcome(move || app.store.update_record(&actor, &id, input)).await
}

async fn agent_create_document(
    State(app): State<App>,
    headers: HeaderMap,
    bytes: Result<Bytes, BytesRejection>,
) -> Response {
    let (actor, input) =
        match agent_actor(&headers).and_then(|a| Ok((a, parse::<CreateDocument>(bytes)?))) {
            Ok(v) => v,
            Err(response) => return response,
        };
    run_outcome(move || app.store.create_document(&actor, input)).await
}

async fn agent_update_document(
    State(app): State<App>,
    headers: HeaderMap,
    Path(id): Path<String>,
    bytes: Result<Bytes, BytesRejection>,
) -> Response {
    let (actor, input) =
        match agent_actor(&headers).and_then(|a| Ok((a, parse::<UpdateDocument>(bytes)?))) {
            Ok(v) => v,
            Err(response) => return response,
        };
    run_outcome(move || app.store.update_document(&actor, &id, input)).await
}

async fn agent_select(
    State(app): State<App>,
    headers: HeaderMap,
    Path((id, section)): Path<(String, String)>,
    bytes: Result<Bytes, BytesRejection>,
) -> Response {
    let (actor, input) =
        match agent_actor(&headers).and_then(|a| Ok((a, parse::<SelectSection>(bytes)?))) {
            Ok(v) => v,
            Err(response) => return response,
        };
    run_outcome(move || app.store.select(&actor, &id, &section, input)).await
}

async fn agent_build(
    State(app): State<App>,
    Path(id): Path<String>,
    bytes: Result<Bytes, BytesRejection>,
) -> ApiResult {
    let input: ExportInput = parse(bytes)?;
    export(&app, id, input.format).await
}

/// Long-polls a card for at most 45 seconds, the same contract as Tracon's
/// `approval_status`: `still_waiting` means ask again with the same id.
async fn agent_wait(
    State(app): State<App>,
    Path(id): Path<String>,
    Query(q): Query<WaitQuery>,
) -> ApiResult {
    let deadline = Instant::now() + Duration::from_secs(q.wait.unwrap_or(0).min(MAX_WAIT));
    loop {
        let store = app.store.clone();
        let lookup = id.clone();
        let approval = blocking(move || store.approval(&lookup))
            .await?
            .map_err(store_error)?;
        if approval.state != "pending" || Instant::now() >= deadline {
            let state = if approval.state == "pending" {
                "still_waiting"
            } else {
                approval.state.as_str()
            };
            return Ok(Json(json!({
                "approval_id": approval.id,
                "state": state,
                "reason": approval.reason,
                "edited": approval.edited,
                "result": approval.result,
                "stale": approval.stale,
            })));
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

// ---- assets and guard -----------------------------------------------------

async fn asset(uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    let path = if path.is_empty() { "index.html" } else { path };
    match Assets::get(path) {
        Some(file) => {
            let mime = mime_guess::from_path(path).first_or_octet_stream();
            let cache = if path.starts_with("assets/") {
                "public, max-age=31536000, immutable"
            } else {
                "no-cache"
            };
            (
                [
                    (header::CONTENT_TYPE, mime.to_string()),
                    (header::CACHE_CONTROL, cache.to_string()),
                ],
                file.data,
            )
                .into_response()
        }
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn fallback(uri: Uri) -> Response {
    if uri.path() == "/api" || uri.path().starts_with("/api/") {
        error(StatusCode::NOT_FOUND, "not_found", "API route not found.")
    } else {
        asset(uri).await
    }
}

fn json_content(request: &Request) -> bool {
    request
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| {
            v.eq_ignore_ascii_case("application/json")
                || v.to_ascii_lowercase().starts_with("application/json;")
        })
}

/// Person routes accept mutations only from the page's own origin. Agent
/// routes accept no browser at all: no Origin header, and the custom agent
/// header a cross-origin page can't send without a preflight.
async fn guard(State(app): State<App>, request: Request, next: Next) -> Response {
    let path = request.uri().path();
    let is_api = path.starts_with("/api");
    let is_agent = path == "/api/agent" || path.starts_with("/api/agent/");
    let ipv4_host = app.origin.strip_prefix("http://").unwrap_or("");
    let port = ipv4_host.strip_prefix("127.0.0.1:").unwrap_or("");
    let host = request
        .headers()
        .get(header::HOST)
        .and_then(|v| v.to_str().ok());
    let ipv4 = host == Some(ipv4_host);
    let localhost = host.and_then(|value| value.strip_prefix("localhost:")) == Some(port);
    let allowed_host = ipv4 || localhost;
    let mutation = !matches!(*request.method(), Method::GET | Method::HEAD);
    let origin = request
        .headers()
        .get(header::ORIGIN)
        .and_then(|v| v.to_str().ok());
    let allowed_origin = (ipv4 && origin == Some(app.origin.as_str()))
        || (localhost
            && origin.and_then(|value| value.strip_prefix("http://localhost:")) == Some(port));
    let has_origin = request.headers().contains_key(header::ORIGIN);
    let refused = if !allowed_host {
        Some("Use the local hoja URL.")
    } else if is_agent && has_origin {
        Some("The agent API takes requests from the hoja CLI, not from browsers.")
    } else if is_agent && !request.headers().contains_key(AGENT_HEADER) {
        Some("Agent requests need an X-Hoja-Agent header.")
    } else if !is_agent && ((has_origin && !allowed_origin) || (mutation && !allowed_origin)) {
        Some("Use the local hoja URL.")
    } else {
        None
    };
    let mut response = if let Some(message) = refused {
        error(StatusCode::FORBIDDEN, "forbidden", message)
    } else if mutation && is_api && !json_content(&request) {
        error(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "JSON Content-Type is required.",
        )
    } else {
        next.run(request).await
    };
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data: blob:; frame-src 'self' blob:; connect-src 'self'; object-src 'none'; base-uri 'none'; frame-ancestors 'self'; form-action 'self'"),
    );
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    if is_api {
        headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    }
    response
}

pub fn router(app: App) -> Router {
    let agent = Router::new()
        .route("/types", get(agent_types))
        .route("/records", get(records).post(agent_create_record))
        .route("/records/{id}", get(record).post(agent_update_record))
        .route("/documents", get(documents).post(agent_create_document))
        .route(
            "/documents/{id}",
            get(agent_document).post(agent_update_document),
        )
        .route("/documents/{id}/sections/{section}", post(agent_select))
        .route("/documents/{id}/build", post(agent_build))
        .route("/approvals/{id}", get(agent_wait));
    Router::new()
        .route("/api/state", get(state))
        .route("/api/types/{name}/change", get(type_change))
        .route("/api/types/{name}/accept", post(accept_type))
        .route("/api/templates/change", get(template_change))
        .route("/api/templates/accept", post(accept_template))
        .route("/api/templates/preview", post(template_preview))
        .route("/api/records", get(records).post(create_record))
        .route("/api/records/{id}", get(record).put(update_record))
        .route("/api/records/{id}/reviewed", post(set_reviewed))
        .route("/api/records/{id}/delete", post(delete_record))
        .route("/api/documents", get(documents).post(create_document))
        .route("/api/documents/{id}", get(document).put(update_document))
        .route("/api/documents/{id}/sections/{section}", put(select))
        .route("/api/documents/{id}/approve", post(approve))
        .route("/api/documents/{id}/preview.pdf", get(preview))
        .route("/api/documents/{id}/export", post(person_export))
        .route("/api/markdown", post(markdown_preview))
        .route("/api/approvals", get(approvals))
        .route("/api/approvals/{id}/decide", post(decide))
        .nest("/api/agent", agent)
        .fallback(fallback)
        .route("/", get(asset))
        .route("/assets/{*path}", get(asset))
        .layer(DefaultBodyLimit::max(64 * 1024))
        .layer(middleware::from_fn_with_state(app.clone(), guard))
        .with_state(app)
}
