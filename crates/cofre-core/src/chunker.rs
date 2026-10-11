use std::io::Read;

use fastcdc::v2020::{Error as CdcError, StreamCDC};
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChunkerParams {
    pub algorithm: ChunkerAlgorithm,
    pub min_size: u32,
    pub avg_size: u32,
    pub max_size: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ChunkerAlgorithm {
    Fastcdc2020,
}

impl Default for ChunkerParams {
    fn default() -> Self {
        ChunkerParams {
            algorithm: ChunkerAlgorithm::Fastcdc2020,
            min_size: 256 * 1024,
            avg_size: 1024 * 1024,
            max_size: 4 * 1024 * 1024,
        }
    }
}

impl ChunkerParams {
    pub fn validate(&self) -> Result<()> {
        let ok = (64..=1_048_576).contains(&self.min_size)
            && (256..=4_194_304).contains(&self.avg_size)
            && (1024..=16_777_216).contains(&self.max_size)
            && self.min_size <= self.avg_size
            && self.avg_size <= self.max_size;
        if ok {
            Ok(())
        } else {
            Err(Error::InvalidParameter(format!(
                "tamanhos de bloco inválidos: {self:?}"
            )))
        }
    }

    pub fn chunks<R: Read>(&self, reader: R) -> impl Iterator<Item = std::io::Result<Vec<u8>>> {
        StreamCDC::new(reader, self.min_size, self.avg_size, self.max_size).map(|r| match r {
            Ok(c) => Ok(c.data),
            Err(CdcError::IoError(e)) => Err(e),
            Err(e) => Err(std::io::Error::other(e.to_string())),
        })
    }
}
