use crate::config::Config;
use crate::source::{norm, Candidate, Source};
use crate::state::{Entry, State};
use crate::{aur, flatpak, pacman, ui};
use anyhow::{bail, Result};
use std::collections::BTreeMap;

const SEARCH_LIMIT: usize = 20;

fn lookup(cfg: &Config, src: Source, name: &str) -> Option<Candidate> {
    match src {
        Source::Pacman => pacman::lookup(name),
        Source::Flatpak => flatpak::lookup(cfg, name),
        Source::Aur => aur::lookup(name),
    }
}

fn resolve_all(cfg: &Config, names: &[String], order: &[Source]) -> Vec<Vec<(Source, Option<Candidate>)>> {
    std::thread::scope(|s| {
        let jobs: Vec<Vec<_>> = names
            .iter()
            .map(|name| {
                order
                    .iter()
                    .map(|&src| s.spawn(move || (src, lookup(cfg, src, name))))
                    .collect()
            })
            .collect();
        jobs.into_iter()
            .map(|hs| hs.into_iter().map(|h| h.join().expect("lookup thread")).collect())
            .collect()
    })
}

fn describe(c: &Candidate) -> String {
    let mut s = match c.source {
        Source::Flatpak => format!("{} {}", ui::bold(&c.id), ui::green(&c.version)),
        _ => format!("{} {}", ui::bold(&c.name), ui::green(&c.version)),
    };
    if let (Source::Pacman, Some(r)) = (c.source, &c.repo) {
        s = format!("{}{}", ui::dim(&format!("{r}/")), s);
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
    let remote = first.repo.clone().unwrap_or_else(|| cfg.flatpak_remote.clone());
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

pub fn install(cfg: &Config, names: &[String], from: Option<Source>, yes: bool) -> Result<()> {
    let order: Vec<Source> = match from {
        Some(s) => vec![s],
        None => cfg.priority.clone(),
    };
    ui::info(&format!("Searching {}...", order.iter().map(|s| s.label()).collect::<Vec<_>>().join(", ")));
    let results = resolve_all(cfg, names, &order);
    let width = names.iter().map(|n| n.len()).max().unwrap_or(0);

    let mut plan: Vec<(String, Candidate)> = vec![];
    let mut missing = vec![];
    for (name, found) in names.iter().zip(results) {
        let marks: Vec<String> = found
            .iter()
            .map(|(src, c)| {
                let mark = if c.is_some() { ui::green("✓") } else { ui::red("✗") };
                format!("{} {mark}", ui::dim(src.label()))
            })
            .collect();
        let hits: Vec<Candidate> = found.into_iter().filter_map(|(_, c)| c).collect();
        let line = format!("  {:width$}  {}", name, marks.join("  "));
        if let Some(already) = hits.iter().find(|c| c.installed) {
            println!("{line}  {} already installed via {}", ui::dim("→"), already.source.label());
            continue;
        }
        match pick(cfg, hits) {
            Some((c, note)) => {
                println!("{line}  {} {} {}{note}", ui::bold("→"), ui::source_tag(c.source), describe(&c));
                plan.push((name.clone(), c));
            }
            None => {
                println!("{line}  {}", ui::red("not found"));
                missing.push(name.clone());
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
            Source::Flatpak => (1, c.repo.clone().unwrap_or_else(|| cfg.flatpak_remote.clone())),
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
            state.packages.insert(name, Entry { source: c.source, id: c.id });
        }
        state.save()?;
    }
    ui::info(&ui::green("Done!"));
    Ok(())
}

pub fn remove(_cfg: &Config, names: &[String], yes: bool) -> Result<()> {
    let mut state = State::load();
    let foreign = pacman::foreign();
    let mut targets: Vec<(Option<String>, Entry)> = vec![];
    let mut missing = vec![];
    for name in names {
        if let Some((key, entry)) = state.find(name) {
            targets.push((Some(key), entry));
        } else if pacman::is_installed(name) {
            let source = if foreign.contains(name) { Source::Aur } else { Source::Pacman };
            targets.push((None, Entry { source, id: name.clone() }));
        } else if let Some(id) = flatpak::find_installed(name) {
            targets.push((None, Entry { source: Source::Flatpak, id }));
        } else {
            missing.push(name.clone());
        }
    }
    if !missing.is_empty() {
        bail!("not installed: {}", missing.join(", "));
    }
    for (_, e) in &targets {
        println!("  {} {}", ui::source_tag(e.source), ui::bold(&e.id));
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

pub fn update(cfg: &Config, yes: bool) -> Result<()> {
    ui::info("Checking for updates...");
    let (p, a, f) = std::thread::scope(|s| {
        let p = s.spawn(pacman::pending_updates);
        let a = s.spawn(|| aur::pending_updates(cfg));
        let f = s.spawn(flatpak::pending_updates);
        (p.join().unwrap(), a.join().unwrap(), f.join().unwrap())
    });
    let show = |label: &str, n: Option<usize>| {
        let v = match n {
            Some(0) => ui::dim("up to date"),
            Some(n) => ui::yellow(&format!("{n} update{}", if n == 1 { "" } else { "s" })),
            None => ui::dim("unknown"),
        };
        println!("  {:8} {v}", label);
    };
    show("pacman", p);
    show("aur", a);
    show("flatpak", if flatpak::available() { f } else { Some(0) });

    if [p, a, f].iter().all(|n| matches!(n, Some(0) | None)) && p.is_some() {
        ui::info(&ui::green("Everything is up to date."));
        return Ok(());
    }
    if !yes && !ui::confirm("Update now?", true) {
        bail!("cancelled");
    }
    pacman::clear_stale_lock()?;
    if p != Some(0) {
        ui::info("Updating pacman packages...");
        pacman::update(true)?;
    }
    if a.is_some_and(|n| n > 0) {
        ui::info("Updating AUR packages...");
        aur::update(cfg, yes)?;
    }
    if f.is_some_and(|n| n > 0) {
        ui::info("Updating flatpaks...");
        flatpak::update(true)?;
    }
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

pub fn search(cfg: &Config, query: &[String], from: Option<Source>) -> Result<()> {
    let order: Vec<Source> = match from {
        Some(s) => vec![s],
        None => cfg.priority.clone(),
    };
    let key = norm(&query.join(""));
    let results: Vec<(Source, Vec<Candidate>)> = std::thread::scope(|s| {
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
        hs.into_iter().map(|h| h.join().expect("search thread")).collect()
    });

    let mut any = false;
    for (src, mut hits) in results {
        if hits.is_empty() {
            continue;
        }
        any = true;
        hits.sort_by_key(|c| rank(&key, c));
        let extra = hits.len().saturating_sub(SEARCH_LIMIT);
        for c in hits.iter().take(SEARCH_LIMIT) {
            let mut line = format!("{} {}", ui::source_tag(src), describe(c));
            if src == Source::Flatpak {
                line.push_str(&format!(" {}", ui::dim(&format!("({})", c.name))));
            }
            if c.installed {
                line.push_str(&format!(" {}", ui::cyan("[installed]")));
            }
            println!("{line}");
            if !c.description.is_empty() {
                println!("    {}", ui::dim(&c.description));
            }
        }
        if extra > 0 {
            println!("    {}", ui::dim(&format!("... and {extra} more from {}", src.label())));
        }
    }
    if !any {
        bail!("no results for \"{}\"", query.join(" "));
    }
    Ok(())
}

pub fn list(from: Option<Source>) -> Result<()> {
    let state = State::load();
    let mut shown = 0;
    for (name, e) in &state.packages {
        if from.is_some_and(|f| f != e.source) {
            continue;
        }
        let id = if &e.id != name { ui::dim(&format!(" ({})", e.id)) } else { String::new() };
        println!("{} {}{id}", ui::source_tag(e.source), ui::bold(name));
        shown += 1;
    }
    if shown == 0 {
        ui::info("Nothing installed with tawaki yet.");
    }
    Ok(())
}
