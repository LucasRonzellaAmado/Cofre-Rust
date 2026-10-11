use std::sync::Arc;

use anyhow::Result;
use cofre_core::{Backend, LocalBackend};
use zeroize::Zeroizing;

use crate::http::HttpBackend;
use crate::s3::S3Backend;

pub fn open_backend(spec: &str, token: Option<Zeroizing<String>>) -> Result<Arc<dyn Backend>> {
    if let Some(rest) = spec.strip_prefix("s3:") {
        return Ok(Arc::new(S3Backend::new(rest)?));
    }
    if spec.starts_with("http://") || spec.starts_with("https://") {
        return Ok(Arc::new(HttpBackend::new(spec, token)?));
    }
    let path = spec.strip_prefix("local:").unwrap_or(spec);
    Ok(Arc::new(LocalBackend::new(path)))
}
