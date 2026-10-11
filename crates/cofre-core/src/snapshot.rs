use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::id::ChunkId;

pub const SNAPSHOT_VERSION: u32 = 1;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    pub time: i64,
    pub hostname: String,
    pub paths: Vec<String>,
    pub tags: Vec<String>,
    pub nodes: Vec<Node>,
    pub summary: Summary,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Summary {
    pub files: u64,
    pub dirs: u64,
    pub symlinks: u64,
    pub bytes: u64,
    pub new_chunks: u64,
    pub new_bytes_stored: u64,
    pub warnings: u64,
    pub duration_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Node {
    pub path: String,
    pub kind: NodeKind,
    pub mode: u32,
    pub mtime: i64,
    pub mtime_nanos: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum NodeKind {
    Dir,
    File { size: u64, chunks: Vec<ChunkId> },
    Symlink { target: String },
}

impl Node {
    pub fn size(&self) -> u64 {
        match &self.kind {
            NodeKind::File { size, .. } => *size,
            _ => 0,
        }
    }
}

impl Snapshot {
    pub fn encode(&self) -> Result<Vec<u8>> {
        let mut out = SNAPSHOT_VERSION.to_le_bytes().to_vec();
        let body = postcard::to_allocvec(self).map_err(|e| Error::Serialization(e.to_string()))?;
        out.extend_from_slice(&body);
        Ok(out)
    }

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        if bytes.len() < 4 {
            return Err(Error::Corrupted("snapshot truncado".into()));
        }
        let mut v = [0u8; 4];
        v.copy_from_slice(&bytes[..4]);
        let version = u32::from_le_bytes(v);
        match version {
            SNAPSHOT_VERSION => {
                postcard::from_bytes(&bytes[4..]).map_err(|e| Error::Serialization(e.to_string()))
            }
            other => Err(Error::UnsupportedVersion(other)),
        }
    }

    pub fn chunk_ids(&self) -> impl Iterator<Item = &ChunkId> {
        self.nodes.iter().flat_map(|n| match &n.kind {
            NodeKind::File { chunks, .. } => chunks.as_slice(),
            _ => &[],
        })
    }
}
