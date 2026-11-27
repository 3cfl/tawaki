use crate::source::{parse_arrows, Candidate, Outdated, Source};
use crate::{fsx, sys, ui};
use anyhow::Result;
use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

pub const PACMAN: &str = "/usr/bin/pacman";
const RM: &str = "/usr/bin/rm";
const LOCAL_DB: &str = "/var/lib/pacman/local";
const SYNC_DB: &str = "/var/lib/pacman/sync";
const LOCK: &str = "/var/lib/pacman/db.lck";
const NAME_MAX: usize = 255;

pub fn split_local_entry(dir: &str) -> Option<(&str, &str)> {
    let rel = dir.rfind('-')?;
    let ver = dir[..rel].rfind('-')?;
    let name = &dir[..ver];
    (!name.is_empty() && rel > ver + 1 && rel + 1 < dir.len()).then(|| (name, &dir[ver + 1..]))
}

fn local() -> Option<&'static HashMap<String, String>> {
    static L: OnceLock<Option<HashMap<String, String>>> = OnceLock::new();
    L.get_or_init(|| {
        let dir = std::fs::read_dir(LOCAL_DB).ok()?;
        Some(
            dir.flatten()
                .filter_map(|e| {
                    let f = e.file_name().into_string().ok()?;
                    let (n, v) = split_local_entry(&f)?;
                    Some((n.to_string(), v.to_string()))
                })
                .collect(),
        )
    })
    .as_ref()
}

pub fn is_installed(name: &str) -> bool {
    match local() {
        Some(l) => l.contains_key(name),
        None => valid_name(name, false) && sys::succeeds("pacman", ["-Q", "--", name]),
    }
}

pub fn parse_si(out: &str) -> Vec<Candidate> {
    let mut res = vec![];
    for block in out.split("\n\n") {
        let mut repo = None;
        let mut pname = None;
        let mut version = String::new();
        let mut description = String::new();
        for line in block.lines() {
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
        let Some(name) = pname else {
            continue;
        };
        res.push(Candidate {
            source: Source::Pacman,
            installed: false,
            id: name.clone(),
            name,
            version,
            description,
            repo,
            verified: true,
            prebuilt: true,
        });
    }
    res
}

pub fn match_si(records: &[Candidate], wanted: &str) -> Option<Candidate> {
    let (repo, name) = match wanted.split_once('/') {
        Some((r, n)) => (Some(r), n),
        None => (None, wanted),
    };
    records
        .iter()
        .find(|c| c.name == name && repo.is_none_or(|r| c.repo.as_deref() == Some(r)))
        .cloned()
}

pub fn lookup_many(names: &[String]) -> Vec<Option<Candidate>> {
    let valid: Vec<&String> = names.iter().filter(|n| valid_name(n, true)).collect();
    if valid.is_empty() {
        return vec![None; names.len()];
    }
    let mut args = vec!["-Si".to_string(), "--".to_string()];
    args.extend(valid.iter().map(|n| n.to_string()));
    let records = sys::output_any("pacman", &args)
        .map(|(_, o)| parse_si(&o))
        .unwrap_or_default();
    names
        .iter()
        .map(|n| {
            if !valid_name(n, true) {
                return None;
            }
            let mut c = match_si(&records, n)?;
            c.installed = is_installed(&c.name);
            Some(c)
        })
        .collect()
}

pub fn parse_ss(out: &str) -> Vec<Candidate> {
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

pub fn search(query: &[String]) -> Vec<Candidate> {
    let mut args = vec!["-Ss".to_string(), "--".to_string()];
    args.extend(query.iter().cloned());
    sys::output("pacman", &args)
        .map(|o| parse_ss(&o))
        .unwrap_or_default()
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

pub fn foreign() -> HashSet<String> {
    foreign_versions().into_iter().map(|(n, _)| n).collect()
}

pub fn require_unlocked() -> Result<()> {
    match std::fs::symlink_metadata(LOCK) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
        Ok(_) => anyhow::bail!(
            "package database is locked; finish the active transaction before retrying"
        ),
    }
}

pub fn install(names: &[String], yes: bool) -> Result<()> {
    let mut args = vec!["-S".to_string(), "--needed".into()];
    if yes {
        args.push("--noconfirm".into());
    }
    check_names(names, true)?;
    args.push("--".into());
    args.extend(names.iter().cloned());
    sys::run_root(PACMAN, &args)
}

pub fn remove(names: &[String], yes: bool) -> Result<()> {
    let mut args = vec!["-Rns".to_string()];
    if yes {
        args.push("--noconfirm".into());
    }
    check_names(names, false)?;
    args.push("--".into());
    args.extend(names.iter().cloned());
    sys::run_root(PACMAN, &args)
}

pub fn stable() -> bool {
    std::fs::read_to_string("/etc/fiordland/channel").is_ok_and(|c| c.trim() == "stable")
}

pub fn shared_dir_ok(meta: &std::fs::Metadata, uid: u32) -> bool {
    !meta.file_type().is_symlink() && meta.is_dir() && meta.uid() == uid && meta.mode() & 0o022 == 0
}

fn checkup_db() -> Option<PathBuf> {
    let uid = sys::euid();
    let shared = std::env::temp_dir().join(format!("checkup-db-{uid}"));
    match std::fs::symlink_metadata(&shared) {
        Ok(m) if shared_dir_ok(&m, uid) => return Some(shared),
        Ok(_) => ui::warn(&format!(
            "ignoring {} (not a private directory owned by you)",
            shared.display()
        )),
        Err(_) => {}
    }
    fsx::cache_sub("checkup-db").ok()
}

pub fn parse_sl_ahead(list: &str, known: &HashSet<String>) -> Vec<Outdated> {
    list.lines()
        .filter_map(|l| {
            let mut it = l.split_whitespace();
            let (_, name, repo) = (it.next()?, it.next()?, it.next()?);
            let local = l.split_once("[installed: ")?.1.strip_suffix(']')?;
            (!known.contains(name) && crate::vercmp::vercmp(local, repo).is_gt()).then(|| {
                Outdated {
                    name: name.to_string(),
                    old: local.to_string(),
                    new: repo.to_string(),
                }
            })
        })
        .collect()
}

fn ahead(db: &Path, known: &HashSet<String>) -> Vec<Outdated> {
    let db = db.to_string_lossy();
    let list = ["core", "extra", "multilib"]
        .iter()
        .filter_map(|&r| sys::output("pacman", ["--dbpath", &db, "-Sl", "--", r]))
        .collect::<Vec<_>>()
        .join("\n");
    parse_sl_ahead(&list, known)
}

pub fn outdated() -> Option<Vec<Outdated>> {
    if !sys::has("checkupdates") {
        return None;
    }
    let db = checkup_db()?;
    let out = sys::command("checkupdates")
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
    crate::vercmp::vercmp(a, b)
}

pub fn is_corrupt_sig(head: &[u8]) -> bool {
    head.iter()
        .find(|b| !b.is_ascii_whitespace())
        .is_some_and(|&b| b == b'<')
}

pub fn sig_target_ok(file_name: &str, sync_dir: &Path) -> bool {
    let Some(db) = file_name.strip_suffix(".sig") else {
        return false;
    };
    let Some(repo) = db.strip_suffix(".db").or_else(|| db.strip_suffix(".files")) else {
        return false;
    };
    !repo.is_empty()
        && !repo.starts_with(['-', '.'])
        && repo
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_".contains(c))
        && std::fs::symlink_metadata(sync_dir.join(db)).is_ok_and(|m| m.is_file())
}

pub fn bad_sigs_in(sync_dir: &Path, owner: u32) -> Vec<PathBuf> {
    let Ok(dir) = std::fs::read_dir(sync_dir) else {
        return vec![];
    };
    let mut out: Vec<PathBuf> = dir
        .flatten()
        .filter(|e| {
            e.file_name()
                .to_str()
                .is_some_and(|n| sig_target_ok(n, sync_dir))
        })
        .map(|e| e.path())
        .filter(|p| {
            std::fs::symlink_metadata(p).is_ok_and(|m| m.file_type().is_file() && m.uid() == owner)
        })
        .filter(|p| {
            let mut head = [0u8; 512];
            std::fs::File::open(p)
                .and_then(|mut f| f.read(&mut head))
                .is_ok_and(|n| is_corrupt_sig(&head[..n]))
        })
        .collect();
    out.sort();
    out
}

pub fn update(yes: bool) -> Result<()> {
    let bad = bad_sigs_in(Path::new(SYNC_DB), 0);
    if !bad.is_empty() {
        let mut rm = vec!["-f".to_string(), "--".to_string()];
        for p in &bad {
            eprintln!(
                "  removing a corrupt database signature (a mirror error page): {}",
                p.display()
            );
            rm.push(p.to_string_lossy().into_owned());
        }
        sys::run_root(RM, &rm)?;
    }
    let mut args = vec![if stable() { "-Syuu" } else { "-Syu" }];
    if yes {
        args.push("--noconfirm");
    }
    sys::run_root(PACMAN, &args)
}

pub fn valid_name(name: &str, allow_repo: bool) -> bool {
    let pkg = match name.split_once('/') {
        Some((repo, pkg)) if allow_repo => {
            if repo.is_empty()
                || repo.starts_with(['-', '.'])
                || !repo
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || "-_".contains(c))
            {
                return false;
            }
            pkg
        }
        Some(_) => return false,
        None => name,
    };
    !pkg.is_empty()
        && pkg.len() <= NAME_MAX
        && !pkg.starts_with(['-', '.'])
        && pkg
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || "@._+-".contains(c))
}

fn check_names(names: &[String], allow_repo: bool) -> Result<()> {
    if let Some(bad) = names.iter().find(|n| !valid_name(n, allow_repo)) {
        anyhow::bail!("invalid package name: {}", ui::clean(bad));
    }
    Ok(())
}

pub struct Desc {
    pub name: String,
    pub depends: Vec<String>,
    pub provides: Vec<String>,
}

fn bare(dep: &str) -> String {
    dep.split(['<', '>', '=', ':'])
        .next()
        .unwrap_or("")
        .trim()
        .to_string()
}

pub fn parse_desc(text: &str) -> Option<Desc> {
    let mut d = Desc {
        name: String::new(),
        depends: vec![],
        provides: vec![],
    };
    let mut section = "";
    for line in text.lines() {
        if line.starts_with('%') && line.ends_with('%') {
            section = line;
            continue;
        }
        if line.is_empty() {
            section = "";
            continue;
        }
        match section {
            "%NAME%" => d.name = line.to_string(),
            "%DEPENDS%" => d.depends.push(bare(line)),
            "%PROVIDES%" => d.provides.push(bare(line)),
            _ => {}
        }
    }
    (!d.name.is_empty()).then_some(d)
}

pub fn closure_of(descs: &[Desc], roots: &[&str]) -> HashSet<String> {
    let mut provider: HashMap<&str, &str> = HashMap::new();
    for d in descs {
        for p in &d.provides {
            provider.entry(p.as_str()).or_insert(d.name.as_str());
        }
    }
    for d in descs {
        provider.insert(d.name.as_str(), d.name.as_str());
    }
    let by_name: HashMap<&str, &Desc> = descs.iter().map(|d| (d.name.as_str(), d)).collect();
    let mut seen = HashSet::new();
    let mut todo: Vec<&str> = roots
        .iter()
        .filter_map(|r| provider.get(r).copied())
        .collect();
    while let Some(n) = todo.pop() {
        if !seen.insert(n.to_string()) {
            continue;
        }
        if let Some(d) = by_name.get(n) {
            todo.extend(
                d.depends
                    .iter()
                    .filter_map(|x| provider.get(x.as_str()).copied()),
            );
        }
    }
    seen
}

pub fn runtime_closure(roots: &[&str]) -> Option<HashSet<String>> {
    let dir = std::fs::read_dir(LOCAL_DB).ok()?;
    let descs: Vec<Desc> = dir
        .flatten()
        .filter_map(|e| std::fs::read_to_string(e.path().join("desc")).ok())
        .filter_map(|t| parse_desc(&t))
        .collect();
    Some(closure_of(&descs, roots))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_follow_arch_grammar() {
        for ok in [
            "firefox",
            "lib32-glibc",
            "gtk+",
            "python3.12",
            "@foo",
            "a_b",
            "core/linux",
        ] {
            assert!(valid_name(ok, true), "{ok}");
        }
        for bad in [
            "",
            "-Syu",
            "--config=/tmp/x",
            ".hidden",
            "Firefox",
            "a b",
            "a;rm -rf /",
            "$(id)",
            "a`id`",
            "x\ny",
            "../etc",
            "core/",
            "/linux",
            "-core/linux",
            "a/b/c",
            "ünïcode",
        ] {
            assert!(!valid_name(bad, true), "{bad:?}");
        }
        assert!(!valid_name("core/linux", false));
        assert!(valid_name(&"a".repeat(255), false));
        assert!(!valid_name(&"a".repeat(256), false));
    }

    #[test]
    fn check_names_reports_bad_name() {
        let e = check_names(&["ok".into(), "-x".into()], false).unwrap_err();
        assert!(e.to_string().contains("-x"));
    }

    #[test]
    fn local_entry_split() {
        assert_eq!(
            split_local_entry("firefox-120.0-1"),
            Some(("firefox", "120.0-1"))
        );
        assert_eq!(
            split_local_entry("lib32-glibc-2.40+r16-1.1"),
            Some(("lib32-glibc", "2.40+r16-1.1"))
        );
        assert_eq!(
            split_local_entry("python-1:3.12-2"),
            Some(("python", "1:3.12-2"))
        );
        assert_eq!(split_local_entry("ALPM_DB_VERSION"), None);
        assert_eq!(split_local_entry("x-1"), None);
        assert_eq!(split_local_entry("-1-2"), None);
    }

    #[test]
    fn si_batch_parsing() {
        let out = "Repository      : core\nName            : bash\nVersion         : 5.2-1\nDescription     : The GNU shell\n\nRepository      : extra\nName            : vim\nVersion         : 9.1-1\nDescription     : Vi Improved\n\nRepository      : testing\nName            : bash\nVersion         : 5.3-1\nDescription     : newer\n";
        let recs = parse_si(out);
        assert_eq!(recs.len(), 3);
        let b = match_si(&recs, "bash").unwrap();
        assert_eq!(b.repo.as_deref(), Some("core"));
        assert_eq!(b.version, "5.2-1");
        let t = match_si(&recs, "testing/bash").unwrap();
        assert_eq!(t.version, "5.3-1");
        assert!(match_si(&recs, "extra/bash").is_none());
        assert!(match_si(&recs, "nano").is_none());
        assert_eq!(match_si(&recs, "vim").unwrap().description, "Vi Improved");
    }

    #[test]
    fn ss_parsing_dedups() {
        let out = "core/bash 5.2-1 [installed]\n    The GNU shell\nextra/bash 5.2-1\n    dup\nextra/zsh 5.9-1\n    Z shell\n";
        let r = parse_ss(out);
        assert_eq!(r.len(), 2);
        assert!(r[0].installed);
        assert_eq!(r[0].description, "The GNU shell");
        assert_eq!(r[1].name, "zsh");
        assert!(!r[1].installed);
    }

    #[test]
    fn sl_ahead_uses_native_vercmp() {
        let list = "core bash 5.2-1 [installed: 5.3-1]\ncore zsh 5.9-1 [installed: 5.9-1]\nextra vim 9.1-1 [installed: 9.0-1]\nextra gcc 14-1 [installed: 1:13-1]\nextra skip 1-1 [installed: 2-1]\n";
        let known: HashSet<String> = ["skip".to_string()].into();
        let r = parse_sl_ahead(list, &known);
        let names: Vec<&str> = r.iter().map(|o| o.name.as_str()).collect();
        assert_eq!(names, ["bash", "gcc"]);
        assert_eq!(r[0].old, "5.3-1");
        assert_eq!(r[0].new, "5.2-1");
    }

    #[test]
    fn corrupt_sig_detection_is_narrow() {
        assert!(is_corrupt_sig(b"<!DOCTYPE html><html>"));
        assert!(is_corrupt_sig(b"\n  <html>"));
        assert!(!is_corrupt_sig(&[0x89, 0x02, 0x33]));
        assert!(!is_corrupt_sig(&[0x88, 0x75]));
        assert!(!is_corrupt_sig(&[0xc2, 0xc0]));
        assert!(!is_corrupt_sig(b"-----BEGIN PGP SIGNATURE-----"));
        assert!(!is_corrupt_sig(b""));
        assert!(!is_corrupt_sig(b"   "));
    }

    #[test]
    fn sig_targets_are_only_sync_db_signatures() {
        let d = fsx::test_dir("sync");
        for f in ["core.db", "extra.files", "weird.db"] {
            std::fs::write(d.join(f), b"db").unwrap();
        }
        assert!(sig_target_ok("core.db.sig", &d));
        assert!(sig_target_ok("extra.files.sig", &d));
        assert!(!sig_target_ok("extra.db.sig", &d));
        assert!(!sig_target_ok("core.db", &d));
        assert!(!sig_target_ok("linux-6.1-1-x86_64.pkg.tar.zst.sig", &d));
        assert!(!sig_target_ok(".db.sig", &d));
        assert!(!sig_target_ok("../core.db.sig", &d));
        std::os::unix::fs::symlink(d.join("core.db"), d.join("link.db")).unwrap();
        assert!(!sig_target_ok("link.db.sig", &d));
    }

    #[test]
    fn only_owned_regular_html_sigs_are_flagged() {
        let d = fsx::test_dir("sigs");
        let me = sys::euid();
        for f in ["core.db", "extra.db", "multilib.db", "community.db"] {
            std::fs::write(d.join(f), b"db").unwrap();
        }
        std::fs::write(d.join("core.db.sig"), b"<html>404</html>").unwrap();
        std::fs::write(d.join("extra.db.sig"), [0x89u8, 0x01, 0x33]).unwrap();
        std::fs::write(d.join("elsewhere"), b"<html>").unwrap();
        std::os::unix::fs::symlink(d.join("elsewhere"), d.join("multilib.db.sig")).unwrap();
        std::fs::write(d.join("orphan.db.sig"), b"<html>").unwrap();
        let bad = bad_sigs_in(&d, me);
        assert_eq!(bad, vec![d.join("core.db.sig")]);
        assert!(bad_sigs_in(&d, me.wrapping_add(1)).is_empty());
    }

    #[test]
    fn desc_and_closure() {
        let a = "%NAME%\nflatpak\n\n%VERSION%\n1.15-1\n\n%DEPENDS%\nostree>=2023\nlibostree-1.so=1-64\nbubblewrap\n\n";
        let b = "%NAME%\nostree\n\n%PROVIDES%\nlibostree-1.so=1-64\n\n%DEPENDS%\nglib2\n\n";
        let c = "%NAME%\nglib2\n\n";
        let d = "%NAME%\nbubblewrap\n\n%DEPENDS%\nglibc\n";
        let e = "%NAME%\nglibc\n\n";
        let f = "%NAME%\nfirefox\n\n%DEPENDS%\ngtk3\n";
        let descs: Vec<Desc> = [a, b, c, d, e, f]
            .iter()
            .filter_map(|t| parse_desc(t))
            .collect();
        assert_eq!(descs[0].depends, ["ostree", "libostree-1.so", "bubblewrap"]);
        let cl = closure_of(&descs, &["flatpak"]);
        let mut v: Vec<&str> = cl.iter().map(String::as_str).collect();
        v.sort();
        assert_eq!(v, ["bubblewrap", "flatpak", "glib2", "glibc", "ostree"]);
        assert!(closure_of(&descs, &["missing"]).is_empty());
        assert!(parse_desc("%VERSION%\n1\n").is_none());
    }
}
