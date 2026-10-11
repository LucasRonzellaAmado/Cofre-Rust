use std::path::PathBuf;
use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Path, Request, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use cofre_core::{Backend, BackendError, Kind, LocalBackend};
use subtle::ConstantTimeEq;
use zeroize::Zeroizing;

pub const MAX_OBJECT_SIZE: usize = 64 * 1024 * 1024;

#[derive(Clone)]
pub struct AppState {
    backend: Arc<LocalBackend>,
    append_only: bool,
    token: Option<Arc<Zeroizing<Vec<u8>>>>,
}

impl AppState {
    pub fn new(
        root: impl Into<PathBuf>,
        append_only: bool,
        token: Option<Zeroizing<String>>,
    ) -> Self {
        AppState {
            backend: Arc::new(LocalBackend::new(root)),
            append_only,
            token: token.map(|t| Arc::new(Zeroizing::new(t.as_bytes().to_vec()))),
        }
    }
}

pub struct ApiError(StatusCode, String);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, self.1).into_response()
    }
}

impl From<BackendError> for ApiError {
    fn from(e: BackendError) -> Self {
        let status = match &e {
            BackendError::NotFound(_) => StatusCode::NOT_FOUND,
            BackendError::AlreadyExists(_) => StatusCode::CONFLICT,
            BackendError::Forbidden(_) => StatusCode::FORBIDDEN,
            BackendError::InvalidName(_) => StatusCode::BAD_REQUEST,
            BackendError::Io(_) | BackendError::Other(_) => {
                tracing::error!(error = %e, "falha no armazenamento");
                StatusCode::INTERNAL_SERVER_ERROR
            }
        };
        ApiError(status, e.to_string())
    }
}

fn parse_kind(kind: &str) -> Result<Kind, ApiError> {
    Kind::parse(kind).ok_or_else(|| ApiError(StatusCode::NOT_FOUND, "tipo desconhecido".into()))
}

async fn blocking<T, F>(f: F) -> Result<T, ApiError>
where
    F: FnOnce() -> Result<T, BackendError> + Send + 'static,
    T: Send + 'static,
{
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .map_err(ApiError::from)
}

async fn list_objects(
    State(s): State<AppState>,
    Path(kind): Path<String>,
) -> Result<Json<Vec<String>>, ApiError> {
    let kind = parse_kind(&kind)?;
    let backend = s.backend.clone();
    let names = blocking(move || backend.list(kind)).await?;
    Ok(Json(names))
}

async fn get_object(
    State(s): State<AppState>,
    Path((kind, name)): Path<(String, String)>,
) -> Result<Response, ApiError> {
    let kind = parse_kind(&kind)?;
    let backend = s.backend.clone();
    let data = blocking(move || backend.get(kind, &name)).await?;
    Ok(([(header::CONTENT_TYPE, "application/octet-stream")], data).into_response())
}

async fn head_object(
    State(s): State<AppState>,
    Path((kind, name)): Path<(String, String)>,
) -> Result<StatusCode, ApiError> {
    let kind = parse_kind(&kind)?;
    let backend = s.backend.clone();
    if blocking(move || backend.exists(kind, &name)).await? {
        Ok(StatusCode::OK)
    } else {
        Ok(StatusCode::NOT_FOUND)
    }
}

async fn put_object(
    State(s): State<AppState>,
    Path((kind, name)): Path<(String, String)>,
    body: Bytes,
) -> Result<StatusCode, ApiError> {
    let kind = parse_kind(&kind)?;
    let backend = s.backend.clone();
    let size = body.len();
    let log_name = name.clone();
    blocking(move || backend.put(kind, &name, &body)).await?;
    tracing::info!(kind = kind.as_str(), name = %log_name, size, "objeto gravado");
    Ok(StatusCode::CREATED)
}

async fn delete_object(
    State(s): State<AppState>,
    Path((kind, name)): Path<(String, String)>,
) -> Result<StatusCode, ApiError> {
    let kind = parse_kind(&kind)?;
    if kind == Kind::Config || (s.append_only && kind != Kind::Lock) {
        tracing::warn!(kind = kind.as_str(), %name, "remoção recusada (append-only)");
        return Err(ApiError(
            StatusCode::FORBIDDEN,
            "servidor em modo append-only: remoção não permitida".into(),
        ));
    }
    let backend = s.backend.clone();
    let log_name = name.clone();
    blocking(move || backend.delete(kind, &name)).await?;
    tracing::info!(kind = kind.as_str(), name = %log_name, "objeto removido");
    Ok(StatusCode::NO_CONTENT)
}

fn bearer(headers: &HeaderMap) -> Option<&[u8]> {
    headers
        .get(header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
        .map(str::as_bytes)
}

async fn auth(State(s): State<AppState>, req: Request, next: Next) -> Response {
    if let Some(expected) = &s.token {
        let ok = bearer(req.headers())
            .map(|got| bool::from(got.ct_eq(expected.as_slice())))
            .unwrap_or(false);
        if !ok {
            return (StatusCode::UNAUTHORIZED, "token inválido").into_response();
        }
    }
    next.run(req).await
}

async fn health() -> &'static str {
    "ok"
}

pub fn app(state: AppState) -> Router {
    let protected = Router::new()
        .route("/{kind}", get(list_objects))
        .route(
            "/{kind}/{name}",
            get(get_object)
                .head(head_object)
                .put(put_object)
                .delete(delete_object),
        )
        .route_layer(middleware::from_fn_with_state(state.clone(), auth))
        .layer(DefaultBodyLimit::max(MAX_OBJECT_SIZE));
    Router::new()
        .route("/health", get(health))
        .merge(protected)
        .with_state(state)
}
