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

pub fn clean(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .filter(|c| {
            !matches!(
                c,
                '\u{200b}'..='\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}'
            )
        })
        .collect()
}

pub fn clean_block(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_control() && c != '\n' && c != '\t' {
                ' '
            } else {
                c
            }
        })
        .collect()
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

pub fn answer(line: Option<&str>, default_yes: bool) -> bool {
    let Some(line) = line else {
        return false;
    };
    match line.trim().to_lowercase().as_str() {
        "" => default_yes,
        "y" | "yes" => true,
        _ => false,
    }
}

pub fn confirm(prompt: &str, default_yes: bool) -> bool {
    let hint = if default_yes { "[Y/n]" } else { "[y/N]" };
    print!("{} {prompt} {hint} ", cyan("::"));
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    let got = match std::io::stdin().read_line(&mut line) {
        Ok(0) | Err(_) => {
            println!();
            None
        }
        Ok(_) => Some(line.as_str()),
    };
    answer(got, default_yes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_strips_terminal_escapes() {
        assert_eq!(clean("a\x1b]52;c;ZXZpbA==\x07b"), "a ]52;c;ZXZpbA== b");
        assert!(!clean("x\x1b[2Jy\r\n\t").contains(['\x1b', '\r', '\n', '\t']));
        assert_eq!(clean("plain text ✔"), "plain text ✔");
        assert_eq!(clean("a\u{202e}b\u{200b}c"), "abc");
    }

    #[test]
    fn clean_block_keeps_layout_only() {
        assert_eq!(clean_block("a\x1b[2Jb\r\nc\td"), "a [2Jb \nc\td");
    }

    #[test]
    fn eof_never_confirms() {
        assert!(!answer(None, true));
        assert!(!answer(None, false));
        assert!(answer(Some("\n"), true));
        assert!(!answer(Some("\n"), false));
        assert!(answer(Some("YES\n"), false));
        assert!(!answer(Some("nope\n"), true));
    }
}
