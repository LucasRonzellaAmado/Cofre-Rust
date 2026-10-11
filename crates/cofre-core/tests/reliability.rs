#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::fs;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use cofre_core::backup::{BackupOptions, run_backup};
use cofre_core::check::{CheckOptions, run_check};
use cofre_core::error::BackendError;
use cofre_core::lock::{RepoLock, list_locks, remove_locks};
use cofre_core::prune::prune;
use cofre_core::restore::{RestoreOptions, run_restore};
use cofre_core::{Backend, Error, Kind, LocalBackend, Repository};
use common::{Fixture, PASSWORD, pseudo_random, restored_root, tree_contents};

struct FailingBackend {
    inner: LocalBackend,
    puts_left: AtomicUsize,
}

impl Backend for FailingBackend {
    fn put(&self, kind: Kind, name: &str, data: &[u8]) -> Result<(), BackendError> {
        if kind == Kind::Data || kind == Kind::Snapshot {
            let left = self.puts_left.load(Ordering::SeqCst);
            if left == 0 {
                return Err(BackendError::Io(std::io::Error::other(
                    "disco cheio simulado",
                )));
            }
            self.puts_left.store(left - 1, Ordering::SeqCst);
        }
        self.inner.put(kind, name, data)
    }
    fn get(&self, kind: Kind, name: &str) -> Result<Vec<u8>, BackendError> {
        self.inner.get(kind, name)
    }
    fn list(&self, kind: Kind) -> Result<Vec<String>, BackendError> {
        self.inner.list(kind)
    }
    fn exists(&self, kind: Kind, name: &str) -> Result<bool, BackendError> {
        self.inner.exists(kind, name)
    }
    fn delete(&self, kind: Kind, name: &str) -> Result<(), BackendError> {
        self.inner.delete(kind, name)
    }
    fn cleanup(&self) -> Result<u64, BackendError> {
        self.inner.cleanup()
    }
    fn describe(&self) -> String {
        "falha-simulada".into()
    }
}

#[test]
fn interrupted_backup_does_not_damage_previous_snapshots() {
    let fx = Fixture::new();
    let src = fx.source();
    fs::write(src.join("antigo.bin"), pseudo_random(100_000, 1)).unwrap();
    let good = fx.backup(&src);
    let before = tree_contents(&src);

    fs::write(src.join("novo.bin"), pseudo_random(400_000, 2)).unwrap();
    let failing: Arc<dyn Backend> = Arc::new(FailingBackend {
        inner: LocalBackend::new(fx.repo_path()),
        puts_left: AtomicUsize::new(5),
    });
    let repo2 = Repository::open(failing, PASSWORD).unwrap();
    let err = run_backup(&repo2, std::slice::from_ref(&src), &BackupOptions::new("t")).unwrap_err();
    assert!(matches!(err, Error::Backend(BackendError::Io(_))));

    assert_eq!(
        fx.repo.list_snapshot_ids().unwrap(),
        vec![good.snapshot_id.clone()]
    );
    assert!(list_locks(&fx.repo).unwrap().is_empty());

    let report = run_check(&fx.repo, &CheckOptions { read_data: true }).unwrap();
    assert!(report.is_ok());
    assert!(report.unreferenced_chunks > 0);

    let target = fx.restore_all(&good.snapshot_id);
    assert_eq!(tree_contents(&restored_root(&target, &src)), before);

    let pr = prune(&fx.repo, "t", false).unwrap();
    assert_eq!(pr.deleted, report.unreferenced_chunks);
    let after = run_check(&fx.repo, &CheckOptions { read_data: true }).unwrap();
    assert!(after.is_ok());
    assert_eq!(after.unreferenced_chunks, 0);

    let resumed = fx.backup(&src);
    let t2 = fx.restore_all(&resumed.snapshot_id);
    assert_eq!(
        tree_contents(&restored_root(&t2, &src)),
        tree_contents(&src)
    );
}

#[test]
fn leftover_temp_files_are_ignored_and_cleaned() {
    let fx = Fixture::new();
    let src = fx.source();
    fs::write(src.join("a"), b"abc").unwrap();
    fx.backup(&src);
    let snap_dir = fx.repo_path().join("snapshots");
    fs::write(snap_dir.join(".tmp-meio-escrito"), b"lixo").unwrap();
    assert_eq!(fx.repo.list_snapshot_ids().unwrap().len(), 1);
    let pr = prune(&fx.repo, "t", false).unwrap();
    assert_eq!(pr.temp_files_removed, 1);
    assert!(!snap_dir.join(".tmp-meio-escrito").exists());
}

#[test]
fn corrupted_chunk_is_detected_by_check_and_restore() {
    let fx = Fixture::new();
    let src = fx.source();
    fs::write(src.join("valioso.bin"), pseudo_random(50_000, 8)).unwrap();
    let report = fx.backup(&src);

    let data_dir = fx.repo_path().join("data");
    let sub = fs::read_dir(&data_dir)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let victim = fs::read_dir(&sub).unwrap().next().unwrap().unwrap().path();
    let mut bytes = fs::read(&victim).unwrap();
    let mid = bytes.len() / 2;
    bytes[mid] ^= 0x40;
    fs::write(&victim, bytes).unwrap();

    let quick = run_check(&fx.repo, &CheckOptions { read_data: false }).unwrap();
    assert!(quick.is_ok());
    let deep = run_check(&fx.repo, &CheckOptions { read_data: true }).unwrap();
    assert_eq!(deep.corrupted_chunks.len(), 1);

    let snap = fx.repo.load_snapshot(&report.snapshot_id).unwrap();
    let target = fx.dir.path().join("r");
    let err = run_restore(&fx.repo, &snap, &target, &RestoreOptions::default()).unwrap_err();
    assert!(matches!(err, Error::Corrupted(_)));
    let restored = restored_root(&target, &src).join("valioso.bin");
    assert!(!restored.exists());
}

#[test]
fn missing_chunk_is_reported() {
    let fx = Fixture::new();
    let src = fx.source();
    fs::write(src.join("x"), pseudo_random(20_000, 9)).unwrap();
    fx.backup(&src);
    let name = fx.backend.list(Kind::Data).unwrap().remove(0);
    fx.backend.delete(Kind::Data, &name).unwrap();
    let report = run_check(&fx.repo, &CheckOptions::default()).unwrap();
    assert_eq!(report.missing_chunks.len(), 1);
    assert!(!report.is_ok());
}

#[test]
fn swapped_snapshot_files_are_rejected() {
    let fx = Fixture::new();
    let src = fx.source();
    fs::write(src.join("x"), b"1").unwrap();
    let a = fx.backup(&src).snapshot_id;
    fs::write(src.join("x"), b"2").unwrap();
    let b = fx.backup(&src).snapshot_id;
    let dir = fx.repo_path().join("snapshots");
    let blob_b = fs::read(dir.join(&b)).unwrap();
    fs::write(dir.join(&a), blob_b).unwrap();
    assert!(matches!(
        fx.repo.load_snapshot(&a),
        Err(Error::Corrupted(_))
    ));
}

#[test]
fn local_backend_never_overwrites() {
    let dir = tempfile::TempDir::new().unwrap();
    let b = LocalBackend::new(dir.path());
    b.put(Kind::Snapshot, "abcd", b"primeiro").unwrap();
    assert!(matches!(
        b.put(Kind::Snapshot, "abcd", b"segundo"),
        Err(BackendError::AlreadyExists(_))
    ));
    assert_eq!(b.get(Kind::Snapshot, "abcd").unwrap(), b"primeiro");
    assert!(matches!(
        b.put(Kind::Snapshot, "../fuga", b"x"),
        Err(BackendError::InvalidName(_))
    ));
}

#[test]
fn prune_refuses_while_backup_lock_is_held() {
    let fx = Fixture::new();
    let held = RepoLock::acquire(&fx.repo, false, "outra-maquina").unwrap();
    assert!(matches!(prune(&fx.repo, "t", false), Err(Error::Locked(_))));
    held.release().unwrap();
    assert!(prune(&fx.repo, "t", false).is_ok());
}

#[test]
fn backup_refuses_while_prune_lock_is_held() {
    let fx = Fixture::new();
    let src = fx.source();
    fs::write(src.join("x"), b"1").unwrap();
    let held = RepoLock::acquire(&fx.repo, true, "admin").unwrap();
    let err = run_backup(&fx.repo, &[src], &BackupOptions::new("t")).unwrap_err();
    assert!(matches!(err, Error::Locked(_)));
    drop(held);
    assert!(list_locks(&fx.repo).unwrap().is_empty());
}

#[test]
fn shared_locks_coexist_and_unlock_removes_them() {
    let fx = Fixture::new();
    let a = RepoLock::acquire(&fx.repo, false, "a").unwrap();
    let b = RepoLock::acquire(&fx.repo, false, "b").unwrap();
    assert_eq!(list_locks(&fx.repo).unwrap().len(), 2);
    assert_eq!(remove_locks(&fx.repo, false).unwrap(), 0);
    std::mem::forget(a);
    std::mem::forget(b);
    assert_eq!(remove_locks(&fx.repo, true).unwrap(), 2);
}
