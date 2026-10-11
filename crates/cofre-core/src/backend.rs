use std::sync::Arc;

use crate::error::BackendError;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    Config,
    Data,
    Snapshot,
    Lock,
}

impl Kind {
    pub const ALL: [Kind; 4] = [Kind::Config, Kind::Data, Kind::Snapshot, Kind::Lock];

    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Config => "config",
            Kind::Data => "data",
            Kind::Snapshot => "snapshots",
            Kind::Lock => "locks",
        }
    }

    pub fn parse(s: &str) -> Option<Kind> {
        Kind::ALL.into_iter().find(|k| k.as_str() == s)
    }

    pub fn relative_path(self, name: &str) -> String {
        match self {
            Kind::Config => name.to_string(),
            Kind::Data => {
                let prefix = name.get(..2).unwrap_or("00");
                format!("data/{prefix}/{name}")
            }
            Kind::Snapshot | Kind::Lock => format!("{}/{name}", self.as_str()),
        }
    }
}

pub fn validate_name(kind: Kind, name: &str) -> Result<(), BackendError> {
    let ok = match kind {
        Kind::Config => name == "config",
        _ => {
            (2..=128).contains(&name.len())
                && name
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        }
    };
    if ok {
        Ok(())
    } else {
        Err(BackendError::InvalidName(name.to_string()))
    }
}

pub trait Backend: Send + Sync {
    fn put(&self, kind: Kind, name: &str, data: &[u8]) -> Result<(), BackendError>;
    fn get(&self, kind: Kind, name: &str) -> Result<Vec<u8>, BackendError>;
    fn list(&self, kind: Kind) -> Result<Vec<String>, BackendError>;
    fn exists(&self, kind: Kind, name: &str) -> Result<bool, BackendError>;
    fn delete(&self, kind: Kind, name: &str) -> Result<(), BackendError>;

    fn cleanup(&self) -> Result<u64, BackendError> {
        Ok(0)
    }

    fn describe(&self) -> String;
}

impl<T: Backend + ?Sized> Backend for Arc<T> {
    fn put(&self, kind: Kind, name: &str, data: &[u8]) -> Result<(), BackendError> {
        (**self).put(kind, name, data)
    }
    fn get(&self, kind: Kind, name: &str) -> Result<Vec<u8>, BackendError> {
        (**self).get(kind, name)
    }
    fn list(&self, kind: Kind) -> Result<Vec<String>, BackendError> {
        (**self).list(kind)
    }
    fn exists(&self, kind: Kind, name: &str) -> Result<bool, BackendError> {
        (**self).exists(kind, name)
    }
    fn delete(&self, kind: Kind, name: &str) -> Result<(), BackendError> {
        (**self).delete(kind, name)
    }
    fn cleanup(&self) -> Result<u64, BackendError> {
        (**self).cleanup()
    }
    fn describe(&self) -> String {
        (**self).describe()
    }
}
