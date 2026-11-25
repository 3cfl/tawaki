use crate::fsx;
use crate::source::Source;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

const MAX_STATE: u64 = 16 << 20;

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Entry {
    pub source: Source,
    pub id: String,
}

#[derive(Serialize, Deserialize, Default)]
pub struct State {
    pub packages: BTreeMap<String, Entry>,
}

const FILE: &str = "installed.json";

impl State {
    pub fn load() -> State {
        fsx::data_root()
            .ok()
            .map(|d| State::load_from(&d.join(FILE)))
            .unwrap_or_default()
    }

    pub fn load_from(path: &Path) -> State {
        fsx::read_limited(path, MAX_STATE)
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) -> Result<()> {
        self.save_to(&fsx::data_root()?.join(FILE))
    }

    pub fn save_to(&self, path: &Path) -> Result<()> {
        fsx::write_atomic(path, &serde_json::to_vec_pretty(self)?)
    }

    pub fn find(&self, name: &str) -> Option<(String, Entry)> {
        self.packages
            .iter()
            .find(|(k, e)| k.as_str() == name || e.id == name)
            .map(|(k, e)| (k.clone(), e.clone()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn round_trip_is_private() {
        let d = fsx::test_dir("state");
        let p = d.join(FILE);
        let mut s = State::default();
        s.packages.insert(
            "spotify".into(),
            Entry {
                source: Source::Flatpak,
                id: "com.spotify.Client".into(),
            },
        );
        s.save_to(&p).unwrap();
        assert_eq!(std::fs::metadata(&p).unwrap().permissions().mode() & 0o777, 0o600);
        let back = State::load_from(&p);
        assert_eq!(back.find("com.spotify.Client").unwrap().0, "spotify");
        assert!(back.find("nope").is_none());
    }

    #[test]
    fn corrupt_or_linked_state_loads_empty() {
        let d = fsx::test_dir("state2");
        let p = d.join(FILE);
        std::fs::write(&p, b"{garbage").unwrap();
        assert!(State::load_from(&p).packages.is_empty());
        let link = d.join("link.json");
        std::os::unix::fs::symlink(&p, &link).unwrap();
        assert!(State::load_from(&link).packages.is_empty());
    }
}
