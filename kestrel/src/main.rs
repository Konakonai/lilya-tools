mod analytics;
mod cli;
mod format;
mod ingest;
mod model;
mod pricing;
mod sources;
mod ui;

use std::path::PathBuf;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "kestrel", version, about = "Token usage, cost, speed and quota for local AI coding agents")]
struct Args {
    #[command(subcommand)]
    cmd: Option<Cmd>,
    /// Leave out the historical Magpie gateway log.
    #[arg(long, global = true)]
    no_magpie: bool,
}

#[derive(Subcommand)]
enum Cmd {
    /// Print a usage report.
    Report {
        #[arg(long, short, value_enum, default_value = "today")]
        range: cli::Range,
        #[arg(long, short, value_enum, default_value = "agent")]
        by: cli::By,
        #[arg(long)]
        json: bool,
    },
    /// One-line JSON for status bars (today, live activity, quotas).
    Status,
    /// Download fresh prices from models.dev and OpenRouter.
    Prices,
}

pub struct Paths {
    pub home: PathBuf,
    pub cache: PathBuf,
    pub config: PathBuf,
}

impl Paths {
    fn new() -> Paths {
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
        Paths {
            cache: dirs::cache_dir().unwrap_or_else(|| home.join(".cache")).join("kestrel"),
            config: dirs::config_dir().unwrap_or_else(|| home.join(".config")).join("kestrel"),
            home,
        }
    }
}

fn main() -> std::process::ExitCode {
    let args = Args::parse();
    let paths = Paths::new();
    let catalog = pricing::Catalog::open(&paths.cache, &paths.config);
    match args.cmd {
        None => ui::run(paths, catalog, !args.no_magpie),
        Some(Cmd::Prices) => match catalog.refresh() {
            Ok(n) => {
                println!("{n} models priced");
                std::process::ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("price refresh failed: {e:#}");
                std::process::ExitCode::FAILURE
            }
        },
        Some(cmd) => {
            if catalog.is_stale(chrono::Utc::now().timestamp())
                && let Err(e) = catalog.refresh()
            {
                eprintln!("warning: price refresh failed: {e:#}");
            }
            let mut engine = ingest::Engine::open(&paths.home, &paths.cache, !args.no_magpie);
            let scan = engine.full_scan();
            let ds = engine.build(&catalog);
            if std::env::var_os("KESTREL_DEBUG").is_some() {
                eprintln!("scan: {scan:?}\nbuild: {:?}", ds.stats);
            }
            match cmd {
                Cmd::Report { range, by, json } => cli::report(&ds, range, by, json),
                Cmd::Status => cli::status(&ds),
                Cmd::Prices => unreachable!(),
            }
            std::process::ExitCode::SUCCESS
        }
    }
}
