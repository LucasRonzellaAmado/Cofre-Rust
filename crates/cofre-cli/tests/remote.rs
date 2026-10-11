#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::fs;
use std::net::SocketAddr;
use std::sync::Arc;

use cofre_cli::http::HttpBackend;
use cofre_core::backup::{BackupOptions, run_backup};
use cofre_core::chunker::{ChunkerAlgorithm, ChunkerParams};
use cofre_core::crypto::KdfParams;
use cofre_core::prune::prune;
use cofre_core::restore::{RestoreOptions, run_restore};
use cofre_core::{Backend, Error, InitOptions, Repository};
use cofre_server::{AppState, app};
use zeroize::Zeroizing;

const TOKEN: &str = "token-de-teste-com-mais-de-32-caracteres";

fn start_server(root: &std::path::Path, append_only: bool) -> SocketAddr {
    let state = AppState::new(root, append_only, Some(Zeroizing::new(TOKEN.into())));
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async move {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            tx.send(listener.local_addr().unwrap()).unwrap();
            axum::serve(listener, app(state)).await.unwrap();
        });
    });
    rx.recv().unwrap()
}

fn opts() -> InitOptions {
    InitOptions {
        kdf: KdfParams::new(1024, 1, 1),
        chunker: ChunkerParams {
            algorithm: ChunkerAlgorithm::Fastcdc2020,
            min_size: 2048,
            avg_size: 8192,
            max_size: 32768,
        },
        compression_level: 3,
    }
}

#[test]
fn backup_and_restore_over_http_with_append_only() {
    let dir = tempfile::TempDir::new().unwrap();
    let addr = start_server(&dir.path().join("server"), true);
    let backend: Arc<dyn Backend> = Arc::new(
        HttpBackend::new(
            &format!("http://{addr}"),
            Some(Zeroizing::new(TOKEN.into())),
        )
        .unwrap(),
    );
    let repo = Repository::init(backend.clone(), b"senha", opts()).unwrap();

    let src = dir.path().join("src");
    fs::create_dir_all(&src).unwrap();
    let data: Vec<u8> = (0..200_000u32)
        .flat_map(|i| i.wrapping_mul(2654435761).to_le_bytes())
        .collect();
    fs::write(src.join("planilha.bin"), &data).unwrap();
    let report = run_backup(&repo, std::slice::from_ref(&src), &BackupOptions::new("t")).unwrap();
    assert!(report.summary.new_chunks > 10);

    let again = Repository::open(backend.clone(), b"senha").unwrap();
    let snap = again.load_snapshot(&report.snapshot_id).unwrap();
    let target = dir.path().join("restore");
    run_restore(&again, &snap, &target, &RestoreOptions::default()).unwrap();
    let canon = fs::canonicalize(&src).unwrap();
    let rel = canon.strip_prefix("/").unwrap_or(&canon);
    assert_eq!(
        fs::read(target.join(rel).join("planilha.bin")).unwrap(),
        data
    );

    let err = again.delete_snapshot(&report.snapshot_id).unwrap_err();
    assert!(matches!(
        err,
        Error::Backend(cofre_core::BackendError::Forbidden(_))
    ));
    let orphan = b"bloco orfao vindo de um backup interrompido";
    again.write_chunk(&again.chunk_id(orphan), orphan).unwrap();
    assert!(prune(&again, "t", false).is_err());
    assert_eq!(again.list_snapshot_ids().unwrap().len(), 1);
}

#[test]
fn wrong_token_is_rejected() {
    let dir = tempfile::TempDir::new().unwrap();
    let addr = start_server(dir.path(), false);
    let backend: Arc<dyn Backend> = Arc::new(
        HttpBackend::new(
            &format!("http://{addr}"),
            Some(Zeroizing::new("errado".into())),
        )
        .unwrap(),
    );
    assert!(Repository::init(backend, b"senha", opts()).is_err());
}
