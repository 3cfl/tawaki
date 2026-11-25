use anyhow::{bail, Context, Result};
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;

pub const SUDO: &str = "/usr/bin/sudo";
pub const ROOT_ALLOWED: [&str; 3] = ["/usr/bin/pacman", "/usr/bin/rm", "/usr/bin/flatpak"];

pub fn clean_path(path: Option<&OsStr>) -> OsString {
    let dirs: Vec<PathBuf> = path
        .map(|p| {
            std::env::split_paths(p)
                .filter(|d| d.is_absolute())
                .collect()
        })
        .unwrap_or_default();
    let dirs = if dirs.is_empty() {
        vec![PathBuf::from("/usr/local/bin"), PathBuf::from("/usr/bin")]
    } else {
        dirs
    };
    std::env::join_paths(dirs).unwrap_or_else(|_| OsString::from("/usr/local/bin:/usr/bin"))
}

fn safe_path() -> &'static OsString {
    static P: OnceLock<OsString> = OnceLock::new();
    P.get_or_init(|| clean_path(std::env::var_os("PATH").as_deref()))
}

pub fn command(cmd: &str) -> Command {
    let mut c = Command::new(cmd);
    c.env("PATH", safe_path());
    c
}

pub fn output<I, S>(cmd: &str, args: I) -> Option<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let (ok, out) = output_any(cmd, args)?;
    ok.then_some(out)
}

pub fn output_any<I, S>(cmd: &str, args: I) -> Option<(bool, String)>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let out = command(cmd)
        .args(args)
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    Some((
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
    ))
}

pub fn succeeds<I, S>(cmd: &str, args: I) -> bool
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    command(cmd)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

pub fn run<I, S>(cmd: &str, args: I, dir: Option<&Path>) -> Result<()>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let mut c = command(cmd);
    c.args(args);
    if let Some(d) = dir {
        c.current_dir(d);
    }
    let status = c
        .status()
        .with_context(|| format!("couldn't start {cmd}"))?;
    if !status.success() {
        bail!("{cmd} failed ({status})");
    }
    Ok(())
}

pub fn root_argv<S: AsRef<str>>(prog: &str, args: &[S], euid: u32) -> Result<Vec<String>> {
    if !ROOT_ALLOWED.contains(&prog) {
        bail!("refusing to run {prog} as root");
    }
    let mut argv = vec![];
    if euid != 0 {
        argv.push(SUDO.to_string());
        argv.push("--".to_string());
    }
    argv.push(prog.to_string());
    argv.extend(args.iter().map(|a| a.as_ref().to_string()));
    Ok(argv)
}

pub fn run_root<S: AsRef<str>>(prog: &str, args: &[S]) -> Result<()> {
    let argv = root_argv(prog, args, euid())?;
    run(&argv[0], &argv[1..], None)
}

pub fn parse_euid(status: &str) -> Option<u32> {
    status
        .lines()
        .find_map(|l| l.strip_prefix("Uid:"))
        .and_then(|rest| rest.split_whitespace().nth(1))
        .and_then(|n| n.parse().ok())
}

pub fn euid() -> u32 {
    static U: OnceLock<u32> = OnceLock::new();
    *U.get_or_init(|| {
        std::fs::read_to_string("/proc/self/status")
            .ok()
            .and_then(|s| parse_euid(&s))
            .unwrap_or_else(|| {
                use std::os::unix::fs::MetadataExt;
                std::fs::metadata("/proc/self").map_or(u32::MAX, |m| m.uid())
            })
    })
}

pub fn has(bin: &str) -> bool {
    std::env::split_paths(safe_path()).any(|d| d.join(bin).is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_path_drops_relative_entries() {
        let p = clean_path(Some(OsStr::new(".:bin::/usr/bin:/opt/x/bin")));
        assert_eq!(p, OsString::from("/usr/bin:/opt/x/bin"));
    }

    #[test]
    fn clean_path_falls_back_when_empty() {
        assert_eq!(
            clean_path(Some(OsStr::new("./x:"))),
            OsString::from("/usr/local/bin:/usr/bin")
        );
        assert_eq!(clean_path(None), OsString::from("/usr/local/bin:/usr/bin"));
    }

    #[test]
    fn root_argv_uses_absolute_sudo_and_allowlist() {
        let a = root_argv("/usr/bin/pacman", &["-Syu"], 1000).unwrap();
        assert_eq!(a, ["/usr/bin/sudo", "--", "/usr/bin/pacman", "-Syu"]);
        let a = root_argv("/usr/bin/pacman", &["-Syu"], 0).unwrap();
        assert_eq!(a, ["/usr/bin/pacman", "-Syu"]);
        assert!(root_argv("pacman", &["-Syu"], 1000).is_err());
        assert!(root_argv("/bin/sh", &["-c", "id"], 1000).is_err());
        assert!(root_argv("/usr/bin/bash", &[] as &[&str], 0).is_err());
    }

    #[test]
    fn euid_parsing() {
        let s = "Name:\ttawaki\nUid:\t1000\t1001\t1000\t1000\nGid:\t1\n";
        assert_eq!(parse_euid(s), Some(1001));
        assert_eq!(parse_euid("Name: x\n"), None);
        assert_eq!(parse_euid("Uid:\tabc\tdef\n"), None);
    }
}
