use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::backend::Kind;
use crate::error::{BackendError, Error, Result};
use crate::id::random_hex_id;
use crate::repo::Repository;
use crate::time::now_unix;

pub const STALE_AFTER_SECS: i64 = 30 * 60;
pub const REFRESH_EVERY: Duration = Duration::from_secs(5 * 60);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LockInfo {
    pub exclusive: bool,
    pub hostname: String,
    pub pid: u32,
    pub time: i64,
}

#[derive(Debug, Clone)]
pub struct LockEntry {
    pub name: String,
    pub info: Option<LockInfo>,
}

impl LockEntry {
    pub fn is_stale(&self, now: i64) -> bool {
        match &self.info {
            Some(info) => now - info.time > STALE_AFTER_SECS,
            None => false,
        }
    }

    pub fn describe(&self) -> String {
        match &self.info {
            Some(i) => format!(
                "{} ({} pid {} {})",
                &self.name[..8.min(self.name.len())],
                i.hostname,
                i.pid,
                if i.exclusive {
                    "exclusivo"
                } else {
                    "compartilhado"
                }
            ),
            None => format!("{} (ilegível)", &self.name[..8.min(self.name.len())]),
        }
    }
}

fn write_lock(repo: &Repository, name: &str, info: &LockInfo) -> Result<()> {
    let json = serde_json::to_vec(info).map_err(|e| Error::Serialization(e.to_string()))?;
    let blob = repo.seal_named("lock", name, &json)?;
    repo.backend().put(Kind::Lock, name, &blob)?;
    Ok(())
}

pub fn list_locks(repo: &Repository) -> Result<Vec<LockEntry>> {
    let mut out = Vec::new();
    for name in repo.backend().list(Kind::Lock)? {
        let info = match repo.backend().get(Kind::Lock, &name) {
            Ok(blob) => repo
                .open_named("lock", &name, &blob)
                .ok()
                .and_then(|plain| serde_json::from_slice::<LockInfo>(&plain).ok()),
            Err(BackendError::NotFound(_)) => continue,
            Err(e) => return Err(e.into()),
        };
        out.push(LockEntry { name, info });
    }
    Ok(out)
}

pub fn remove_locks(repo: &Repository, include_active: bool) -> Result<usize> {
    let now = now_unix();
    let mut removed = 0;
    for entry in list_locks(repo)? {
        if include_active || entry.is_stale(now) {
            match repo.backend().delete(Kind::Lock, &entry.name) {
                Ok(()) | Err(BackendError::NotFound(_)) => removed += 1,
                Err(e) => return Err(e.into()),
            }
        }
    }
    Ok(removed)
}

pub struct RepoLock {
    repo: Repository,
    current: Arc<Mutex<String>>,
    stop: Option<Sender<()>>,
    handle: Option<JoinHandle<()>>,
}

impl RepoLock {
    pub fn acquire(repo: &Repository, exclusive: bool, hostname: &str) -> Result<Self> {
        let name = random_hex_id();
        let info = LockInfo {
            exclusive,
            hostname: hostname.to_string(),
            pid: std::process::id(),
            time: now_unix(),
        };
        write_lock(repo, &name, &info)?;
        let now = now_unix();
        let conflicts: Vec<LockEntry> = match list_locks(repo) {
            Ok(list) => list
                .into_iter()
                .filter(|e| e.name != name && !e.is_stale(now))
                .filter(|e| exclusive || e.info.as_ref().is_none_or(|i| i.exclusive))
                .collect(),
            Err(e) => {
                let _ = repo.backend().delete(Kind::Lock, &name);
                return Err(e);
            }
        };
        if !conflicts.is_empty() {
            let _ = repo.backend().delete(Kind::Lock, &name);
            let desc: Vec<String> = conflicts.iter().map(LockEntry::describe).collect();
            return Err(Error::Locked(format!(
                "outra operação está em andamento: {}. Se tiver certeza de que nada está rodando, use `cofre unlock`",
                desc.join(", ")
            )));
        }

        let current = Arc::new(Mutex::new(name));
        let (tx, rx) = mpsc::channel::<()>();
        let thread_repo = repo.clone();
        let thread_current = Arc::clone(&current);
        let handle = std::thread::Builder::new()
            .name("cofre-lock-refresh".into())
            .spawn(move || {
                while let Err(RecvTimeoutError::Timeout) = rx.recv_timeout(REFRESH_EVERY) {
                    let new_name = random_hex_id();
                    let new_info = LockInfo {
                        time: now_unix(),
                        ..info.clone()
                    };
                    if let Err(e) = write_lock(&thread_repo, &new_name, &new_info) {
                        tracing::warn!(error = %e, "falha ao renovar trava");
                        continue;
                    }
                    let mut guard = thread_current.lock().unwrap_or_else(|p| p.into_inner());
                    let old = std::mem::replace(&mut *guard, new_name);
                    drop(guard);
                    let _ = thread_repo.backend().delete(Kind::Lock, &old);
                }
            })
            .map_err(|e| Error::io("thread de trava", e))?;

        Ok(RepoLock {
            repo: repo.clone(),
            current,
            stop: Some(tx),
            handle: Some(handle),
        })
    }

    pub fn release(mut self) -> Result<()> {
        self.shutdown()
    }

    fn shutdown(&mut self) -> Result<()> {
        if let Some(tx) = self.stop.take() {
            let _ = tx.send(());
        }
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
        let name = self
            .current
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone();
        match self.repo.backend().delete(Kind::Lock, &name) {
            Ok(()) | Err(BackendError::NotFound(_)) => Ok(()),
            Err(e) => Err(e.into()),
        }
    }
}

impl Drop for RepoLock {
    fn drop(&mut self) {
        if self.stop.is_some() {
            if let Err(e) = self.shutdown() {
                tracing::warn!(error = %e, "falha ao remover trava");
            }
        }
    }
}
