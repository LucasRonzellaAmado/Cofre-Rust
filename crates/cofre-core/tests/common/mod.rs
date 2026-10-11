#![allow(dead_code, clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use cofre_core::backup::{BackupOptions, BackupReport, run_backup};
use cofre_core::chunker::{ChunkerAlgorithm, ChunkerParams};
use cofre_core::crypto::KdfParams;
use cofre_core::restore::{RestoreOptions, run_restore};
use cofre_core::{Backend, InitOptions, LocalBackend, Repository};
use tempfile::TempDir;

pub const PASSWORD: &[u8] = b"senha-de-teste";

pub fn test_options() -> InitOptions {
    InitOptions {
        kdf: KdfParams::new(1024, 1, 1),
        chunker: ChunkerParams {
            algorithm: ChunkerAlgorithm::Fastcdc2020,
            min_size: 2 * 1024,
            avg_size: 8 * 1024,
            max_size: 32 * 1024,
        },
        compression_level: 3,
    }
}

pub struct Fixture {
    pub dir: TempDir,
    pub backend: Arc<dyn Backend>,
    pub repo: Repository,
}

impl Fixture {
    pub fn new() -> Self {
        let dir = TempDir::new().unwrap();
        let backend: Arc<dyn Backend> = Arc::new(LocalBackend::new(dir.path().join("repo")));
        let repo = Repository::init(backend.clone(), PASSWORD, test_options()).unwrap();
        Fixture { dir, backend, repo }
    }

    pub fn repo_path(&self) -> PathBuf {
        self.dir.path().join("repo")
    }

    pub fn source(&self) -> PathBuf {
        let p = self.dir.path().join("src");
        fs::create_dir_all(&p).unwrap();
        p
    }

    pub fn backup(&self, path: &Path) -> BackupReport {
        run_backup(
            &self.repo,
            &[path.to_path_buf()],
            &BackupOptions::new("teste"),
        )
        .unwrap()
    }

    pub fn restore_all(&self, snapshot_id: &str) -> PathBuf {
        let target = self
            .dir
            .path()
            .join(format!("restore-{}", &snapshot_id[..8]));
        let snap = self.repo.load_snapshot(snapshot_id).unwrap();
        run_restore(&self.repo, &snap, &target, &RestoreOptions::default()).unwrap();
        target
    }
}

pub fn pseudo_random(len: usize, seed: u64) -> Vec<u8> {
    let mut x = seed
        .wrapping_mul(6364136223846793005)
        .wrapping_add(1442695040888963407)
        | 1;
    let mut out = Vec::with_capacity(len);
    while out.len() < len {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        out.extend_from_slice(&x.to_le_bytes());
    }
    out.truncate(len);
    out
}

pub fn tree_contents(root: &Path) -> BTreeMap<String, Option<Vec<u8>>> {
    let mut out = BTreeMap::new();
    for entry in walkdir::WalkDir::new(root).min_depth(1).sort_by_file_name() {
        let entry = entry.unwrap();
        let rel = entry
            .path()
            .strip_prefix(root)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        let ft = entry.file_type();
        if ft.is_file() {
            out.insert(rel, Some(fs::read(entry.path()).unwrap()));
        } else if ft.is_dir() {
            out.insert(rel, None);
        } else if ft.is_symlink() {
            let t = fs::read_link(entry.path()).unwrap();
            out.insert(
                rel,
                Some(format!("link:{}", t.to_string_lossy()).into_bytes()),
            );
        }
    }
    out
}

pub fn restored_root(target: &Path, source: &Path) -> PathBuf {
    let abs = fs::canonicalize(source).unwrap();
    let key = cofre_core::paths::path_to_key(&abs).unwrap();
    target.join(cofre_core::paths::key_to_relative(&key).unwrap())
}
