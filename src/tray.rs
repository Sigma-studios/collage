//! System tray icon (StatusNotifierItem — shows up in Plasma's tray).

use ksni::blocking::{Handle, TrayMethods};
use ksni::menu::StandardItem;
use smithay_client_toolkit::reexports::calloop::channel::Sender;

use crate::daemon::Event;

pub struct CollageTray {
    tx: Sender<Event>,
}

impl ksni::Tray for CollageTray {
    fn id(&self) -> String {
        "collage".into()
    }

    fn title(&self) -> String {
        "Collage".into()
    }

    fn icon_name(&self) -> String {
        "edit-paste".into()
    }

    fn tool_tip(&self) -> ksni::ToolTip {
        ksni::ToolTip {
            title: "Collage".into(),
            description: "Meta+V: history & notes\nMeta+S: save highlighted text as a note".into(),
            ..Default::default()
        }
    }

    fn activate(&mut self, _x: i32, _y: i32) {
        let _ = self.tx.send(Event::OpenPopup);
    }

    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        vec![
            StandardItem {
                label: "History & notes".into(),
                icon_name: "edit-paste".into(),
                activate: Box::new(|tray: &mut Self| {
                    let _ = tray.tx.send(Event::OpenPopup);
                }),
                ..Default::default()
            }
            .into(),
            ksni::MenuItem::Separator,
            StandardItem {
                label: "Quit".into(),
                icon_name: "application-exit".into(),
                activate: Box::new(|tray: &mut Self| {
                    let _ = tray.tx.send(Event::Quit);
                }),
                ..Default::default()
            }
            .into(),
        ]
    }
}

pub fn spawn(tx: Sender<Event>) -> Option<Handle<CollageTray>> {
    match (CollageTray { tx }).spawn() {
        Ok(handle) => Some(handle),
        Err(e) => {
            log::warn!("tray icon unavailable: {e}");
            None
        }
    }
}
