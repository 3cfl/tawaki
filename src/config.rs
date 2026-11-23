use crate::source::Source;
use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::PathBuf;

#[derive(Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum FlatpakScope {
    User,
    System,
}

#[derive(Deserialize, Debug)]
#[serde(default)]
pub struct Config {
    pub priority: Vec<Source>,
    pub aur_helper: String,
    pub flatpak_scope: FlatpakScope,
    pub flatpak_remote: String,
    pub prefer_light: bool,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            priority: Source::ALL.to_vec(),
            aur_helper: "auto".into(),
            flatpak_scope: FlatpakScope::User,
            flatpak_remote: "flathub".into(),
            prefer_light: true,
        }
    }
}

pub fn path() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("tawaki").join("config.toml"))
}

pub fn load() -> Result<Config> {
    let Some(p) = path() else {
        return Ok(Config::default());
    };
    let Ok(text) = std::fs::read_to_string(&p) else {
        return Ok(Config::default());
    };
    let mut cfg: Config =
        toml::from_str(&text).with_context(|| format!("bad config at {}", p.display()))?;
    for s in Source::ALL {
        if !cfg.priority.contains(&s) {
            cfg.priority.push(s);
        }
    }
    Ok(cfg)
}
