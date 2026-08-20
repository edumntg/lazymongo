mod agg;
mod app;
mod config;
mod event;
mod input;
mod json_view;
mod modal;
mod term;
mod textarea;
mod theme;
mod ui;
mod util;

use anyhow::Result;

const USAGE: &str = "\
lazymongo — a fast, lightweight terminal UI for MongoDB

USAGE:
    lazymongo [OPTIONS] [CONNECTION_STRING]

ARGS:
    CONNECTION_STRING   mongodb:// or mongodb+srv:// URI
                        (default: mongodb://localhost:27017)

OPTIONS:
    -r, --readonly      block all write operations
        --theme NAME    dark | light | claude-dark | claude-light |
                        termius | high-contrast (default from config)
    -h, --help          print this help
    -V, --version       print version

KEYS:
    press ? inside the app for the full keymap
";

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let mut uri: Option<String> = None;
    let mut read_only = false;
    let mut cli_theme: Option<String> = None;
    let mut expect_theme = false;
    for arg in std::env::args().skip(1) {
        if expect_theme {
            cli_theme = Some(arg);
            expect_theme = false;
            continue;
        }
        match arg.as_str() {
            "-h" | "--help" => {
                print!("{USAGE}");
                return Ok(());
            }
            "-V" | "--version" => {
                println!("lazymongo {}", env!("CARGO_PKG_VERSION"));
                return Ok(());
            }
            "-r" | "--readonly" | "--read-only" => read_only = true,
            "--theme" => expect_theme = true,
            other if other.starts_with('-') => {
                eprintln!("unknown option: {other}\n\n{USAGE}");
                std::process::exit(2);
            }
            other => uri = Some(other.to_string()),
        }
    }
    // Theme precedence: --theme > config.toml > default (dark).
    let config_theme = config::load_config().ok().and_then(|c| c.theme);
    if let Some(name) = cli_theme.as_deref().or(config_theme.as_deref()) {
        if !theme::set_by_name(name) {
            eprintln!(
                "unknown theme \"{name}\" — available: {}",
                theme::NAMES.join(", ")
            );
            std::process::exit(2);
        }
    }

    term::install_panic_hook();
    let mut terminal = term::init()?;
    let result = app::run(&mut terminal, uri, read_only).await;
    term::restore();
    result
}
