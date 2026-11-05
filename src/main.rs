mod config;
mod daemon;
mod ipc;
mod popup;
mod store;
mod text;
mod tray;

use anyhow::Result;
use clap::{Parser, Subcommand};

use crate::ipc::Request;
use crate::store::{Kind, Store};

/// Clipboard manager for KDE Plasma on Wayland: copy toasts, notes from the
/// highlighted text, and a searchable history popup.
#[derive(Parser)]
#[command(version, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run the background daemon (clipboard watcher, toasts, tray icon).
    Daemon,
    /// Save the currently highlighted text as a note (bind to Meta+S).
    Note,
    /// Open the history & notes popup; closes it if already open (bind to Meta+V).
    Popup {
        /// Start on the Notes tab.
        #[arg(long)]
        notes: bool,
    },
    /// Print notes (or clipboard history) to stdout.
    List {
        /// List clipboard history instead of notes.
        #[arg(long)]
        history: bool,
        #[arg(short = 'n', long, default_value_t = 20)]
        limit: usize,
    },
    /// Put an entry (by id, see `list`) back on the clipboard.
    Copy { id: i64 },
    /// Delete all clipboard history (notes are kept).
    ClearHistory,
    /// Stop the running daemon.
    Quit,
}

fn main() -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("collage=info")).init();

    match Cli::parse().command {
        Command::Daemon => daemon::run(config::Config::load()),
        Command::Note => ipc::send(&Request::Note).map(drop),
        Command::Popup { notes } => popup::run(notes),
        Command::List { history, limit } => {
            let kind = if history { Kind::Clip } else { Kind::Note };
            for entry in Store::open()?.list(kind, limit)?.into_iter().rev() {
                println!("#{}  {}", entry.id, text::human_date(entry.created_at));
                for line in entry.text.lines() {
                    println!("    {line}");
                }
                println!();
            }
            Ok(())
        }
        Command::Copy { id } => ipc::send(&Request::Copy(id)).map(drop),
        Command::ClearHistory => {
            let n = Store::open()?.clear(Kind::Clip)?;
            println!("deleted {n} history entries");
            Ok(())
        }
        Command::Quit => ipc::send(&Request::Quit).map(drop),
    }
}
