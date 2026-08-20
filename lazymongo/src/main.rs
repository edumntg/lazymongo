mod agg;
mod app;
mod config;
mod event;
mod input;
mod json_view;
mod modal;
mod term;
mod textarea;
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
    -h, --help          print this help
    -V, --version       print version

KEYS:
    press ? inside the app for the full keymap
";

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let mut uri: Option<String> = None;
    let mut read_only = false;
    for arg in std::env::args().skip(1) {
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
            other if other.starts_with('-') => {
                eprintln!("unknown option: {other}\n\n{USAGE}");
                std::process::exit(2);
            }
            other => uri = Some(other.to_string()),
        }
    }
    term::install_panic_hook();
    let mut terminal = term::init()?;
    let result = app::run(&mut terminal, uri, read_only).await;
    term::restore();
    result
}
