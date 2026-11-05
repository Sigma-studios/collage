//! wlr-data-control protocol plumbing.

use std::io::Write;
use std::os::fd::BorrowedFd;
use std::sync::{Arc, Mutex};

use anyhow::{Result, bail};
use wayland_client::{
    Connection, Dispatch, Proxy, QueueHandle, event_created_child,
    globals::GlobalList,
    protocol::wl_seat::{self, WlSeat},
};
// The standardized protocol (what current KWin offers)...
use wayland_protocols::ext::data_control::v1::client::{
    ext_data_control_device_v1 as ext_device, ext_data_control_manager_v1 as ext_manager,
    ext_data_control_offer_v1 as ext_offer, ext_data_control_source_v1 as ext_source,
};
// ...and the older wlroots one it replaced, kept as a fallback.
use wayland_protocols_wlr::data_control::v1::client::{
    zwlr_data_control_device_v1 as wlr_device, zwlr_data_control_manager_v1 as wlr_manager,
    zwlr_data_control_offer_v1 as wlr_offer, zwlr_data_control_source_v1 as wlr_source,
};

use super::Daemon;

// ---- protocol-agnostic wrappers ----------------------------------------------
//
// ext-data-control-v1 and zwlr-data-control-v1 are the same protocol under
// two names, so each wrapper just forwards to whichever one we bound.

pub enum Manager {
    Ext(ext_manager::ExtDataControlManagerV1),
    Wlr(wlr_manager::ZwlrDataControlManagerV1),
}

pub enum Device {
    Ext(ext_device::ExtDataControlDeviceV1),
    Wlr(wlr_device::ZwlrDataControlDeviceV1),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Offer {
    Ext(ext_offer::ExtDataControlOfferV1),
    Wlr(wlr_offer::ZwlrDataControlOfferV1),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    Ext(ext_source::ExtDataControlSourceV1),
    Wlr(wlr_source::ZwlrDataControlSourceV1),
}

impl Manager {
    pub fn bind(globals: &GlobalList, qh: &QueueHandle<Daemon>) -> Result<Self> {
        if let Ok(m) = globals.bind(qh, 1..=1, ()) {
            return Ok(Manager::Ext(m));
        }
        if let Ok(m) = globals.bind(qh, 1..=2, ()) {
            return Ok(Manager::Wlr(m));
        }
        bail!(
            "the compositor supports neither ext-data-control-v1 nor wlr-data-control \
             (one is needed to watch the clipboard)"
        )
    }

    pub fn name(&self) -> String {
        match self {
            Manager::Ext(m) => format!("ext_data_control_manager_v1 v{}", m.version()),
            Manager::Wlr(m) => format!("zwlr_data_control_manager_v1 v{}", m.version()),
        }
    }

    /// Primary selection (highlighted text) is needed for notes.
    pub fn supports_primary(&self) -> bool {
        match self {
            Manager::Ext(_) => true,
            Manager::Wlr(m) => m.version() >= 2,
        }
    }

    pub fn get_device(&self, seat: &WlSeat, qh: &QueueHandle<Daemon>) -> Device {
        match self {
            Manager::Ext(m) => Device::Ext(m.get_data_device(seat, qh, ())),
            Manager::Wlr(m) => Device::Wlr(m.get_data_device(seat, qh, ())),
        }
    }

    pub fn create_source(&self, qh: &QueueHandle<Daemon>, data: SourceData) -> Source {
        match self {
            Manager::Ext(m) => Source::Ext(m.create_data_source(qh, data)),
            Manager::Wlr(m) => Source::Wlr(m.create_data_source(qh, data)),
        }
    }
}

impl Device {
    pub fn set_selection(&self, source: &Source) {
        match (self, source) {
            (Device::Ext(d), Source::Ext(s)) => d.set_selection(Some(s)),
            (Device::Wlr(d), Source::Wlr(s)) => d.set_selection(Some(s)),
            _ => log::error!("source and device come from different protocols"),
        }
    }
}

impl Offer {
    pub fn mimes(&self) -> Vec<String> {
        let data = match self {
            Offer::Ext(o) => o.data::<OfferData>(),
            Offer::Wlr(o) => o.data::<OfferData>(),
        };
        data.map(|d| d.mimes.lock().unwrap().clone()).unwrap_or_default()
    }

    pub fn receive(&self, mime: &str, fd: BorrowedFd<'_>) {
        match self {
            Offer::Ext(o) => o.receive(mime.to_string(), fd),
            Offer::Wlr(o) => o.receive(mime.to_string(), fd),
        }
    }

    pub fn destroy(&self) {
        match self {
            Offer::Ext(o) => o.destroy(),
            Offer::Wlr(o) => o.destroy(),
        }
    }
}

impl Source {
    pub fn offer(&self, mime: &str) {
        match self {
            Source::Ext(s) => s.offer(mime.to_string()),
            Source::Wlr(s) => s.offer(mime.to_string()),
        }
    }

    pub fn destroy(&self) {
        match self {
            Source::Ext(s) => s.destroy(),
            Source::Wlr(s) => s.destroy(),
        }
    }
}

/// Offered alongside our own sources so we can recognise them.
pub const MARKER: &str = "application/x-collage-owned";
/// KDE convention used by KeePassXC & co. for secrets.
pub const SECRET_HINT: &str = "x-kde-passwordManagerHint";
/// Text mime types, best first.
pub const TEXT_MIMES: &[&str] = &[
    "text/plain;charset=utf-8",
    "UTF8_STRING",
    "text/plain",
    "STRING",
    "TEXT",
];

pub fn pick_text(mimes: &[String]) -> Option<&'static str> {
    TEXT_MIMES
        .iter()
        .copied()
        .find(|wanted| mimes.iter().any(|m| m == wanted))
}

pub fn is_secret(mimes: &[String]) -> bool {
    mimes.iter().any(|m| m == SECRET_HINT)
}

#[derive(Default)]
pub struct OfferData {
    mimes: Mutex<Vec<String>>,
}

pub struct SourceData {
    bytes: Arc<Vec<u8>>,
}

impl SourceData {
    pub fn new(bytes: Vec<u8>) -> Self {
        Self { bytes: Arc::new(bytes) }
    }
}

impl Dispatch<WlSeat, ()> for Daemon {
    fn event(_: &mut Self, _: &WlSeat, _: wl_seat::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}

/// Event handling is identical for both protocols; generate it for each.
macro_rules! data_control_dispatch {
    ($variant:ident, $manager:ident, $device:ident, $offer:ident, $source:ident,
     $Manager:ident, $Device:ident, $Offer:ident, $Source:ident) => {
        impl Dispatch<$manager::$Manager, ()> for Daemon {
            fn event(
                _: &mut Self,
                _: &$manager::$Manager,
                _: $manager::Event,
                _: &(),
                _: &Connection,
                _: &QueueHandle<Self>,
            ) {
            }
        }

        impl Dispatch<$device::$Device, ()> for Daemon {
            fn event(
                state: &mut Self,
                _: &$device::$Device,
                event: $device::Event,
                _: &(),
                _: &Connection,
                _: &QueueHandle<Self>,
            ) {
                match event {
                    // The offer's mime types arrive on the offer object itself.
                    $device::Event::DataOffer { .. } => {}
                    $device::Event::Selection { id } => state.on_selection(id.map(Offer::$variant)),
                    $device::Event::PrimarySelection { id } => state.on_primary(id.map(Offer::$variant)),
                    $device::Event::Finished => {
                        log::error!("data-control device finished (seat gone?), exiting");
                        state.exit = true;
                    }
                    _ => {}
                }
            }

            event_created_child!(Daemon, $device::$Device, [
                $device::EVT_DATA_OFFER_OPCODE => ($offer::$Offer, OfferData::default()),
            ]);
        }

        impl Dispatch<$offer::$Offer, OfferData> for Daemon {
            fn event(
                _: &mut Self,
                _: &$offer::$Offer,
                event: $offer::Event,
                data: &OfferData,
                _: &Connection,
                _: &QueueHandle<Self>,
            ) {
                if let $offer::Event::Offer { mime_type } = event {
                    data.mimes.lock().unwrap().push(mime_type);
                }
            }
        }

        impl Dispatch<$source::$Source, SourceData> for Daemon {
            fn event(
                state: &mut Self,
                source: &$source::$Source,
                event: $source::Event,
                data: &SourceData,
                _: &Connection,
                _: &QueueHandle<Self>,
            ) {
                match event {
                    $source::Event::Send { fd, .. } => serve(data, fd),
                    $source::Event::Cancelled => {
                        let ours = Source::$variant(source.clone());
                        if state.own_source.as_ref() == Some(&ours) {
                            state.own_source = None;
                        }
                        source.destroy();
                    }
                    _ => {}
                }
            }
        }
    };
}

data_control_dispatch!(
    Ext, ext_manager, ext_device, ext_offer, ext_source,
    ExtDataControlManagerV1, ExtDataControlDeviceV1, ExtDataControlOfferV1, ExtDataControlSourceV1
);
data_control_dispatch!(
    Wlr, wlr_manager, wlr_device, wlr_offer, wlr_source,
    ZwlrDataControlManagerV1, ZwlrDataControlDeviceV1, ZwlrDataControlOfferV1, ZwlrDataControlSourceV1
);

/// Write our clipboard contents into the requesting app's pipe, off-thread
/// because big pastes can exceed the pipe buffer.
fn serve(data: &SourceData, fd: std::os::fd::OwnedFd) {
    let bytes = data.bytes.clone();
    std::thread::spawn(move || {
        let mut file = std::fs::File::from(fd);
        if let Err(e) = file.write_all(&bytes) {
            log::debug!("serving paste: {e}");
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_best_text_mime() {
        let mimes = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(pick_text(&mimes(&["image/png"])), None);
        assert_eq!(pick_text(&mimes(&["STRING", "text/plain;charset=utf-8"])), Some("text/plain;charset=utf-8"));
        assert_eq!(pick_text(&mimes(&["TEXT", "UTF8_STRING"])), Some("UTF8_STRING"));
        assert!(is_secret(&mimes(&["text/plain", SECRET_HINT])));
    }
}
