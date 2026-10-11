use anyhow::{Context, Result, bail};
use cofre_core::{Backend, BackendError, Kind};
use futures::TryStreamExt;
use object_store::aws::{AmazonS3, AmazonS3Builder};
use object_store::path::Path as ObjPath;
use object_store::{ObjectStore, PutMode, PutOptions, PutPayload};
use tokio::runtime::Runtime;

pub struct S3Backend {
    store: AmazonS3,
    bucket: String,
    prefix: String,
    rt: Runtime,
}

impl S3Backend {
    pub fn new(spec: &str) -> Result<Self> {
        let (bucket, prefix) = match spec.split_once('/') {
            Some((b, p)) => (b, p.trim_matches('/')),
            None => (spec, ""),
        };
        if bucket.is_empty() {
            bail!("use s3:<bucket>/<prefixo>");
        }
        let store = AmazonS3Builder::from_env()
            .with_bucket_name(bucket)
            .build()
            .context("configurando S3 (veja AWS_ACCESS_KEY_ID, AWS_SECRET_ACCESS_KEY, AWS_REGION, AWS_ENDPOINT)")?;
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .context("criando runtime tokio")?;
        Ok(S3Backend {
            store,
            bucket: bucket.to_string(),
            prefix: prefix.to_string(),
            rt,
        })
    }

    fn full(&self, rel: &str) -> ObjPath {
        if self.prefix.is_empty() {
            ObjPath::from(rel)
        } else {
            ObjPath::from(format!("{}/{rel}", self.prefix))
        }
    }

    fn map(e: object_store::Error, name: &str) -> BackendError {
        match e {
            object_store::Error::NotFound { .. } => BackendError::NotFound(name.to_string()),
            object_store::Error::AlreadyExists { .. }
            | object_store::Error::Precondition { .. } => {
                BackendError::AlreadyExists(name.to_string())
            }
            object_store::Error::PermissionDenied { .. }
            | object_store::Error::Unauthenticated { .. } => BackendError::Forbidden(e.to_string()),
            other => BackendError::Other(other.to_string()),
        }
    }
}

impl Backend for S3Backend {
    fn put(&self, kind: Kind, name: &str, data: &[u8]) -> Result<(), BackendError> {
        cofre_core::backend::validate_name(kind, name)?;
        let path = self.full(&kind.relative_path(name));
        let payload = PutPayload::from(data.to_vec());
        let opts = PutOptions {
            mode: PutMode::Create,
            ..Default::default()
        };
        let result = self
            .rt
            .block_on(self.store.put_opts(&path, payload.clone(), opts));
        match result {
            Ok(_) => Ok(()),
            Err(object_store::Error::NotImplemented) => {
                if self.exists(kind, name)? {
                    return Err(BackendError::AlreadyExists(name.to_string()));
                }
                self.rt
                    .block_on(self.store.put(&path, payload))
                    .map(|_| ())
                    .map_err(|e| Self::map(e, name))
            }
            Err(e) => Err(Self::map(e, name)),
        }
    }

    fn get(&self, kind: Kind, name: &str) -> Result<Vec<u8>, BackendError> {
        cofre_core::backend::validate_name(kind, name)?;
        let path = self.full(&kind.relative_path(name));
        self.rt
            .block_on(async {
                let r = self.store.get(&path).await?;
                r.bytes().await
            })
            .map(|b| b.to_vec())
            .map_err(|e| Self::map(e, name))
    }

    fn list(&self, kind: Kind) -> Result<Vec<String>, BackendError> {
        if kind == Kind::Config {
            return Ok(if self.exists(Kind::Config, "config")? {
                vec!["config".to_string()]
            } else {
                Vec::new()
            });
        }
        let prefix = self.full(kind.as_str());
        let metas = self
            .rt
            .block_on(self.store.list(Some(&prefix)).try_collect::<Vec<_>>())
            .map_err(|e| Self::map(e, kind.as_str()))?;
        let mut names: Vec<String> = metas
            .into_iter()
            .filter_map(|m| m.location.filename().map(str::to_string))
            .filter(|n| cofre_core::backend::validate_name(kind, n).is_ok())
            .collect();
        names.sort();
        Ok(names)
    }

    fn exists(&self, kind: Kind, name: &str) -> Result<bool, BackendError> {
        cofre_core::backend::validate_name(kind, name)?;
        let path = self.full(&kind.relative_path(name));
        match self.rt.block_on(self.store.head(&path)) {
            Ok(_) => Ok(true),
            Err(object_store::Error::NotFound { .. }) => Ok(false),
            Err(e) => Err(Self::map(e, name)),
        }
    }

    fn delete(&self, kind: Kind, name: &str) -> Result<(), BackendError> {
        cofre_core::backend::validate_name(kind, name)?;
        let path = self.full(&kind.relative_path(name));
        self.rt
            .block_on(self.store.delete(&path))
            .map_err(|e| Self::map(e, name))
    }

    fn describe(&self) -> String {
        format!("s3:{}/{}", self.bucket, self.prefix)
    }
}
