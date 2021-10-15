use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;

use hashbrown::HashSet;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AccessListMode {
    /// Only serve torrents with info hash present in file
    Require,
    /// Do not serve torrents if info hash present in file
    Forbid,
    /// Turn off access list functionality
    Ignore,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AccessListConfig {
    pub mode: AccessListMode,
    /// Path to access list file consisting of newline-separated hex-encoded info hashes.
    ///
    /// If using chroot mode, path must be relative to new root.
    pub path: PathBuf,
}

impl Default for AccessListConfig {
    fn default() -> Self {
        Self {
            path: "".into(),
            mode: AccessListMode::Ignore,
        }
    }
}

#[derive(Default)]
pub struct AccessList(HashSet<[u8; 20]>);

impl AccessList {
    fn parse_info_hash(line: String) -> anyhow::Result<[u8; 20]> {
        let mut bytes = [0u8; 20];

        hex::decode_to_slice(line, &mut bytes)?;

        Ok(bytes)
    }

    pub fn update_from_path(&mut self, path: &PathBuf) -> anyhow::Result<()> {
        let file = File::open(path)?;
        let reader = BufReader::new(file);

        let mut new_list = HashSet::new();

        for line in reader.lines() {
            new_list.insert(Self::parse_info_hash(line?)?);
        }

        self.0 = new_list;

        Ok(())
    }

    pub fn allows(&self, list_type: AccessListMode, info_hash_bytes: &[u8; 20]) -> bool {
        match list_type {
            AccessListMode::Require => self.0.contains(info_hash_bytes),
            AccessListMode::Forbid => !self.0.contains(info_hash_bytes),
            AccessListMode::Ignore => true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_info_hash() {
        let f = AccessList::parse_info_hash;

        assert!(f("aaaabbbbccccddddeeeeaaaabbbbccccddddeeee".into()).is_ok());
        assert!(f("aaaabbbbccccddddeeeeaaaabbbbccccddddeeeef".into()).is_err());
        assert!(f("aaaabbbbccccddddeeeeaaaabbbbccccddddeee".into()).is_err());
        assert!(f("aaaabbbbccccddddeeeeaaaabbbbccccddddeeeö".into()).is_err());
    }
}