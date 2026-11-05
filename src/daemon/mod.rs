//! The background daemon: watches the clipboard and primary selection through
//! the wlr-data-control protocol, records history, saves notes on request,
//! keeps the clipboard alive when the source app quits, and shows toasts.

mod clipboard;
mod toast;

use std::io::Read;
use std::os::fd::AsFd;
use std::os::unix::net::UnixStream;
use std::process::Command;

use anyhow::{Context, Result, anyhow};
use smithay_client_toolkit::{
    compositor::CompositorState,
    delegate_registry,
    output::OutputState,
    reexports::{
        calloop::{
            self, EventLoop, Interest, LoopHandle, Mode, PostAction, channel::Sender,
            generic::Generic,
        },
        calloop_wayland_source::WaylandSource,
    },
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    shell::wlr_layer::LayerShell,
    shm::Shm,
};
use wayland_client::{Connection, QueueHandle, globals::registry_queue_init, protocol::wl_seat::WlSeat};

use crate::config::Config;
use crate::ipc::{self, Request};
use crate::store::{Kind, Store};
use crate::text;
use crate::tray;
use clipboard::{Device, MARKER, Manager, Offer, Source, SourceData, TEXT_MIMES, is_secret, pick_text};
use toast::{Toast, ToastKind};

/// Messages delivered to the main loop from helper threads and the tray.
pub enum Event {
    ReadDone { purpose: Purpose, text: Result<String, String> },
    OpenPopup,
    Quit,
}

#[derive(Debug, Clone, Copy)]
pub enum Purpose {
    /// A new clipboard selection; `generation` drops stale reads.
    Clip { generation: u64, announce: bool },
    /// The user asked to save the highlighted text as a note.
    Note,
}

pub struct Daemon {
    cfg: Config,
    conn: Connection,
    qh: QueueHandle<Daemon>,
    loop_handle: LoopHandle<'static, Daemon>,
    tx: Sender<Event>,
    store: Store,

    registry_state: RegistryState,
    output_state: OutputState,
    compositor: CompositorState,
    layer_shell: LayerShell,
    shm: Shm,

    manager: Manager,
    device: Device,
    clip_offer: Option<Offer>,
    primary_offer: Option<Offer>,
    own_source: Option<Source>,
    /// Last text we know was on the clipboard, re-offered if it empties.
    last_text: Option<String>,
    clip_generation: u64,
    seen_first_selection: bool,

    toast: Toast,
    _tray: Option<ksni::blocking::Handle<tray::CollageTray>>,
    exit: bool,
}

pub fn run(cfg: Config) -> Result<()> {
    let listener = ipc::bind()?;
    let store = Store::open()?;

    let conn = Connection::connect_to_env().context("can't connect to the Wayland compositor")?;
    let (globals, event_queue) = registry_queue_init::<Daemon>(&conn)?;
    let qh = event_queue.handle();

    let mut event_loop: EventLoop<'static, Daemon> = EventLoop::try_new()?;
    let loop_handle = event_loop.handle();
    WaylandSource::new(conn.clone(), event_queue)
        .insert(loop_handle.clone())
        .map_err(|e| anyhow!("inserting wayland source: {e}"))?;

    let compositor = CompositorState::bind(&globals, &qh).context("wl_compositor missing")?;
    let layer_shell = LayerShell::bind(&globals, &qh).context("layer-shell missing")?;
    let shm = Shm::bind(&globals, &qh).context("wl_shm missing")?;
    let seat: WlSeat = globals.bind(&qh, 1..=8, ()).context("wl_seat missing")?;
    let manager = Manager::bind(&globals, &qh)?;
    log::info!("clipboard protocol: {}", manager.name());
    if !manager.supports_primary() {
        log::warn!("no primary selection support: saving notes with Meta+S won't work");
    }
    let device = manager.get_device(&seat, &qh);

    let (tx, rx) = calloop::channel::channel::<Event>();
    loop_handle
        .insert_source(rx, |event, _, state: &mut Daemon| {
            if let calloop::channel::Event::Msg(event) = event {
                state.on_event(event);
            }
        })
        .map_err(|e| anyhow!("inserting channel: {e}"))?;

    loop_handle
        .insert_source(
            Generic::new(listener, Interest::READ, Mode::Level),
            |_, listener, state: &mut Daemon| {
                loop {
                    match listener.accept() {
                        Ok((stream, _)) => state.on_client(stream),
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                        Err(e) => {
                            log::warn!("accept: {e}");
                            break;
                        }
                    }
                }
                Ok(PostAction::Continue)
            },
        )
        .map_err(|e| anyhow!("inserting socket: {e}"))?;

    let output_state = OutputState::new(&globals, &qh);
    let tray = if cfg.tray { tray::spawn(tx.clone()) } else { None };

    let mut daemon = Daemon {
        toast: Toast::new(),
        cfg,
        conn,
        qh,
        loop_handle,
        tx,
        store,
        registry_state: RegistryState::new(&globals),
        output_state,
        compositor,
        layer_shell,
        shm,
        manager,
        device,
        clip_offer: None,
        primary_offer: None,
        own_source: None,
        last_text: None,
        clip_generation: 0,
        seen_first_selection: false,
        _tray: tray,
        exit: false,
    };

    log::info!("collage daemon running ({})", ipc::socket_path().display());
    while !daemon.exit {
        event_loop.dispatch(None, &mut daemon)?;
    }
    std::fs::remove_file(ipc::socket_path()).ok();
    Ok(())
}

impl Daemon {
    fn on_event(&mut self, event: Event) {
        match event {
            Event::ReadDone { purpose, text } => self.on_read_done(purpose, text),
            Event::OpenPopup => spawn_popup(),
            Event::Quit => self.exit = true,
        }
    }

    fn on_client(&mut self, stream: UnixStream) {
        let request = match ipc::read_request(&stream) {
            Ok(Some(req)) => req,
            Ok(None) => return ipc::reply(&stream, Err(anyhow!("unknown request"))),
            Err(e) => return log::debug!("bad client: {e}"),
        };
        let result = match request {
            Request::Note => self.save_selection_as_note().map(|()| "saving".into()),
            Request::Copy(id) => self.copy_entry(id).map(|()| "copied".into()),
            Request::Popup => {
                spawn_popup();
                Ok("opening".into())
            }
            Request::Ping => Ok("pong".into()),
            Request::Quit => {
                self.exit = true;
                Ok("bye".into())
            }
        };
        if let Err(e) = &result {
            log::warn!("{request:?}: {e}");
        }
        ipc::reply(&stream, result);
    }

    // ---- clipboard ------------------------------------------------------

    fn on_selection(&mut self, offer: Option<Offer>) {
        if let Some(old) = self.clip_offer.take() {
            old.destroy();
        }
        self.clip_generation += 1;
        let announce = std::mem::replace(&mut self.seen_first_selection, true);

        let Some(offer) = offer else {
            // The owner went away (usually: the app you copied from quit).
            if self.cfg.keep_clipboard
                && let Some(text) = self.last_text.clone()
            {
                log::debug!("clipboard emptied, restoring last text");
                self.set_clipboard(text);
            }
            return;
        };

        let mimes = offer.mimes();
        if mimes.iter().any(|m| m == MARKER) {
            // Our own source — nothing new.
        } else if is_secret(&mimes) {
            self.last_text = None;
            if announce {
                self.show_toast(ToastKind::Copied, "Copied", Some("password hidden".into()));
            }
        } else if let Some(mime) = pick_text(&mimes) {
            let purpose = Purpose::Clip { generation: self.clip_generation, announce };
            self.read_offer(&offer, mime, purpose);
        } else if !mimes.is_empty() {
            self.last_text = None;
            if announce {
                let what = if mimes.iter().any(|m| m.starts_with("image/")) {
                    "Copied image"
                } else {
                    "Copied"
                };
                self.show_toast(ToastKind::Copied, what, None);
            }
        }
        self.clip_offer = Some(offer);
    }

    fn on_primary(&mut self, offer: Option<Offer>) {
        if let Some(old) = std::mem::replace(&mut self.primary_offer, offer) {
            old.destroy();
        }
    }

    fn save_selection_as_note(&mut self) -> Result<()> {
        if !self.manager.supports_primary() {
            self.show_toast(ToastKind::Error, "Can't save note", Some("compositor lacks primary selection support".into()));
            return Err(anyhow!("primary selection unsupported"));
        }
        let Some(offer) = self.primary_offer.clone() else {
            self.show_toast(ToastKind::Info, "Nothing selected", Some("highlight some text first".into()));
            return Ok(());
        };
        let mimes = offer.mimes();
        if is_secret(&mimes) {
            self.show_toast(ToastKind::Info, "Not saved", Some("selection is a password".into()));
            return Ok(());
        }
        match pick_text(&mimes) {
            Some(mime) => self.read_offer(&offer, mime, Purpose::Note),
            None => self.show_toast(ToastKind::Info, "Nothing to save", Some("selection isn't text".into())),
        }
        Ok(())
    }

    /// Ask the selection owner to write `mime` into a pipe; a helper thread
    /// reads it and reports back through the channel, so a slow app never
    /// blocks the event loop.
    fn read_offer(&self, offer: &Offer, mime: &str, purpose: Purpose) {
        let (mut reader, writer) = match std::io::pipe() {
            Ok(pipe) => pipe,
            Err(e) => return log::warn!("pipe: {e}"),
        };
        offer.receive(mime, writer.as_fd());
        if let Err(e) = self.conn.flush() {
            log::warn!("flush: {e}");
        }
        drop(writer);

        let tx = self.tx.clone();
        let max = self.cfg.max_entry_bytes;
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            let text = match (&mut reader).take(max as u64 + 1).read_to_end(&mut buf) {
                Ok(_) if buf.len() > max => Err(format!("bigger than {max} bytes")),
                Ok(_) => Ok(String::from_utf8_lossy(&buf).into_owned()),
                Err(e) => Err(e.to_string()),
            };
            let _ = tx.send(Event::ReadDone { purpose, text });
        });
    }

    fn on_read_done(&mut self, purpose: Purpose, text: Result<String, String>) {
        match purpose {
            Purpose::Clip { generation, announce } => {
                if generation != self.clip_generation {
                    return; // superseded by a newer copy
                }
                let text = match text {
                    Ok(text) => text,
                    Err(e) => return log::info!("ignoring clipboard contents: {e}"),
                };
                if text.trim().is_empty() {
                    return;
                }
                if let Err(e) = self.store.add_clip(&text, self.cfg.history_limit) {
                    log::warn!("saving history: {e}");
                }
                let preview = self.cfg.toast_preview.then(|| text::one_line(&text, 60));
                self.last_text = Some(text);
                if announce {
                    self.show_toast(ToastKind::Copied, "Copied", preview);
                }
            }
            Purpose::Note => match text {
                Ok(text) if !text.trim().is_empty() => {
                    let note = text.trim_end_matches(['\n', '\r']);
                    match self.store.add_note(note) {
                        Ok(_) => {
                            let preview = self.cfg.toast_preview.then(|| text::one_line(note, 60));
                            self.show_toast(ToastKind::Note, "Saved to notes", preview);
                        }
                        Err(e) => self.show_toast(ToastKind::Error, "Couldn't save note", Some(e.to_string())),
                    }
                }
                Ok(_) => self.show_toast(ToastKind::Info, "Nothing selected", None),
                Err(e) => self.show_toast(ToastKind::Error, "Couldn't read selection", Some(e)),
            },
        }
    }

    fn copy_entry(&mut self, id: i64) -> Result<()> {
        let entry = self.store.get(id)?.ok_or_else(|| anyhow!("entry {id} not found"))?;
        if entry.kind == Kind::Clip {
            self.store.touch(id)?;
        }
        let preview = self.cfg.toast_preview.then(|| text::one_line(&entry.text, 60));
        self.set_clipboard(entry.text);
        self.show_toast(ToastKind::Copied, "Copied", preview);
        Ok(())
    }

    /// Become the clipboard owner and serve `text`.
    fn set_clipboard(&mut self, text: String) {
        let data = SourceData::new(text.as_bytes().to_vec());
        let source = self.manager.create_source(&self.qh, data);
        for mime in TEXT_MIMES {
            source.offer(mime);
        }
        source.offer(MARKER);
        self.device.set_selection(&source);
        if let Some(old) = self.own_source.replace(source) {
            old.destroy();
        }
        self.last_text = Some(text);
    }
}

fn spawn_popup() {
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(e) => return log::warn!("can't find own executable: {e}"),
    };
    match Command::new(exe).arg("popup").spawn() {
        // Reap it so it doesn't linger as a zombie.
        Ok(mut child) => drop(std::thread::spawn(move || child.wait())),
        Err(e) => log::warn!("spawning popup: {e}"),
    }
}

impl ProvidesRegistryState for Daemon {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }
    registry_handlers![OutputState];
}

delegate_registry!(Daemon);
