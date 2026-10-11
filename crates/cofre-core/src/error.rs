use std::path::PathBuf;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum BackendError {
    #[error("objeto não encontrado: {0}")]
    NotFound(String),
    #[error("objeto já existe: {0}")]
    AlreadyExists(String),
    #[error("operação não permitida pelo destino: {0}")]
    Forbidden(String),
    #[error("nome de objeto inválido: {0}")]
    InvalidName(String),
    #[error("erro de I/O no destino: {0}")]
    Io(#[from] std::io::Error),
    #[error("erro no destino: {0}")]
    Other(String),
}

#[derive(Debug, Error)]
pub enum Error {
    #[error(transparent)]
    Backend(#[from] BackendError),
    #[error("senha incorreta ou arquivo de chaves adulterado")]
    WrongPassword,
    #[error("dado corrompido ou adulterado: {0}")]
    Corrupted(String),
    #[error("versão de formato não suportada: {0}")]
    UnsupportedVersion(u32),
    #[error("já existe um repositório neste destino")]
    AlreadyInitialized,
    #[error("repositório não encontrado ou não inicializado")]
    NotInitialized,
    #[error("falha de serialização: {0}")]
    Serialization(String),
    #[error("I/O em {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("snapshot não encontrado: {0}")]
    SnapshotNotFound(String),
    #[error("prefixo de snapshot ambíguo: {0}")]
    AmbiguousSnapshot(String),
    #[error("repositório travado: {0}")]
    Locked(String),
    #[error("caminho inválido: {0}")]
    InvalidPath(String),
    #[error("parâmetro inválido: {0}")]
    InvalidParameter(String),
    #[error("falha de criptografia: {0}")]
    Crypto(String),
}

impl Error {
    pub fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Error::Io {
            path: path.into(),
            source,
        }
    }
}

pub type Result<T> = std::result::Result<T, Error>;
