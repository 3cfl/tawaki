use crate::source::Source;
use anyhow::{bail, Context, Result};
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

pub fn parse(text: &str) -> Result<Config> {
    let mut cfg: Config = toml::from_str(text)?;
    if !crate::flatpak::valid_remote(&cfg.flatpak_remote) {
        bail!(
            "flatpak_remote {:?} isn't a valid remote name",
            cfg.flatpak_remote
        );
    }
    let mut seen = vec![];
    cfg.priority.retain(|s| {
        let fresh = !seen.contains(s);
        seen.push(*s);
        fresh
    });
    for s in Source::ALL {
        if !cfg.priority.contains(&s) {
            cfg.priority.push(s);
        }
    }
    Ok(cfg)
}

pub fn load() -> Result<Config> {
    let Some(p) = path() else {
        return Ok(Config::default());
    };
    let Ok(text) = std::fs::read_to_string(&p) else {
        return Ok(Config::default());
    };
    parse(&text).with_context(|| format!("bad config at {}", p.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_and_priority_fill() {
        let c = parse("priority = [\"aur\", \"aur\"]\n").unwrap();
        assert_eq!(c.priority, [Source::Aur, Source::Pacman, Source::Flatpak]);
        assert_eq!(c.flatpak_remote, "flathub");
        assert!(c.prefer_light);
    }

    #[test]
    fn hostile_remote_is_rejected() {
        assert!(parse("flatpak_remote = \"--from=https://x\"\n").is_err());
        assert!(parse("flatpak_remote = \"a b\"\n").is_err());
        assert!(parse("flatpak_remote = \"flathub-beta\"\n").is_ok());
    }
}
