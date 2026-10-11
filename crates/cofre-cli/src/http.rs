use std::time::Duration;

use anyhow::{Context, Result};
use cofre_core::{Backend, BackendError, Kind};
use reqwest::Method;
use reqwest::StatusCode;
use reqwest::blocking::{Client, Response};
use zeroize::Zeroizing;

const MAX_ATTEMPTS: u32 = 5;

pub struct HttpBackend {
    base: String,
    client: Client,
    token: Option<Zeroizing<String>>,
}

impl HttpBackend {
    pub fn new(base: &str, token: Option<Zeroizing<String>>) -> Result<Self> {
        let base = base.trim_end_matches('/').to_string();
        if base.starts_with("http://")
            && !base.starts_with("http://127.0.0.1")
            && !base.starts_with("http://localhost")
        {
            tracing::warn!("conexão sem TLS: o token trafega em texto puro; prefira https://");
        }
        let client = Client::builder()
            .user_agent(concat!("cofre/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(300))
            .build()
            .context("criando cliente HTTP")?;
        Ok(HttpBackend {
            base,
            client,
            token,
        })
    }

    fn url(&self, kind: Kind, name: Option<&str>) -> String {
        match name {
            Some(n) => format!("{}/{}/{}", self.base, kind.as_str(), n),
            None => format!("{}/{}", self.base, kind.as_str()),
        }
    }

    fn send(
        &self,
        method: Method,
        url: &str,
        body: Option<&[u8]>,
    ) -> Result<(Response, bool), BackendError> {
        let mut retried = false;
        let mut last_err = String::new();
        for attempt in 0..MAX_ATTEMPTS {
            if attempt > 0 {
                retried = true;
                let wait = Duration::from_millis(500 * 2u64.pow(attempt - 1));
                tracing::warn!(%url, attempt, "repetindo requisição após falha: {last_err}");
                std::thread::sleep(wait);
            }
            let mut rb = self.client.request(method.clone(), url);
            if let Some(t) = &self.token {
                rb = rb.bearer_auth(t.as_str());
            }
            if let Some(b) = body {
                rb = rb.body(b.to_vec());
            }
            match rb.send() {
                Ok(resp) if resp.status().is_server_error() => {
                    last_err = format!("HTTP {}", resp.status());
                }
                Ok(resp) => return Ok((resp, retried)),
                Err(e) => last_err = e.to_string(),
            }
        }
        Err(BackendError::Other(format!(
            "servidor inacessível após {MAX_ATTEMPTS} tentativas: {last_err}"
        )))
    }

    fn status_error(resp: Response, name: &str) -> BackendError {
        let status = resp.status();
        let text = resp.text().unwrap_or_default();
        match status {
            StatusCode::NOT_FOUND => BackendError::NotFound(name.to_string()),
            StatusCode::CONFLICT => BackendError::AlreadyExists(name.to_string()),
            StatusCode::FORBIDDEN => BackendError::Forbidden(text),
            StatusCode::UNAUTHORIZED => {
                BackendError::Other("token do servidor inválido (COFRE_SERVER_TOKEN)".into())
            }
            StatusCode::BAD_REQUEST => BackendError::InvalidName(name.to_string()),
            s => BackendError::Other(format!("HTTP {s}: {text}")),
        }
    }
}

impl Backend for HttpBackend {
    fn put(&self, kind: Kind, name: &str, data: &[u8]) -> Result<(), BackendError> {
        let (resp, retried) = self.send(Method::PUT, &self.url(kind, Some(name)), Some(data))?;
        let status = resp.status();
        if status.is_success() {
            return Ok(());
        }
        if status == StatusCode::CONFLICT && retried && kind != Kind::Config {
            return Ok(());
        }
        Err(Self::status_error(resp, name))
    }

    fn get(&self, kind: Kind, name: &str) -> Result<Vec<u8>, BackendError> {
        let (resp, _) = self.send(Method::GET, &self.url(kind, Some(name)), None)?;
        if resp.status().is_success() {
            resp.bytes()
                .map(|b| b.to_vec())
                .map_err(|e| BackendError::Other(e.to_string()))
        } else {
            Err(Self::status_error(resp, name))
        }
    }

    fn list(&self, kind: Kind) -> Result<Vec<String>, BackendError> {
        let (resp, _) = self.send(Method::GET, &self.url(kind, None), None)?;
        if resp.status().is_success() {
            resp.json::<Vec<String>>()
                .map_err(|e| BackendError::Other(format!("listagem inválida: {e}")))
        } else {
            Err(Self::status_error(resp, kind.as_str()))
        }
    }

    fn exists(&self, kind: Kind, name: &str) -> Result<bool, BackendError> {
        let (resp, _) = self.send(Method::HEAD, &self.url(kind, Some(name)), None)?;
        match resp.status() {
            s if s.is_success() => Ok(true),
            StatusCode::NOT_FOUND => Ok(false),
            _ => Err(Self::status_error(resp, name)),
        }
    }

    fn delete(&self, kind: Kind, name: &str) -> Result<(), BackendError> {
        let (resp, retried) = self.send(Method::DELETE, &self.url(kind, Some(name)), None)?;
        let status = resp.status();
        if status.is_success() || (status == StatusCode::NOT_FOUND && retried) {
            Ok(())
        } else {
            Err(Self::status_error(resp, name))
        }
    }

    fn describe(&self) -> String {
        self.base.clone()
    }
}
