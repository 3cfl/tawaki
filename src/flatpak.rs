use crate::config::{Config, FlatpakScope};
use crate::source::{norm, Candidate, Outdated, Source};
use crate::{fpindex, sys, ui};
use anyhow::{bail, Result};
use std::collections::{HashMap, HashSet};

const GENERIC: [&str; 6] = ["client", "app", "desktop", "application", "flatpak", "gui"];

pub fn available() -> bool {
    sys::has("flatpak")
}

pub fn valid_id(id: &str) -> bool {
    if id.is_empty() || id.len() > 255 {
        return false;
    }
    let segs: Vec<&str> = id.split('.').collect();
    if segs.len() < 3 {
        return false;
    }
    let last = segs.len() - 1;
    segs.iter().enumerate().all(|(i, s)| {
        let dash = i == last;
        let mut b = s.bytes();
        b.next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == b'_' || (dash && c == b'-'))
            && b.all(|c| c.is_ascii_alphanumeric() || c == b'_' || (dash && c == b'-'))
    })
}

fn valid_token(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 128
        && !s.starts_with(['-', '.'])
        && s.bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"-_.".contains(&c))
}

pub fn valid_remote(r: &str) -> bool {
    valid_token(r)
}

pub fn valid_branch(b: &str) -> bool {
    valid_token(b)
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
    search_in(apps, &installed, &cfg.flatpak_remote, query)
}

pub fn search_in(
    apps: &[fpindex::App],
    installed: &HashSet<String>,
    preferred: &str,
    query: &[String],
) -> Vec<Candidate> {
    let terms: Vec<String> = query.iter().map(|q| q.to_lowercase()).collect();
    let mut seen = HashSet::new();
    let mut hits: Vec<&fpindex::App> = apps
        .iter()
        .filter(|a| {
            let hay = format!("{} {} {}", a.name, a.id, a.summary).to_lowercase();
            terms.iter().all(|t| hay.contains(t.as_str()))
        })
        .collect();
    hits.sort_by_key(|a| a.remote != preferred);
    hits.into_iter()
        .filter(|a| seen.insert(a.id.clone()))
        .map(|a| to_candidate(a, installed))
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

pub fn find_app<'a>(
    apps: &'a [fpindex::App],
    preferred: &str,
    name: &str,
) -> Option<&'a fpindex::App> {
    let key = norm(name);
    if key.is_empty() {
        return None;
    }
    let pref = |a: &&fpindex::App| a.remote == preferred;
    let by_name = |a: &&fpindex::App| norm(&a.name) == key;
    let by_id = |a: &&fpindex::App| a.id.eq_ignore_ascii_case(name) || id_matches(&a.id, &key);
    apps.iter()
        .filter(pref)
        .find(by_id)
        .or_else(|| apps.iter().filter(pref).find(by_name))
        .or_else(|| apps.iter().find(by_id))
        .or_else(|| apps.iter().find(by_name))
}

pub fn lookup_many(cfg: &Config, names: &[String]) -> Vec<Option<Candidate>> {
    if !available() {
        return vec![None; names.len()];
    }
    let apps = fpindex::load();
    let installed = fpindex::installed_ids();
    names
        .iter()
        .map(|n| find_app(apps, &cfg.flatpak_remote, n).map(|a| to_candidate(a, &installed)))
        .collect()
}

fn ensure_remote(cfg: &Config, remote: &str) {
    if remote != "flathub" || !matches!(cfg.flatpak_scope, FlatpakScope::User) {
        return;
    }
    sys::succeeds(
        "flatpak",
        [
            "remote-add",
            "--user",
            "--if-not-exists",
            "flathub",
            "https://dl.flathub.org/repo/flathub.flatpakrepo",
        ],
    );
}

fn check_ids(ids: &[String]) -> Result<()> {
    if let Some(bad) = ids.iter().find(|i| !valid_id(i)) {
        bail!("invalid Flatpak app id: {}", ui::clean(bad));
    }
    Ok(())
}

pub fn install(cfg: &Config, remote: &str, ids: &[String], yes: bool) -> Result<()> {
    if !valid_remote(remote) {
        bail!("invalid Flatpak remote: {}", ui::clean(remote));
    }
    check_ids(ids)?;
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
    check_ids(ids)?;
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
        (valid_id(id)
            && (id.eq_ignore_ascii_case(name) || norm(n) == key || id_matches(id, &key)))
        .then(|| id.to_string())
    })
}

pub fn parse_updates(
    out: &str,
    installed: &HashMap<String, String>,
    seen: &mut HashSet<String>,
) -> Vec<Outdated> {
    out.lines()
        .filter_map(|l| {
            let mut it = l.split('\t');
            let id = it.next()?.trim().to_string();
            if id.is_empty() || !seen.insert(id.clone()) {
                return None;
            }
            let new = match it.next().unwrap_or("").trim() {
                "" => "latest".to_string(),
                v => v.to_string(),
            };
            let old = installed.get(&id).cloned().unwrap_or_default();
            Some(Outdated { name: id, old, new })
        })
        .collect()
}

#[derive(Default, Debug)]
pub struct Updates {
    pub all: Vec<Outdated>,
    pub user: usize,
    pub system: usize,
}

fn remote_updates(scope: &str) -> Option<String> {
    sys::output(
        "flatpak",
        [
            "remote-ls",
            scope,
            "--updates",
            "--columns=application,version",
        ],
    )
}

pub fn outdated() -> Option<Updates> {
    if !available() {
        return None;
    }
    let (installed, user, system) = std::thread::scope(|s| {
        let i = s.spawn(|| sys::output("flatpak", ["list", "--columns=application,version"]));
        let u = s.spawn(|| remote_updates("--user"));
        let y = s.spawn(|| remote_updates("--system"));
        (
            i.join().ok().flatten(),
            u.join().ok().flatten(),
            y.join().ok().flatten(),
        )
    });
    if user.is_none() && system.is_none() {
        return None;
    }
    let installed: HashMap<String, String> = installed
        .unwrap_or_default()
        .lines()
        .filter_map(|l| {
            let (id, v) = l.split_once('\t')?;
            Some((id.trim().to_string(), v.trim().to_string()))
        })
        .collect();
    let mut seen = HashSet::new();
    let mut all = parse_updates(user.as_deref().unwrap_or(""), &installed, &mut seen);
    let mut sys_list = parse_updates(system.as_deref().unwrap_or(""), &installed, &mut seen);
    let user = all.len();
    let system = sys_list.len();
    all.append(&mut sys_list);
    Some(Updates { all, user, system })
}

pub fn update(scope: Option<&str>, yes: bool) -> Result<()> {
    let mut args = vec!["update"];
    if let Some(s) = scope {
        args.push(s);
    }
    if yes {
        args.push("-y");
    }
    sys::run("flatpak", args, None)
}

pub fn update_user_quiet() -> (bool, String) {
    let out = sys::command("flatpak")
        .args(["update", "--user", "-y", "--noninteractive"])
        .stdin(std::process::Stdio::null())
        .output();
    match out {
        Ok(o) => {
            let mut text = String::from_utf8_lossy(&o.stdout).into_owned();
            text.push_str(&String::from_utf8_lossy(&o.stderr));
            (o.status.success(), text)
        }
        Err(e) => (false, format!("couldn't start flatpak: {e}")),
    }
}

fn field(info: &str, key: &str) -> Option<String> {
    info.lines().find_map(|l| {
        let (k, v) = l.split_once(':')?;
        (k.trim() == key).then(|| v.trim().to_string())
    })
}

pub fn bytes(size: &str) -> u64 {
    let mut it = size.split_whitespace();
    let n: f64 = it.next().and_then(|n| n.parse().ok()).unwrap_or(0.0);
    let mult = match it.next().unwrap_or("") {
        "kB" => 1e3,
        "MB" => 1e6,
        "GB" => 1e9,
        _ => 1.0,
    };
    let v = n * mult;
    if v.is_finite() && v > 0.0 {
        v as u64
    } else {
        0
    }
}

pub fn parse_runtime(rt: &str) -> Option<(&str, &str, &str)> {
    let mut parts = rt.split('/');
    let (name, arch, branch) = (parts.next()?, parts.next()?, parts.next()?);
    (parts.next().is_none() && valid_id(name) && valid_branch(arch) && valid_branch(branch))
        .then_some((name, arch, branch))
}

pub fn missing_runtime(cfg: &Config, remote: &str, id: &str) -> Option<(String, u64)> {
    if !valid_remote(remote) || !valid_id(id) {
        return None;
    }
    let info = sys::output("flatpak", ["remote-info", scope_flag(cfg), remote, id])?;
    let rt = field(&info, "Runtime")?;
    let (name, _, branch) = parse_runtime(&rt)?;
    if sys::succeeds("flatpak", ["info", &format!("{name}//{branch}")]) {
        return None;
    }
    let rinfo = sys::output(
        "flatpak",
        [
            "remote-info",
            scope_flag(cfg),
            remote,
            &format!("runtime/{rt}"),
        ],
    )?;
    Some((
        format!("{name} {branch}"),
        bytes(&field(&rinfo, "Download Size")?),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app(id: &str, name: &str, remote: &str) -> fpindex::App {
        fpindex::App {
            id: id.into(),
            name: name.into(),
            summary: format!("{name} summary"),
            version: "1.0".into(),
            remote: remote.into(),
            verified: false,
        }
    }

    #[test]
    fn app_id_grammar() {
        for ok in [
            "org.mozilla.firefox",
            "com.spotify.Client",
            "org.gnome.Platform",
            "io.github.some_one.my-app",
            "com.valvesoftware.Steam",
            "org.freedesktop.Platform.GL.default",
        ] {
            assert!(valid_id(ok), "{ok}");
        }
        for bad in [
            "",
            "firefox",
            "org.mozilla",
            "--from=https://evil/x.flatpakref",
            "-org.a.b",
            "org..b.c",
            "org.a.b.",
            ".org.a.b",
            "org.1a.b",
            "org.my-app.b",
            "org.a.b c",
            "org.a.b;id",
            "org.a/b.c",
            "org.a.b\n",
        ] {
            assert!(!valid_id(bad), "{bad:?}");
        }
        assert!(!valid_id(&format!("a.b.{}", "c".repeat(260))));
    }

    #[test]
    fn remote_and_branch_grammar() {
        assert!(valid_remote("flathub"));
        assert!(valid_remote("flathub-beta"));
        assert!(!valid_remote("--no-gpg-verify"));
        assert!(!valid_remote("a/b"));
        assert!(!valid_remote(""));
        assert!(valid_branch("23.08"));
        assert!(valid_branch("x86_64"));
        assert!(!valid_branch("-x"));
    }

    #[test]
    fn runtime_ref_parsing() {
        assert_eq!(
            parse_runtime("org.gnome.Platform/x86_64/46"),
            Some(("org.gnome.Platform", "x86_64", "46"))
        );
        assert!(parse_runtime("--evil/x86_64/46").is_none());
        assert!(parse_runtime("org.gnome.Platform/x86_64").is_none());
        assert!(parse_runtime("org.gnome.Platform/x86_64/46/x").is_none());
    }

    #[test]
    fn download_size_parsing() {
        assert_eq!(bytes("1.5 MB"), 1_500_000);
        assert_eq!(bytes("300 kB"), 300_000);
        assert_eq!(bytes("2 GB"), 2_000_000_000);
        assert_eq!(bytes("NaN MB"), 0);
        assert_eq!(bytes("-5 MB"), 0);
        assert_eq!(bytes("1e400 GB"), 0);
        assert_eq!(bytes(""), 0);
    }

    #[test]
    fn lookup_prefers_configured_remote() {
        let apps = vec![
            app("org.example.Spotify", "Spotify", "other"),
            app("com.spotify.Client", "Spotify", "flathub"),
            app("org.mozilla.firefox", "Firefox", "flathub"),
        ];
        assert_eq!(
            find_app(&apps, "flathub", "spotify").unwrap().id,
            "com.spotify.Client"
        );
        assert_eq!(
            find_app(&apps, "other", "spotify").unwrap().id,
            "org.example.Spotify"
        );
        assert_eq!(
            find_app(&apps, "flathub", "Firefox").unwrap().id,
            "org.mozilla.firefox"
        );
        assert!(find_app(&apps, "flathub", "steam").is_none());
        assert!(find_app(&apps, "flathub", "--").is_none());
    }

    #[test]
    fn search_dedups_and_marks_installed() {
        let apps = vec![
            app("org.a.Obs", "OBS", "other"),
            app("org.a.Obs", "OBS", "flathub"),
            app("org.b.Gimp", "GIMP", "flathub"),
        ];
        let installed: HashSet<String> = ["org.a.Obs".to_string()].into();
        let r = search_in(&apps, &installed, "flathub", &["obs".into()]);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].repo.as_deref(), Some("flathub"));
        assert!(r[0].installed);
    }

    #[test]
    fn update_listing() {
        let installed: HashMap<String, String> =
            [("org.a.B".to_string(), "1.0".to_string())].into();
        let mut seen = HashSet::new();
        let u = parse_updates("org.a.B\t1.1\norg.c.D\t\n", &installed, &mut seen);
        assert_eq!(u.len(), 2);
        assert_eq!(u[0].old, "1.0");
        assert_eq!(u[1].new, "latest");
        let s = parse_updates("org.a.B\t1.1\norg.e.F\t2\n", &installed, &mut seen);
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].name, "org.e.F");
    }
}
