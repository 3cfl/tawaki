mod aur;
mod commands;
mod config;
mod flatpak;
mod fpindex;
mod pacman;
mod source;
mod state;
mod sys;
mod ui;

use clap::{Parser, Subcommand};
use source::Source;

#[derive(Parser)]
#[command(name = "tawaki", version, about = "The Fiordland Linux package manager")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    #[command(visible_alias = "i", about = "Install packages from the cleanest available source")]
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
    #[command(visible_alias = "s", about = "Search every source at once")]
    Search {
        #[arg(required = true)]
        query: Vec<String>,
        #[arg(long, value_enum, help = "Only search one source")]
        from: Option<Source>,
    },
    #[command(visible_alias = "ls", about = "List packages installed with tawaki")]
    List {
        #[arg(long, value_enum, help = "Only show one source")]
        from: Option<Source>,
    },
}

fn main() {
    let cli = Cli::parse();
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
        Cmd::Search { query, from } => commands::search(&cfg, &query, from),
        Cmd::List { from } => commands::list(from),
    };
    if let Err(e) = result {
        ui::error(&format!("{e:#}"));
        std::process::exit(1);
    }
}
