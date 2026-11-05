//! wlr-data-control protocol plumbing.

use std::io::Write;
use std::sync::{Arc, Mutex};

use wayland_client::{
    Connection, Dispatch, Proxy, QueueHandle, event_created_child,
    protocol::wl_seat::{self, WlSeat},
};
use wayland_protocols_wlr::data_control::v1::client::{
    zwlr_data_control_device_v1::{self, ZwlrDataControlDeviceV1},
    zwlr_data_control_manager_v1::{self, ZwlrDataControlManagerV1},
    zwlr_data_control_offer_v1::{self, ZwlrDataControlOfferV1},
    zwlr_data_control_source_v1::{self, ZwlrDataControlSourceV1},
};

use super::Daemon;

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

impl OfferData {
    pub fn mimes(offer: &ZwlrDataControlOfferV1) -> Vec<String> {
        offer
            .data::<OfferData>()
            .map(|d| d.mimes.lock().unwrap().clone())
            .unwrap_or_default()
    }
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

impl Dispatch<ZwlrDataControlManagerV1, ()> for Daemon {
    fn event(
        _: &mut Self,
        _: &ZwlrDataControlManagerV1,
        _: zwlr_data_control_manager_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ZwlrDataControlDeviceV1, ()> for Daemon {
    fn event(
        state: &mut Self,
        _: &ZwlrDataControlDeviceV1,
        event: zwlr_data_control_device_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        use zwlr_data_control_device_v1::Event;
        match event {
            // The offer's mime types arrive on the offer object itself.
            Event::DataOffer { .. } => {}
            Event::Selection { id } => state.on_selection(id),
            Event::PrimarySelection { id } => state.on_primary(id),
            Event::Finished => {
                log::error!("data-control device finished (seat gone?), exiting");
                state.exit = true;
            }
            _ => {}
        }
    }

    event_created_child!(Daemon, ZwlrDataControlDeviceV1, [
        zwlr_data_control_device_v1::EVT_DATA_OFFER_OPCODE => (ZwlrDataControlOfferV1, OfferData::default()),
    ]);
}

impl Dispatch<ZwlrDataControlOfferV1, OfferData> for Daemon {
    fn event(
        _: &mut Self,
        _: &ZwlrDataControlOfferV1,
        event: zwlr_data_control_offer_v1::Event,
        data: &OfferData,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let zwlr_data_control_offer_v1::Event::Offer { mime_type } = event {
            data.mimes.lock().unwrap().push(mime_type);
        }
    }
}

impl Dispatch<ZwlrDataControlSourceV1, SourceData> for Daemon {
    fn event(
        state: &mut Self,
        source: &ZwlrDataControlSourceV1,
        event: zwlr_data_control_source_v1::Event,
        data: &SourceData,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        use zwlr_data_control_source_v1::Event;
        match event {
            Event::Send { fd, .. } => {
                let bytes = data.bytes.clone();
                // Write off-thread: big pastes can exceed the pipe buffer.
                std::thread::spawn(move || {
                    let mut file = std::fs::File::from(fd);
                    if let Err(e) = file.write_all(&bytes) {
                        log::debug!("serving paste: {e}");
                    }
                });
            }
            Event::Cancelled => {
                if state.own_source.as_ref() == Some(source) {
                    state.own_source = None;
                }
                source.destroy();
            }
            _ => {}
        }
    }
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
