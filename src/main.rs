mod aur;
mod commands;
mod config;
mod flatpak;
mod fpindex;
mod fsx;
mod pacman;
mod source;
mod state;
mod sys;
mod ui;
mod vercmp;

use clap::{CommandFactory, Parser, Subcommand};
use source::Source;

#[derive(Parser)]
#[command(
    name = "tawaki",
    version,
    about = "The Fiordland Linux package manager"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    #[command(
        visible_alias = "i",
        about = "Install packages from the cleanest available source"
    )]
    Install {
        #[arg(required = true)]
        names: Vec<String>,
        #[arg(long, value_enum, help = "Force a specific source")]
        from: Option<Source>,
        #[arg(short, long, help = "Don't ask for confirmation")]
        yes: bool,
    },
    #[command(visible_aliases = ["r", "rm"], about = "Remove packages, wherever they came from")]
    Remove {
        #[arg(required = true)]
        names: Vec<String>,
        #[arg(short, long, help = "Don't ask for confirmation")]
        yes: bool,
    },
    #[command(visible_aliases = ["u", "up"], about = "Update everything from every source")]
    Update {
        #[arg(short, long, help = "Don't ask for confirmation")]
        yes: bool,
    },
    #[command(about = "List pending updates without installing them")]
    Check {
        #[arg(long, help = "Print machine-readable JSON")]
        json: bool,
    },
    #[command(visible_alias = "s", about = "Search every source at once")]
    Search {
        #[arg(required = true)]
        query: Vec<String>,
        #[arg(long, value_enum, help = "Only search one source")]
        from: Option<Source>,
        #[arg(long, help = "Print machine-readable JSON")]
        json: bool,
    },
    #[command(visible_alias = "ls", about = "List packages installed with tawaki")]
    List {
        #[arg(long, value_enum, help = "Only show one source")]
        from: Option<Source>,
        #[arg(long, help = "Print machine-readable JSON")]
        json: bool,
    },
    #[command(about = "Print a shell completion script")]
    Completions {
        #[arg(value_enum)]
        shell: clap_complete::Shell,
    },
}

fn main() {
    let cli = Cli::parse();
    if let Cmd::Completions { shell } = cli.cmd {
        clap_complete::generate(shell, &mut Cli::command(), "tawaki", &mut std::io::stdout());
        return;
    }
    let cfg = match config::load() {
        Ok(c) => c,
        Err(e) => {
            ui::error(&format!("{e:#}"));
            std::process::exit(1);
        }
    };
    let result = match cli.cmd {
        Cmd::Install { names, from, yes } => commands::install(&cfg, &names, from, yes),
        Cmd::Remove { names, yes } => commands::remove(&cfg, &names, yes),
        Cmd::Update { yes } => commands::update(&cfg, yes),
        Cmd::Check { json } => commands::check(&cfg, json),
        Cmd::Search { query, from, json } => commands::search(&cfg, &query, from, json),
        Cmd::List { from, json } => commands::list(from, json),
        Cmd::Completions { .. } => Ok(()),
    };
    if let Err(e) = result {
        ui::error(&format!("{e:#}"));
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_is_consistent() {
        Cli::command().debug_assert();
    }

    #[test]
    fn existing_invocations_still_parse() {
        for argv in [
            &["tawaki", "install", "firefox", "--from", "aur", "-y"][..],
            &["tawaki", "i", "a", "b"],
            &["tawaki", "rm", "x", "--yes"],
            &["tawaki", "up", "-y"],
            &["tawaki", "check", "--json"],
            &["tawaki", "s", "obs", "studio", "--from", "flatpak"],
            &["tawaki", "ls", "--from", "pacman"],
            &["tawaki", "completions", "bash"],
        ] {
            assert!(Cli::try_parse_from(argv).is_ok(), "{argv:?}");
        }
        assert!(Cli::try_parse_from(["tawaki", "install"]).is_err());
    }

    #[test]
    fn completions_render() {
        for shell in [
            clap_complete::Shell::Bash,
            clap_complete::Shell::Zsh,
            clap_complete::Shell::Fish,
        ] {
            let mut out = Vec::new();
            clap_complete::generate(shell, &mut Cli::command(), "tawaki", &mut out);
            let text = String::from_utf8(out).unwrap();
            assert!(text.contains("install"));
        }
    }
}
