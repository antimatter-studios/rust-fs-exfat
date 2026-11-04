use crate::directory::{DirectoryEntry, Stream};
use crate::volume::Volume;
use crate::{Error, Result};
use fs_core::BlockRead;

pub struct File<'volume, Device: BlockRead> {
    volume: &'volume Volume<Device>,
    stream: Stream,
    clusters: Vec<u32>,
}

impl<'volume, Device: BlockRead> File<'volume, Device> {
    pub(crate) fn open(volume: &'volume Volume<Device>, entry: &DirectoryEntry) -> Result<Self> {
        if entry.is_directory() {
            return Err(Error::Invalid("entry is not a file"));
        }
        Ok(Self {
            volume,
            stream: entry.stream().clone(),
            clusters: volume.stream_clusters(entry.stream())?,
        })
    }

    pub fn len(&self) -> u64 {
        self.stream.data_length
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn valid_data_length(&self) -> u64 {
        self.stream.valid_data_length
    }

    pub fn read_at(&self, offset: u64, output: &mut [u8]) -> Result<usize> {
        if offset >= self.len() || output.is_empty() {
            return Ok(0);
        }
        let count = (self.len() - offset).min(output.len() as u64) as usize;
        output[..count].fill(0);
        let valid = self
            .stream
            .valid_data_length
            .saturating_sub(offset)
            .min(count as u64) as usize;
        let cluster_size = self.volume.boot.cluster_size();
        let mut copied = 0;
        while copied < valid {
            let position = offset + copied as u64;
            let index = usize::try_from(position / cluster_size).map_err(|_| Error::Bounds)?;
            let within = position % cluster_size;
            let chunk = (cluster_size - within).min((valid - copied) as u64) as usize;
            let cluster = *self
                .clusters
                .get(index)
                .ok_or(Error::Invalid("short file chain"))?;
            let physical = self.volume.boot.cluster_offset(cluster)? + within;
            self.volume
                .reader
                .read_at(physical, &mut output[copied..copied + chunk])?;
            copied += chunk;
        }
        Ok(count)
    }
}
