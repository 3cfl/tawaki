use crate::source::{parse_arrows, Candidate, Outdated, Source};
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
    check_names(names, true)?;
    args.push("--".into());
    args.extend(names.iter().cloned());
    sys::run("sudo", &args, None)
}

pub fn remove(names: &[String], yes: bool) -> Result<()> {
    let mut args = vec!["pacman".to_string(), "-Rns".into()];
    if yes {
        args.push("--noconfirm".into());
    }
    check_names(names, true)?;
    args.push("--".into());
    args.extend(names.iter().cloned());
    sys::run("sudo", &args, None)
}

pub fn stable() -> bool {
    std::fs::read_to_string("/etc/fiordland/channel").is_ok_and(|c| c.trim() == "stable")
}

fn checkup_db() -> std::path::PathBuf {
    let uid = sys::output("id", ["-u"]).unwrap_or_default();
    std::env::temp_dir().join(format!("checkup-db-{}", uid.trim()))
}

fn ahead(db: &std::path::Path, known: &HashSet<String>) -> Vec<Outdated> {
    let db = db.to_string_lossy();
    let list = sys::output("pacman", ["--dbpath", &db, "-Sl", "core", "extra", "multilib"]).unwrap_or_default();
    list.lines()
        .filter_map(|l| {
            let mut it = l.split_whitespace();
            let (_, name, repo) = (it.next()?, it.next()?, it.next()?);
            let local = l.split_once("[installed: ")?.1.strip_suffix(']')?;
            (!known.contains(name) && vercmp(local, repo).is_gt()).then(|| Outdated {
                name: name.to_string(),
                old: local.to_string(),
                new: repo.to_string(),
            })
        })
        .collect()
}

pub fn outdated() -> Option<Vec<Outdated>> {
    if !sys::has("checkupdates") {
        return None;
    }
    let db = checkup_db();
    let out = std::process::Command::new("checkupdates")
        .arg("--nocolor")
        .env("LC_ALL", "C")
        .env("CHECKUPDATES_DB", &db)
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    let mut list = match out.status.code() {
        Some(0) => parse_arrows(Some(String::from_utf8_lossy(&out.stdout).into_owned())),
        Some(2) => vec![],
        _ => return None,
    };
    if stable() {
        let known = list.iter().map(|o| o.name.clone()).collect();
        list.extend(ahead(&db, &known));
    }
    Some(list)
}

pub fn vercmp(a: &str, b: &str) -> std::cmp::Ordering {
    sys::output("vercmp", [a, b])
        .and_then(|o| o.trim().parse::<i32>().ok())
        .map(|n| n.cmp(&0))
        .unwrap_or(std::cmp::Ordering::Equal)
}

pub fn foreign_versions() -> Vec<(String, String)> {
    sys::output("pacman", ["-Qm"])
        .unwrap_or_default()
        .lines()
        .filter_map(|l| {
            let (n, v) = l.split_once(' ')?;
            Some((n.to_string(), v.trim().to_string()))
        })
        .collect()
}

fn bad_sigs() -> Vec<String> {
    std::fs::read_dir("/var/lib/pacman/sync")
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "sig"))
        .filter(|p| std::fs::read(p).is_ok_and(|b| b.first() == Some(&b'<')))
        .map(|p| p.to_string_lossy().into_owned())
        .collect()
}

pub fn update(yes: bool) -> Result<()> {
    let bad = bad_sigs();
    if !bad.is_empty() {
        let mut rm = vec!["rm", "-f", "--"];
        rm.extend(bad.iter().map(String::as_str));
        sys::run("sudo", rm, None)?;
    }
    let mut args = vec!["pacman", if stable() { "-Syuu" } else { "-Syu" }];
    if yes {
        args.push("--noconfirm");
    }
    sys::run("sudo", args, None)
}

pub fn valid_name(name: &str, allow_repo: bool) -> bool {
    let pkg = match name.split_once('/') {
        Some((repo, pkg)) if allow_repo => {
            if repo.is_empty() || !repo.chars().all(|c| c.is_ascii_alphanumeric() || "-_".contains(c)) {
                return false;
            }
            pkg
        }
        Some(_) => return false,
        None => name,
    };
    !pkg.is_empty()
        && !pkg.starts_with(['-', '.'])
        && pkg.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || "@._+-".contains(c))
}

fn check_names(names: &[String], allow_repo: bool) -> Result<()> {
    if let Some(bad) = names.iter().find(|n| !valid_name(n, allow_repo)) {
        anyhow::bail!("invalid package name: {bad}");
    }
    Ok(())
}
