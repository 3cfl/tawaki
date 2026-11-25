use crate::config::Config;
use crate::source::{norm, Candidate, Outdated, Source};
use crate::state::{Entry, State};
use crate::{aur, flatpak, pacman, ui};
use anyhow::{bail, Result};
use std::collections::{BTreeMap, HashSet};

const SEARCH_LIMIT: usize = 20;

fn lookup_many(cfg: &Config, src: Source, names: &[String]) -> Vec<Option<Candidate>> {
    match src {
        Source::Pacman => pacman::lookup_many(names),
        Source::Flatpak => flatpak::lookup_many(cfg, names),
        Source::Aur => aur::lookup_many(names),
    }
}

pub fn transpose(
    names: usize,
    per_src: Vec<(Source, Vec<Option<Candidate>>)>,
) -> Vec<Vec<(Source, Option<Candidate>)>> {
    (0..names)
        .map(|i| {
            per_src
                .iter()
                .map(|(src, v)| (*src, v.get(i).cloned().flatten()))
                .collect()
        })
        .collect()
}

fn resolve_all(
    cfg: &Config,
    names: &[String],
    order: &[Source],
) -> Vec<Vec<(Source, Option<Candidate>)>> {
    let per_src = std::thread::scope(|s| {
        let jobs: Vec<_> = order
            .iter()
            .map(|&src| (src, s.spawn(move || lookup_many(cfg, src, names))))
            .collect();
        jobs.into_iter()
            .map(|(src, h)| (src, h.join().unwrap_or_default()))
            .collect()
    });
    transpose(names.len(), per_src)
}

fn describe(c: &Candidate) -> String {
    let title = match c.source {
        Source::Flatpak => &c.id,
        _ => &c.name,
    };
    let mut s = format!(
        "{} {}",
        ui::bold(&ui::clean(title)),
        ui::green(&ui::clean(&c.version))
    );
    if let (Source::Pacman, Some(r)) = (c.source, &c.repo) {
        s = format!("{}{}", ui::dim(&format!("{}/", ui::clean(r))), s);
    }
    if c.source == Source::Flatpak && c.verified {
        s.push_str(&format!(" {}", ui::cyan("✔ verified")));
    }
    s
}

const HEAVY_RUNTIME: u64 = 50_000_000;

fn mb(b: u64) -> String {
    format!("{} MB", b / 1_000_000)
}

fn pick(cfg: &Config, hits: Vec<Candidate>) -> Option<(Candidate, String)> {
    let first = hits.first()?.clone();
    if first.source != Source::Flatpak {
        return Some((first, String::new()));
    }
    let remote = first
        .repo
        .clone()
        .unwrap_or_else(|| cfg.flatpak_remote.clone());
    let Some((rt, size)) = flatpak::missing_runtime(cfg, &remote, &first.id) else {
        return Some((first, String::new()));
    };
    if cfg.prefer_light && size >= HEAVY_RUNTIME {
        if let Some(light) = hits.iter().skip(1).find(|c| c.prebuilt) {
            let note = ui::dim(&format!("  (skips the {} {rt} Flatpak runtime)", mb(size)));
            return Some((light.clone(), note));
        }
    }
    Some((first, ui::dim(&format!("  (+ {rt} runtime, {})", mb(size)))))
}

type Found = Vec<(Source, Option<Candidate>)>;

fn pick_all(cfg: &Config, results: Vec<Found>) -> Vec<(Found, Option<(Candidate, String)>)> {
    std::thread::scope(|s| {
        let hs: Vec<_> = results
            .into_iter()
            .map(|found| {
                s.spawn(move || {
                    let hits: Vec<Candidate> =
                        found.iter().filter_map(|(_, c)| c.clone()).collect();
                    let picked = if hits.iter().any(|c| c.installed) {
                        None
                    } else {
                        pick(cfg, hits)
                    };
                    (found, picked)
                })
            })
            .collect();
        hs.into_iter()
            .map(|h| h.join().unwrap_or_else(|_| (vec![], None)))
            .collect()
    })
}

pub fn install(cfg: &Config, names: &[String], from: Option<Source>, yes: bool) -> Result<()> {
    let order: Vec<Source> = match from {
        Some(s) => vec![s],
        None => cfg.priority.clone(),
    };
    ui::info(&format!(
        "Searching {}...",
        order
            .iter()
            .map(|s| s.label())
            .collect::<Vec<_>>()
            .join(", ")
    ));
    let results = pick_all(cfg, resolve_all(cfg, names, &order));
    let width = names.iter().map(|n| n.len()).max().unwrap_or(0);

    let mut plan: Vec<(String, Candidate)> = vec![];
    let mut missing = vec![];
    for (name, (found, picked)) in names.iter().zip(results) {
        let marks: Vec<String> = found
            .iter()
            .map(|(src, c)| {
                let mark = if c.is_some() {
                    ui::green("✓")
                } else {
                    ui::red("✗")
                };
                format!("{} {mark}", ui::dim(src.label()))
            })
            .collect();
        let name_shown = ui::clean(name);
        let line = format!("  {:width$}  {}", name_shown, marks.join("  "));
        let already = found
            .iter()
            .filter_map(|(_, c)| c.as_ref())
            .find(|c| c.installed);
        if let Some(already) = already {
            println!(
                "{line}  {} already installed via {}",
                ui::dim("→"),
                already.source.label()
            );
            continue;
        }
        match picked {
            Some((c, note)) => {
                println!(
                    "{line}  {} {} {}{note}",
                    ui::bold("→"),
                    ui::source_tag(c.source),
                    describe(&c)
                );
                plan.push((name.clone(), c));
            }
            None => {
                println!("{line}  {}", ui::red("not found"));
                missing.push(name_shown);
            }
        }
    }

    if !missing.is_empty() {
        bail!("couldn't find: {}", missing.join(", "));
    }
    if plan.is_empty() {
        ui::info("Nothing to do.");
        return Ok(());
    }
    if plan.iter().any(|(_, c)| c.source == Source::Aur) {
        ui::warn("AUR packages are community-made build scripts, review them if you're unsure");
    }
    if !yes && !ui::confirm("Proceed with installation?", true) {
        bail!("cancelled");
    }

    let mut state = State::load();
    let mut by_src: BTreeMap<(u8, String), Vec<(String, Candidate)>> = BTreeMap::new();
    for (name, c) in plan {
        let key = match c.source {
            Source::Pacman => (0, String::new()),
            Source::Flatpak => (
                1,
                c.repo.clone().unwrap_or_else(|| cfg.flatpak_remote.clone()),
            ),
            Source::Aur => (2, String::new()),
        };
        by_src.entry(key).or_default().push((name, c));
    }
    if by_src.keys().any(|(r, _)| *r != 1) {
        pacman::clear_stale_lock()?;
    }
    for ((rank, remote), group) in by_src {
        let ids: Vec<String> = group.iter().map(|(_, c)| c.id.clone()).collect();
        match rank {
            0 => pacman::install(&ids, true)?,
            1 => flatpak::install(cfg, &remote, &ids, true)?,
            _ => aur::install(cfg, &ids, yes)?,
        }
        for (name, c) in group {
            state.packages.insert(
                name,
                Entry {
                    source: c.source,
                    id: c.id,
                },
            );
        }
        state.save()?;
    }
    ui::info(&ui::green("Done!"));
    Ok(())
}

pub fn remove(_cfg: &Config, names: &[String], yes: bool) -> Result<()> {
    let mut state = State::load();
    let mut foreign: Option<HashSet<String>> = None;
    let mut targets: Vec<(Option<String>, Entry)> = vec![];
    let mut missing = vec![];
    for name in names {
        if let Some((key, entry)) = state.find(name) {
            targets.push((Some(key), entry));
        } else if pacman::valid_name(name, false) && pacman::is_installed(name) {
            let foreign = foreign.get_or_insert_with(pacman::foreign);
            let source = if foreign.contains(name) {
                Source::Aur
            } else {
                Source::Pacman
            };
            targets.push((
                None,
                Entry {
                    source,
                    id: name.clone(),
                },
            ));
        } else if let Some(id) = flatpak::find_installed(name) {
            targets.push((
                None,
                Entry {
                    source: Source::Flatpak,
                    id,
                },
            ));
        } else {
            missing.push(ui::clean(name));
        }
    }
    if !missing.is_empty() {
        bail!("not installed: {}", missing.join(", "));
    }
    for (_, e) in &targets {
        println!("  {} {}", ui::source_tag(e.source), ui::bold(&ui::clean(&e.id)));
    }
    if !yes && !ui::confirm("Remove these?", true) {
        bail!("cancelled");
    }

    let native: Vec<String> = targets
        .iter()
        .filter(|(_, e)| e.source != Source::Flatpak)
        .map(|(_, e)| e.id.clone())
        .collect();
    let flat: Vec<String> = targets
        .iter()
        .filter(|(_, e)| e.source == Source::Flatpak)
        .map(|(_, e)| e.id.clone())
        .collect();
    if !native.is_empty() {
        pacman::remove(&native, true)?;
    }
    if !flat.is_empty() {
        flatpak::remove(&flat, true)?;
    }
    for (key, e) in targets {
        match key {
            Some(k) => {
                state.packages.remove(&k);
            }
            None => state.packages.retain(|_, v| v.id != e.id),
        }
    }
    state.save()?;
    ui::info(&ui::green("Done!"));
    Ok(())
}

fn release_version() -> Option<String> {
    let text = std::fs::read_to_string("/usr/lib/os-release").ok()?;
    text.lines()
        .find_map(|l| l.strip_prefix("VERSION_ID="))
        .map(|v| v.trim_matches('"').to_string())
}

#[derive(serde::Serialize)]
pub struct Pending {
    pub release: Option<String>,
    pub pacman: Option<Vec<Outdated>>,
    pub aur: Option<Vec<Outdated>>,
    pub flatpak: Option<Vec<Outdated>>,
    #[serde(skip)]
    pub flatpak_user: usize,
    #[serde(skip)]
    pub flatpak_system: usize,
}

impl Pending {
    fn total(&self) -> usize {
        [&self.pacman, &self.aur, &self.flatpak]
            .iter()
            .map(|l| l.as_ref().map_or(0, Vec::len))
            .sum()
    }
}

pub fn pending(cfg: &Config) -> Pending {
    let (pacman, aur, flatpak) = std::thread::scope(|s| {
        let p = s.spawn(pacman::outdated);
        let a = s.spawn(|| aur::outdated(cfg));
        let f = s.spawn(flatpak::outdated);
        (
            p.join().ok().flatten(),
            a.join().ok().flatten(),
            f.join().ok().flatten(),
        )
    });
    let (fu, fs) = flatpak.as_ref().map_or((0, 0), |u| (u.user, u.system));
    Pending {
        release: release_version(),
        pacman,
        aur,
        flatpak: if flatpak::available() {
            flatpak.map(|u| u.all)
        } else {
            Some(vec![])
        },
        flatpak_user: fu,
        flatpak_system: fs,
    }
}

fn show_pending(p: &Pending, list: bool) {
    let show = |label: &str, l: &Option<Vec<Outdated>>| {
        let v = match l.as_ref().map(Vec::len) {
            Some(0) => ui::dim("up to date"),
            Some(n) => ui::yellow(&format!("{n} update{}", if n == 1 { "" } else { "s" })),
            None => ui::dim("couldn't check"),
        };
        println!("  {:8} {v}", label);
        if list {
            for o in l.iter().flatten() {
                println!(
                    "    {} {} -> {}",
                    ui::clean(&o.name),
                    ui::dim(&ui::clean(&o.old)),
                    ui::clean(&o.new)
                );
            }
        }
    };
    show("pacman", &p.pacman);
    show("aur", &p.aur);
    show("flatpak", &p.flatpak);
}

pub fn check(cfg: &Config, json: bool) -> Result<()> {
    let p = pending(cfg);
    if json {
        println!("{}", serde_json::to_string(&p)?);
    } else {
        show_pending(&p, true);
    }
    Ok(())
}

pub fn overlap_safe(
    pacman_up: &[Outdated],
    aur_up: &[Outdated],
    closure: Option<&HashSet<String>>,
) -> bool {
    let Some(cl) = closure.filter(|c| c.contains("flatpak")) else {
        return false;
    };
    !pacman_up.iter().chain(aur_up).any(|o| cl.contains(&o.name))
}

fn print_background(ok: bool, text: &str) {
    ui::info("User flatpaks (updated alongside pacman):");
    let text = ui::clean_block(text.trim_end());
    if !text.is_empty() {
        println!("{text}");
    }
    if !ok {
        ui::warn("the background flatpak update didn't finish, retrying it now");
    }
}

pub fn update(cfg: &Config, yes: bool) -> Result<()> {
    ui::info("Checking for updates...");
    let pend = pending(cfg);
    show_pending(&pend, false);
    let count = |l: &Option<Vec<Outdated>>| l.as_ref().map(Vec::len);
    let (p, a, f) = (count(&pend.pacman), count(&pend.aur), count(&pend.flatpak));

    if pend.total() == 0 && p.is_some() {
        ui::info(&ui::green("Everything is up to date."));
        return Ok(());
    }
    if !yes && !ui::confirm("Update now?", true) {
        bail!("cancelled");
    }
    pacman::clear_stale_lock()?;
    let run_pacman = p != Some(0);
    let run_aur = a.is_some_and(|n| n > 0);
    let run_flatpak = f.is_some_and(|n| n > 0);
    let overlap = run_pacman
        && run_flatpak
        && pend.flatpak_user > 0
        && overlap_safe(
            pend.pacman.as_deref().unwrap_or(&[]),
            pend.aur.as_deref().unwrap_or(&[]),
            pacman::runtime_closure(&["flatpak"]).as_ref(),
        );

    std::thread::scope(|s| -> Result<()> {
        let bg = overlap.then(|| s.spawn(flatpak::update_user_quiet));
        let native = (|| -> Result<()> {
            let before = release_version();
            if run_pacman {
                ui::info("Updating pacman packages...");
                pacman::update(true)?;
            }
            let after = release_version();
            if let Some(new) = after.as_ref().filter(|n| before.as_ref() != Some(*n)) {
                let line = match &before {
                    Some(old) => format!("Fiordland {old} -> {new}"),
                    None => format!("Fiordland is now {new}"),
                };
                ui::info(&ui::green(&ui::clean(&line)));
                if let Ok(news) =
                    std::fs::read_to_string(format!("/usr/share/fiordland/news/{new}.txt"))
                {
                    println!("{}", ui::clean_block(news.trim_end()));
                }
            }
            if run_aur {
                ui::info("Updating AUR packages...");
                aur::update(cfg, yes)?;
            }
            Ok(())
        })();
        let bg_ok = match bg {
            Some(h) => {
                let (ok, text) = h
                    .join()
                    .unwrap_or_else(|_| (false, "flatpak update stopped".to_string()));
                print_background(ok, &text);
                Some(ok)
            }
            None => None,
        };
        native?;
        match bg_ok {
            Some(ok) => {
                if !ok {
                    flatpak::update(Some("--user"), true)?;
                }
                if pend.flatpak_system > 0 {
                    ui::info("Updating flatpaks...");
                    flatpak::update(Some("--system"), true)?;
                }
            }
            None if run_flatpak => {
                ui::info("Updating flatpaks...");
                flatpak::update(None, true)?;
            }
            None => {}
        }
        Ok(())
    })?;
    ui::info(&ui::green("Done!"));
    Ok(())
}

fn rank(key: &str, c: &Candidate) -> u8 {
    let n = norm(&c.name);
    if n == key || norm(&c.id) == key {
        0
    } else if n.starts_with(key) {
        1
    } else {
        2
    }
}

pub fn search(cfg: &Config, query: &[String], from: Option<Source>, json: bool) -> Result<()> {
    let order: Vec<Source> = match from {
        Some(s) => vec![s],
        None => cfg.priority.clone(),
    };
    let key = norm(&query.join(""));
    let mut results: Vec<(Source, Vec<Candidate>)> = std::thread::scope(|s| {
        let hs: Vec<_> = order
            .iter()
            .map(|&src| {
                s.spawn(move || {
                    let r = match src {
                        Source::Pacman => pacman::search(query),
                        Source::Flatpak => flatpak::search(cfg, query),
                        Source::Aur => aur::search(query),
                    };
                    (src, r)
                })
            })
            .collect();
        hs.into_iter()
            .zip(&order)
            .map(|(h, &src)| h.join().unwrap_or((src, vec![])))
            .collect()
    });
    for (_, hits) in results.iter_mut() {
        hits.sort_by_key(|c| rank(&key, c));
    }
    if json {
        let all: Vec<&Candidate> = results.iter().flat_map(|(_, h)| h.iter()).collect();
        println!("{}", serde_json::to_string(&all)?);
        return Ok(());
    }

    let mut any = false;
    for (src, hits) in results {
        if hits.is_empty() {
            continue;
        }
        any = true;
        let extra = hits.len().saturating_sub(SEARCH_LIMIT);
        for c in hits.iter().take(SEARCH_LIMIT) {
            let mut line = format!("{} {}", ui::source_tag(src), describe(c));
            if src == Source::Flatpak {
                line.push_str(&format!(" {}", ui::dim(&format!("({})", ui::clean(&c.name)))));
            }
            if c.installed {
                line.push_str(&format!(" {}", ui::cyan("[installed]")));
            }
            println!("{line}");
            if !c.description.is_empty() {
                println!("    {}", ui::dim(&ui::clean(&c.description)));
            }
        }
        if extra > 0 {
            println!(
                "    {}",
                ui::dim(&format!("... and {extra} more from {}", src.label()))
            );
        }
    }
    if !any {
        bail!("no results for \"{}\"", ui::clean(&query.join(" ")));
    }
    Ok(())
}

#[derive(serde::Serialize)]
struct Listed<'a> {
    name: &'a str,
    source: Source,
    id: &'a str,
}

pub fn list(from: Option<Source>, json: bool) -> Result<()> {
    let state = State::load();
    let rows: Vec<Listed> = state
        .packages
        .iter()
        .filter(|(_, e)| from.is_none_or(|f| f == e.source))
        .map(|(name, e)| Listed {
            name,
            source: e.source,
            id: &e.id,
        })
        .collect();
    if json {
        println!("{}", serde_json::to_string(&rows)?);
        return Ok(());
    }
    for r in &rows {
        let id = if r.id != r.name {
            ui::dim(&format!(" ({})", ui::clean(r.id)))
        } else {
            String::new()
        };
        println!("{} {}{id}", ui::source_tag(r.source), ui::bold(&ui::clean(r.name)));
    }
    if rows.is_empty() {
        ui::info("Nothing installed with tawaki yet.");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn up(name: &str) -> Outdated {
        Outdated {
            name: name.into(),
            old: "1".into(),
            new: "2".into(),
        }
    }

    fn cand(src: Source, name: &str) -> Candidate {
        Candidate {
            source: src,
            id: name.into(),
            name: name.into(),
            version: "1".into(),
            description: String::new(),
            repo: None,
            verified: false,
            installed: false,
            prebuilt: true,
        }
    }

    #[test]
    fn overlap_only_when_flatpak_stack_untouched() {
        let cl: HashSet<String> = ["flatpak", "ostree", "glibc"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert!(overlap_safe(&[up("firefox")], &[], Some(&cl)));
        assert!(!overlap_safe(&[up("glibc")], &[], Some(&cl)));
        assert!(!overlap_safe(&[up("firefox")], &[up("ostree")], Some(&cl)));
        assert!(!overlap_safe(&[up("firefox")], &[], None));
        assert!(!overlap_safe(&[up("firefox")], &[], Some(&HashSet::new())));
    }

    #[test]
    fn transpose_keeps_source_order() {
        let per = vec![
            (Source::Pacman, vec![Some(cand(Source::Pacman, "a")), None]),
            (Source::Aur, vec![None, Some(cand(Source::Aur, "b"))]),
            (Source::Flatpak, vec![]),
        ];
        let t = transpose(2, per);
        assert_eq!(t.len(), 2);
        assert_eq!(t[0][0].0, Source::Pacman);
        assert!(t[0][0].1.is_some());
        assert!(t[0][1].1.is_none());
        assert!(t[1][1].1.is_some());
        assert!(t[1][2].1.is_none());
    }

    #[test]
    fn pending_json_schema_is_stable() {
        let p = Pending {
            release: Some("1.0".into()),
            pacman: Some(vec![up("a")]),
            aur: None,
            flatpak: Some(vec![]),
            flatpak_user: 3,
            flatpak_system: 4,
        };
        let v: serde_json::Value = serde_json::to_value(&p).unwrap();
        let mut keys: Vec<&String> = v.as_object().unwrap().keys().collect();
        keys.sort();
        assert_eq!(keys, ["aur", "flatpak", "pacman", "release"]);
        assert_eq!(v["pacman"][0]["name"], "a");
        assert_eq!(p.total(), 1);
    }

    #[test]
    fn ranking() {
        let k = norm("obs");
        assert_eq!(rank(&k, &cand(Source::Aur, "obs")), 0);
        assert_eq!(rank(&k, &cand(Source::Aur, "obs-studio")), 1);
        assert_eq!(rank(&k, &cand(Source::Aur, "x-obs")), 2);
    }
}
