use crate::config::Config;
use crate::pacman;
use crate::source::{parse_arrows, Candidate, Outdated, Source};
use crate::{sys, ui};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::Duration;

const RPC: &str = "https://aur.archlinux.org/rpc/v5";

#[derive(Deserialize, Serialize)]
struct Resp {
    #[serde(default)]
    results: Vec<Pkg>,
    #[serde(default)]
    error: Option<String>,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "PascalCase")]
struct Pkg {
    name: String,
    version: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    popularity: f64,
    #[serde(default)]
    out_of_date: Option<i64>,
    #[serde(default)]
    make_depends: Option<Vec<String>>,
}

fn get(url: &str, query: &[(&str, &str)]) -> Option<Resp> {
    let agent = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(8))
        .user_agent(concat!("tawaki/", env!("CARGO_PKG_VERSION")))
        .build();
    let mut req = agent.get(url);
    for (k, v) in query {
        req = req.query(k, v);
    }
    let resp: Resp = req.call().ok()?.into_json().ok()?;
    if resp.error.is_some() {
        return None;
    }
    Some(resp)
}

const SEARCH_TTL: u64 = 1800;

fn search_cache(key: &str) -> PathBuf {
    let safe: String = key
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    dirs::cache_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("tawaki")
        .join("search")
        .join(format!("{safe}.json"))
}

fn cached_search(term: &str) -> Option<Resp> {
    let path = search_cache(term);
    let fresh = std::fs::metadata(&path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.elapsed().ok())
        .is_some_and(|age| age.as_secs() < SEARCH_TTL);
    if fresh {
        if let Some(r) = std::fs::read(&path)
            .ok()
            .and_then(|b| serde_json::from_slice::<Resp>(&b).ok())
        {
            return Some(r);
        }
    }
    let r = get(
        &format!("{RPC}/search/{}", encode(term)),
        &[("by", "name-desc")],
    )?;
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(b) = serde_json::to_vec(&r) {
        let _ = std::fs::write(&path, b);
    }
    Some(r)
}

fn to_candidate(p: &Pkg, installed: bool) -> Candidate {
    Candidate {
        source: Source::Aur,
        id: p.name.clone(),
        name: p.name.clone(),
        version: p.version.clone(),
        description: p.description.clone().unwrap_or_default(),
        repo: None,
        verified: false,
        installed,
        prebuilt: p.name.ends_with("-bin")
            || p.make_depends
                .as_ref()
                .is_none_or(|m| m.iter().all(|d| d.starts_with("git"))),
    }
}

pub fn lookup(name: &str) -> Option<Candidate> {
    let wanted = [
        format!("{name}-bin"),
        name.to_string(),
        format!("{name}-git"),
    ];
    let q: Vec<(&str, &str)> = wanted.iter().map(|w| ("arg[]", w.as_str())).collect();
    let resp = get(&format!("{RPC}/info"), &q)?;
    let find = |fresh_only: bool| {
        wanted.iter().find_map(|w| {
            resp.results
                .iter()
                .find(|p| &p.name == w && (!fresh_only || p.out_of_date.is_none()))
        })
    };
    let pick = find(true).or_else(|| find(false))?;
    Some(to_candidate(pick, pacman::is_installed(&pick.name)))
}

fn encode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' | b'_' | b'.' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

pub fn search(query: &[String]) -> Vec<Candidate> {
    let Some(longest) = query.iter().max_by_key(|q| q.len()) else {
        return vec![];
    };
    let Some(resp) = cached_search(longest) else {
        return vec![];
    };
    let terms: Vec<String> = query.iter().map(|q| q.to_lowercase()).collect();
    let mut hits: Vec<&Pkg> = resp
        .results
        .iter()
        .filter(|p| {
            let hay = format!(
                "{} {}",
                p.name.to_lowercase(),
                p.description.as_deref().unwrap_or("").to_lowercase()
            );
            terms.iter().all(|t| hay.contains(t.as_str()))
        })
        .collect();
    hits.sort_by(|a, b| b.popularity.total_cmp(&a.popularity));
    hits.truncate(15);
    let foreign = pacman::foreign();
    hits.into_iter()
        .map(|p| to_candidate(p, foreign.contains(&p.name)))
        .collect()
}

pub fn helper(cfg: &Config) -> Option<&'static str> {
    match cfg.aur_helper.as_str() {
        "none" => None,
        "paru" => Some("paru"),
        "yay" => Some("yay"),
        _ => ["paru", "yay"].into_iter().find(|h| sys::has(h)),
    }
}

fn build_dir(name: &str) -> PathBuf {
    dirs::cache_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("tawaki")
        .join("aur")
        .join(name)
}

fn build_one(name: &str, yes: bool) -> Result<()> {
    if !pacman::valid_name(name, false) {
        anyhow::bail!("invalid package name: {name}");
    }
    let dir = build_dir(name);
    if dir.join(".git").is_dir() {
        sys::run(
            "git",
            ["-C", &dir.to_string_lossy(), "pull", "--ff-only", "-q"],
            None,
        )?;
    } else {
        std::fs::create_dir_all(dir.parent().context("bad cache path")?)?;
        let url = format!("https://aur.archlinux.org/{name}.git");
        sys::run("git", ["clone", "-q", &url, &dir.to_string_lossy()], None)?;
    }
    if !yes && ui::confirm(&format!("Review PKGBUILD for {name}?"), true) {
        let pager = std::env::var("PAGER").unwrap_or_else(|_| "less".into());
        let _ = sys::run(&pager, [dir.join("PKGBUILD")], None);
        if !ui::confirm(&format!("Build and install {name}?"), true) {
            anyhow::bail!("cancelled");
        }
    }
    let mut args = vec!["-si", "--needed"];
    if yes {
        args.push("--noconfirm");
    }
    sys::run("makepkg", args, Some(&dir))
}

pub fn install(cfg: &Config, names: &[String], yes: bool) -> Result<()> {
    if let Some(h) = helper(cfg) {
        let mut args = vec!["-S".to_string(), "--aur".into(), "--needed".into()];
        if yes {
            args.push("--noconfirm".into());
        }
        args.extend(names.iter().cloned());
        return sys::run(h, &args, None);
    }
    ui::warn(
        "no AUR helper found, building with makepkg (AUR-only dependencies won't be resolved)",
    );
    for n in names {
        build_one(n, yes)?;
    }
    Ok(())
}

pub fn outdated(cfg: &Config) -> Option<Vec<Outdated>> {
    if let Some(h) = helper(cfg) {
        let out = sys::output(h, ["-Qua"]).or_else(|| Some(String::new()));
        return Some(parse_arrows(out));
    }
    if cfg.aur_helper == "none" {
        return Some(vec![]);
    }
    let local = pacman::foreign_versions();
    if local.is_empty() {
        return Some(vec![]);
    }
    let mut found = Vec::new();
    for chunk in local.chunks(150) {
        let q: Vec<(&str, &str)> = chunk.iter().map(|(n, _)| ("arg[]", n.as_str())).collect();
        let resp = get(&format!("{RPC}/info"), &q)?;
        for p in resp.results {
            let Some((_, old)) = chunk.iter().find(|(n, _)| *n == p.name) else {
                continue;
            };
            if pacman::vercmp(&p.version, old) == std::cmp::Ordering::Greater {
                found.push(Outdated {
                    name: p.name.clone(),
                    old: old.clone(),
                    new: p.version.clone(),
                });
            }
        }
    }
    Some(found)
}

pub fn update(cfg: &Config, yes: bool) -> Result<()> {
    let Some(h) = helper(cfg) else {
        for o in outdated(cfg).unwrap_or_default() {
            ui::info(&format!("Building {} {}", o.name, o.new));
            build_one(&o.name, yes)?;
        }
        return Ok(());
    };
    let mut args = vec!["-Sua"];
    if yes {
        args.push("--noconfirm");
    }
    sys::run(h, args, None)
}
