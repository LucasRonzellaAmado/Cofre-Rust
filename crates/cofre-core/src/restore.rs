use std::fs::{self, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use filetime::FileTime;
use rayon::prelude::*;

use crate::error::{Error, Result};
use crate::id::ChunkId;
use crate::paths::{key_matches_prefix, key_to_relative, normalize_key};
use crate::repo::Repository;
use crate::snapshot::{Node, NodeKind, Snapshot};

#[derive(Clone, Debug, Default)]
pub struct RestoreOptions {
    pub include: Option<String>,
    pub overwrite: bool,
}

#[derive(Clone, Debug, Default)]
pub struct RestoreReport {
    pub files: u64,
    pub dirs: u64,
    pub symlinks: u64,
    pub bytes: u64,
    pub skipped: u64,
    pub warnings: Vec<String>,
}

pub fn select_nodes<'a>(snapshot: &'a Snapshot, include: Option<&str>) -> Vec<&'a Node> {
    let prefix = include.map(normalize_key).unwrap_or_default();
    snapshot
        .nodes
        .iter()
        .filter(|n| key_matches_prefix(&n.path, &prefix))
        .collect()
}

fn set_times(path: &Path, node: &Node) -> std::io::Result<()> {
    let t = FileTime::from_unix_time(node.mtime, node.mtime_nanos);
    filetime::set_file_mtime(path, t)
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
}

#[cfg(not(unix))]
fn set_mode(path: &Path, mode: u32) -> std::io::Result<()> {
    let mut perms = fs::metadata(path)?.permissions();
    perms.set_readonly(mode & 0o222 == 0);
    fs::set_permissions(path, perms)
}

#[cfg(unix)]
fn make_symlink(target: &str, link: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

#[cfg(windows)]
fn make_symlink(target: &str, link: &Path) -> std::io::Result<()> {
    std::os::windows::fs::symlink_file(target, link)
}

fn restore_file(repo: &Repository, dest: &Path, node: &Node, chunks: &[ChunkId]) -> Result<u64> {
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent).map_err(|e| Error::io(parent, e))?;
    }
    let file_name = dest
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let tmp = dest.with_file_name(format!(".{file_name}.cofre-restore"));
    let result = (|| -> Result<u64> {
        let file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&tmp)
            .map_err(|e| Error::io(&tmp, e))?;
        let mut writer = BufWriter::with_capacity(1 << 20, file);
        let mut written = 0u64;
        for id in chunks {
            let data = repo.read_chunk(id)?;
            writer.write_all(&data).map_err(|e| Error::io(&tmp, e))?;
            written += data.len() as u64;
        }
        let file = writer
            .into_inner()
            .map_err(|e| Error::io(&tmp, e.into_error()))?;
        file.sync_all().map_err(|e| Error::io(&tmp, e))?;
        drop(file);
        if written != node.size() {
            return Err(Error::Corrupted(format!(
                "{}: tamanho restaurado {written} difere do registrado {}",
                node.path,
                node.size()
            )));
        }
        if dest.exists() {
            fs::remove_file(dest).map_err(|e| Error::io(dest, e))?;
        }
        fs::rename(&tmp, dest).map_err(|e| Error::io(dest, e))?;
        Ok(written)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    let written = result?;
    set_mode(dest, node.mode).map_err(|e| Error::io(dest, e))?;
    set_times(dest, node).map_err(|e| Error::io(dest, e))?;
    Ok(written)
}

pub fn run_restore(
    repo: &Repository,
    snapshot: &Snapshot,
    target: &Path,
    opts: &RestoreOptions,
) -> Result<RestoreReport> {
    let nodes = select_nodes(snapshot, opts.include.as_deref());
    if nodes.is_empty() {
        return Err(Error::InvalidPath(format!(
            "nada no snapshot corresponde a '{}'",
            opts.include.as_deref().unwrap_or("")
        )));
    }
    fs::create_dir_all(target).map_err(|e| Error::io(target, e))?;

    let mut planned: Vec<(PathBuf, &Node)> = Vec::with_capacity(nodes.len());
    for node in nodes {
        planned.push((target.join(key_to_relative(&node.path)?), node));
    }

    let report = Mutex::new(RestoreReport::default());
    let add_warning = |msg: String| {
        tracing::warn!("{msg}");
        let mut r = report.lock().unwrap_or_else(|p| p.into_inner());
        r.warnings.push(msg);
        r.skipped += 1;
    };

    for (dest, node) in &planned {
        if matches!(node.kind, NodeKind::Dir) {
            fs::create_dir_all(dest).map_err(|e| Error::io(dest, e))?;
            report.lock().unwrap_or_else(|p| p.into_inner()).dirs += 1;
        }
    }

    planned
        .par_iter()
        .filter(|(_, n)| matches!(n.kind, NodeKind::File { .. }))
        .map(|(dest, node)| -> Result<()> {
            let NodeKind::File { chunks, .. } = &node.kind else {
                return Ok(());
            };
            if dest.exists() && !opts.overwrite {
                add_warning(format!("{} já existe, mantido", dest.display()));
                return Ok(());
            }
            let written = restore_file(repo, dest, node, chunks)?;
            let mut r = report.lock().unwrap_or_else(|p| p.into_inner());
            r.files += 1;
            r.bytes += written;
            Ok(())
        })
        .collect::<Result<()>>()?;

    for (dest, node) in &planned {
        if let NodeKind::Symlink {
            target: link_target,
        } = &node.kind
        {
            if let Some(parent) = dest.parent() {
                fs::create_dir_all(parent).map_err(|e| Error::io(parent, e))?;
            }
            if fs::symlink_metadata(dest).is_ok() {
                if !opts.overwrite {
                    add_warning(format!("{} já existe, mantido", dest.display()));
                    continue;
                }
                fs::remove_file(dest).map_err(|e| Error::io(dest, e))?;
            }
            match make_symlink(link_target, dest) {
                Ok(()) => report.lock().unwrap_or_else(|p| p.into_inner()).symlinks += 1,
                Err(e) => add_warning(format!("link {} não criado: {e}", dest.display())),
            }
        }
    }

    for (dest, node) in planned.iter().rev() {
        if matches!(node.kind, NodeKind::Dir) {
            if let Err(e) = set_mode(dest, node.mode).and_then(|()| set_times(dest, node)) {
                add_warning(format!(
                    "metadados de {} não aplicados: {e}",
                    dest.display()
                ));
            }
        }
    }

    Ok(report.into_inner().unwrap_or_else(|p| p.into_inner()))
}
