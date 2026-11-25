use crate::{flatpak, fsx};
use quick_xml::events::{BytesStart, Event};
use quick_xml::Reader;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

const MAX_INDEX: u64 = 128 << 20;

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct App {
    pub id: String,
    pub name: String,
    pub summary: String,
    pub version: String,
    pub remote: String,
    pub verified: bool,
}

#[derive(Serialize, Deserialize, Default)]
struct Index {
    key: Vec<String>,
    apps: Vec<App>,
}

fn roots() -> Vec<PathBuf> {
    let mut r = vec![];
    if let Some(d) = dirs::data_dir() {
        r.push(d.join("flatpak"));
    }
    r.push(PathBuf::from("/var/lib/flatpak"));
    r
}

fn sources() -> Vec<(String, PathBuf, String)> {
    let arch = std::env::consts::ARCH;
    let mut seen: HashSet<String> = HashSet::new();
    let mut out = vec![];
    for root in roots() {
        let Ok(remotes) = std::fs::read_dir(root.join("appstream")) else {
            continue;
        };
        for remote in remotes.flatten() {
            let name = remote.file_name().to_string_lossy().into_owned();
            let active = remote.path().join(arch).join("active");
            let xml = active.join("appstream.xml");
            if !flatpak::valid_remote(&name) || !xml.is_file() || seen.contains(&name) {
                continue;
            }
            let commit = std::fs::canonicalize(&active)
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default();
            seen.insert(name.clone());
            out.push((name, xml, commit));
        }
    }
    out.sort();
    out
}

fn cache_path() -> Option<PathBuf> {
    fsx::cache_root().ok().map(|d| d.join("flatpak-index.json"))
}

pub fn sane(a: &App) -> bool {
    flatpak::valid_id(&a.id) && flatpak::valid_remote(&a.remote)
}

pub fn installed_ids() -> HashSet<String> {
    roots()
        .into_iter()
        .filter_map(|r| std::fs::read_dir(r.join("app")).ok())
        .flat_map(|d| d.flatten())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect()
}

fn fetch_catalogue() {
    if !crate::sys::has("flatpak") {
        return;
    }
    eprintln!("  fetching the Flathub catalogue (first run only)…");
    crate::sys::succeeds(
        "flatpak",
        ["update", "--appstream", "--system", "--noninteractive"],
    );
    if sources().is_empty() {
        let args = ["update", "--appstream", "--system", "--noninteractive"];
        if let Ok(argv) = crate::sys::root_argv("/usr/bin/flatpak", &args, crate::sys::euid()) {
            crate::sys::succeeds(&argv[0], &argv[1..]);
        }
    }
}

fn read_cache(path: &Path, key: &[String]) -> Option<Vec<App>> {
    let idx: Index = serde_json::from_slice(&fsx::read_limited(path, MAX_INDEX)?).ok()?;
    (idx.key == key).then(|| idx.apps.into_iter().filter(sane).collect())
}

pub fn load() -> &'static [App] {
    static APPS: OnceLock<Vec<App>> = OnceLock::new();
    APPS.get_or_init(build)
}

fn build() -> Vec<App> {
    let mut srcs = sources();
    if srcs.is_empty() {
        fetch_catalogue();
        srcs = sources();
    }
    let key: Vec<String> = srcs.iter().map(|(n, _, c)| format!("{n}={c}")).collect();
    let path = cache_path();
    if let Some(apps) = path.as_deref().and_then(|p| read_cache(p, &key)) {
        return apps;
    }
    let apps: Vec<App> = std::thread::scope(|s| {
        let hs: Vec<_> = srcs
            .iter()
            .map(|(name, xml, _)| s.spawn(move || parse(xml, name)))
            .collect();
        hs.into_iter()
            .flat_map(|h| h.join().unwrap_or_default())
            .filter(sane)
            .collect()
    });
    let idx = Index { key, apps };
    if let (Some(p), Ok(bytes)) = (path, serde_json::to_vec(&idx)) {
        let _ = fsx::write_atomic(&p, &bytes);
    }
    idx.apps
}

fn has_lang(e: &BytesStart) -> bool {
    e.try_get_attribute("xml:lang").ok().flatten().is_some()
}

fn attr(e: &BytesStart, key: &str) -> Option<String> {
    e.try_get_attribute(key)
        .ok()
        .flatten()
        .and_then(|a| a.unescape_value().ok().map(|v| v.into_owned()))
}

#[derive(PartialEq)]
enum Field {
    None,
    Id,
    Name,
    Summary,
    Verified,
    Bundle,
}

pub fn parse(path: &Path, remote: &str) -> Vec<App> {
    let Ok(file) = std::fs::File::open(path) else {
        return vec![];
    };
    let mut reader = Reader::from_reader(BufReader::with_capacity(1 << 20, file));
    reader.config_mut().trim_text(true);
    let mut buf = Vec::new();
    let mut apps = vec![];
    let mut cur: Option<App> = None;
    let mut bundle = String::new();
    let mut depth = 0usize;
    let mut comp_depth = 0usize;
    let mut field = Field::None;
    let mut got_release = false;

    while let Ok(ev) = reader.read_event_into(&mut buf) {
        match ev {
            Event::Start(e) => {
                depth += 1;
                let tag = e.name();
                let tag = tag.as_ref();
                if tag == b"component" {
                    cur = Some(App {
                        remote: remote.to_string(),
                        ..Default::default()
                    });
                    bundle.clear();
                    comp_depth = depth;
                    got_release = false;
                } else if cur.is_some() {
                    let top = depth == comp_depth + 1;
                    field = match tag {
                        b"id" if top => Field::Id,
                        b"name" if top && !has_lang(&e) => Field::Name,
                        b"summary" if top && !has_lang(&e) => Field::Summary,
                        b"bundle" if top => Field::Bundle,
                        b"value"
                            if attr(&e, "key").as_deref()
                                == Some("flathub::verification::verified") =>
                        {
                            Field::Verified
                        }
                        b"release" if !got_release => {
                            if let (Some(app), Some(v)) = (cur.as_mut(), attr(&e, "version")) {
                                app.version = v;
                                got_release = true;
                            }
                            Field::None
                        }
                        _ => Field::None,
                    };
                }
            }
            Event::Empty(e) => {
                if e.name().as_ref() == b"release" && !got_release {
                    if let (Some(app), Some(v)) = (cur.as_mut(), attr(&e, "version")) {
                        app.version = v;
                        got_release = true;
                    }
                }
            }
            Event::Text(t) => {
                if field != Field::None {
                    if let (Some(app), Ok(text)) = (cur.as_mut(), t.unescape()) {
                        let text = text.trim().to_string();
                        match field {
                            Field::Id => app.id = text,
                            Field::Name => app.name = text,
                            Field::Summary => app.summary = text,
                            Field::Verified => app.verified = text == "true",
                            Field::Bundle => bundle = text,
                            Field::None => {}
                        }
                    }
                }
            }
            Event::End(e) => {
                field = Field::None;
                if e.name().as_ref() == b"component" {
                    if let Some(mut app) = cur.take() {
                        if let Some(rest) = bundle.strip_prefix("app/") {
                            if let Some(id) = rest.split('/').next() {
                                app.id = id.to_string();
                            }
                            if app.name.is_empty() {
                                app.name = app.id.clone();
                            }
                            apps.push(app);
                        }
                    }
                }
                depth = depth.saturating_sub(1);
            }
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }
    apps
}

#[cfg(test)]
mod tests {
    use super::*;

    const XML: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<components version="0.8" origin="flathub">
  <component type="desktop-application">
    <id>org.mozilla.firefox.desktop</id>
    <name>Firefox</name>
    <name xml:lang="de">Feuerfuchs</name>
    <summary>Fast, private browser</summary>
    <releases><release version="133.0" timestamp="1"/><release version="132.0"/></releases>
    <bundle type="flatpak">app/org.mozilla.firefox/x86_64/stable</bundle>
    <metadata><value key="flathub::verification::verified">true</value></metadata>
  </component>
  <component type="desktop-application">
    <id>evil</id>
    <name>Evil</name>
    <bundle type="flatpak">app/--from=https:/x/x86_64/stable</bundle>
  </component>
  <component type="runtime">
    <id>org.gnome.Platform</id>
    <bundle type="flatpak">runtime/org.gnome.Platform/x86_64/46</bundle>
  </component>
</components>
"#;

    #[test]
    fn appstream_parsing_and_sanity_filter() {
        let d = fsx::test_dir("as");
        let p = d.join("appstream.xml");
        std::fs::write(&p, XML).unwrap();
        let apps = parse(&p, "flathub");
        assert_eq!(apps.len(), 2);
        let ff = &apps[0];
        assert_eq!(ff.id, "org.mozilla.firefox");
        assert_eq!(ff.name, "Firefox");
        assert_eq!(ff.version, "133.0");
        assert!(ff.verified);
        let sane_apps: Vec<&App> = apps.iter().filter(|a| sane(a)).collect();
        assert_eq!(sane_apps.len(), 1);
    }

    #[test]
    fn poisoned_cache_is_filtered_or_ignored() {
        let d = fsx::test_dir("idx");
        let p = d.join("flatpak-index.json");
        let idx = Index {
            key: vec!["flathub=/c1".into()],
            apps: vec![
                App {
                    id: "org.a.Good".into(),
                    remote: "flathub".into(),
                    ..Default::default()
                },
                App {
                    id: "--from=https://evil.example/x.flatpakref".into(),
                    remote: "flathub".into(),
                    ..Default::default()
                },
                App {
                    id: "org.a.Bad".into(),
                    remote: "--no-deps".into(),
                    ..Default::default()
                },
            ],
        };
        fsx::write_atomic(&p, &serde_json::to_vec(&idx).unwrap()).unwrap();
        let apps = read_cache(&p, &["flathub=/c1".to_string()]).unwrap();
        assert_eq!(apps.len(), 1);
        assert_eq!(apps[0].id, "org.a.Good");
        assert!(read_cache(&p, &["flathub=/c2".to_string()]).is_none());
        std::fs::write(d.join("junk.json"), b"{not json").unwrap();
        assert!(read_cache(&d.join("junk.json"), &[]).is_none());
    }
}
