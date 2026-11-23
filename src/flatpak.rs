use crate::config::{Config, FlatpakScope};
use crate::source::{norm, Candidate, Source};
use crate::{fpindex, sys};
use anyhow::Result;
use std::collections::HashSet;

const GENERIC: [&str; 6] = ["client", "app", "desktop", "application", "flatpak", "gui"];

pub fn available() -> bool {
    sys::has("flatpak")
}

fn scope_flag(cfg: &Config) -> &'static str {
    match cfg.flatpak_scope {
        FlatpakScope::User => "--user",
        FlatpakScope::System => "--system",
    }
}

fn to_candidate(a: &fpindex::App, installed: &HashSet<String>) -> Candidate {
    Candidate {
        source: Source::Flatpak,
        installed: installed.contains(&a.id),
        id: a.id.clone(),
        name: a.name.clone(),
        version: a.version.clone(),
        description: a.summary.clone(),
        repo: Some(a.remote.clone()),
        verified: a.verified,
        prebuilt: true,
    }
}

pub fn search(cfg: &Config, query: &[String]) -> Vec<Candidate> {
    if !available() {
        return vec![];
    }
    let apps = fpindex::load();
    let installed = fpindex::installed_ids();
    let terms: Vec<String> = query.iter().map(|q| q.to_lowercase()).collect();
    let mut seen = HashSet::new();
    let mut hits: Vec<&fpindex::App> = apps
        .iter()
        .filter(|a| {
            let hay = format!("{} {} {}", a.name, a.id, a.summary).to_lowercase();
            terms.iter().all(|t| hay.contains(t.as_str()))
        })
        .collect();
    hits.sort_by_key(|a| a.remote != cfg.flatpak_remote);
    hits.into_iter()
        .filter(|a| seen.insert(a.id.clone()))
        .map(|a| to_candidate(a, &installed))
        .collect()
}

fn id_matches(id: &str, key: &str) -> bool {
    let segs: Vec<&str> = id.split('.').collect();
    let Some(last) = segs.last() else {
        return false;
    };
    if norm(last) == key {
        return true;
    }
    segs.len() >= 3
        && GENERIC.contains(&last.to_lowercase().as_str())
        && norm(segs[segs.len() - 2]) == key
}

pub fn lookup(cfg: &Config, name: &str) -> Option<Candidate> {
    if !available() {
        return None;
    }
    let apps = fpindex::load();
    let key = norm(name);
    let preferred = |a: &&fpindex::App| a.remote == cfg.flatpak_remote;
    let by_name = |a: &&fpindex::App| norm(&a.name) == key;
    let by_id = |a: &&fpindex::App| a.id.eq_ignore_ascii_case(name) || id_matches(&a.id, &key);
    let best = apps
        .iter()
        .filter(preferred)
        .find(by_id)
        .or_else(|| apps.iter().filter(preferred).find(by_name))
        .or_else(|| apps.iter().find(by_id))
        .or_else(|| apps.iter().find(by_name))?;
    Some(to_candidate(best, &fpindex::installed_ids()))
}

fn ensure_remote(cfg: &Config, remote: &str) {
    if remote != "flathub" || !matches!(cfg.flatpak_scope, FlatpakScope::User) {
        return;
    }
    sys::succeeds("flatpak", ["remote-add", "--user", "--if-not-exists", "flathub", "https://dl.flathub.org/repo/flathub.flatpakrepo"]);
}

pub fn install(cfg: &Config, remote: &str, ids: &[String], yes: bool) -> Result<()> {
    ensure_remote(cfg, remote);
    let mut args = vec!["install".to_string(), scope_flag(cfg).into()];
    if yes {
        args.push("-y".into());
    }
    args.push(remote.to_string());
    args.extend(ids.iter().cloned());
    sys::run("flatpak", &args, None)
}

pub fn remove(ids: &[String], yes: bool) -> Result<()> {
    let mut args = vec!["uninstall".to_string()];
    if yes {
        args.push("-y".into());
    }
    args.extend(ids.iter().cloned());
    sys::run("flatpak", &args, None)
}

pub fn find_installed(name: &str) -> Option<String> {
    let out = sys::output("flatpak", ["list", "--app", "--columns=application,name"])?;
    let key = norm(name);
    out.lines().find_map(|l| {
        let (id, n) = l.split_once('\t')?;
        let id = id.trim();
        (id.eq_ignore_ascii_case(name) || norm(n) == key || id_matches(id, &key))
            .then(|| id.to_string())
    })
}

pub fn pending_updates() -> Option<usize> {
    if !available() {
        return None;
    }
    Some(sys::count_lines(sys::output(
        "flatpak",
        ["remote-ls", "--updates", "--columns=application"],
    )))
}

pub fn update(yes: bool) -> Result<()> {
    let mut args = vec!["update"];
    if yes {
        args.push("-y");
    }
    sys::run("flatpak", args, None)
}

fn field(info: &str, key: &str) -> Option<String> {
    info.lines().find_map(|l| {
        let (k, v) = l.split_once(':')?;
        (k.trim() == key).then(|| v.trim().to_string())
    })
}

fn bytes(size: &str) -> u64 {
    let mut it = size.split_whitespace();
    let n: f64 = it.next().and_then(|n| n.parse().ok()).unwrap_or(0.0);
    let mult = match it.next().unwrap_or("") {
        "kB" => 1e3,
        "MB" => 1e6,
        "GB" => 1e9,
        _ => 1.0,
    };
    (n * mult) as u64
}

pub fn missing_runtime(cfg: &Config, remote: &str, id: &str) -> Option<(String, u64)> {
    let info = sys::output("flatpak", ["remote-info", scope_flag(cfg), remote, id])?;
    let rt = field(&info, "Runtime")?;
    let mut parts = rt.split('/');
    let (name, _, branch) = (parts.next()?, parts.next()?, parts.next()?);
    if sys::succeeds("flatpak", ["info", &format!("{name}//{branch}")]) {
        return None;
    }
    let rinfo = sys::output("flatpak", ["remote-info", scope_flag(cfg), remote, &format!("runtime/{rt}")])?;
    Some((format!("{name} {branch}"), bytes(&field(&rinfo, "Download Size")?)))
}
