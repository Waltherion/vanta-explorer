//! Drag and drop and the system clipboard for a winit window on Wayland, which winit does not
//! offer: dragging files OUT of the window to other applications, taking files dropped INTO it
//! from another window, and text and files on the clipboard. Vanta Explorer uses all of it,
//! Vanta Viewer the drags.
//!
//! `VANTA_DND_DEBUG=1` (or the older `VANTAEXPLORER_DND_DEBUG=1`) logs the handshake: which
//! serial was used, what the target asked for, how much was written, and how it ended. Kept
//! rather than removed once the feature worked, because whether a drop LANDS depends on the
//! receiving application, Krita and kitty accepted this drag while Zed at first did not, and
//! that is not diagnosable from here without seeing the exchange.
//!
//! winit has no API for starting a drag-and-drop operation, `Window::drag_window` moves the
//! window itself, so we speak `wl_data_device` directly, on winit's own Wayland connection.
//! The app starts a drag once the pointer, with the button still down, leaves the window: the
//! implicit grab, and the serial of the press that began it, are still valid then.
//!
//! The awkward part is the serial. `start_drag` must be given the serial of the implicit grab
//! that began the gesture, i.e. the pointer button press, and winit exposes no serials at
//! all. So we bind our own `wl_seat` and `wl_pointer` on the shared connection purely to watch
//! them go by. Binding a second seat is legitimate: the compositor sends the same events to
//! every bound object, and serials are per-connection, so what we observe is what start_drag
//! will accept.
//!
//! The system clipboard goes through here too, for an app that wants it. Hyprland gives a
//! program's drags AND its clipboard to the first wl_data_device that program made, and only to
//! that one: with egui-winit's own clipboard device beside ours, whichever was made first got
//! both, and the other got nothing, so drops in or clipboard paste failed depending on a race at
//! startup. So make this before anything else that could make a data device, and route copy and
//! paste here rather than through egui-winit.
//!
//! A drag coming IN is read as it enters (the paths, from `text/uri-list`, on a thread of its
//! own), so the window can show what it would do while the pointer is still moving; the drop is
//! handed over once its paths are in.

use anyhow::{anyhow, Result};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
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
/// Offered beside the files when the drag was made with the right button: the window it lands
/// in asks Copy or Move, as a right-button drag between Explorer's panes does. A drag carries no
/// button, so this is how the other window knows. The name is what Explorer windows already
/// look for, so it stays.
const ASK: &str = "application/x-vanta-explorer-ask";

/// Whether the handshake is logged, see the module note.
fn debug() -> bool {
    std::env::var_os("VANTA_DND_DEBUG").is_some() || std::env::var_os("VANTAEXPLORER_DND_DEBUG").is_some()
}
/// Clipboard text formats, best first. The X11 names are what XWayland programs look for.
const CLIPBOARD_FORMATS: [&str; 5] = [TEXT_UTF8, "UTF8_STRING", TEXT, "STRING", "TEXT"];
/// Copied or cut files as GNOME (Nautilus) writes them: "copy" or "cut", then one URI a line.
const GNOME_FILES: &str = "x-special/gnome-copied-files";
/// KDE (Dolphin) marks cut files with "1" in this, beside a plain uri-list.
const KDE_CUT: &str = "application/x-kde-cutselection";

/// What a data source of ours is for.
#[derive(Clone, Copy, PartialEq)]
enum SourceKind {
    Drag,
    Clipboard,
}

pub struct Dnd {
    conn: Connection,
    queue: EventQueue<State>,
    state: State,
    /// Our window, as the drag origin.
    surface: WlSurface,
}

/// A drag from elsewhere over our window.
struct Incoming {
    offer: WlDataOffer,
    /// Surface-local, in logical pixels.
    pos: (f64, f64),
    paths: Option<Vec<PathBuf>>,
    /// The paths still being read from the source.
    reading: Option<mpsc::Receiver<String>>,
    /// The source lets us copy only; it does not allow a move.
    copy_only: bool,
    /// Made with the right button: ask Copy or Move on the drop.
    ask: bool,
    /// The button was released over us; waiting for the paths if they are not in yet.
    dropped: bool,
}

/// What the app needs to know about a drag coming in.
pub struct IncomingView {
    pub pos: (f64, f64),
    pub paths: Option<Vec<PathBuf>>,
    pub ask: bool,
}

/// A drop that has landed, with its paths read.
pub struct Dropped {
    pub pos: (f64, f64),
    pub paths: Vec<PathBuf>,
    pub copy_only: bool,
    pub ask: bool,
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
    /// The drag in flight was made with the right button.
    drag_ask: bool,
    /// Our window's surface, to tell a drag over it from one over another of our surfaces.
    surface: Option<ObjectId>,
    /// What each data offer advertises: its formats and the actions its source allows.
    offers: HashMap<ObjectId, (Vec<String>, DndAction)>,
    /// The clipboard's current offer, destroyed when the next one replaces it.
    selection: Option<WlDataOffer>,
    incoming: Option<Incoming>,
    /// A drag over us that we said no to, destroyed when it leaves.
    declined: Option<WlDataOffer>,
    /// False while the window must not take drops (as the file chooser).
    accepting: bool,
    /// The clipboard while it is ours: what each format holds, the files when it is files
    /// (and whether they were cut), and the source the compositor asks for it.
    clipboard_formats: Vec<(String, String)>,
    clipboard_files: Option<(Vec<PathBuf>, bool)>,
    clipboard_source: Option<WlDataSource>,
    /// Cut files on another program's clipboard, read once per clipboard.
    cut_cache: Option<(ObjectId, Vec<PathBuf>)>,
    /// Something about the incoming drag changed since `poll` last said so.
    changed: bool,
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
        state.surface = Some(surface.id());
        state.accepting = true;

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

    /// Hand `paths` to the compositor as a drag, to be copied or moved as the target likes. No-op
    /// if one is already running, or if no button press has been seen to borrow a serial from.
    pub fn start(&mut self, paths: &[PathBuf], ask: bool) {
        self.start_with(paths, ask, DndAction::Copy | DndAction::Move);
    }

    /// The same, but the files may only be copied: a viewer's picture dropped in a file manager
    /// must not be moved out from under it.
    pub fn start_copy(&mut self, paths: &[PathBuf]) {
        self.start_with(paths, false, DndAction::Copy);
    }

    fn start_with(&mut self, paths: &[PathBuf], ask: bool, actions: DndAction) {
        if debug() {
            eprintln!(
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
        let source = manager.create_data_source(&qh, SourceKind::Drag);
        for mime in [URI_LIST, TEXT_UTF8, TEXT] {
            source.offer(mime.to_string());
        }
        if ask {
            source.offer(ASK.to_string());
        }
        self.state.drag_ask = ask;
        if source.version() >= 3 {
            // Copy first: dropping a file into a chat or an editor should not move it off
            // the disk. The target picks from what we offer.
            source.set_actions(actions);
        }
        device.start_drag(Some(&source), &self.surface, None, serial);
        if debug() {
            eprintln!("dnd: start_drag issued, serial={serial}, uris={:?}", self.state.uri_payload);
        }
        self.state.in_flight = true;
        let _ = self.conn.flush();
    }

    /// Dispatch whatever the compositor has sent us. Call once per frame. True when a drag
    /// coming in moved, got its paths, dropped or left, and the window should be drawn.
    ///
    /// winit reads the socket for the whole connection, so by the time we get here our events
    /// are already queued and only need dispatching, we must NOT read the socket ourselves.
    pub fn poll(&mut self) -> bool {
        if let Err(e) = self.queue.dispatch_pending(&mut self.state) {
            log::warn!("dnd: dispatch failed: {e}");
        }
        if let Some(inc) = &mut self.state.incoming {
            if let Some(rx) = &inc.reading {
                match rx.try_recv() {
                    Ok(list) => {
                        inc.paths = Some(parse_uri_list(&list));
                        inc.reading = None;
                        self.state.changed = true;
                    }
                    Err(mpsc::TryRecvError::Disconnected) => {
                        inc.paths = Some(Vec::new());
                        inc.reading = None;
                        self.state.changed = true;
                    }
                    Err(mpsc::TryRecvError::Empty) => {}
                }
            }
        }
        let _ = self.conn.flush();
        std::mem::take(&mut self.state.changed)
    }

    /// Whether drops are taken at all (not while the window is a file chooser).
    pub fn set_accepting(&mut self, accepting: bool) {
        self.state.accepting = accepting;
    }

    /// A drag from elsewhere over the window, not yet dropped.
    pub fn incoming(&self) -> Option<IncomingView> {
        self.state.incoming.as_ref().filter(|i| !i.dropped).map(|i| IncomingView { pos: i.pos, paths: i.paths.clone(), ask: i.ask })
    }

    /// A drag dropped on the window, once its paths are in. The source is told the drop is
    /// done; what happens to the files is up to the caller.
    pub fn take_drop(&mut self) -> Option<Dropped> {
        let inc = self.state.incoming.as_ref()?;
        if !inc.dropped || inc.paths.is_none() {
            return None;
        }
        let inc = self.state.incoming.take()?;
        let paths = inc.paths.unwrap_or_default();
        if inc.offer.version() >= 3 && !paths.is_empty() {
            inc.offer.finish();
        }
        self.state.offers.remove(&inc.offer.id());
        inc.offer.destroy();
        let _ = self.conn.flush();
        Some(Dropped { pos: inc.pos, paths, copy_only: inc.copy_only, ask: inc.ask })
    }

    /// Put `text` on the system clipboard.
    pub fn set_clipboard(&mut self, text: String) {
        let formats = CLIPBOARD_FORMATS.iter().map(|m| (m.to_string(), text.clone())).collect();
        self.offer_clipboard(formats, None);
    }

    /// Put copied or cut files on the system clipboard, in the forms other file managers read
    /// (and as their paths, for a terminal or a text field).
    pub fn set_clipboard_files(&mut self, paths: &[PathBuf], cut: bool) {
        let uris: Vec<String> = paths.iter().filter_map(|p| file_uri(p)).collect();
        if uris.is_empty() {
            return;
        }
        let text = paths.iter().map(|p| p.to_string_lossy().into_owned()).collect::<Vec<_>>().join("\n");
        let mut formats = vec![
            (GNOME_FILES.to_string(), format!("{}\n{}", if cut { "cut" } else { "copy" }, uris.join("\n"))),
            (URI_LIST.to_string(), format!("{}\r\n", uris.join("\r\n"))),
        ];
        if cut {
            formats.push((KDE_CUT.to_string(), "1".to_string()));
        }
        formats.extend(CLIPBOARD_FORMATS.iter().map(|m| (m.to_string(), text.clone())));
        self.offer_clipboard(formats, Some((paths.to_vec(), cut)));
    }

    /// Empty the system clipboard (after cut files were pasted, which moved them).
    pub fn clear_clipboard(&mut self) {
        let Some(device) = self.state.device.clone() else { return };
        device.set_selection(None, self.state.press_serial.unwrap_or(0));
        if let Some(old) = self.state.clipboard_source.take() {
            old.destroy();
        }
        self.state.clipboard_formats.clear();
        self.state.clipboard_files = None;
        let _ = self.conn.flush();
    }

    fn offer_clipboard(&mut self, formats: Vec<(String, String)>, files: Option<(Vec<PathBuf>, bool)>) {
        let (Some(manager), Some(device)) = (self.state.manager.clone(), self.state.device.clone()) else { return };
        let source = manager.create_data_source(&self.queue.handle(), SourceKind::Clipboard);
        for (mime, _) in &formats {
            source.offer(mime.clone());
        }
        // The serial is the latest input we saw; Hyprland does not check it.
        device.set_selection(Some(&source), self.state.press_serial.unwrap_or(0));
        if let Some(old) = self.state.clipboard_source.replace(source) {
            old.destroy();
        }
        self.state.clipboard_formats = formats;
        self.state.clipboard_files = files;
        let _ = self.conn.flush();
    }

    /// The formats the clipboard holds now, and its offer when another program owns it.
    fn foreign_clipboard(&mut self) -> Option<(WlDataOffer, Vec<String>)> {
        let _ = self.queue.dispatch_pending(&mut self.state);
        if self.state.clipboard_source.is_some() {
            return None;
        }
        let offer = self.state.selection.clone()?;
        let mimes = self.state.offers.get(&offer.id()).map(|o| o.0.clone()).unwrap_or_default();
        Some((offer, mimes))
    }

    /// What the clipboard's owner writes in `mime`. Waits a moment for it: a paste is a
    /// keystroke away from being used, so it cannot come later.
    fn read_clipboard(&self, offer: &WlDataOffer, mime: &str) -> Option<String> {
        let rx = read_offer(&self.conn, offer, mime)?;
        rx.recv_timeout(std::time::Duration::from_millis(500)).ok()
    }

    /// Files that are cut (on the clipboard to be moved), from us or another window, so they
    /// can be drawn faded until they are pasted.
    pub fn cut_files(&mut self) -> Vec<PathBuf> {
        if self.state.clipboard_source.is_some() {
            return self.state.clipboard_files.as_ref().filter(|f| f.1).map(|f| f.0.clone()).unwrap_or_default();
        }
        let Some((offer, mimes)) = self.foreign_clipboard() else { return Vec::new() };
        if let Some((id, paths)) = &self.state.cut_cache {
            if *id == offer.id() {
                return paths.clone();
            }
        }
        // Only a clipboard that says it holds cut files is read; text copied elsewhere is not.
        let paths = if mimes.iter().any(|m| m == GNOME_FILES || m == KDE_CUT) {
            self.clipboard_files().filter(|f| f.1).map(|f| f.0).unwrap_or_default()
        } else {
            Vec::new()
        };
        self.state.cut_cache = Some((offer.id(), paths.clone()));
        paths
    }

    /// The text on the system clipboard, if there is any.
    pub fn clipboard(&mut self) -> Option<String> {
        // Ours: reading it through the pipe would wait on this thread to write it.
        if self.state.clipboard_source.is_some() {
            return self.state.clipboard_formats.iter().find(|(m, _)| m == TEXT_UTF8).map(|(_, t)| t.clone());
        }
        let (offer, mimes) = self.foreign_clipboard()?;
        let mime = CLIPBOARD_FORMATS.iter().find(|f| mimes.iter().any(|m| m == *f))?;
        Some(self.read_clipboard(&offer, mime)?.replace("\r\n", "\n"))
    }

    /// Whether the clipboard holds files (from us or any file manager), without reading them.
    pub fn clipboard_has_files(&mut self) -> bool {
        if self.state.clipboard_source.is_some() {
            return self.state.clipboard_files.is_some();
        }
        self.foreign_clipboard().is_some_and(|(_, m)| m.iter().any(|m| m == GNOME_FILES || m == URI_LIST))
    }

    /// The files on the clipboard, and whether they were cut rather than copied.
    pub fn clipboard_files(&mut self) -> Option<(Vec<PathBuf>, bool)> {
        if self.state.clipboard_source.is_some() {
            return self.state.clipboard_files.clone();
        }
        let (offer, mimes) = self.foreign_clipboard()?;
        let has = |f: &str| mimes.iter().any(|m| m == f);
        let (paths, cut) = if has(GNOME_FILES) {
            parse_gnome_files(&self.read_clipboard(&offer, GNOME_FILES)?)
        } else if has(URI_LIST) {
            let paths = parse_uri_list(&self.read_clipboard(&offer, URI_LIST)?);
            let cut = has(KDE_CUT) && self.read_clipboard(&offer, KDE_CUT).is_some_and(|v| v.trim() == "1");
            (paths, cut)
        } else {
            return None;
        };
        (!paths.is_empty()).then_some((paths, cut))
    }

    /// True while a drop is waiting for its paths, so the loop should come back soon.
    pub fn waiting(&self) -> bool {
        self.state.incoming.as_ref().is_some_and(|i| i.reading.is_some())
    }
}

/// The local files in a `text/uri-list` (RFC 2483): one URI per line, `#` lines are comments.
/// Anything not a `file://` URI on this machine is left out.
pub fn parse_uri_list(list: &str) -> Vec<PathBuf> {
    use std::os::unix::ffi::OsStringExt;
    list.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .filter_map(|l| {
            let rest = l.strip_prefix("file://")?;
            // file:///path, or file://localhost/path.
            let rest = rest.strip_prefix("localhost").unwrap_or(rest);
            if !rest.starts_with('/') {
                return None;
            }
            let bytes = rest.as_bytes();
            let mut out = Vec::with_capacity(bytes.len());
            let mut i = 0;
            while i < bytes.len() {
                let hex = |b: u8| (b as char).to_digit(16);
                if bytes[i] == b'%' && i + 2 < bytes.len() {
                    if let (Some(h), Some(l)) = (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                        out.push((h * 16 + l) as u8);
                        i += 3;
                        continue;
                    }
                }
                out.push(bytes[i]);
                i += 1;
            }
            Some(PathBuf::from(std::ffi::OsString::from_vec(out)))
        })
        .collect()
}

/// Nautilus' form: "copy" or "cut" on the first line, then the files as URIs.
fn parse_gnome_files(text: &str) -> (Vec<PathBuf>, bool) {
    let mut lines = text.lines();
    let cut = lines.next().is_some_and(|l| l.trim() == "cut");
    (parse_uri_list(&lines.collect::<Vec<_>>().join("\n")), cut)
}

/// Read what the source writes for `offer` in `mime`, on a thread: the source may be slow, or
/// be another window of this very process waiting for its own turn of the event loop.
fn read_offer(conn: &Connection, offer: &WlDataOffer, mime: &str) -> Option<mpsc::Receiver<String>> {
    let (mut reader, writer) = std::io::pipe().ok()?;
    offer.receive(mime.to_string(), std::os::fd::AsFd::as_fd(&writer));
    // Our end of the write side closes here, so the read ends when the source closes its own.
    drop(writer);
    let _ = conn.flush();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut text = String::new();
        if reader.read_to_string(&mut text).is_ok() {
            let _ = tx.send(text);
        }
    });
    Some(rx)
}

/// Percent-encode a path into a `file://` URI. Spaces and `#` are the ones that actually bite.
///
/// Listing paths are already absolute, and they are used as-is: canonicalising would resolve
/// symlinks, so dragging a link would hand over its target instead of the link itself.
pub fn file_uri(path: &Path) -> Option<String> {
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

impl Dispatch<WlDataSource, SourceKind> for State {
    fn event(
        state: &mut Self,
        source: &WlDataSource,
        event: wl_data_source::Event,
        kind: &SourceKind,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if *kind == SourceKind::Clipboard {
            match event {
                wl_data_source::Event::Send { mime_type, fd } => {
                    if let Some((_, body)) = state.clipboard_formats.iter().find(|(m, _)| *m == mime_type) {
                        let _ = std::fs::File::from(fd).write_all(body.as_bytes());
                    }
                }
                // Another program took the clipboard.
                wl_data_source::Event::Cancelled => {
                    if state.clipboard_source.as_ref() == Some(source) {
                        state.clipboard_source = None;
                    }
                    source.destroy();
                }
                _ => {}
            }
            return;
        }
        match event {
            // The target accepted and is asking for one of the formats we advertised.
            wl_data_source::Event::Send { mime_type, fd } => {
                if debug() {
                    eprintln!("dnd: target asked for {mime_type}");
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
                        if debug() {
                            eprintln!("dnd: wrote {bytes} bytes of {mime_type}");
                        }
                    }
                    Err(e) => log::warn!("dnd: writing {mime_type} failed: {e}"),
                }
            }
            wl_data_source::Event::Cancelled | wl_data_source::Event::DndFinished => {
                if debug() {
                    eprintln!("dnd: drag ended ({event:?})");
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
        state: &mut Self,
        offer: &WlDataOffer,
        event: <WlDataOffer as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        use wayland_client::protocol::wl_data_offer::Event;
        let entry = state.offers.entry(offer.id()).or_insert_with(|| (Vec::new(), DndAction::empty()));
        match event {
            Event::Offer { mime_type } => entry.0.push(mime_type),
            Event::SourceActions { source_actions: wayland_client::WEnum::Value(a) } => {
                entry.1 = a;
                if let Some(inc) = state.incoming.as_mut().filter(|i| i.offer == *offer) {
                    inc.copy_only = !a.contains(DndAction::Move);
                }
            }
            _ => {}
        }
    }
}

impl Dispatch<WlDataDevice, ()> for State {
    fn event(
        state: &mut Self,
        _: &WlDataDevice,
        event: wl_data_device::Event,
        _: &(),
        conn: &Connection,
        _: &QueueHandle<Self>,
    ) {
        use wl_data_device::Event;
        let debug = debug();
        match event {
            Event::Enter { serial, surface, x, y, id } => {
                let Some(offer) = id else { return };
                let (mimes, actions) = state.offers.get(&offer.id()).cloned().unwrap_or((Vec::new(), DndAction::empty()));
                let ours = state.surface.as_ref() == Some(&surface.id());
                if !ours || !state.accepting || !mimes.iter().any(|m| m == URI_LIST) {
                    offer.accept(serial, None);
                    if let Some(old) = state.declined.replace(offer) {
                        state.offers.remove(&old.id());
                        old.destroy();
                    }
                    if debug {
                        eprintln!("dnd in: declined (ours={ours}, accepting={}, formats={mimes:?})", state.accepting);
                    }
                    return;
                }
                offer.accept(serial, Some(URI_LIST.to_string()));
                let copy_only = !actions.is_empty() && !actions.contains(DndAction::Move);
                if offer.version() >= 3 {
                    let preferred = if copy_only { DndAction::Copy } else { DndAction::Move };
                    offer.set_actions(DndAction::Copy | DndAction::Move, preferred);
                }
                // Our own drag come back: the paths are at hand, and reading them through the
                // pipe would wait on this same thread to write them.
                let (paths, reading) = if state.in_flight {
                    (Some(parse_uri_list(&state.uri_payload)), None)
                } else {
                    (None, read_offer(conn, &offer, URI_LIST))
                };
                let ask = mimes.iter().any(|m| m == ASK) || (state.in_flight && state.drag_ask);
                if debug {
                    eprintln!("dnd in: enter at {x},{y}, own drag={}, copy only={copy_only}, ask={ask}", state.in_flight);
                }
                state.incoming = Some(Incoming { offer, pos: (x, y), paths, reading, copy_only, ask, dropped: false });
                state.changed = true;
            }
            Event::Motion { x, y, .. } => {
                if let Some(inc) = state.incoming.as_mut().filter(|i| !i.dropped) {
                    inc.pos = (x, y);
                    state.changed = true;
                }
            }
            Event::Leave => {
                if let Some(old) = state.declined.take() {
                    state.offers.remove(&old.id());
                    old.destroy();
                }
                // After a drop the offer stays until its paths are in and it is finished.
                if state.incoming.as_ref().is_some_and(|i| !i.dropped) {
                    if let Some(inc) = state.incoming.take() {
                        state.offers.remove(&inc.offer.id());
                        inc.offer.destroy();
                    }
                    state.changed = true;
                }
            }
            Event::Drop => {
                if let Some(inc) = state.incoming.as_mut() {
                    inc.dropped = true;
                    state.changed = true;
                    if debug {
                        eprintln!("dnd in: dropped at {:?}", inc.pos);
                    }
                }
            }
            Event::Selection { id } => {
                if let Some(old) = state.selection.take() {
                    state.offers.remove(&old.id());
                    old.destroy();
                }
                state.selection = id;
            }
            _ => {}
        }
    }

    // Binding a data device means the compositor offers US drags (and the clipboard) too: the
    // offer objects have to be constructible or dispatch fails.
    event_created_child!(State, WlDataDevice, [
        wl_data_device::EVT_DATA_OFFER_OPCODE => (WlDataOffer, ()),
    ]);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_uri_list_gives_the_local_files() {
        let list = "# from somewhere\r\nfile:///home/me/My%20Videos/Rice.mp4\r\nfile://localhost/tmp/a%23b\r\nhttps://example.com/x\r\nfile:///%C3%A6%C3%B8%C3%A5.txt\r\n";
        assert_eq!(
            parse_uri_list(list),
            vec![PathBuf::from("/home/me/My Videos/Rice.mp4"), PathBuf::from("/tmp/a#b"), PathBuf::from("/æøå.txt")]
        );
    }

    #[test]
    fn nautilus_copied_files_read_back() {
        let (paths, cut) = parse_gnome_files("cut\nfile:///tmp/a%20b.txt\nfile:///tmp/c");
        assert!(cut);
        assert_eq!(paths, vec![PathBuf::from("/tmp/a b.txt"), PathBuf::from("/tmp/c")]);
        assert!(!parse_gnome_files("copy\nfile:///tmp/c").1);
    }

    /// What we write when dragging out reads back as the same paths.
    #[test]
    fn our_own_drag_reads_back() {
        let paths = [PathBuf::from("/tmp/x y/Ærø #1.png"), PathBuf::from("/tmp/b")];
        let list: Vec<String> = paths.iter().filter_map(|p| file_uri(p)).collect();
        assert_eq!(parse_uri_list(&format!("{}\r\n", list.join("\r\n"))), paths.to_vec());
    }
}
