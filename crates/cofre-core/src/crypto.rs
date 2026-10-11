use argon2::{Algorithm, Argon2, Params, Version};
use chacha20poly1305::aead::rand_core::RngCore;
use chacha20poly1305::aead::{Aead, OsRng, Payload};
use chacha20poly1305::{Key, KeyInit, XChaCha20Poly1305, XNonce};
use serde::{Deserialize, Serialize};
use zeroize::{Zeroize, Zeroizing};

use crate::error::{Error, Result};
use crate::id::ChunkId;

pub const KEY_LEN: usize = 32;
pub const NONCE_LEN: usize = 24;
pub const TAG_LEN: usize = 16;
pub const BLOB_VERSION: u8 = 1;
pub const BLOB_OVERHEAD: usize = 1 + NONCE_LEN + TAG_LEN;

const KEYS_AAD: &[u8] = b"cofre:keys:v1";

pub fn random_bytes<const N: usize>() -> [u8; N] {
    let mut buf = [0u8; N];
    OsRng.fill_bytes(&mut buf);
    buf
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct KdfParams {
    pub algorithm: String,
    pub memory_kib: u32,
    pub iterations: u32,
    pub parallelism: u32,
    pub salt: String,
}

impl KdfParams {
    pub const ALGORITHM: &'static str = "argon2id-v19";

    pub fn new(memory_kib: u32, iterations: u32, parallelism: u32) -> Self {
        KdfParams {
            algorithm: Self::ALGORITHM.to_string(),
            memory_kib,
            iterations,
            parallelism,
            salt: hex::encode(random_bytes::<16>()),
        }
    }

    pub fn recommended() -> Self {
        Self::new(64 * 1024, 3, 1)
    }

    pub fn derive(&self, password: &[u8]) -> Result<Zeroizing<[u8; KEY_LEN]>> {
        if self.algorithm != Self::ALGORITHM {
            return Err(Error::Crypto(format!(
                "algoritmo de derivação desconhecido: {}",
                self.algorithm
            )));
        }
        let salt = hex::decode(&self.salt)
            .map_err(|_| Error::Crypto("salt inválido na configuração".into()))?;
        let params = Params::new(
            self.memory_kib,
            self.iterations,
            self.parallelism,
            Some(KEY_LEN),
        )
        .map_err(|e| Error::Crypto(format!("parâmetros do Argon2 inválidos: {e}")))?;
        let argon = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
        let mut out = Zeroizing::new([0u8; KEY_LEN]);
        argon
            .hash_password_into(password, &salt, out.as_mut())
            .map_err(|e| Error::Crypto(format!("falha no Argon2: {e}")))?;
        Ok(out)
    }
}

pub fn seal_with(key: &[u8; KEY_LEN], aad: &[u8], plaintext: &[u8]) -> Result<Vec<u8>> {
    let cipher = XChaCha20Poly1305::new(Key::from_slice(key));
    let nonce = random_bytes::<NONCE_LEN>();
    let ciphertext = cipher
        .encrypt(
            XNonce::from_slice(&nonce),
            Payload {
                msg: plaintext,
                aad,
            },
        )
        .map_err(|_| Error::Crypto("falha ao criptografar".into()))?;
    let mut out = Vec::with_capacity(BLOB_OVERHEAD + plaintext.len());
    out.push(BLOB_VERSION);
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&ciphertext);
    Ok(out)
}

pub fn open_with(
    key: &[u8; KEY_LEN],
    aad: &[u8],
    blob: &[u8],
    context: &str,
) -> Result<Zeroizing<Vec<u8>>> {
    if blob.len() < BLOB_OVERHEAD {
        return Err(Error::Corrupted(format!("{context}: objeto truncado")));
    }
    if blob[0] != BLOB_VERSION {
        return Err(Error::UnsupportedVersion(u32::from(blob[0])));
    }
    let cipher = XChaCha20Poly1305::new(Key::from_slice(key));
    let nonce = XNonce::from_slice(&blob[1..1 + NONCE_LEN]);
    cipher
        .decrypt(
            nonce,
            Payload {
                msg: &blob[1 + NONCE_LEN..],
                aad,
            },
        )
        .map(Zeroizing::new)
        .map_err(|_| Error::Corrupted(format!("{context}: autenticação falhou")))
}

pub struct MasterKeys {
    enc: [u8; KEY_LEN],
    id: [u8; KEY_LEN],
}

impl Drop for MasterKeys {
    fn drop(&mut self) {
        self.enc.zeroize();
        self.id.zeroize();
    }
}

impl MasterKeys {
    pub fn generate() -> Self {
        MasterKeys {
            enc: random_bytes(),
            id: random_bytes(),
        }
    }

    pub fn wrap(&self, kek: &[u8; KEY_LEN]) -> Result<Vec<u8>> {
        let mut raw = Zeroizing::new([0u8; 2 * KEY_LEN]);
        raw[..KEY_LEN].copy_from_slice(&self.enc);
        raw[KEY_LEN..].copy_from_slice(&self.id);
        seal_with(kek, KEYS_AAD, raw.as_ref())
    }

    pub fn unwrap_with(kek: &[u8; KEY_LEN], blob: &[u8]) -> Result<Self> {
        let raw = open_with(kek, KEYS_AAD, blob, "chaves").map_err(|e| match e {
            Error::Corrupted(_) => Error::WrongPassword,
            other => other,
        })?;
        if raw.len() != 2 * KEY_LEN {
            return Err(Error::Corrupted("chaves: tamanho inesperado".into()));
        }
        let mut keys = MasterKeys {
            enc: [0u8; KEY_LEN],
            id: [0u8; KEY_LEN],
        };
        keys.enc.copy_from_slice(&raw[..KEY_LEN]);
        keys.id.copy_from_slice(&raw[KEY_LEN..]);
        Ok(keys)
    }

    pub fn chunk_id(&self, data: &[u8]) -> ChunkId {
        ChunkId(*blake3::keyed_hash(&self.id, data).as_bytes())
    }

    pub fn seal(&self, aad: &[u8], plaintext: &[u8]) -> Result<Vec<u8>> {
        seal_with(&self.enc, aad, plaintext)
    }

    pub fn open(&self, aad: &[u8], blob: &[u8], context: &str) -> Result<Zeroizing<Vec<u8>>> {
        open_with(&self.enc, aad, blob, context)
    }
}
