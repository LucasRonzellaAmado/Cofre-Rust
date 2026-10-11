use std::collections::HashSet;
use std::sync::Arc;

use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::backend::{Backend, Kind};
use crate::chunker::ChunkerParams;
use crate::crypto::{KdfParams, MasterKeys};
use crate::error::{BackendError, Error, Result};
use crate::id::{ChunkId, random_hex_id};
use crate::snapshot::Snapshot;
use crate::time::now_unix;

pub const FORMAT_VERSION: u32 = 1;
pub const CONFIG_NAME: &str = "config";

const ENCODING_RAW: u8 = 0;
const ENCODING_ZSTD: u8 = 1;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepoConfig {
    pub format_version: u32,
    pub repo_id: String,
    pub created: i64,
    pub kdf: KdfParams,
    pub chunker: ChunkerParams,
    pub compression_level: i32,
    pub keys: String,
}

#[derive(Clone, Debug)]
pub struct InitOptions {
    pub kdf: KdfParams,
    pub chunker: ChunkerParams,
    pub compression_level: i32,
}

impl Default for InitOptions {
    fn default() -> Self {
        InitOptions {
            kdf: KdfParams::recommended(),
            chunker: ChunkerParams::default(),
            compression_level: 3,
        }
    }
}

struct Inner {
    backend: Arc<dyn Backend>,
    config: RepoConfig,
    keys: MasterKeys,
}

#[derive(Clone)]
pub struct Repository {
    inner: Arc<Inner>,
}

fn data_aad(id: &ChunkId) -> Vec<u8> {
    let mut aad = b"cofre:data:".to_vec();
    aad.extend_from_slice(&id.0);
    aad
}

fn named_aad(kind: &str, name: &str) -> Vec<u8> {
    format!("cofre:{kind}:{name}").into_bytes()
}

impl Repository {
    pub fn init(backend: Arc<dyn Backend>, password: &[u8], opts: InitOptions) -> Result<Self> {
        opts.chunker.validate()?;
        if !(1..=22).contains(&opts.compression_level) {
            return Err(Error::InvalidParameter(
                "nível de compressão deve estar entre 1 e 22".into(),
            ));
        }
        if backend.exists(Kind::Config, CONFIG_NAME)? {
            return Err(Error::AlreadyInitialized);
        }
        let keys = MasterKeys::generate();
        let kek = opts.kdf.derive(password)?;
        let wrapped = keys.wrap(&kek)?;
        let config = RepoConfig {
            format_version: FORMAT_VERSION,
            repo_id: hex::encode(crate::crypto::random_bytes::<16>()),
            created: now_unix(),
            kdf: opts.kdf,
            chunker: opts.chunker,
            compression_level: opts.compression_level,
            keys: hex::encode(wrapped),
        };
        let json =
            serde_json::to_vec_pretty(&config).map_err(|e| Error::Serialization(e.to_string()))?;
        match backend.put(Kind::Config, CONFIG_NAME, &json) {
            Ok(()) => {}
            Err(BackendError::AlreadyExists(_)) => return Err(Error::AlreadyInitialized),
            Err(e) => return Err(e.into()),
        }
        Ok(Repository {
            inner: Arc::new(Inner {
                backend,
                config,
                keys,
            }),
        })
    }

    pub fn open(backend: Arc<dyn Backend>, password: &[u8]) -> Result<Self> {
        let raw = match backend.get(Kind::Config, CONFIG_NAME) {
            Ok(r) => r,
            Err(BackendError::NotFound(_)) => return Err(Error::NotInitialized),
            Err(e) => return Err(e.into()),
        };
        let config = Self::parse_config(&raw)?;
        let kek = config.kdf.derive(password)?;
        let wrapped = Zeroizing::new(
            hex::decode(&config.keys)
                .map_err(|_| Error::Corrupted("chaves na configuração".into()))?,
        );
        let keys = MasterKeys::unwrap_with(&kek, &wrapped)?;
        Ok(Repository {
            inner: Arc::new(Inner {
                backend,
                config,
                keys,
            }),
        })
    }

    pub fn parse_config(raw: &[u8]) -> Result<RepoConfig> {
        #[derive(Deserialize)]
        struct VersionProbe {
            format_version: u32,
        }
        let probe: VersionProbe = serde_json::from_slice(raw)
            .map_err(|e| Error::Corrupted(format!("configuração ilegível: {e}")))?;
        if probe.format_version != FORMAT_VERSION {
            return Err(Error::UnsupportedVersion(probe.format_version));
        }
        let config: RepoConfig = serde_json::from_slice(raw)
            .map_err(|e| Error::Corrupted(format!("configuração ilegível: {e}")))?;
        config.chunker.validate()?;
        Ok(config)
    }

    pub fn backend(&self) -> &Arc<dyn Backend> {
        &self.inner.backend
    }

    pub fn config(&self) -> &RepoConfig {
        &self.inner.config
    }

    pub fn keys(&self) -> &MasterKeys {
        &self.inner.keys
    }

    pub fn chunk_id(&self, data: &[u8]) -> ChunkId {
        self.inner.keys.chunk_id(data)
    }

    pub fn encode_chunk(&self, id: &ChunkId, data: &[u8]) -> Result<Vec<u8>> {
        let compressed = zstd::bulk::compress(data, self.inner.config.compression_level)
            .map_err(|e| Error::Serialization(format!("zstd: {e}")))?;
        let mut plain = Zeroizing::new(Vec::with_capacity(compressed.len().min(data.len()) + 1));
        if compressed.len() < data.len() {
            plain.push(ENCODING_ZSTD);
            plain.extend_from_slice(&compressed);
        } else {
            plain.push(ENCODING_RAW);
            plain.extend_from_slice(data);
        }
        self.inner.keys.seal(&data_aad(id), &plain)
    }

    pub fn decode_chunk(&self, id: &ChunkId, blob: &[u8]) -> Result<Vec<u8>> {
        let context = format!("bloco {id}");
        let plain = self.inner.keys.open(&data_aad(id), blob, &context)?;
        let (encoding, payload) = plain
            .split_first()
            .ok_or_else(|| Error::Corrupted(format!("{context}: vazio")))?;
        let data = match *encoding {
            ENCODING_RAW => payload.to_vec(),
            ENCODING_ZSTD => {
                let cap = self.inner.config.chunker.max_size as usize;
                zstd::bulk::decompress(payload, cap)
                    .map_err(|e| Error::Corrupted(format!("{context}: zstd: {e}")))?
            }
            other => {
                return Err(Error::Corrupted(format!(
                    "{context}: codificação desconhecida {other}"
                )));
            }
        };
        if self.chunk_id(&data) != *id {
            return Err(Error::Corrupted(format!("{context}: hash não confere")));
        }
        Ok(data)
    }

    pub fn read_chunk(&self, id: &ChunkId) -> Result<Vec<u8>> {
        let blob = self.inner.backend.get(Kind::Data, &id.to_hex())?;
        self.decode_chunk(id, &blob)
    }

    pub fn write_chunk(&self, id: &ChunkId, data: &[u8]) -> Result<Option<u64>> {
        let blob = self.encode_chunk(id, data)?;
        match self.inner.backend.put(Kind::Data, &id.to_hex(), &blob) {
            Ok(()) => Ok(Some(blob.len() as u64)),
            Err(BackendError::AlreadyExists(_)) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    pub fn list_chunks(&self) -> Result<HashSet<ChunkId>> {
        let names = self.inner.backend.list(Kind::Data)?;
        let mut out = HashSet::with_capacity(names.len());
        for name in names {
            match ChunkId::from_hex(&name) {
                Ok(id) => {
                    out.insert(id);
                }
                Err(_) => tracing::warn!(%name, "objeto estranho em data/ ignorado"),
            }
        }
        Ok(out)
    }

    pub fn seal_named(&self, kind: &str, name: &str, plain: &[u8]) -> Result<Vec<u8>> {
        self.inner.keys.seal(&named_aad(kind, name), plain)
    }

    pub fn open_named(&self, kind: &str, name: &str, blob: &[u8]) -> Result<Zeroizing<Vec<u8>>> {
        self.inner
            .keys
            .open(&named_aad(kind, name), blob, &format!("{kind} {name}"))
    }

    pub fn save_snapshot(&self, snapshot: &Snapshot) -> Result<String> {
        let encoded = snapshot.encode()?;
        let compressed = zstd::bulk::compress(&encoded, self.inner.config.compression_level)
            .map_err(|e| Error::Serialization(format!("zstd: {e}")))?;
        let id = random_hex_id();
        let blob = self.seal_named("snapshot", &id, &compressed)?;
        self.inner.backend.put(Kind::Snapshot, &id, &blob)?;
        Ok(id)
    }

    pub fn load_snapshot(&self, id: &str) -> Result<Snapshot> {
        let blob = match self.inner.backend.get(Kind::Snapshot, id) {
            Ok(b) => b,
            Err(BackendError::NotFound(_)) => return Err(Error::SnapshotNotFound(id.into())),
            Err(e) => return Err(e.into()),
        };
        let plain = self.open_named("snapshot", id, &blob)?;
        let encoded = zstd::stream::decode_all(plain.as_slice())
            .map_err(|e| Error::Corrupted(format!("snapshot {id}: zstd: {e}")))?;
        Snapshot::decode(&encoded)
    }

    pub fn list_snapshot_ids(&self) -> Result<Vec<String>> {
        Ok(self.inner.backend.list(Kind::Snapshot)?)
    }

    pub fn load_all_snapshots(&self) -> Result<Vec<(String, Snapshot)>> {
        let ids = self.list_snapshot_ids()?;
        let mut snaps: Vec<(String, Snapshot)> = ids
            .into_par_iter()
            .map(|id| self.load_snapshot(&id).map(|s| (id, s)))
            .collect::<Result<_>>()?;
        snaps.sort_by(|a, b| a.1.time.cmp(&b.1.time).then_with(|| a.0.cmp(&b.0)));
        Ok(snaps)
    }

    pub fn resolve_snapshot(&self, spec: &str) -> Result<String> {
        if spec == "latest" {
            return self
                .load_all_snapshots()?
                .pop()
                .map(|(id, _)| id)
                .ok_or_else(|| Error::SnapshotNotFound("latest".into()));
        }
        let spec = spec.to_ascii_lowercase();
        let matches: Vec<String> = self
            .list_snapshot_ids()?
            .into_iter()
            .filter(|id| id.starts_with(&spec))
            .collect();
        match matches.len() {
            0 => Err(Error::SnapshotNotFound(spec)),
            1 => Ok(matches.into_iter().next().unwrap_or_default()),
            _ => Err(Error::AmbiguousSnapshot(spec)),
        }
    }

    pub fn delete_snapshot(&self, id: &str) -> Result<()> {
        self.inner.backend.delete(Kind::Snapshot, id)?;
        Ok(())
    }
}
