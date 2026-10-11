use std::collections::{BTreeMap, HashSet};
use std::fs::{self, File, Metadata};
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Instant;

use globset::{Glob, GlobSet, GlobSetBuilder};
use rayon::prelude::*;
use walkdir::WalkDir;

use crate::error::{Error, Result};
use crate::id::ChunkId;
use crate::lock::RepoLock;
use crate::paths::path_to_key;
use crate::repo::Repository;
use crate::snapshot::{Node, NodeKind, Snapshot, Summary};
use crate::time::{now_unix, system_time_to_unix};

#[derive(Clone, Debug)]
pub struct BackupOptions {
    pub hostname: String,
    pub tags: Vec<String>,
    pub excludes: Vec<String>,
    pub batch_size: usize,
}

impl BackupOptions {
    pub fn new(hostname: impl Into<String>) -> Self {
        BackupOptions {
            hostname: hostname.into(),
            tags: Vec::new(),
            excludes: Vec::new(),
            batch_size: 16,
        }
    }
}

#[derive(Clone, Debug)]
pub struct BackupReport {
    pub snapshot_id: String,
    pub summary: Summary,
    pub warnings: Vec<String>,
}

struct Ctx<'a> {
    repo: &'a Repository,
    known: Mutex<HashSet<ChunkId>>,
    excludes: GlobSet,
    batch_size: usize,
    summary: Summary,
    warnings: Vec<String>,
}

impl Ctx<'_> {
    fn warn(&mut self, msg: String) {
        tracing::warn!("{msg}");
        self.warnings.push(msg);
        self.summary.warnings += 1;
    }

    fn is_excluded(&self, path: &Path) -> bool {
        if self.excludes.is_empty() {
            return false;
        }
        if self.excludes.is_match(path) {
            return true;
        }
        path.file_name()
            .is_some_and(|name| self.excludes.is_match(Path::new(name)))
    }

    fn store_batch(&mut self, batch: &[Vec<u8>], out: &mut Vec<ChunkId>) -> Result<()> {
        let repo = self.repo;
        let known = &self.known;
        let ids: Vec<ChunkId> = batch.par_iter().map(|d| repo.chunk_id(d)).collect();
        let stored: Vec<Option<u64>> = batch
            .par_iter()
            .zip(ids.par_iter())
            .map(|(data, id)| {
                let fresh = known.lock().unwrap_or_else(|p| p.into_inner()).insert(*id);
                if fresh {
                    repo.write_chunk(id, data)
                } else {
                    Ok(None)
                }
            })
            .collect::<Result<_>>()?;
        for written in stored.into_iter().flatten() {
            self.summary.new_chunks += 1;
            self.summary.new_bytes_stored += written;
        }
        out.extend(ids);
        Ok(())
    }

    fn backup_file(
        &mut self,
        path: &Path,
        before: &Metadata,
    ) -> Result<Option<(u64, Vec<ChunkId>)>> {
        let file = match File::open(path) {
            Ok(f) => f,
            Err(e) => {
                self.warn(format!("não foi possível abrir {}: {e}", path.display()));
                return Ok(None);
            }
        };
        let params = self.repo.config().chunker;
        let mut chunks = Vec::new();
        let mut batch: Vec<Vec<u8>> = Vec::with_capacity(self.batch_size);
        let mut size: u64 = 0;
        for piece in params.chunks(BufReader::with_capacity(1 << 20, file)) {
            let data = match piece {
                Ok(d) => d,
                Err(e) => {
                    self.warn(format!("erro lendo {}: {e}", path.display()));
                    return Ok(None);
                }
            };
            size += data.len() as u64;
            batch.push(data);
            if batch.len() >= self.batch_size {
                self.store_batch(&batch, &mut chunks)?;
                batch.clear();
            }
        }
        if !batch.is_empty() {
            self.store_batch(&batch, &mut chunks)?;
        }
        if let Ok(after) = fs::metadata(path) {
            let changed = after.len() != before.len()
                || after.modified().ok() != before.modified().ok()
                || size != before.len();
            if changed {
                self.warn(format!(
                    "{} mudou durante o backup; foi salvo o conteúdo lido ({} bytes)",
                    path.display(),
                    size
                ));
            }
        }
        Ok(Some((size, chunks)))
    }
}

#[cfg(unix)]
fn mode_of(meta: &Metadata) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    meta.permissions().mode() & 0o7777
}

#[cfg(not(unix))]
fn mode_of(meta: &Metadata) -> u32 {
    if meta.permissions().readonly() {
        0o444
    } else {
        0o644
    }
}

fn mtime_of(meta: &Metadata) -> (i64, u32) {
    meta.modified().map(system_time_to_unix).unwrap_or((0, 0))
}

fn build_excludes(patterns: &[String]) -> Result<GlobSet> {
    let mut builder = GlobSetBuilder::new();
    for p in patterns {
        let glob = Glob::new(p)
            .map_err(|e| Error::InvalidParameter(format!("padrão de exclusão '{p}': {e}")))?;
        builder.add(glob);
    }
    builder
        .build()
        .map_err(|e| Error::InvalidParameter(format!("padrões de exclusão: {e}")))
}

pub fn run_backup(
    repo: &Repository,
    sources: &[PathBuf],
    opts: &BackupOptions,
) -> Result<BackupReport> {
    if sources.is_empty() {
        return Err(Error::InvalidParameter("nenhum caminho para backup".into()));
    }
    let started = Instant::now();
    let lock = RepoLock::acquire(repo, false, &opts.hostname)?;

    let mut roots = Vec::with_capacity(sources.len());
    for src in sources {
        let abs = fs::canonicalize(src).map_err(|e| Error::io(src, e))?;
        roots.push(abs);
    }

    let known = repo.list_chunks()?;
    let mut ctx = Ctx {
        repo,
        known: Mutex::new(known),
        excludes: build_excludes(&opts.excludes)?,
        batch_size: opts.batch_size.max(1),
        summary: Summary::default(),
        warnings: Vec::new(),
    };

    let mut nodes: BTreeMap<String, Node> = BTreeMap::new();
    for root in &roots {
        let walker = WalkDir::new(root)
            .follow_links(false)
            .sort_by_file_name()
            .into_iter();
        let mut it = walker;
        while let Some(entry) = it.next() {
            let entry = match entry {
                Ok(e) => e,
                Err(e) => {
                    ctx.warn(format!("erro ao percorrer: {e}"));
                    continue;
                }
            };
            let path = entry.path();
            if entry.depth() > 0 && ctx.is_excluded(path) {
                if entry.file_type().is_dir() {
                    it.skip_current_dir();
                }
                continue;
            }
            let key = match path_to_key(path) {
                Ok(k) => k,
                Err(e) => {
                    ctx.warn(format!("ignorado {}: {e}", path.display()));
                    continue;
                }
            };
            if key.is_empty() || nodes.contains_key(&key) {
                continue;
            }
            let meta = match fs::symlink_metadata(path) {
                Ok(m) => m,
                Err(e) => {
                    ctx.warn(format!("sem metadados para {}: {e}", path.display()));
                    continue;
                }
            };
            let (mtime, mtime_nanos) = mtime_of(&meta);
            let mode = mode_of(&meta);
            let ft = meta.file_type();
            let kind = if ft.is_dir() {
                ctx.summary.dirs += 1;
                NodeKind::Dir
            } else if ft.is_symlink() {
                match fs::read_link(path) {
                    Ok(target) => match target.to_str() {
                        Some(t) => {
                            ctx.summary.symlinks += 1;
                            NodeKind::Symlink {
                                target: t.to_string(),
                            }
                        }
                        None => {
                            ctx.warn(format!("link com destino não UTF-8: {}", path.display()));
                            continue;
                        }
                    },
                    Err(e) => {
                        ctx.warn(format!("não foi possível ler link {}: {e}", path.display()));
                        continue;
                    }
                }
            } else if ft.is_file() {
                match ctx.backup_file(path, &meta)? {
                    Some((size, chunks)) => {
                        ctx.summary.files += 1;
                        ctx.summary.bytes += size;
                        NodeKind::File { size, chunks }
                    }
                    None => continue,
                }
            } else {
                ctx.warn(format!("tipo especial ignorado: {}", path.display()));
                continue;
            };
            nodes.insert(
                key.clone(),
                Node {
                    path: key,
                    kind,
                    mode,
                    mtime,
                    mtime_nanos,
                },
            );
        }
    }

    let mut paths = Vec::with_capacity(roots.len());
    for r in &roots {
        paths.push(path_to_key(r)?);
    }
    ctx.summary.duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    let snapshot = Snapshot {
        time: now_unix(),
        hostname: opts.hostname.clone(),
        paths,
        tags: opts.tags.clone(),
        nodes: nodes.into_values().collect(),
        summary: ctx.summary.clone(),
    };
    let snapshot_id = repo.save_snapshot(&snapshot)?;
    lock.release()?;
    Ok(BackupReport {
        snapshot_id,
        summary: ctx.summary,
        warnings: ctx.warnings,
    })
}
