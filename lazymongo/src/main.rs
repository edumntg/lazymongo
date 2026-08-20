mod app;
mod event;
mod json_view;
mod term;
mod ui;

use anyhow::Result;

const USAGE: &str = "\
lazymongo — a fast, lightweight terminal UI for MongoDB

USAGE:
    lazymongo [CONNECTION_STRING]

ARGS:
    CONNECTION_STRING   mongodb:// or mongodb+srv:// URI
                        (default: mongodb://localhost:27017)

KEYS:
    press ? inside the app for the full keymap
";

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let uri = match args.next() {
        Some(a) if a == "-h" || a == "--help" => {
            print!("{USAGE}");
            return Ok(());
        }
        Some(a) if a == "-V" || a == "--version" => {
            println!("lazymongo {}", env!("CARGO_PKG_VERSION"));
            return Ok(());
        }
        Some(uri) => uri,
        None => "mongodb://localhost:27017".to_string(),
    };

    term::install_panic_hook();
    let mut terminal = term::init()?;
    let result = app::run(&mut terminal, uri).await;
    term::restore();
    result
}
