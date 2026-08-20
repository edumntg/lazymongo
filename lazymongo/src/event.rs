//! Input pump: a dedicated OS thread blocks on crossterm events and forwards
//! them into the async event loop. This keeps the tokio runtime
//! single-threaded and the UI loop a plain `select!`.

use ratatui::crossterm::event::{self, Event};
use tokio::sync::mpsc;

pub fn input_channel() -> mpsc::Receiver<Event> {
    let (tx, rx) = mpsc::channel::<Event>(128);
    std::thread::spawn(move || {
        while let Ok(ev) = event::read() {
            if tx.blocking_send(ev).is_err() {
                break; // app shut down
            }
        }
    });
    rx
}
