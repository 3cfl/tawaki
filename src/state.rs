use crate::source::Source;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Entry {
    pub source: Source,
    pub id: String,
}

#[derive(Serialize, Deserialize, Default)]
pub struct State {
    pub packages: BTreeMap<String, Entry>,
}

fn path() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("tawaki")
        .join("installed.json")
}

impl State {
    pub fn load() -> State {
        std::fs::read_to_string(path())
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) -> Result<()> {
        let p = path();
        if let Some(dir) = p.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(&p, serde_json::to_string_pretty(self)?)
            .with_context(|| format!("couldn't write {}", p.display()))
    }

    pub fn find(&self, name: &str) -> Option<(String, Entry)> {
        self.packages
            .iter()
            .find(|(k, e)| k.as_str() == name || e.id == name)
            .map(|(k, e)| (k.clone(), e.clone()))
    }
}
