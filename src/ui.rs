use crate::source::Source;
use std::io::{IsTerminal, Write};
use std::sync::OnceLock;

fn color_on() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("NO_COLOR").is_none() && std::io::stdout().is_terminal())
}

fn paint(code: &str, s: &str) -> String {
    if color_on() {
        format!("\x1b[{code}m{s}\x1b[0m")
    } else {
        s.to_string()
    }
}

pub fn bold(s: &str) -> String {
    paint("1", s)
}

pub fn dim(s: &str) -> String {
    paint("2", s)
}

pub fn green(s: &str) -> String {
    paint("32", s)
}

pub fn yellow(s: &str) -> String {
    paint("33", s)
}

pub fn red(s: &str) -> String {
    paint("31", s)
}

pub fn cyan(s: &str) -> String {
    paint("36", s)
}

pub fn source_tag(src: Source) -> String {
    let t = format!("[{}]", src.label());
    match src {
        Source::Pacman => paint("1;34", &t),
        Source::Flatpak => paint("1;36", &t),
        Source::Aur => paint("1;33", &t),
    }
}

pub fn info(msg: &str) {
    println!("{} {msg}", cyan("::"));
}

pub fn warn(msg: &str) {
    eprintln!("{} {msg}", yellow("⚠"));
}

pub fn error(msg: &str) {
    eprintln!("{} {msg}", red("error:"));
}

pub fn confirm(prompt: &str, default_yes: bool) -> bool {
    let hint = if default_yes { "[Y/n]" } else { "[y/N]" };
    print!("{} {prompt} {hint} ", cyan("::"));
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    if std::io::stdin().read_line(&mut line).is_err() {
        return default_yes;
    }
    match line.trim().to_lowercase().as_str() {
        "" => default_yes,
        "y" | "yes" => true,
        _ => false,
    }
}
