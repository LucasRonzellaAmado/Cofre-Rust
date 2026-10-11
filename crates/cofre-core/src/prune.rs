use std::collections::HashSet;

use rayon::prelude::*;

use crate::backend::Kind;
use crate::error::{BackendError, Error, Result};
use crate::id::ChunkId;
use crate::lock::RepoLock;
use crate::repo::Repository;
use crate::retention::{Candidate, Decision, RetentionPolicy, apply_policy};

#[derive(Clone, Debug, Default)]
pub struct PruneReport {
    pub referenced: usize,
    pub stored: usize,
    pub deleted: usize,
    pub temp_files_removed: u64,
}

#[derive(Clone, Debug, Default)]
pub struct ForgetReport {
    pub decision: Decision,
    pub deleted: usize,
}

pub fn group_key(hostname: &str, paths: &[String]) -> String {
    let mut p = paths.to_vec();
    p.sort();
    format!("{hostname}\u{1f}{}", p.join("\u{1e}"))
}

pub fn forget(
    repo: &Repository,
    policy: &RetentionPolicy,
    hostname: &str,
    dry_run: bool,
) -> Result<ForgetReport> {
    if policy.is_empty() {
        return Err(Error::InvalidParameter(
            "política de retenção vazia apagaria tudo; informe ao menos uma regra --keep-*".into(),
        ));
    }
    let lock = RepoLock::acquire(repo, false, hostname)?;
    let snaps = repo.load_all_snapshots()?;
    let candidates: Vec<Candidate> = snaps
        .iter()
        .map(|(id, s)| Candidate {
            id: id.clone(),
            time: s.time,
            group: group_key(&s.hostname, &s.paths),
        })
        .collect();
    let decision = apply_policy(&candidates, policy);
    let mut deleted = 0;
    if !dry_run {
        for id in &decision.remove {
            repo.delete_snapshot(id)?;
            deleted += 1;
        }
    }
    lock.release()?;
    Ok(ForgetReport { decision, deleted })
}

pub fn forget_ids(repo: &Repository, ids: &[String], hostname: &str) -> Result<usize> {
    let lock = RepoLock::acquire(repo, false, hostname)?;
    let mut deleted = 0;
    for spec in ids {
        let id = repo.resolve_snapshot(spec)?;
        repo.delete_snapshot(&id)?;
        deleted += 1;
    }
    lock.release()?;
    Ok(deleted)
}

pub fn prune(repo: &Repository, hostname: &str, dry_run: bool) -> Result<PruneReport> {
    let lock = RepoLock::acquire(repo, true, hostname)?;
    let mut referenced: HashSet<ChunkId> = HashSet::new();
    for id in repo.list_snapshot_ids()? {
        let snap = repo.load_snapshot(&id).map_err(|e| {
            Error::Corrupted(format!(
                "snapshot {id} ilegível ({e}); prune abortado para não apagar blocos em uso"
            ))
        })?;
        referenced.extend(snap.chunk_ids().copied());
    }
    let stored = repo.list_chunks()?;
    let orphans: Vec<&ChunkId> = stored.difference(&referenced).collect();
    let mut report = PruneReport {
        referenced: referenced.len(),
        stored: stored.len(),
        deleted: 0,
        temp_files_removed: 0,
    };
    if !dry_run {
        let backend = repo.backend();
        orphans
            .par_iter()
            .map(|id| match backend.delete(Kind::Data, &id.to_hex()) {
                Ok(()) | Err(BackendError::NotFound(_)) => Ok(()),
                Err(e) => Err(Error::from(e)),
            })
            .collect::<Result<()>>()?;
        report.deleted = orphans.len();
        report.temp_files_removed = backend.cleanup()?;
    } else {
        report.deleted = orphans.len();
    }
    lock.release()?;
    Ok(report)
}
