use std::fs::{self, File, OpenOptions};
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};

use crate::backend::{Backend, Kind, validate_name};
use crate::error::BackendError;

const TMP_PREFIX: &str = ".tmp-";

#[derive(Debug, Clone)]
pub struct LocalBackend {
    root: PathBuf,
}

impl LocalBackend {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        LocalBackend { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn path(&self, kind: Kind, name: &str) -> Result<PathBuf, BackendError> {
        validate_name(kind, name)?;
        Ok(self.root.join(kind.relative_path(name)))
    }

    fn map_not_found(err: std::io::Error, name: &str) -> BackendError {
        if err.kind() == ErrorKind::NotFound {
            BackendError::NotFound(name.to_string())
        } else {
            BackendError::Io(err)
        }
    }

    fn write_tmp(tmp: &Path, data: &[u8]) -> std::io::Result<()> {
        let mut f = OpenOptions::new().write(true).create_new(true).open(tmp)?;
        f.write_all(data)?;
        f.sync_all()
    }

    fn list_dir_files(dir: &Path, out: &mut Vec<String>) -> Result<(), BackendError> {
        let entries = match fs::read_dir(dir) {
            Ok(e) => e,
            Err(e) if e.kind() == ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e.into()),
        };
        for entry in entries {
            let entry = entry?;
            if !entry.file_type()?.is_file() {
                continue;
            }
            if let Some(name) = entry.file_name().to_str() {
                if !name.starts_with('.') {
                    out.push(name.to_string());
                }
            }
        }
        Ok(())
    }

    fn clean_dir(dir: &Path) -> Result<u64, BackendError> {
        let mut removed = 0;
        let entries = match fs::read_dir(dir) {
            Ok(e) => e,
            Err(e) if e.kind() == ErrorKind::NotFound => return Ok(0),
            Err(e) => return Err(e.into()),
        };
        for entry in entries {
            let entry = entry?;
            let ft = entry.file_type()?;
            if ft.is_dir() {
                removed += Self::clean_dir(&entry.path())?;
            } else if entry
                .file_name()
                .to_str()
                .is_some_and(|n| n.starts_with(TMP_PREFIX))
            {
                fs::remove_file(entry.path())?;
                removed += 1;
            }
        }
        Ok(removed)
    }
}

#[cfg(unix)]
fn sync_dir(dir: &Path) -> std::io::Result<()> {
    File::open(dir)?.sync_all()
}

#[cfg(not(unix))]
fn sync_dir(_dir: &Path) -> std::io::Result<()> {
    Ok(())
}

impl Backend for LocalBackend {
    fn put(&self, kind: Kind, name: &str, data: &[u8]) -> Result<(), BackendError> {
        let path = self.path(kind, name)?;
        if path.exists() {
            return Err(BackendError::AlreadyExists(name.to_string()));
        }
        let dir = path
            .parent()
            .ok_or_else(|| BackendError::Other("caminho sem diretório pai".into()))?;
        fs::create_dir_all(dir)?;
        let suffix = hex::encode(crate::crypto::random_bytes::<8>());
        let tmp = dir.join(format!("{TMP_PREFIX}{name}-{suffix}"));
        if let Err(e) = Self::write_tmp(&tmp, data) {
            let _ = fs::remove_file(&tmp);
            return Err(e.into());
        }
        let result = match fs::hard_link(&tmp, &path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == ErrorKind::AlreadyExists => {
                Err(BackendError::AlreadyExists(name.to_string()))
            }
            Err(_) => {
                if path.exists() {
                    Err(BackendError::AlreadyExists(name.to_string()))
                } else {
                    fs::rename(&tmp, &path).map_err(BackendError::from)
                }
            }
        };
        let _ = fs::remove_file(&tmp);
        result?;
        sync_dir(dir)?;
        Ok(())
    }

    fn get(&self, kind: Kind, name: &str) -> Result<Vec<u8>, BackendError> {
        let path = self.path(kind, name)?;
        fs::read(&path).map_err(|e| Self::map_not_found(e, name))
    }

    fn list(&self, kind: Kind) -> Result<Vec<String>, BackendError> {
        let mut out = Vec::new();
        match kind {
            Kind::Config => {
                if self.root.join("config").is_file() {
                    out.push("config".to_string());
                }
            }
            Kind::Data => {
                let data_dir = self.root.join("data");
                let entries = match fs::read_dir(&data_dir) {
                    Ok(e) => e,
                    Err(e) if e.kind() == ErrorKind::NotFound => return Ok(out),
                    Err(e) => return Err(e.into()),
                };
                for entry in entries {
                    let entry = entry?;
                    if entry.file_type()?.is_dir() {
                        Self::list_dir_files(&entry.path(), &mut out)?;
                    }
                }
            }
            Kind::Snapshot | Kind::Lock => {
                Self::list_dir_files(&self.root.join(kind.as_str()), &mut out)?;
            }
        }
        out.retain(|n| validate_name(kind, n).is_ok());
        out.sort();
        Ok(out)
    }

    fn exists(&self, kind: Kind, name: &str) -> Result<bool, BackendError> {
        Ok(self.path(kind, name)?.is_file())
    }

    fn delete(&self, kind: Kind, name: &str) -> Result<(), BackendError> {
        let path = self.path(kind, name)?;
        fs::remove_file(&path).map_err(|e| Self::map_not_found(e, name))?;
        if let Some(dir) = path.parent() {
            sync_dir(dir)?;
        }
        Ok(())
    }

    fn cleanup(&self) -> Result<u64, BackendError> {
        Self::clean_dir(&self.root)
    }

    fn describe(&self) -> String {
        format!("local:{}", self.root.display())
    }
}
