//! The Meta+V popup: clipboard history and notes, searchable, keyboard-driven.
//! A full-screen transparent layer surface with a centered card; clicking the
//! dimmed backdrop closes it.

use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, Write};

use anyhow::Result;
use iced::widget::{button, column, container, mouse_area, operation, row, scrollable, text, text_input};
use iced::{Alignment, Background, Border, Color, Element, Event, Font, Length, Shadow, Task, Theme, Vector, event, keyboard};
use iced_layershell::reexport::{Anchor, KeyboardInteractivity, Layer};
use iced_layershell::settings::{LayerShellSettings, Settings, StartMode};
use iced_layershell::to_layer_message;

use crate::ipc::{self, Request};
use crate::store::{Entry, Kind, Store};
use crate::text as fmt;

const INPUT_ID: &str = "collage-search";
const LIST_ID: &str = "collage-list";
const CARD_W: f32 = 680.0;
const CARD_H: f32 = 560.0;
const ROW_H: f32 = 66.0;
const ROW_GAP: f32 = 4.0;
/// Rough height of the scrollable list inside the card.
const LIST_H: f32 = CARD_H - 170.0;

const BG: Color = Color::from_rgb8(30, 31, 37);
const SURFACE: Color = Color::from_rgb8(40, 42, 50);
const SELECTED: Color = Color::from_rgb8(46, 64, 84);
const TEXT: Color = Color::from_rgb8(228, 230, 235);
const MUTED: Color = Color::from_rgb8(140, 145, 155);
const ACCENT: Color = Color::from_rgb8(61, 174, 233);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tab {
    History,
    Notes,
}

struct Item {
    entry: Entry,
    lower: String,
    date: String,
    preview: String,
}

impl Item {
    fn new(entry: Entry) -> Self {
        Self {
            lower: entry.text.to_lowercase(),
            date: fmt::human_date(entry.created_at),
            preview: fmt::lines(&entry.text, 2, 110),
            entry,
        }
    }
}

struct Popup {
    store: Option<Store>,
    tab: Tab,
    query: String,
    clips: Vec<Item>,
    notes: Vec<Item>,
    /// Indices into the current tab's items that match the query.
    filtered: Vec<usize>,
    /// Position in `filtered`.
    selected: usize,
    status: Option<String>,
    /// Hover only moves the selection after a real mouse move, so keyboard
    /// scrolling doesn't get hijacked by rows sliding under the cursor.
    mouse_active: bool,
}

#[to_layer_message]
#[derive(Debug, Clone)]
enum Message {
    Query(String),
    SwitchTab(Tab),
    Hover(usize),
    Activate(usize),
    Delete(usize),
    Key(keyboard::Key, keyboard::Modifiers),
    MouseMoved,
    Close,
    Ignore,
}

pub fn run(start_on_notes: bool) -> Result<()> {
    // Pressing Meta+V again while open closes the popup.
    let Some(_lock) = acquire_lock()? else {
        return Ok(());
    };

    let boot = move || {
        let mut popup = Popup::load();
        if start_on_notes {
            popup.tab = Tab::Notes;
            popup.refilter();
        }
        (popup, operation::focus(INPUT_ID))
    };

    iced_layershell::application(boot, namespace, update, view)
        .style(|_, _| iced::theme::Style { background_color: Color::TRANSPARENT, text_color: TEXT })
        .theme(|_: &Popup| Some(Theme::Dark))
        .subscription(|_| event::listen_with(key_event))
        .settings(Settings {
            layer_settings: LayerShellSettings {
                anchor: Anchor::Top | Anchor::Bottom | Anchor::Left | Anchor::Right,
                layer: Layer::Overlay,
                exclusive_zone: -1,
                size: Some((0, 0)),
                keyboard_interactivity: KeyboardInteractivity::Exclusive,
                start_mode: StartMode::Active,
                ..Default::default()
            },
            ..Default::default()
        })
        .run()?;
    Ok(())
}

fn namespace() -> String {
    "collage-popup".into()
}

fn key_event(event: Event, _: event::Status, _: iced::window::Id) -> Option<Message> {
    match event {
        Event::Keyboard(keyboard::Event::KeyPressed { key, modifiers, .. }) => Some(Message::Key(key, modifiers)),
        Event::Mouse(iced::mouse::Event::CursorMoved { .. }) => Some(Message::MouseMoved),
        _ => None,
    }
}

impl Popup {
    fn load() -> Self {
        let mut popup = Popup {
            store: None,
            tab: Tab::History,
            query: String::new(),
            clips: Vec::new(),
            notes: Vec::new(),
            filtered: Vec::new(),
            selected: 0,
            status: None,
            mouse_active: false,
        };
        match Store::open() {
            Ok(store) => {
                popup.clips = store.list(Kind::Clip, 2000).unwrap_or_default().into_iter().map(Item::new).collect();
                popup.notes = store.list(Kind::Note, 100_000).unwrap_or_default().into_iter().map(Item::new).collect();
                popup.store = Some(store);
            }
            Err(e) => popup.status = Some(format!("Can't open database: {e}")),
        }
        if ipc::send(&Request::Ping).is_err() {
            popup.status = Some("Daemon not running — start `collage daemon`".into());
        }
        popup.refilter();
        popup
    }

    fn items(&self) -> &[Item] {
        match self.tab {
            Tab::History => &self.clips,
            Tab::Notes => &self.notes,
        }
    }

    fn refilter(&mut self) {
        let terms = fmt::search_terms(&self.query);
        self.filtered = self
            .items()
            .iter()
            .enumerate()
            .filter(|(_, item)| fmt::matches(&item.lower, &terms))
            .map(|(i, _)| i)
            .collect();
        self.selected = self.selected.min(self.filtered.len().saturating_sub(1));
    }

    fn move_selection(&mut self, delta: isize) -> Task<Message> {
        if self.filtered.is_empty() {
            return Task::none();
        }
        let last = self.filtered.len() as isize - 1;
        self.selected = (self.selected as isize + delta).clamp(0, last) as usize;
        self.scroll_to_selected()
    }

    fn scroll_to_selected(&self) -> Task<Message> {
        let y = (self.selected as f32 * (ROW_H + ROW_GAP) - (LIST_H - ROW_H) / 2.0).max(0.0);
        operation::scroll_to(LIST_ID, scrollable::AbsoluteOffset { x: 0.0, y })
    }

    fn switch_tab(&mut self, tab: Tab) -> Task<Message> {
        self.tab = tab;
        self.selected = 0;
        self.refilter();
        Task::batch([self.scroll_to_selected(), operation::focus(INPUT_ID)])
    }

    fn activate(&mut self, pos: usize) -> Task<Message> {
        let Some(&idx) = self.filtered.get(pos) else { return Task::none() };
        let id = self.items()[idx].entry.id;
        match ipc::send(&Request::Copy(id)) {
            Ok(_) => iced::exit(),
            Err(e) => {
                self.status = Some(e.to_string());
                Task::none()
            }
        }
    }

    fn delete(&mut self, pos: usize) -> Task<Message> {
        let Some(&idx) = self.filtered.get(pos) else { return Task::none() };
        let id = self.items()[idx].entry.id;
        if let Some(store) = &self.store
            && let Err(e) = store.delete(id)
        {
            self.status = Some(format!("Delete failed: {e}"));
            return Task::none();
        }
        match self.tab {
            Tab::History => self.clips.remove(idx),
            Tab::Notes => self.notes.remove(idx),
        };
        self.refilter();
        Task::none()
    }

    fn on_key(&mut self, key: keyboard::Key, modifiers: keyboard::Modifiers) -> Task<Message> {
        use keyboard::key::Named;
        match key.as_ref() {
            keyboard::Key::Named(Named::Escape) => iced::exit(),
            keyboard::Key::Named(Named::ArrowDown) => self.move_selection(1),
            keyboard::Key::Named(Named::ArrowUp) => self.move_selection(-1),
            keyboard::Key::Named(Named::PageDown) => self.move_selection(6),
            keyboard::Key::Named(Named::PageUp) => self.move_selection(-6),
            keyboard::Key::Named(Named::Enter) => self.activate(self.selected),
            keyboard::Key::Named(Named::Tab) => {
                let next = if self.tab == Tab::History { Tab::Notes } else { Tab::History };
                self.switch_tab(next)
            }
            keyboard::Key::Named(Named::Delete) if modifiers.shift() => self.delete(self.selected),
            _ => Task::none(),
        }
    }
}

fn update(popup: &mut Popup, message: Message) -> Task<Message> {
    match message {
        Message::Query(query) => {
            popup.query = query;
            popup.selected = 0;
            popup.refilter();
            popup.scroll_to_selected()
        }
        Message::SwitchTab(tab) => popup.switch_tab(tab),
        Message::Hover(pos) => {
            if popup.mouse_active {
                popup.selected = pos;
            }
            Task::none()
        }
        Message::MouseMoved => {
            popup.mouse_active = true;
            Task::none()
        }
        Message::Activate(pos) => popup.activate(pos),
        Message::Delete(pos) => popup.delete(pos),
        Message::Key(key, modifiers) => {
            popup.mouse_active = false;
            popup.on_key(key, modifiers)
        }
        Message::Close => iced::exit(),
        _ => Task::none(),
    }
}

fn view(popup: &Popup) -> Element<'_, Message> {
    let search = text_input("Search…", &popup.query)
        .id(INPUT_ID)
        .on_input(Message::Query)
        .padding(12)
        .size(16)
        .style(|theme, status| {
            let mut style = text_input::default(theme, status);
            style.background = Background::Color(SURFACE);
            style.border = Border { radius: 10.0.into(), width: 1.0, color: Color { a: 0.5, ..ACCENT } };
            style.value = TEXT;
            style.placeholder = MUTED;
            style
        });

    let tabs = row![
        tab_button("History", popup.clips.len(), Tab::History, popup.tab),
        tab_button("Notes", popup.notes.len(), Tab::Notes, popup.tab),
    ]
    .spacing(6);

    let items = popup.items();
    let list: Element<'_, Message> = if popup.filtered.is_empty() {
        let msg = match (popup.tab, popup.query.is_empty()) {
            (_, false) => "No matches",
            (Tab::History, true) => "Nothing copied yet",
            (Tab::Notes, true) => "No notes yet — highlight text anywhere and press Meta+S",
        };
        container(text(msg).color(MUTED)).center(Length::Fill).into()
    } else {
        let rows = popup
            .filtered
            .iter()
            .enumerate()
            .map(|(pos, &idx)| entry_row(&items[idx], pos, pos == popup.selected));
        scrollable(column(rows).spacing(ROW_GAP).padding(iced::Padding { right: 10.0, ..Default::default() }))
            .id(LIST_ID)
            .height(Length::Fill)
            .into()
    };

    let footer = text(
        popup
            .status
            .clone()
            .unwrap_or_else(|| "↑↓ select   Enter copy   Tab switch   Shift+Del delete   Esc close".into()),
    )
    .size(12)
    .color(if popup.status.is_some() { Color::from_rgb8(218, 68, 83) } else { MUTED });

    let card = container(column![search, tabs, list, footer].spacing(12).padding(16))
        .width(CARD_W)
        .height(CARD_H)
        .style(|_| container::Style {
            background: Some(Background::Color(BG)),
            border: Border { radius: 14.0.into(), width: 1.0, color: Color::from_rgba8(255, 255, 255, 0.08) },
            shadow: Shadow { color: Color::from_rgba8(0, 0, 0, 0.5), offset: Vector::new(0.0, 8.0), blur_radius: 32.0 },
            text_color: Some(TEXT),
            ..Default::default()
        });

    // The inner mouse_area swallows clicks so only the backdrop closes.
    let backdrop = container(mouse_area(card).on_press(Message::Ignore))
        .center(Length::Fill)
        .style(|_| container::Style {
            background: Some(Background::Color(Color::from_rgba8(0, 0, 0, 0.35))),
            ..Default::default()
        });
    mouse_area(backdrop).on_press(Message::Close).into()
}

fn tab_button(label: &str, count: usize, tab: Tab, current: Tab) -> Element<'_, Message> {
    let active = tab == current;
    button(text(format!("{label}  {count}")).size(14))
        .padding([6, 14])
        .on_press(Message::SwitchTab(tab))
        .style(move |_, status| button::Style {
            background: Some(Background::Color(match (active, status) {
                (true, _) => Color { a: 0.18, ..ACCENT },
                (false, button::Status::Hovered) => SURFACE,
                _ => Color::TRANSPARENT,
            })),
            text_color: if active { ACCENT } else { MUTED },
            border: Border { radius: 8.0.into(), ..Default::default() },
            ..Default::default()
        })
        .into()
}

fn entry_row(item: &Item, pos: usize, selected: bool) -> Element<'_, Message> {
    let body = column![
        text(item.date.as_str()).size(12).color(MUTED),
        text(item.preview.as_str()).size(13).font(Font::MONOSPACE),
    ]
    .spacing(4);

    let delete = button(text("✕").size(13))
        .padding([2, 8])
        .on_press(Message::Delete(pos))
        .style(|_, status| button::Style {
            background: None,
            text_color: if status == button::Status::Hovered { Color::from_rgb8(218, 68, 83) } else { MUTED },
            ..Default::default()
        });

    let content = row![container(body).width(Length::Fill).clip(true), delete]
        .spacing(8)
        .align_y(Alignment::Center);

    let entry = button(content)
        .width(Length::Fill)
        .height(ROW_H)
        .padding([8, 12])
        .on_press(Message::Activate(pos))
        .style(move |_, _| button::Style {
            background: Some(Background::Color(if selected { SELECTED } else { Color::TRANSPARENT })),
            text_color: TEXT,
            border: Border { radius: 10.0.into(), ..Default::default() },
            ..Default::default()
        });

    mouse_area(entry).on_enter(Message::Hover(pos)).into()
}

/// Single-instance lock. If another popup holds it, ask that one to close
/// (SIGTERM) and return `None` — this makes Meta+V a toggle.
fn acquire_lock() -> Result<Option<File>> {
    let path = ipc::runtime_dir().join("collage-popup.lock");
    let mut file = OpenOptions::new().create(true).truncate(false).read(true).write(true).open(&path)?;
    match file.try_lock() {
        Ok(()) => {
            file.set_len(0)?;
            file.rewind()?;
            write!(file, "{}", std::process::id())?;
            file.flush()?;
            Ok(Some(file))
        }
        Err(std::fs::TryLockError::WouldBlock) => {
            let mut pid = String::new();
            file.read_to_string(&mut pid)?;
            if let Ok(pid) = pid.trim().parse::<i32>() {
                // SAFETY: plain syscall; worst case the pid is stale and kill fails.
                unsafe { libc::kill(pid, libc::SIGTERM) };
            }
            Ok(None)
        }
        Err(std::fs::TryLockError::Error(e)) => Err(e.into()),
    }
}
