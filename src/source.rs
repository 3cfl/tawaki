use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    Pacman,
    Flatpak,
    Aur,
}

impl Source {
    pub const ALL: [Source; 3] = [Source::Pacman, Source::Flatpak, Source::Aur];

    pub fn label(self) -> &'static str {
        match self {
            Source::Pacman => "pacman",
            Source::Flatpak => "flatpak",
            Source::Aur => "aur",
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct Candidate {
    pub source: Source,
    pub id: String,
    pub name: String,
    pub version: String,
    pub description: String,
    pub repo: Option<String>,
    pub verified: bool,
    pub installed: bool,
    pub prebuilt: bool,
}

pub fn norm(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(|c| c.to_lowercase())
        .collect()
}

#[derive(serde::Serialize, Clone, Debug)]
pub struct Outdated {
    pub name: String,
    pub old: String,
    pub new: String,
}

pub fn parse_arrows(out: Option<String>) -> Vec<Outdated> {
    out.unwrap_or_default()
        .lines()
        .filter_map(|l| {
            let mut it = l.split_whitespace();
            let name = it.next()?.to_string();
            let old = it.next()?.to_string();
            if it.next()? != "->" {
                return None;
            }
            let new = it.next()?.to_string();
            Some(Outdated { name, old, new })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arrows_parsing() {
        let out = "firefox 1.0-1 -> 1.1-1\nbroken line\nvim 9.0 9.1\n\nzsh 5.8-1 -> 5.9-1\n";
        let r = parse_arrows(Some(out.into()));
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].name, "firefox");
        assert_eq!(r[1].new, "5.9-1");
        assert!(parse_arrows(None).is_empty());
    }

    #[test]
    fn norm_strips_punctuation() {
        assert_eq!(norm("OBS-Studio"), "obsstudio");
        assert_eq!(norm("--"), "");
    }
}
