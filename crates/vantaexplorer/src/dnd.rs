//! Dragging files OUT of the window, to other applications.
//!
//! `VANTAEXPLORER_DND_DEBUG=1` logs the handshake: which serial was used, what the target asked for,
//! how much was written, and how it ended. Kept rather than removed once the feature worked,
//! because whether a drop LANDS depends on the receiving application, Krita and kitty
//! accepted this drag while Zed at first did not, and that is not diagnosable from here
//! without seeing the exchange.
//!
//! winit has no API for starting a drag-and-drop operation, `Window::drag_window` moves the
//! window itself, so we speak `wl_data_device` directly, on winit's own Wayland connection.
//! Internal drags between panes stay on egui's own machinery; this takes over only once the
//! pointer leaves the window (see `App::window_event`).
//!
//! The awkward part is the serial. `start_drag` must be given the serial of the implicit grab
//! that began the gesture, i.e. the pointer button press, and winit exposes no serials at
//! all. So we bind our own `wl_seat` and `wl_pointer` on the shared connection purely to watch
//! them go by. Binding a second seat is legitimate: the compositor sends the same events to
//! every bound object, and serials are per-connection, so what we observe is what start_drag
//! will accept.

use anyhow::{anyhow, Result};
use std::io::Write;
use std::path::{Path, PathBuf};
use wayland_client::backend::{Backend, ObjectId};
use wayland_client::protocol::{
    wl_data_device::{self, WlDataDevice},
    wl_data_device_manager::{DndAction, WlDataDeviceManager},
    wl_data_offer::WlDataOffer,
    wl_data_source::{self, WlDataSource},
    wl_pointer::{self, WlPointer},
    wl_registry::{self, WlRegistry},
    wl_seat::{self, WlSeat},
    wl_surface::WlSurface,
};
use wayland_client::{event_created_child, Connection, Dispatch, EventQueue, Proxy, QueueHandle};

/// What we offer, best first. `text/uri-list` is the file-drop format every file manager
/// speaks, but plenty of drop targets, editors, chat inputs, only look for plain text, so
/// the paths are offered that way too. Dolphin advertises the same spread, which is why a
/// drag from it lands in places a uri-list-only drag does not.
const URI_LIST: &str = "text/uri-list";
const TEXT_UTF8: &str = "text/plain;charset=utf-8";
const TEXT: &str = "text/plain";

pub struct Dnd {
    conn: Connection,
    queue: EventQueue<State>,
    state: State,
    /// Our window, as the drag origin.
    surface: WlSurface,
}

#[derive(Default)]
struct State {
    manager: Option<WlDataDeviceManager>,
    seat: Option<WlSeat>,
    device: Option<WlDataDevice>,
    /// Serial of the most recent pointer button PRESS, see the module note.
    press_serial: Option<u32>,
    /// The drag in flight, in both forms. The compositor asks for one of them later, on its
    /// own schedule, so they have to outlive the start_drag call.
    uri_payload: String,
    text_payload: String,
    in_flight: bool,
}

impl Dnd {
    /// `display` and `surface` are winit's own; we do not own either.
    pub fn new(display: *mut std::ffi::c_void, surface: *mut std::ffi::c_void) -> Result<Self> {
        // SAFETY: the pointers come from winit's raw handles and outlive us, App drops Dnd
        // with the rest of Gfx, before the window goes.
        let backend = unsafe { Backend::from_foreign_display(display.cast()) };
        let conn = Connection::from_backend(backend);
        let mut queue = conn.new_event_queue();
        let qh = queue.handle();
        let mut state = State::default();

        conn.display().get_registry(&qh, ());
        // Two round trips: the first brings the globals, the second the seat's capabilities
        // (which is what creates the pointer).
        queue.roundtrip(&mut state)?;
        queue.roundtrip(&mut state)?;

        let manager = state
            .manager
            .clone()
            .ok_or_else(|| anyhow!("compositor has no wl_data_device_manager"))?;
        let seat = state
            .seat
            .clone()
            .ok_or_else(|| anyhow!("compositor has no wl_seat"))?;
        state.device = Some(manager.get_data_device(&seat, &qh, ()));

        // Wrap winit's surface as a proxy on our connection. We only ever pass it as an
        // argument; its events stay on winit's queue.
        let id = unsafe { ObjectId::from_ptr(WlSurface::interface(), surface.cast())? };
        let surface = WlSurface::from_id(&conn, id)?;

        Ok(Self {
            conn,
            queue,
            state,
            surface,
        })
    }

    /// True while the compositor owns a drag we started.
    pub fn in_flight(&self) -> bool {
        self.state.in_flight
    }

    /// Hand `paths` to the compositor as a drag. No-op if one is already running, or if no
    /// button press has been seen to borrow a serial from.
    pub fn start(&mut self, paths: &[PathBuf]) {
        if std::env::var_os("VANTAEXPLORER_DND_DEBUG").is_some() {
            log::info!(
                "dnd start: in_flight={} serial={:?} manager={} device={} paths={}",
                self.state.in_flight,
                self.state.press_serial,
                self.state.manager.is_some(),
                self.state.device.is_some(),
                paths.len()
            );
        }
        if self.state.in_flight {
            return;
        }
        let (Some(serial), Some(manager), Some(device)) = (
            self.state.press_serial,
            self.state.manager.clone(),
            self.state.device.clone(),
        ) else {
            return;
        };
        let uris: Vec<String> = paths.iter().filter_map(|p| file_uri(p)).collect();
        if uris.is_empty() {
            return;
        }
        // uri-list is CRLF-separated and terminated, per RFC 2483.
        self.state.uri_payload = format!("{}\r\n", uris.join("\r\n"));
        // Plain text gets the bare paths, a file:// URI pasted into an editor is noise.
        self.state.text_payload = paths
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("\n");

        let qh = self.queue.handle();
        let source = manager.create_data_source(&qh, ());
        for mime in [URI_LIST, TEXT_UTF8, TEXT] {
            source.offer(mime.to_string());
        }
        if source.version() >= 3 {
            // Copy first: dropping a file into a chat or an editor should not move it off
            // the disk. The target picks from what we offer.
            source.set_actions(DndAction::Copy | DndAction::Move);
        }
        device.start_drag(Some(&source), &self.surface, None, serial);
        if std::env::var_os("VANTAEXPLORER_DND_DEBUG").is_some() {
            log::info!("dnd: start_drag issued, serial={serial}, uris={:?}", self.state.uri_payload);
        }
        self.state.in_flight = true;
        let _ = self.conn.flush();
    }

    /// Dispatch whatever the compositor has sent us. Call once per frame.
    ///
    /// winit reads the socket for the whole connection, so by the time we get here our events
    /// are already queued and only need dispatching, we must NOT read the socket ourselves.
    pub fn poll(&mut self) {
        let _ = self.queue.dispatch_pending(&mut self.state);
        let _ = self.conn.flush();
    }
}

/// Percent-encode a path into a `file://` URI. Spaces and `#` are the ones that actually bite.
///
/// Listing paths are already absolute, and they are used as-is: canonicalising would resolve
/// symlinks, so dragging a link would hand over its target instead of the link itself.
fn file_uri(path: &Path) -> Option<String> {
    let abs = if path.is_absolute() {
        path.to_path_buf()
    } else {
        path.canonicalize().ok()?
    };
    let mut out = String::from("file://");
    for byte in abs.to_string_lossy().as_bytes() {
        match byte {
            b'/' | b'-' | b'_' | b'.' | b'~' => out.push(*byte as char),
            b if b.is_ascii_alphanumeric() => out.push(*b as char),
            b => out.push_str(&format!("%{b:02X}")),
        }
    }
    Some(out)
}

impl Dispatch<WlRegistry, ()> for State {
    fn event(
        state: &mut Self,
        registry: &WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
        else {
            return;
        };
        match interface.as_str() {
            // Version 3 is what set_actions needs.
            "wl_data_device_manager" => {
                state.manager = Some(registry.bind(name, version.min(3), qh, ()));
            }
            "wl_seat" => {
                state.seat = Some(registry.bind(name, version.min(5), qh, ()));
            }
            _ => {}
        }
    }
}

impl Dispatch<WlSeat, ()> for State {
    fn event(
        _: &mut Self,
        seat: &WlSeat,
        event: wl_seat::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_seat::Event::Capabilities {
            capabilities: wayland_client::WEnum::Value(caps),
        } = event
        {
            if caps.contains(wl_seat::Capability::Pointer) {
                seat.get_pointer(qh, ());
            }
        }
    }
}

impl Dispatch<WlPointer, ()> for State {
    fn event(
        state: &mut Self,
        _: &WlPointer,
        event: wl_pointer::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_pointer::Event::Button {
            serial,
            state: wayland_client::WEnum::Value(wl_pointer::ButtonState::Pressed),
            ..
        } = event
        {
            state.press_serial = Some(serial);
        }
    }
}

impl Dispatch<WlDataSource, ()> for State {
    fn event(
        state: &mut Self,
        source: &WlDataSource,
        event: wl_data_source::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            // The target accepted and is asking for one of the formats we advertised.
            wl_data_source::Event::Send { mime_type, fd } => {
                if std::env::var_os("VANTAEXPLORER_DND_DEBUG").is_some() {
                    log::info!("dnd: target asked for {mime_type}");
                }
                let body = if mime_type == URI_LIST {
                    &state.uri_payload
                } else {
                    &state.text_payload
                };
                // Dropping the File closes the fd, which is what signals EOF to the target.
                let bytes = body.len();
                let mut file = std::fs::File::from(fd);
                match file.write_all(body.as_bytes()) {
                    Ok(()) => {
                        if std::env::var_os("VANTAEXPLORER_DND_DEBUG").is_some() {
                            log::info!("dnd: wrote {bytes} bytes of {mime_type}");
                        }
                    }
                    Err(e) => log::warn!("dnd: writing {mime_type} failed: {e}"),
                }
            }
            wl_data_source::Event::Cancelled | wl_data_source::Event::DndFinished => {
                if std::env::var_os("VANTAEXPLORER_DND_DEBUG").is_some() {
                    log::info!("dnd: drag ended ({event:?})");
                }
                source.destroy();
                state.in_flight = false;
            }
            _ => {}
        }
    }
}

// No events we act on, but every proxy on our queue needs an impl.
impl Dispatch<WlDataDeviceManager, ()> for State {
    fn event(
        _: &mut Self,
        _: &WlDataDeviceManager,
        _: <WlDataDeviceManager as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WlDataOffer, ()> for State {
    fn event(
        _: &mut Self,
        _: &WlDataOffer,
        _: <WlDataOffer as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WlDataDevice, ()> for State {
    fn event(
        _: &mut Self,
        _: &WlDataDevice,
        _: wl_data_device::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }

    // Binding a data device means the compositor offers US drags too. We ignore them, but the
    // offer object still has to be constructible or dispatch fails.
    event_created_child!(State, WlDataDevice, [
        wl_data_device::EVT_DATA_OFFER_OPCODE => (WlDataOffer, ()),
    ]);
}
