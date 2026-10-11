use std::collections::HashSet;
use std::sync::Mutex;

use rayon::prelude::*;

use crate::error::Result;
use crate::id::ChunkId;
use crate::repo::Repository;

#[derive(Clone, Debug, Default)]
pub struct CheckOptions {
    pub read_data: bool,
}

#[derive(Clone, Debug, Default)]
pub struct CheckReport {
    pub snapshots: usize,
    pub broken_snapshots: Vec<(String, String)>,
    pub referenced_chunks: usize,
    pub stored_chunks: usize,
    pub missing_chunks: Vec<ChunkId>,
    pub corrupted_chunks: Vec<(ChunkId, String)>,
    pub unreferenced_chunks: usize,
    pub chunks_read: usize,
}

impl CheckReport {
    pub fn is_ok(&self) -> bool {
        self.broken_snapshots.is_empty()
            && self.missing_chunks.is_empty()
            && self.corrupted_chunks.is_empty()
    }
}

pub fn run_check(repo: &Repository, opts: &CheckOptions) -> Result<CheckReport> {
    let mut report = CheckReport::default();
    let mut referenced: HashSet<ChunkId> = HashSet::new();
    for id in repo.list_snapshot_ids()? {
        report.snapshots += 1;
        match repo.load_snapshot(&id) {
            Ok(snap) => referenced.extend(snap.chunk_ids().copied()),
            Err(e) => report.broken_snapshots.push((id, e.to_string())),
        }
    }
    let stored = repo.list_chunks()?;
    report.referenced_chunks = referenced.len();
    report.stored_chunks = stored.len();
    report.unreferenced_chunks = stored.difference(&referenced).count();
    let mut missing: Vec<ChunkId> = referenced.difference(&stored).copied().collect();
    missing.sort();
    report.missing_chunks = missing;

    if opts.read_data {
        let corrupted = Mutex::new(Vec::new());
        let to_read: Vec<&ChunkId> = referenced.intersection(&stored).collect();
        report.chunks_read = to_read.len();
        to_read.par_iter().for_each(|id| {
            if let Err(e) = repo.read_chunk(id) {
                corrupted
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .push((**id, e.to_string()));
            }
        });
        let mut c = corrupted.into_inner().unwrap_or_else(|p| p.into_inner());
        c.sort_by_key(|a| a.0);
        report.corrupted_chunks = c;
    }
    Ok(report)
}
