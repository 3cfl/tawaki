use quick_xml::events::{BytesStart, Event};
use quick_xml::Reader;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::BufReader;
use std::path::{Path, PathBuf};

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
    let mut seen: HashMap<String, ()> = HashMap::new();
    let mut out = vec![];
    for root in roots() {
        let Ok(remotes) = std::fs::read_dir(root.join("appstream")) else {
            continue;
        };
        for remote in remotes.flatten() {
            let name = remote.file_name().to_string_lossy().into_owned();
            let active = remote.path().join(arch).join("active");
            let xml = active.join("appstream.xml");
            if !xml.is_file() || seen.contains_key(&name) {
                continue;
            }
            let commit = std::fs::canonicalize(&active)
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default();
            seen.insert(name.clone(), ());
            out.push((name, xml, commit));
        }
    }
    out.sort();
    out
}

fn cache_path() -> PathBuf {
    dirs::cache_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("tawaki")
        .join("flatpak-index.json")
}

pub fn installed_ids() -> std::collections::HashSet<String> {
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
    crate::sys::succeeds("flatpak", ["update", "--appstream", "--system", "--noninteractive"]);
    if sources().is_empty() {
        crate::sys::succeeds("sudo", ["flatpak", "update", "--appstream", "--system", "--noninteractive"]);
    }
}

pub fn load() -> Vec<App> {
    let mut srcs = sources();
    if srcs.is_empty() {
        fetch_catalogue();
        srcs = sources();
    }
    let key: Vec<String> = srcs.iter().map(|(n, _, c)| format!("{n}={c}")).collect();
    if let Some(idx) = std::fs::read(cache_path())
        .ok()
        .and_then(|b| serde_json::from_slice::<Index>(&b).ok())
    {
        if idx.key == key {
            return idx.apps;
        }
    }
    let apps: Vec<App> = std::thread::scope(|s| {
        let hs: Vec<_> = srcs
            .iter()
            .map(|(name, xml, _)| s.spawn(move || parse(xml, name)))
            .collect();
        hs.into_iter().flat_map(|h| h.join().unwrap_or_default()).collect()
    });
    let idx = Index { key, apps };
    let p = cache_path();
    if let Some(dir) = p.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(bytes) = serde_json::to_vec(&idx) {
        let _ = std::fs::write(&p, bytes);
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

fn parse(path: &Path, remote: &str) -> Vec<App> {
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

    loop {
        let ev = match reader.read_event_into(&mut buf) {
            Ok(ev) => ev,
            Err(_) => break,
        };
        match ev {
            Event::Start(e) => {
                depth += 1;
                let tag = e.name();
                let tag = tag.as_ref();
                if tag == b"component" {
                    cur = Some(App { remote: remote.to_string(), ..Default::default() });
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
                        b"value" if attr(&e, "key").as_deref() == Some("flathub::verification::verified") => {
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
