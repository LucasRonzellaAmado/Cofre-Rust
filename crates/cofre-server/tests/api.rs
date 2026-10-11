#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use cofre_server::{AppState, app};
use tower::ServiceExt;
use zeroize::Zeroizing;

const TOKEN: &str = "token-de-teste-com-mais-de-32-caracteres";

fn state(dir: &tempfile::TempDir, append_only: bool) -> AppState {
    AppState::new(
        dir.path(),
        append_only,
        Some(Zeroizing::new(TOKEN.to_string())),
    )
}

async fn call(
    state: &AppState,
    method: &str,
    uri: &str,
    body: Vec<u8>,
    token: Option<&str>,
) -> (StatusCode, Vec<u8>) {
    let mut req = Request::builder().method(method).uri(uri);
    if let Some(t) = token {
        req = req.header("authorization", format!("Bearer {t}"));
    }
    let resp = app(state.clone())
        .oneshot(req.body(Body::from(body)).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let bytes = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    (status, bytes.to_vec())
}

#[tokio::test]
async fn health_needs_no_auth() {
    let dir = tempfile::TempDir::new().unwrap();
    let (s, b) = call(&state(&dir, false), "GET", "/health", vec![], None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(b, b"ok");
}

#[tokio::test]
async fn requests_without_valid_token_are_rejected() {
    let dir = tempfile::TempDir::new().unwrap();
    let st = state(&dir, false);
    assert_eq!(
        call(&st, "GET", "/snapshots", vec![], None).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        call(&st, "GET", "/snapshots", vec![], Some("errado"))
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        call(&st, "GET", "/snapshots", vec![], Some(TOKEN)).await.0,
        StatusCode::OK
    );
}

#[tokio::test]
async fn put_get_list_head() {
    let dir = tempfile::TempDir::new().unwrap();
    let st = state(&dir, false);
    let name = "ab".repeat(32);
    let uri = format!("/data/{name}");
    assert_eq!(
        call(&st, "HEAD", &uri, vec![], Some(TOKEN)).await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        call(&st, "PUT", &uri, b"blob".to_vec(), Some(TOKEN))
            .await
            .0,
        StatusCode::CREATED
    );
    assert_eq!(
        call(&st, "HEAD", &uri, vec![], Some(TOKEN)).await.0,
        StatusCode::OK
    );
    let (s, body) = call(&st, "GET", &uri, vec![], Some(TOKEN)).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(body, b"blob");
    let (_, list) = call(&st, "GET", "/data", vec![], Some(TOKEN)).await;
    let names: Vec<String> = serde_json::from_slice(&list).unwrap();
    assert_eq!(names, vec![name]);
}

#[tokio::test]
async fn overwrite_is_always_refused() {
    let dir = tempfile::TempDir::new().unwrap();
    let st = state(&dir, false);
    assert_eq!(
        call(&st, "PUT", "/snapshots/aaaa", b"1".to_vec(), Some(TOKEN))
            .await
            .0,
        StatusCode::CREATED
    );
    assert_eq!(
        call(&st, "PUT", "/snapshots/aaaa", b"2".to_vec(), Some(TOKEN))
            .await
            .0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        call(&st, "GET", "/snapshots/aaaa", vec![], Some(TOKEN))
            .await
            .1,
        b"1"
    );
}

#[tokio::test]
async fn append_only_blocks_deletes_except_locks() {
    let dir = tempfile::TempDir::new().unwrap();
    let st = state(&dir, true);
    call(&st, "PUT", "/snapshots/aaaa", b"1".to_vec(), Some(TOKEN)).await;
    call(&st, "PUT", "/locks/bbbb", b"1".to_vec(), Some(TOKEN)).await;
    assert_eq!(
        call(&st, "DELETE", "/snapshots/aaaa", vec![], Some(TOKEN))
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        call(&st, "GET", "/snapshots/aaaa", vec![], Some(TOKEN))
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        call(&st, "DELETE", "/locks/bbbb", vec![], Some(TOKEN))
            .await
            .0,
        StatusCode::NO_CONTENT
    );
}

#[tokio::test]
async fn normal_mode_allows_deletes_but_never_config() {
    let dir = tempfile::TempDir::new().unwrap();
    let st = state(&dir, false);
    call(&st, "PUT", "/snapshots/aaaa", b"1".to_vec(), Some(TOKEN)).await;
    call(&st, "PUT", "/config/config", b"{}".to_vec(), Some(TOKEN)).await;
    assert_eq!(
        call(&st, "DELETE", "/snapshots/aaaa", vec![], Some(TOKEN))
            .await
            .0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        call(&st, "DELETE", "/config/config", vec![], Some(TOKEN))
            .await
            .0,
        StatusCode::FORBIDDEN
    );
}

#[tokio::test]
async fn path_traversal_and_bad_kinds_are_rejected() {
    let dir = tempfile::TempDir::new().unwrap();
    let st = state(&dir, false);
    assert_eq!(
        call(
            &st,
            "PUT",
            "/snapshots/..%2F..%2Fetc",
            b"x".to_vec(),
            Some(TOKEN)
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        call(&st, "PUT", "/config/outro", b"x".to_vec(), Some(TOKEN))
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        call(&st, "GET", "/segredos", vec![], Some(TOKEN)).await.0,
        StatusCode::NOT_FOUND
    );
}
