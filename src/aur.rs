use crate::config::Config;
use crate::pacman;
use crate::source::{parse_arrows, Candidate, Outdated, Source};
use crate::{fsx, sys, ui};
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

const RPC: &str = "https://aur.archlinux.org/rpc/v5";
const AUR_GIT: &str = "https://aur.archlinux.org";
const MAX_BODY: u64 = 16 << 20;
const MAX_CACHE: u64 = 16 << 20;
const SEARCH_TTL: Duration = Duration::from_secs(1800);
const INFO_TTL: Duration = Duration::from_secs(120);
const INFO_CHUNK: usize = 150;
const MAX_REVIEW_FILE: u64 = 256 << 10;

#[derive(Deserialize, Serialize, Default)]
pub struct Resp {
    #[serde(default)]
    pub results: Vec<Pkg>,
    #[serde(default)]
    pub error: Option<String>,
}

#[derive(Deserialize, Serialize, Clone, Debug)]
#[serde(rename_all = "PascalCase")]
pub struct Pkg {
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub popularity: f64,
    #[serde(default)]
    pub out_of_date: Option<i64>,
    #[serde(default)]
    pub make_depends: Option<Vec<String>>,
}

#[derive(Deserialize, Serialize)]
struct InfoEntry {
    pkg: Option<Pkg>,
}

pub fn valid_name(name: &str) -> bool {
    let mut b = name.bytes();
    b.next()
        .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        && name.len() <= 255
        && b.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || b".+_-@".contains(&c))
}

fn agent() -> &'static ureq::Agent {
    static A: OnceLock<ureq::Agent> = OnceLock::new();
    A.get_or_init(|| {
        ureq::AgentBuilder::new()
            .https_only(true)
            .timeout_connect(Duration::from_secs(5))
            .timeout(Duration::from_secs(10))
            .redirects(2)
            .user_agent(concat!("tawaki/", env!("CARGO_PKG_VERSION")))
            .build()
    })
}

pub fn parse_resp(bytes: &[u8]) -> Option<Resp> {
    let r: Resp = serde_json::from_slice(bytes).ok()?;
    r.error.is_none().then_some(r)
}

pub fn read_body<R: Read>(r: R, max: u64) -> Option<Vec<u8>> {
    let mut buf = Vec::new();
    r.take(max + 1).read_to_end(&mut buf).ok()?;
    (buf.len() as u64 <= max).then_some(buf)
}

fn get(url: &str, query: &[(&str, &str)]) -> Option<Resp> {
    let mut req = agent().get(url);
    for (k, v) in query {
        req = req.query(k, v);
    }
    let resp = req.call().ok()?;
    parse_resp(&read_body(resp.into_reader(), MAX_BODY)?)
}

pub fn cache_key(term: &str) -> String {
    let safe: String = term
        .chars()
        .take(48)
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let mut h = std::collections::hash_map::DefaultHasher::new();
    term.hash(&mut h);
    format!("{safe}-{:016x}.json", h.finish())
}

fn cached_search(term: &str) -> Option<Resp> {
    let path = fsx::cache_sub("search")
        .ok()
        .map(|d| d.join(cache_key(term)));
    if let Some(p) = path.as_ref().filter(|p| fsx::fresh(p, SEARCH_TTL)) {
        if let Some(r) = fsx::read_limited(p, MAX_CACHE).and_then(|b| parse_resp(&b)) {
            return Some(r);
        }
    }
    let r = get(
        &format!("{RPC}/search/{}", encode(term)),
        &[("by", "name-desc")],
    )?;
    if let (Some(p), Ok(b)) = (path, serde_json::to_vec(&r)) {
        let _ = fsx::write_atomic(&p, &b);
    }
    Some(r)
}

fn info_cached(dir: Option<&Path>, name: &str) -> Option<Option<Pkg>> {
    let p = dir?.join(format!("{name}.json"));
    if !fsx::fresh(&p, INFO_TTL) {
        return None;
    }
    let e: InfoEntry = serde_json::from_slice(&fsx::read_limited(&p, MAX_CACHE)?).ok()?;
    match e.pkg {
        Some(pkg) if pkg.name != name => None,
        other => Some(other),
    }
}

fn info_store(dir: Option<&Path>, name: &str, pkg: Option<&Pkg>) {
    let Some(dir) = dir else {
        return;
    };
    let entry = InfoEntry { pkg: pkg.cloned() };
    if let Ok(b) = serde_json::to_vec(&entry) {
        let _ = fsx::write_atomic(&dir.join(format!("{name}.json")), &b);
    }
}

pub fn info_many(names: &[String]) -> Option<HashMap<String, Pkg>> {
    let dir = fsx::cache_sub("aur-info").ok();
    let mut found = HashMap::new();
    let mut todo: Vec<&String> = vec![];
    for n in names.iter().filter(|n| valid_name(n)) {
        if found.contains_key(n) || todo.contains(&n) {
            continue;
        }
        match info_cached(dir.as_deref(), n) {
            Some(Some(p)) => {
                found.insert(n.clone(), p);
            }
            Some(None) => {}
            None => todo.push(n),
        }
    }
    let mut ok = true;
    for chunk in todo.chunks(INFO_CHUNK) {
        let q: Vec<(&str, &str)> = chunk.iter().map(|n| ("arg[]", n.as_str())).collect();
        let Some(resp) = get(&format!("{RPC}/info"), &q) else {
            ok = false;
            continue;
        };
        for n in chunk {
            let pkg = resp.results.iter().find(|p| &&p.name == n);
            info_store(dir.as_deref(), n, pkg);
            if let Some(p) = pkg {
                found.insert(p.name.clone(), p.clone());
            }
        }
    }
    ok.then_some(found)
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

pub fn variants(name: &str) -> Vec<String> {
    if !valid_name(name) {
        return vec![];
    }
    [
        format!("{name}-bin"),
        name.to_string(),
        format!("{name}-git"),
    ]
    .into_iter()
    .filter(|w| valid_name(w))
    .collect()
}

pub fn pick(info: &HashMap<String, Pkg>, name: &str) -> Option<Pkg> {
    let wanted = variants(name);
    let find = |fresh_only: bool| {
        wanted.iter().find_map(|w| {
            info.get(w)
                .filter(|p| !fresh_only || p.out_of_date.is_none())
        })
    };
    find(true).or_else(|| find(false)).cloned()
}

pub fn lookup_many(names: &[String]) -> Vec<Option<Candidate>> {
    let all: Vec<String> = names.iter().flat_map(|n| variants(n)).collect();
    if all.is_empty() {
        return vec![None; names.len()];
    }
    let info = info_many(&all).unwrap_or_default();
    names
        .iter()
        .map(|n| {
            let p = pick(&info, n)?;
            Some(to_candidate(&p, pacman::is_installed(&p.name)))
        })
        .collect()
}

fn encode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' | b'_' | b'.' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

pub fn filter_hits<'a>(resp: &'a Resp, query: &[String]) -> Vec<&'a Pkg> {
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
    hits
}

pub fn search(query: &[String]) -> Vec<Candidate> {
    let Some(longest) = query.iter().max_by_key(|q| q.len()) else {
        return vec![];
    };
    let Some(resp) = cached_search(longest) else {
        return vec![];
    };
    filter_hits(&resp, query)
        .into_iter()
        .map(|p| to_candidate(p, pacman::is_installed(&p.name)))
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

fn check_names(names: &[String]) -> Result<()> {
    if let Some(bad) = names.iter().find(|n| !valid_name(n)) {
        bail!("invalid AUR package name: {}", ui::clean(bad));
    }
    Ok(())
}

pub fn clone_url(name: &str) -> String {
    format!("{AUR_GIT}/{name}.git")
}

fn git(dir: &Path, args: &[&str]) -> Option<String> {
    let mut full = vec!["-C".to_string(), dir.to_string_lossy().into_owned()];
    full.extend(args.iter().map(|a| a.to_string()));
    sys::output("git", &full)
}

fn fetch(name: &str, dir: &Path) -> Result<()> {
    let url = clone_url(name);
    if dir.join(".git").is_dir() {
        let origin = git(dir, &["config", "--get", "remote.origin.url"]).unwrap_or_default();
        if origin.trim() == url {
            return sys::run(
                "git",
                ["-C", &dir.to_string_lossy(), "pull", "--ff-only", "-q"],
                None,
            );
        }
        ui::warn(&format!(
            "the cached clone of {name} doesn't point at {url}, cloning it again"
        ));
    }
    std::fs::remove_dir_all(dir).with_context(|| format!("couldn't clear {}", dir.display()))?;
    fsx::private_dir(dir)?;
    sys::run(
        "git",
        ["clone", "-q", "--", &url, &dir.to_string_lossy()],
        None,
    )?;
    let origin = git(dir, &["config", "--get", "remote.origin.url"]).unwrap_or_default();
    if origin.trim() != url {
        bail!("{name}: clone source mismatch");
    }
    Ok(())
}

pub fn pager_argv(pager: Option<&str>) -> Vec<String> {
    let argv: Vec<String> = pager
        .unwrap_or("")
        .split_whitespace()
        .map(str::to_string)
        .collect();
    if argv.is_empty() {
        vec!["less".to_string()]
    } else {
        argv
    }
}

fn review_text(name: &str, dir: &Path, since: Option<&str>) -> String {
    let mut text = String::new();
    if let Some(old) = since {
        if let Some(diff) = git(
            dir,
            &["--no-pager", "diff", "--no-color", old, "HEAD", "--"],
        ) {
            text.push_str(&format!(
                "Changes to {name} since you last reviewed it:\n\n"
            ));
            text.push_str(&diff);
            return ui::clean_block(&text);
        }
    }
    let files: Vec<String> = git(dir, &["ls-files", "-z"])
        .unwrap_or_default()
        .split('\0')
        .filter(|f| !f.is_empty())
        .map(str::to_string)
        .collect();
    let mut order: Vec<&String> = files.iter().filter(|f| *f == "PKGBUILD").collect();
    order.extend(files.iter().filter(|f| f.ends_with(".install")));
    order.extend(
        files
            .iter()
            .filter(|f| *f != "PKGBUILD" && !f.ends_with(".install")),
    );
    text.push_str(&format!("Files in {name}:\n"));
    for f in &files {
        text.push_str(&format!("  {f}\n"));
    }
    for f in order {
        let p = dir.join(f);
        let meta = std::fs::symlink_metadata(&p).ok();
        let shown = meta
            .filter(|m| m.is_file() && m.len() <= MAX_REVIEW_FILE)
            .and_then(|_| std::fs::read(&p).ok())
            .and_then(|b| String::from_utf8(b).ok());
        match shown {
            Some(body) => text.push_str(&format!("\n===== {f} =====\n{body}\n")),
            None => text.push_str(&format!(
                "\n===== {f} ===== (binary, large or a link, not shown)\n"
            )),
        }
    }
    ui::clean_block(&text)
}

fn page(text: &str) {
    let argv = pager_argv(std::env::var("PAGER").ok().as_deref());
    let child = sys::command(&argv[0])
        .args(&argv[1..])
        .stdin(std::process::Stdio::piped())
        .spawn();
    match child {
        Ok(mut c) => {
            if let Some(mut stdin) = c.stdin.take() {
                let _ = stdin.write_all(text.as_bytes());
            }
            if c.wait().is_ok_and(|s| s.success()) {
                return;
            }
        }
        Err(_) => ui::warn(&format!(
            "couldn't start pager {}, printing instead",
            argv[0]
        )),
    }
    println!("{text}");
}

fn reviewed_path(name: &str) -> Option<PathBuf> {
    fsx::cache_sub("aur-reviewed").ok().map(|d| d.join(name))
}

fn build_one(name: &str, yes: bool) -> Result<()> {
    if !valid_name(name) {
        bail!("invalid package name: {}", ui::clean(name));
    }
    if sys::euid() == 0 {
        bail!("refusing to build AUR packages as root, run tawaki as your normal user");
    }
    let dir = fsx::private_dir(&fsx::cache_sub("aur")?.join(name))?;
    fetch(name, &dir)?;
    let head = git(&dir, &["rev-parse", "HEAD"]).map(|h| h.trim().to_string());
    let rpath = reviewed_path(name);
    let last = rpath
        .as_ref()
        .and_then(|p| fsx::read_limited(p, 128))
        .and_then(|b| String::from_utf8(b).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| matches!(s.len(), 40 | 64) && s.bytes().all(|b| b.is_ascii_hexdigit()));
    let mut reviewed = false;
    if !yes {
        if ui::confirm(&format!("Review the build files for {name}?"), true) {
            let since = last.as_deref().filter(|l| head.as_deref() != Some(*l));
            page(&review_text(name, &dir, since));
            reviewed = true;
        }
        if !ui::confirm(&format!("Build and install {name}?"), true) {
            bail!("cancelled");
        }
    }
    let mut args = vec!["-si", "--needed"];
    if yes {
        args.push("--noconfirm");
    }
    sys::run("makepkg", args, Some(&dir))?;
    if let (true, Some(h), Some(p)) = (reviewed, head, rpath) {
        let _ = fsx::write_atomic(&p, h.as_bytes());
    }
    Ok(())
}

pub fn install(cfg: &Config, names: &[String], yes: bool) -> Result<()> {
    check_names(names)?;
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

pub fn newer(local: &[(String, String)], info: &HashMap<String, Pkg>) -> Vec<Outdated> {
    local
        .iter()
        .filter_map(|(n, old)| {
            let p = info.get(n)?;
            (pacman::vercmp(&p.version, old) == std::cmp::Ordering::Greater).then(|| Outdated {
                name: p.name.clone(),
                old: old.clone(),
                new: p.version.clone(),
            })
        })
        .collect()
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
    let names: Vec<String> = local.iter().map(|(n, _)| n.clone()).collect();
    let info = info_many(&names)?;
    Some(newer(&local, &info))
}

pub fn update(cfg: &Config, yes: bool) -> Result<()> {
    let Some(h) = helper(cfg) else {
        for o in outdated(cfg).unwrap_or_default() {
            ui::info(&format!("Building {} {}", o.name, ui::clean(&o.new)));
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

#[cfg(test)]
mod tests {
    use super::*;

    fn pkg(name: &str, version: &str, ood: bool) -> Pkg {
        Pkg {
            name: name.into(),
            version: version.into(),
            description: Some(format!("{name} desc")),
            popularity: 1.0,
            out_of_date: ood.then_some(1),
            make_depends: None,
        }
    }

    #[test]
    fn aur_name_grammar() {
        for ok in [
            "paru",
            "paru-bin",
            "visual-studio-code-bin",
            "python-foo.bar",
            "0ad",
            "g++",
        ] {
            assert!(valid_name(ok), "{ok}");
        }
        for bad in [
            "", "-x", ".x", "_x", "@x", "X", "a/b", "..", "a b", "a;b", "$(id)", "a\0b", "é",
        ] {
            assert!(!valid_name(bad), "{bad:?}");
        }
        assert!(!valid_name(&"a".repeat(256)));
    }

    #[test]
    fn clone_url_is_pinned_to_aur_https() {
        assert_eq!(clone_url("paru"), "https://aur.archlinux.org/paru.git");
    }

    #[test]
    fn variants_are_validated() {
        assert_eq!(variants("foo"), ["foo-bin", "foo", "foo-git"]);
        assert!(variants("-foo").is_empty());
        assert!(variants("../x").is_empty());
    }

    #[test]
    fn pick_prefers_bin_then_fresh() {
        let mut info = HashMap::new();
        for p in [
            pkg("foo", "1", false),
            pkg("foo-bin", "1", true),
            pkg("foo-git", "1", false),
        ] {
            info.insert(p.name.clone(), p);
        }
        assert_eq!(pick(&info, "foo").unwrap().name, "foo");
        info.get_mut("foo-bin").unwrap().out_of_date = None;
        assert_eq!(pick(&info, "foo").unwrap().name, "foo-bin");
        assert!(pick(&info, "bar").is_none());
        let only_stale: HashMap<String, Pkg> = [("baz".to_string(), pkg("baz", "1", true))].into();
        assert_eq!(pick(&only_stale, "baz").unwrap().name, "baz");
    }

    #[test]
    fn rpc_parsing_is_defensive() {
        assert!(parse_resp(b"not json").is_none());
        assert!(parse_resp(br#"{"error":"Too many package results.","results":[]}"#).is_none());
        assert!(parse_resp(br#"{"results":[{"Name":1}]}"#).is_none());
        let deep = format!("{}{}", "[".repeat(100_000), "]".repeat(100_000));
        assert!(parse_resp(deep.as_bytes()).is_none());
        let r = parse_resp(
            br#"{"resultcount":1,"results":[{"Name":"x","Version":"1-1","Description":null,"Popularity":0.5,"OutOfDate":null,"Extra":[1,2]}],"type":"multiinfo","version":5}"#,
        )
        .unwrap();
        assert_eq!(r.results[0].name, "x");
        assert!(r.results[0].description.is_none());
    }

    #[test]
    fn body_size_is_capped() {
        let big = vec![b' '; 1000];
        assert!(read_body(&big[..], 999).is_none());
        assert_eq!(read_body(&big[..], 1000).unwrap().len(), 1000);
    }

    #[test]
    fn cache_keys_are_safe_and_distinct() {
        let a = cache_key("a?b");
        let b = cache_key("a_b");
        assert_ne!(a, b);
        for k in [
            cache_key("../../etc/passwd"),
            cache_key(&"x".repeat(5000)),
            a,
            b,
        ] {
            assert!(!k.contains('/'));
            assert!(!k.starts_with('.'));
            assert!(k.len() < 100);
        }
    }

    #[test]
    fn search_filtering() {
        let resp = Resp {
            results: vec![
                Pkg {
                    popularity: 0.1,
                    ..pkg("obs-studio-git", "1", false)
                },
                Pkg {
                    popularity: 5.0,
                    ..pkg("obs-studio-bin", "1", false)
                },
                pkg("other", "1", false),
            ],
            error: None,
        };
        let hits = filter_hits(&resp, &["OBS".into(), "studio".into()]);
        let names: Vec<&str> = hits.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["obs-studio-bin", "obs-studio-git"]);
    }

    #[test]
    fn newer_uses_vercmp() {
        let info: HashMap<String, Pkg> = [
            ("a".to_string(), pkg("a", "1.10-1", false)),
            ("b".to_string(), pkg("b", "1.0-1", false)),
            ("c".to_string(), pkg("c", "1:0.1-1", false)),
        ]
        .into();
        let local = vec![
            ("a".to_string(), "1.9-1".to_string()),
            ("b".to_string(), "1.0-1".to_string()),
            ("c".to_string(), "2.0-1".to_string()),
            ("d".to_string(), "1-1".to_string()),
        ];
        let r = newer(&local, &info);
        let names: Vec<&str> = r.iter().map(|o| o.name.as_str()).collect();
        assert_eq!(names, ["a", "c"]);
    }

    #[test]
    fn pager_is_split_without_a_shell() {
        assert_eq!(pager_argv(None), ["less"]);
        assert_eq!(pager_argv(Some("  ")), ["less"]);
        assert_eq!(pager_argv(Some("less -R")), ["less", "-R"]);
        assert_eq!(
            pager_argv(Some("less; rm -rf ~")),
            ["less;", "rm", "-rf", "~"]
        );
    }

    #[test]
    fn info_cache_rejects_mismatched_names() {
        let d = fsx::test_dir("info");
        info_store(Some(&d), "foo", Some(&pkg("evil", "1", false)));
        assert!(info_cached(Some(&d), "foo").is_none());
        info_store(Some(&d), "bar", Some(&pkg("bar", "2", false)));
        assert_eq!(info_cached(Some(&d), "bar").unwrap().unwrap().version, "2");
        info_store(Some(&d), "gone", None);
        assert!(info_cached(Some(&d), "gone").unwrap().is_none());
        assert!(info_cached(Some(&d), "never").is_none());
    }
}
