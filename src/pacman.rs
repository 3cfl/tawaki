use crate::source::{Candidate, Source};
use crate::sys;
use anyhow::Result;
use std::collections::HashSet;

pub fn lookup(name: &str) -> Option<Candidate> {
    let out = sys::output("pacman", ["-Si", name])?;
    let mut repo = None;
    let mut pname = None;
    let mut version = String::new();
    let mut description = String::new();
    for line in out.lines() {
        if line.trim().is_empty() {
            if pname.is_some() {
                break;
            }
            continue;
        }
        let Some((k, v)) = line.split_once(" : ") else {
            continue;
        };
        let v = v.trim().to_string();
        match k.trim() {
            "Repository" => repo = Some(v),
            "Name" => pname = Some(v),
            "Version" => version = v,
            "Description" => description = v,
            _ => {}
        }
    }
    let name = pname?;
    Some(Candidate {
        source: Source::Pacman,
        installed: is_installed(&name),
        id: name.clone(),
        name,
        version,
        description,
        repo,
        verified: true,
        prebuilt: true,
    })
}

pub fn search(query: &[String]) -> Vec<Candidate> {
    let mut args = vec!["-Ss".to_string()];
    args.extend(query.iter().cloned());
    let Some(out) = sys::output("pacman", &args) else {
        return vec![];
    };
    let mut res: Vec<Candidate> = vec![];
    let mut seen = HashSet::new();
    let mut skip = false;
    for line in out.lines() {
        if line.starts_with(char::is_whitespace) {
            if skip {
                continue;
            }
            if let Some(last) = res.last_mut() {
                last.description = line.trim().to_string();
            }
            continue;
        }
        let mut parts = line.split_whitespace();
        let (Some(full), Some(ver)) = (parts.next(), parts.next()) else {
            continue;
        };
        let (repo, name) = full.split_once('/').unwrap_or(("", full));
        skip = !seen.insert(name.to_string());
        if skip {
            continue;
        }
        res.push(Candidate {
            source: Source::Pacman,
            id: name.into(),
            name: name.into(),
            version: ver.into(),
            description: String::new(),
            repo: Some(repo.into()),
            verified: true,
            installed: line.contains("[installed"),
            prebuilt: true,
        });
    }
    res
}

pub fn is_installed(name: &str) -> bool {
    let Ok(dir) = std::fs::read_dir("/var/lib/pacman/local") else {
        return sys::succeeds("pacman", ["-Q", name]);
    };
    let prefix = format!("{name}-");
    dir.flatten().any(|e| {
        e.file_name()
            .to_str()
            .and_then(|f| f.strip_prefix(&prefix))
            .is_some_and(|rest| rest.matches('-').count() == 1)
    })
}

pub fn foreign() -> HashSet<String> {
    sys::output("pacman", ["-Qqm"])
        .map(|o| o.lines().map(str::to_string).collect())
        .unwrap_or_default()
}

pub fn clear_stale_lock() -> Result<()> {
    let lock = "/var/lib/pacman/db.lck";
    if !std::path::Path::new(lock).exists() || sys::succeeds("pgrep", ["-x", "pacman"]) {
        return Ok(());
    }
    eprintln!("  removing a stale pacman lock left by an interrupted update");
    sys::run("sudo", ["rm", "-f", lock], None)
}

pub fn install(names: &[String], yes: bool) -> Result<()> {
    let mut args = vec!["pacman".to_string(), "-S".into(), "--needed".into()];
    if yes {
        args.push("--noconfirm".into());
    }
    args.extend(names.iter().cloned());
    sys::run("sudo", &args, None)
}

pub fn remove(names: &[String], yes: bool) -> Result<()> {
    let mut args = vec!["pacman".to_string(), "-Rns".into()];
    if yes {
        args.push("--noconfirm".into());
    }
    args.extend(names.iter().cloned());
    sys::run("sudo", &args, None)
}

pub fn pending_updates() -> Option<usize> {
    if !sys::has("checkupdates") {
        return None;
    }
    Some(sys::count_lines(sys::output("checkupdates", ["--nocolor"])))
}

pub fn update(yes: bool) -> Result<()> {
    let mut args = vec!["pacman", "-Syu"];
    if yes {
        args.push("--noconfirm");
    }
    sys::run("sudo", args, None)
}
