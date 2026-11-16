//! Vanta Explorer.
//!
//! Rendering: winit window on an ash-owned scRGB swapchain, egui composited through our own
//! present shader (see vk.rs). The Vulkan WSI tags the surface itself from the swapchain
//! colorspace (observed on NVIDIA), and manual wp-color-management tagging on top is a fatal
//! protocol error; HDR<->SDR switching recreates the swapchain instead.

mod apps;
mod browser;
mod chooser;
mod config;
mod dir_size;
mod help;
mod dnd;
mod docs;
mod drives;
mod fileops;
mod glyph;
mod hdrmon;
mod icons;
mod model;
mod panes;
mod preview;
mod props;
mod settings;
mod sidebar;
mod text;
mod thumbs;
mod vk;
mod watch;

use anyhow::{anyhow, Context, Result};
use egui_ash_renderer::{Options, RenderMode};
use raw_window_handle::{HasDisplayHandle, HasWindowHandle, RawDisplayHandle, RawWindowHandle};
use std::sync::Arc;
use std::time::{Duration, Instant};
use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::window::{Window, WindowId};

/// egui hands back `Duration::MAX` for "nothing scheduled"; clamp before adding to an
/// Instant so we do not overflow.
fn deadline(after: Duration) -> Instant {
    Instant::now() + after.min(Duration::from_secs(3600))
}

fn main() -> Result<()> {
    // Thumbnails and previews are decoded on worker threads a few megabytes at a time; above
    // glibc's default threshold those come from per-thread heaps that keep what is freed. Mapped
    // instead, they go back to the system when done.
    unsafe { libc::mallopt(libc::M_MMAP_THRESHOLD, 128 * 1024) };
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    // Crashes must never be silent: the app is usually launched from a keybind, so stderr
    // goes nowhere. Every panic lands in ~/.local/state/vantaexplorer/panic.log with a backtrace.
    std::panic::set_hook(Box::new(|info| {
        let bt = std::backtrace::Backtrace::force_capture();
        let msg = format!(
            "==== {} ====\n{info}\n{bt}\n",
            chrono::Local::now().format("%Y-%m-%d %H:%M:%S")
        );
        eprintln!("{msg}");
        if let Some(dir) = model::state_dir() {
            let _ = std::fs::create_dir_all(&dir);
            use std::io::Write;
            if let Ok(mut f) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(dir.join("panic.log"))
            {
                let _ = f.write_all(msg.as_bytes());
            }
        }
    }));

    let probe = std::env::args().any(|a| a == "--probe");

    // `--apps-probe <file>`: list Open With candidates for a file, default first.
    if let Some(i) = args_probe(&std::env::args().collect::<Vec<_>>(), "--apps-probe") {
        let all: Vec<String> = std::env::args().collect();
        for path in &all[i + 1..] {
            let p = std::path::Path::new(path);
            println!("{path}  ({})", apps::mime_for(p).unwrap_or_default());
            for a in apps::candidates(p) {
                println!("  {} [{}]", a.name, a.desktop_id);
            }
        }
        // Several files: also what takes all of them, which is what "Open with" offers then.
        if all.len() > i + 2 {
            let paths: Vec<std::path::PathBuf> = all[i + 1..].iter().map(std::path::PathBuf::from).collect();
            println!("all of them:");
            for a in apps::candidates_for(&paths) {
                println!("  {} [{}]", a.name, a.desktop_id);
            }
        }
        return Ok(());
    }

    // `--doc-probe <file>`: exercise the document renderer without the UI, mirroring
    // vanta-hdr's probe binary. Diagnosis beats guessing when a preview looks wrong.
    let args: Vec<String> = std::env::args().collect();
    if let Some(i) = args.iter().position(|a| a == "--doc-probe") {
        for path in &args[i + 1..] {
            let p = std::path::Path::new(path);
            let t = std::time::Instant::now();
            match docs::render(p, 2048, 0) {
                Ok(docs::DocContent::Raster { img, .. }) => println!(
                    "{path}: raster {}x{} in {:?}",
                    img.width(),
                    img.height(),
                    t.elapsed()
                ),
                Ok(docs::DocContent::Text(job)) => println!(
                    "{path}: text, {} chars, {} sections in {:?}",
                    job.text.len(),
                    job.sections.len(),
                    t.elapsed()
                ),
                Err(e) => println!("{path}: FAILED: {e}"),
            }
        }
        return Ok(());
    }

    // `--choose`: be the window another program's "open" or "save" gets (see chooser.rs). The
    // request is JSON on standard input; the answer goes to standard output as the window closes.
    let choosing = if args.iter().any(|a| a == "--choose") {
        let mut text = String::new();
        std::io::Read::read_to_string(&mut std::io::stdin(), &mut text).context("read the request")?;
        let request: chooser::Request = serde_json::from_str(&text).context("the request is not the JSON --choose expects")?;
        Some((chooser::State::new(request), chooser::Prefs::load()))
    } else {
        None
    };

    let event_loop = EventLoop::new().context("create event loop")?;
    event_loop.set_control_flow(ControlFlow::Poll);

    let (cfg, prefs) = config::load();
    // `vantaexplorer <path>`: the first argument that is not a flag (the probes have returned).
    let start_folder = args.iter().skip(1).find(|a| !a.starts_with("--")).filter(|_| choosing.is_none()).cloned();
    let session = if choosing.is_none() && start_folder.is_none() { panes::lock_session() } else { None };
    // The chooser is one pane on the folder it was asked for, with its own few preferences; it
    // neither reads nor writes the file manager's session.
    let ws = match &choosing {
        Some((state, remembered)) => {
            let mut ws = panes::Workspace::single(remembered.start_folder(&state.request, panes::home_dir()));
            ws.preview_open = remembered.preview_open;
            ws.preview_w = remembered.preview_w;
            ws.sidebar_width = remembered.sidebar_width;
            let pane = ws.active_pane();
            pane.zoom = remembered.zoom;
            pane.show_hidden = remembered.show_hidden;
            ws
        }
        // Only the first window started without a folder keeps the session (see `session`):
        // a window opened on a folder, or a second one, starts on its own and saves nothing, so
        // it can never overwrite the tabs of the window that does.
        None if start_folder.is_some() => panes::single_like_session(panes::home_dir()),
        None => match &session {
            Some(_) => panes::load(panes::home_dir()),
            None => panes::single_like_session(panes::home_dir()),
        },
    };
    let (chooser, chooser_prefs) = match choosing {
        Some((state, remembered)) => (Some(state), remembered),
        None => (None, chooser::Prefs::default()),
    };
    if chooser.is_none() {
        thumbs::clean_cache();
    }
    let theme = browser::Theme::from_config(&cfg);
    let icons = icons::IconCache::new(prefs.icon_themes.clone(), theme.folder);
    fileops::set_keep_dates(prefs.keep_dates);
    model::set_folders_first(prefs.folders_first);
    let mut app = App {
        probe,
        state: None,
        theme,
        cfg,
        watcher: config::Watcher::new(),
        icons,
        settings: prefs,
        ws,
        drives: drives::Drives::new(),
        watch: watch::Watch::new(),
        sidebar: sidebar::Sidebar::new(),
        preview: preview::Preview::new(),
        ops_state: OpsState {
            ops: fileops::FileOps::new(),
            active: Vec::new(),
            drop_menu: None,
            pending_conflict: None,
            pending_error: None,
            clipboard: None,
            open_with: None,
            properties: None,
            pending_delete: None,
            undo: Vec::new(),
            redo: Vec::new(),
            reversing: std::collections::HashMap::new(),
            toast: None,
        },
        thumbs: std::cell::RefCell::new(thumbs::Thumbs::new()),
        dir_size: dir_size::DirSize::new(),
        font_family: None,
        needs_draw: false,
        paste_key: false,
        winit_mods: Default::default(),
        last_draw: Instant::now(),
        egui_repaint_at: Instant::now(),
        input_at: None,
        latency_probe: std::env::var_os("VANTAEXPLORER_LATENCY").is_some(),
        events_since_draw: 0,
        focused: true,
        last_session_save: Instant::now(),
        session,
        chooser,
        chooser_prefs,
        exit_code: 0,
    };
    // The chooser borrows the file manager's places; rearranging them there is not saved.
    app.sidebar.persist = app.chooser.is_none();
    // `vantaexplorer <path>`: open THAT, not whatever the last session had. This is what every
    // "Open in Vanta Explorer" from another program, `Exec=vantaexplorer %f` in the .desktop entry, and
    // `xdg-mime default vantaexplorer.desktop inode/directory` all go through, without it they
    // launch the app and then silently show the wrong folder. A file argument opens its
    // parent with the file selected. Probe flags have already returned above, so anything
    // left that does not start with `--` is the path.
    if let Some(arg) = &start_folder {
        let target = std::fs::canonicalize(arg).unwrap_or_else(|_| std::path::PathBuf::from(arg));
        let (dir, reveal) = if target.is_dir() {
            (Some(target), None)
        } else if target.is_file() {
            (
                target.parent().map(std::path::Path::to_path_buf),
                target.file_name().map(|n| n.to_string_lossy().into_owned()),
            )
        } else {
            log::warn!("start path does not exist: {}", target.display());
            (None, None)
        };
        if let Some(dir) = dir {
            // A window of its own on that folder: its history starts there.
            let tab = app.ws.active_pane().tab();
            *tab = panes::Tab::new(dir);
            tab.pending_reveal = reveal;
        }
    }
    event_loop.run_app(&mut app).context("run event loop")?;
    // The chooser's answer is its exit code as well as its output: 0 chosen, 1 cancelled.
    if app.exit_code != 0 {
        std::process::exit(app.exit_code);
    }
    Ok(())
}

/// File-operation state that travels together: the worker handle, the progress mirrors,
/// a blocked conflict prompt, and the app-internal clipboard.
/// What the destructive confirmation modal is asking about.
enum Confirm {
    Delete(Vec<std::path::PathBuf>),
    EmptyTrash,
}

struct OpsState {
    ops: fileops::FileOps,
    active: Vec<OpUi>,
    /// (op, name, both are folders, answer)
    pending_conflict: Option<(
        u64,
        String,
        bool,
        std::sync::mpsc::Sender<fileops::ConflictAnswer>,
    )>,
    /// An item of a copy or move failed: (op, item, error, answer).
    pending_error: Option<(u64, String, String, std::sync::mpsc::Sender<fileops::ErrorAnswer>)>,
    drop_menu: Option<(Vec<std::path::PathBuf>, std::path::PathBuf, egui::Pos2)>,
    clipboard: Option<(Vec<std::path::PathBuf>, bool)>,
    open_with: Option<OpenWithState>,
    /// The Properties window, when one is open.
    properties: Option<props::Properties>,
    /// Paths awaiting a "really delete permanently?" answer, plus which button the keyboard
    /// has selected (false = Cancel, the deliberate default).
    pending_delete: Option<(Confirm, bool)>,
    /// Undo/redo stacks. An entry is only pushed once its operation reports Done, so a
    /// cancelled or failed op never becomes something you can "undo".
    undo: Vec<fileops::UndoRecord>,
    redo: Vec<fileops::UndoRecord>,
    /// Ops in flight that are themselves an undo or redo, so their Done lands on the opposite
    /// stack, otherwise Ctrl+Z would toggle the same operation forever.
    reversing: std::collections::HashMap<u64, Reversal>,
    /// Short-lived status-bar message after an undo/redo.
    toast: Option<(String, std::time::Instant)>,
}

/// State of the "Open with..." modal.
struct OpenWithState {
    paths: Vec<std::path::PathBuf>,
    search: String,
    apps: Vec<apps::AppEntry>,
    /// Show only the applications that say they handle this kind of file.
    relevant_only: bool,
    /// The first half of the type, such as "image".
    major: Option<String>,
    /// The files' whole type, such as "text/plain"; empty when they are of several.
    mime: String,
    /// Make the application picked the default for the type, as well as opening the file in it.
    make_default: bool,
}

/// Which stack an in-flight reversal's result belongs on.
#[derive(Clone, Copy, PartialEq)]
enum Reversal {
    Undo,
    Redo,
}

/// UI-side mirror of a running file operation.
struct OpUi {
    id: u64,
    kind: fileops::OpKind,
    total_bytes: u64,
    total_items: u64,
    bytes_done: u64,
    items_done: u64,
    current: String,
    error: Option<String>,
    /// The operation went through, but `error` lists what it left out: shown until dismissed.
    finished: bool,
}

struct Gfx {
    // FIELD ORDER IS LOAD-BEARING: Rust drops in declaration order, and the Vulkan objects
    // reference the winit window's wl_surface. With `window` first, the surface was torn
    // down before vkDestroySurfaceKHR ran, and the NVIDIA driver segfaulted marshalling on
    // the dead connection during App drop (coredump: wl_proxy_marshal <- nvidia-glcore <-
    // drop_glue<Renderer>). GPU objects first, the window LAST.
    egui_renderer: egui_ash_renderer::Renderer<egui_ash_renderer::allocator::DefaultAllocator>,
    renderer: vk::Renderer,
    egui_ctx: egui::Context,
    egui_winit: egui_winit::State,
    hdr: hdrmon::HdrMonitor,
    /// Dragging files out to other apps, and taking drops from them. None if the compositor
    /// lacks the globals, the app works fine without it, you just cannot drag in or out.
    /// Drops before `window`, like the rest.
    dnd: Option<dnd::Dnd>,
    /// A drag from another window is over ours (see `feed_incoming_drag`).
    incoming_drag: bool,
    /// The drag payload egui holds was put there for that drag, not by a drag between panes.
    incoming_payload: bool,
    /// A drop was handed to egui last frame: whatever nothing took is cleared next frame.
    drop_cleanup: bool,
    /// The modifiers to give back after a drop that had to pretend Ctrl was held.
    restore_modifiers: Option<egui::Modifiers>,
    /// Clipboard text read at Ctrl+V, handed to egui as a paste at the next frame.
    pending_paste: Option<String>,
    /// A drag of ours went to the compositor while this button was down: egui is told it was
    /// let go, away from the window.
    release_after_drag: Option<egui::PointerButton>,
    window: Arc<Window>,
}

struct App {
    probe: bool,
    state: Option<Gfx>,
    cfg: config::Config,
    theme: browser::Theme,
    watcher: config::Watcher,
    settings: settings::Settings,
    ws: panes::Workspace,
    drives: drives::Drives,
    watch: watch::Watch,
    icons: icons::IconCache,
    sidebar: sidebar::Sidebar,
    preview: preview::Preview,
    ops_state: OpsState,
    thumbs: std::cell::RefCell<thumbs::Thumbs>,
    dir_size: dir_size::DirSize,
    font_family: Option<String>,
    /// Ctrl+V seen on the raw winit event, before egui-winit swallows it.
    ///
    /// Pasting FILES has nothing to do with text on the system clipboard, but it used to
    /// depend on it: egui-winit only emits Event::Paste if it can READ the clipboard, and
    /// when that read fails the event never arrives and paste silently does nothing. The
    /// file clipboard is our own state, so the keystroke is detected directly instead.
    paste_key: bool,
    /// Modifier state from winit, for the above. Same source egui-winit itself uses.
    winit_mods: winit::keyboard::ModifiersState,
    /// Set by input events; consumed by about_to_wait, which draws directly.
    needs_draw: bool,
    /// When the last frame was drawn, for the software frame cap (see about_to_wait).
    last_draw: Instant,
    /// When egui wants its next frame. egui is immediate-mode: hover fades and click
    /// feedback need a follow-up frame, and egui asks for it via `repaint_delay`.
    ///
    /// This MUST be a reason to draw, not merely a sleep hint. Treating it as a hint alone
    /// deadlocks the loop: egui says "repaint now", nothing else is dirty so we never draw,
    /// the value is never refreshed, and we spin at 100% CPU forever, which starves input
    /// handling and is what made the UI feel frozen unless the mouse kept moving.
    egui_repaint_at: Instant,
    /// When the current input burst arrived, for the VANTAEXPLORER_LATENCY probe.
    input_at: Option<Instant>,
    /// VANTAEXPLORER_LATENCY=1 logs input-to-present timing at debug level.
    latency_probe: bool,
    /// Window focus. egui's cosmetic repaints (cursor blink, hover fades) are suppressed
    /// while unfocused: a hidden window that keeps drawing is what runs the swapchain dry
    /// and triggers the acquire hang in the first place.
    focused: bool,
    /// Layout used to persist only on clean close; with the app still crash-prone that lost
    /// the session every time, so it also saves on a slow timer.
    last_session_save: Instant,
    /// Held by the one window that keeps the session; None: this window saves nothing.
    session: Option<panes::SessionLock>,
    /// How many input events piled up since the last frame, if this climbs, we are behind.
    events_since_draw: u32,
    /// Set when the window is choosing a file for another program (`--choose`).
    chooser: Option<chooser::State>,
    /// What the chooser remembers between uses; written when it closes.
    chooser_prefs: chooser::Prefs,
    /// What the process exits with once the event loop has ended.
    exit_code: i32,
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.state.is_some() {
            return;
        }
        match self.init(event_loop) {
            Ok(gfx) => {
                apply_font(&gfx.egui_ctx, &self.cfg, &mut self.font_family);
                self.state = Some(gfx);
            }
            Err(e) => {
                if !e.to_string().contains("__probe_done__") {
                    eprintln!("vantaexplorer: initialisation failed: {e:#}");
                }
                event_loop.exit();
            }
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        let Some(gfx) = &mut self.state else { return };

        if matches!(
            event,
            WindowEvent::MouseInput { .. } | WindowEvent::CursorMoved { .. }
        ) && self.latency_probe
            && self.input_at.is_none()
        {
            self.input_at = Some(Instant::now());
        }
        // BEFORE egui-winit: it intercepts Ctrl+V and returns without emitting a key event,
        // so this is the only place the keystroke is still visible as itself.
        match &event {
            WindowEvent::ModifiersChanged(m) => self.winit_mods = m.state(),
            WindowEvent::KeyboardInput { event: key, .. }
                if key.state == winit::event::ElementState::Pressed
                    && self.winit_mods.control_key()
                    && !self.winit_mods.alt_key() =>
            {
                use winit::keyboard::Key;
                let is_v = match &key.logical_key {
                    Key::Character(c) => c.eq_ignore_ascii_case("v"),
                    _ => false,
                } || matches!(
                    key.physical_key,
                    winit::keyboard::PhysicalKey::Code(winit::keyboard::KeyCode::KeyV)
                );
                if is_v {
                    self.paste_key = true;
                    self.needs_draw = true;
                    // Text for a text field. egui-winit's own clipboard is off (see dnd.rs), so
                    // the paste event is ours to make.
                    gfx.pending_paste = gfx.dnd.as_mut().and_then(|d| d.clipboard()).filter(|t| !t.is_empty());
                }
            }
            _ => {}
        }
        let response = gfx.egui_winit.on_window_event(&gfx.window, &event);
        if response.repaint {
            // The Wayland on-demand-render trap, via winit: request_redraw() waits for a frame
            // callback, which only arrives after a commit, and we only commit when we draw.
            // Deadlock. So we never trust RedrawRequested; about_to_wait draws instead.
            //
            // Do NOT draw here: a draw blocks on vsync (a whole frame period), and this runs once
            // per event. A cursor sweep emits thousands of CursorMoved, which then queues
            // seconds of backlog. about_to_wait runs once per drained batch, which is the
            // correct place.
            self.needs_draw = true;
            self.events_since_draw += 1;
        }

        match event {
            WindowEvent::CursorMoved { position, .. } => {
                // An internal drag that wanders outside the window becomes a real Wayland
                // drag, so it can land in another application.
                //
                // The trigger is the POSITION, not CursorLeft: while a button is held the
                // compositor keeps delivering pointer events to the grabbing surface, so no
                // leave event arrives. The button still being down is exactly why this works
                //, the implicit grab, and the serial we recorded from its press, are still
                // valid.
                let size = gfx.window.inner_size();
                let outside = position.x < 0.0
                    || position.y < 0.0
                    || position.x > size.width as f64
                    || position.y > size.height as f64;
                if outside {
                    let payload = gfx.egui_ctx.memory(|m| {
                        m.data
                            .get_temp::<browser::DragPayload>(egui::Id::new("dnd"))
                    });
                    if let Some(payload) = payload {
                        if let Some(d) = &mut gfx.dnd {
                            d.start(&payload.source_paths, payload.secondary);
                            if d.in_flight() {
                                // The gesture belongs to the compositor now. Drop our own
                                // copy, or a later release inside the window would fire a
                                // second, bogus internal drop.
                                gfx.egui_ctx.memory_mut(|m| {
                                    m.data.remove_temp::<browser::DragPayload>(
                                        egui::Id::new("dnd"),
                                    )
                                });
                                // The release of the button goes to the compositor, never to
                                // us: egui would hold it down for good, and with a button held
                                // it hovers nothing, so no later drop could land here.
                                gfx.release_after_drag = Some(if payload.secondary {
                                    egui::PointerButton::Secondary
                                } else {
                                    egui::PointerButton::Primary
                                });
                                self.needs_draw = true;
                            }
                        }
                    }
                }
            }
            WindowEvent::Focused(f) => {
                self.focused = f;
                // Repaint once on both edges so focus styling and any missed state catch up.
                self.needs_draw = true;
            }
            WindowEvent::Occluded(occluded) => {
                if !occluded {
                    self.needs_draw = true;
                }
            }
            WindowEvent::CloseRequested => {
                match &mut self.chooser {
                    // Closing the chooser is saying no; about_to_wait answers and leaves.
                    Some(c) => c.cancel(),
                    None => {
                        if self.session.is_some() {
                            panes::save(&self.ws, true);
                        }
                        event_loop.exit();
                    }
                }
            }
            WindowEvent::Resized(size) => {
                if let Err(e) = gfx.renderer.resize(size.width.max(1), size.height.max(1)) {
                    log::error!("resize failed: {e:#}");
                }
                self.needs_draw = true;
            }
            WindowEvent::RedrawRequested => {
                match draw(
                    gfx,
                    &self.cfg,
                    &self.theme,
                    &self.settings,
                    &mut self.ws,
                    &mut self.drives,
                    &mut self.icons,
                    &mut self.sidebar,
                    &mut self.preview,
                    &self.thumbs,
                    &mut self.ops_state,
                    &mut self.dir_size,
                    &mut self.paste_key,
                    &mut self.chooser,
                ) {
                    Ok(after) => self.egui_repaint_at = deadline(after),
                    Err(e) => log::error!("draw failed: {e:#}"),
                }
                self.last_draw = Instant::now();
            }
            _ => {}
        }
    }

    /// Tear down every GPU object while the event loop, and with it the Wayland
    /// connection the driver's surface objects reference, is still alive.
    ///
    /// Dropping `App` after `run_app()` returns is TOO LATE. winit has shut the Wayland
    /// backend down by then, so `vkDestroySwapchainKHR` marshals on a dead `wl_proxy` and
    /// the NVIDIA driver segfaults on every single exit (coredump stack:
    /// `wl_proxy_marshal_array_flags` <- `nvidia-glcore` <- `drop_glue<vk::Renderer>` <-
    /// `drop_glue<App>` <- `main`). Hyprland then keeps the dead window mapped and shows
    /// its "not responding, Wait / Terminate" dialog.
    ///
    /// `Gfx`'s field order stays load-bearing on top of this: it drops the GPU objects
    /// before the window, and this drops the whole lot before the connection.
    fn exiting(&mut self, _event_loop: &ActiveEventLoop) {
        log::debug!("exiting: releasing GPU objects before the Wayland connection closes");
        self.state = None;
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if self.finish_choice(event_loop) {
            return;
        }
        let Some(gfx) = &mut self.state else { return };

        // All of this happens outside input events. The Wayland on-demand-render trap means a
        // frame callback may never arrive from here, so on any change we draw DIRECTLY.
        let now = Instant::now();
        // egui's own repaint request counts as dirty, see egui_repaint_at, but only while
        // focused: unfocused, those are cosmetic (cursor blink, fades) and drawing a hidden
        // window is what exhausts the swapchain.
        let mut dirty =
            std::mem::take(&mut self.needs_draw) || (self.focused && now >= self.egui_repaint_at);

        // Live refresh: watch the folder every visible pane shows, reload the ones that
        // changed underneath us. A tab still streaming is left alone and retried later:
        // reloading mid-stream would only restart the same scan.
        self.watch
            .sync(self.ws.panes.iter().map(|p| p.tabs[p.active_tab].path()));
        for dir in self.watch.poll() {
            let mut deferred = false;
            for pane in &mut self.ws.panes {
                let tab = &mut pane.tabs[pane.active_tab];
                if tab.path() != dir {
                    continue;
                }
                if tab.rx.is_some() {
                    deferred = true;
                } else {
                    log::debug!("live reload: {}", dir.display());
                    tab.reload();
                    dirty = true;
                }
            }
            if deferred {
                self.watch.defer(dir);
            }
        }
        dirty |= self.ws.poll();
        dirty |= self.drives.poll();
        if let Some(done) = self.drives.take_eject_result() {
            let msg = match done {
                Ok(dev) => format!("{dev} can be removed"),
                Err(e) => format!("Eject failed: {e}"),
            };
            self.ops_state.toast = Some((msg, Instant::now()));
            dirty = true;
        }
        if self.dir_size.poll() {
            dirty = true;
        }

        if let Some((cfg, s)) = self.watcher.poll() {
            self.theme = browser::Theme::from_config(&cfg);
            self.icons.clear(); // repainted per-theme SVGs get re-read
            self.icons.set_folder_color(self.theme.folder);
            apply_font(&gfx.egui_ctx, &cfg, &mut self.font_family);
            self.cfg = cfg;
            // A different icon theme means everything drawn from the old one is stale.
            self.icons.set_themes(s.icon_themes.clone());
            fileops::set_keep_dates(s.keep_dates);
            if model::set_folders_first(s.folders_first) {
                for tab in self.ws.panes.iter_mut().flat_map(|p| p.tabs.iter_mut()) {
                    tab.listing.apply_sort();
                }
            }
            self.settings = s;
            dirty = true;
        }

        gfx.hdr
            .set_output(gfx.window.current_monitor().and_then(|m| m.name()));
        if let Some(mc) = gfx.hdr.poll() {
            log::info!(
                "monitor colour state -> hdr={} sdrBrightness={:.2} sdrSaturation={:.2} ref={:.0}nits",
                mc.hdr, mc.sdr_brightness, mc.sdr_saturation, mc.sdr_max_luminance
            );
            if let Err(e) = gfx.renderer.set_hdr(mc.hdr) {
                log::error!("HDR mode switch failed: {e:#}");
            }
            dirty = true;
        }

        dirty |= self.sidebar.poll();

        while let Ok(ev) = self.ops_state.ops.rx.try_recv() {
            dirty = true;
            match ev {
                fileops::Event::Started {
                    id,
                    kind,
                    total_bytes,
                    total_items,
                } => {
                    self.ops_state.active.push(OpUi {
                        id,
                        kind,
                        total_bytes,
                        total_items,
                        bytes_done: 0,
                        items_done: 0,
                        current: String::new(),
                        error: None,
                        finished: false,
                    });
                }
                fileops::Event::Progress {
                    id,
                    bytes_done,
                    items_done,
                    current,
                } => {
                    if let Some(op) = self.ops_state.active.iter_mut().find(|o| o.id == id) {
                        op.bytes_done = bytes_done;
                        op.items_done = items_done;
                        op.current = current;
                    }
                }
                fileops::Event::Conflict { id, name, folders, reply } => {
                    self.ops_state.pending_conflict = Some((id, name, folders, reply));
                }
                fileops::Event::Error { id, name, error, reply } => {
                    self.ops_state.pending_error = Some((id, name, error, reply));
                }
                fileops::Event::Done {
                    id,
                    affected,
                    undo,
                    warning,
                } => {
                    // What was left out stays on screen until dismissed: a toast is gone before
                    // anyone reads a list of files.
                    let notice = warning.and_then(|w| {
                        let op = self.ops_state.active.iter_mut().find(|o| o.id == id)?;
                        op.error = Some(w);
                        op.finished = true;
                        Some(())
                    });
                    if let Some(rec) = undo {
                        match self.ops_state.reversing.remove(&id) {
                            Some(Reversal::Undo) => self.ops_state.redo.push(rec),
                            Some(Reversal::Redo) => self.ops_state.undo.push(rec),
                            None => {
                                // A fresh user action invalidates any redo future.
                                self.ops_state.undo.push(rec);
                                self.ops_state.redo.clear();
                            }
                        }
                    } else {
                        self.ops_state.reversing.remove(&id);
                    }
                    if notice.is_none() {
                        self.ops_state.active.retain(|o| o.id != id);
                    }
                    self.ops_state.ops.forget(id);
                    reload_affected(&mut self.ws, &affected);
                }
                fileops::Event::Failed {
                    id,
                    error,
                    affected,
                } => {
                    self.ops_state.reversing.remove(&id);
                    if let Some(op) = self.ops_state.active.iter_mut().find(|o| o.id == id) {
                        op.error = Some(error);
                    }
                    self.ops_state.ops.forget(id);
                    reload_affected(&mut self.ws, &affected);
                }
            }
        }
        let mut dnd_waiting = false;
        if let Some(d) = &mut gfx.dnd {
            d.set_accepting(self.chooser.is_none());
            dirty |= d.poll();
            dnd_waiting = d.waiting();
        }
        dirty |= self.thumbs.borrow_mut().poll(&gfx.egui_ctx);

        if let Some(img) = self.preview.poll(&gfx.egui_ctx) {
            match gfx.renderer.upload_hdr(img.w, img.h, &img.rgba16f) {
                Ok(()) => log::debug!(
                    "preview uploaded {}x{} kind={:?} primaries={:?}",
                    img.w,
                    img.h,
                    img.kind,
                    img.primaries
                ),
                Err(e) => log::error!("preview upload failed: {e:#}"),
            }
            dirty = true;
        }
        // Independent of the config reload on purpose, see IconCache::poll_theme.
        dirty |= self.icons.poll_theme();

        // Software frame cap. With FIFO, the blocking acquire accidentally paced input-driven
        // redraws to the refresh rate; MAILBOX (which fixed the idle->input freeze, see
        // vk.rs) removes that back-pressure, and a cursor sweep then redraws at event rate,
        // measured at about 500 fps. Cap at ~166 fps: below the display period, so the compositor
        // samples the same frames it would have anyway and no latency is added, but the
        // event-rate spin is gone. Deferring re-arms needs_draw so every dirty source polled
        // above still lands in the deferred frame, and pulls egui_repaint_at forward so the
        // sleep scheduling below wakes us in time, a plain skip would hit the Wayland
        // on-demand-render trap and might never draw.
        const MIN_FRAME: Duration = Duration::from_millis(6);
        if dirty && self.last_draw.elapsed() < MIN_FRAME {
            self.needs_draw = true;
            dirty = false;
            self.egui_repaint_at = self.egui_repaint_at.min(self.last_draw + MIN_FRAME);
        }

        if dirty {
            self.last_draw = Instant::now();
            let started = Instant::now();
            let mut requested = Duration::MAX;
            match draw(
                gfx,
                &self.cfg,
                &self.theme,
                &self.settings,
                &mut self.ws,
                &mut self.drives,
                &mut self.icons,
                &mut self.sidebar,
                &mut self.preview,
                &self.thumbs,
                &mut self.ops_state,
                &mut self.dir_size,
                &mut self.paste_key,
                &mut self.chooser,
            ) {
                Ok(after) => {
                    requested = after;
                    self.egui_repaint_at = deadline(after);
                }
                Err(e) => log::error!("draw failed: {e:#}"),
            }
            // AFTER the draw, which is when the UI has actually made its thumbnail requests.
            // It used to run before, which contradicted end_frame's own contract and became a
            // real fault once queued-but-no-longer-visible work started being dropped: on an
            // iteration with no draw, the visible set read as EMPTY, so every pending
            // thumbnail looked scrolled-away and was discarded along with its slot. The next
            // draw re-requested them, the next idle tick threw them away again, and grid
            // thumbnails never got dispatched at all.
            self.thumbs.borrow_mut().end_frame(&gfx.egui_ctx);
            if self.latency_probe {
                let queued = std::mem::take(&mut self.events_since_draw);
                let since_input = self
                    .input_at
                    .take()
                    .map(|t| t.elapsed().as_secs_f32() * 1000.0)
                    .unwrap_or(f32::NAN);
                log::debug!(
                    "frame: draw {:.1} ms | input->present {:.1} ms | {queued} events queued | egui next in {:?}",
                    started.elapsed().as_secs_f32() * 1000.0,
                    since_input,
                    requested
                );
            } else {
                self.events_since_draw = 0;
                self.input_at = None;
            }
        }

        if self.session.is_some() && self.last_session_save.elapsed() >= Duration::from_secs(30) {
            self.last_session_save = Instant::now();
            panes::save(&self.ws, false);
        }

        // Sleep until whichever comes first: egui's next frame, or the poll floor that keeps
        // the config/HDR/drive channels drained. The 1 ms lower bound is a hard guarantee that
        // we never busy-spin, no matter what egui asks for.
        let streaming = self
            .ws
            .panes
            .iter()
            .any(|p| p.tabs.iter().any(|t| t.rx.is_some()));
        let floor = if streaming || dnd_waiting {
            Duration::from_millis(8)
        } else {
            Duration::from_millis(100)
        };
        let now = Instant::now();
        // Unfocused, egui's repaint time is ignored for drawing (above), so it must not set the
        // sleep either: a past request would otherwise wake the loop every millisecond, draw
        // nothing, and do it again until focus comes back.
        let until_egui = if self.focused { self.egui_repaint_at.saturating_duration_since(now) } else { floor };
        let delay = until_egui.min(floor).max(Duration::from_millis(1));
        event_loop.set_control_flow(ControlFlow::WaitUntil(now + delay));
    }
}

impl App {
    /// Once the chooser has its answer: remember where it was made and how the window was left,
    /// write the answer out, and leave. True when the loop is ending.
    fn finish_choice(&mut self, event_loop: &ActiveEventLoop) -> bool {
        let Some(outcome) = self.chooser.as_ref().and_then(|c| c.outcome.clone()) else { return false };
        let filter = self.chooser.as_ref().and_then(|c| c.filter);
        let app = self.chooser.as_ref().map(|c| c.request.app.clone()).unwrap_or_default();
        let prefs = &mut self.chooser_prefs;
        if let Some(gfx) = &self.state {
            let size = gfx.window.inner_size().to_logical::<f32>(gfx.window.scale_factor());
            (prefs.width, prefs.height) = (size.width.max(480.0), size.height.max(320.0));
        }
        prefs.preview_open = self.ws.preview_open;
        prefs.preview_w = self.ws.preview_w;
        prefs.sidebar_width = self.ws.sidebar_width;
        let pane = self.ws.active_pane();
        (prefs.zoom, prefs.show_hidden) = (pane.zoom, pane.show_hidden);
        match outcome {
            chooser::Outcome::Chosen(paths) => {
                prefs.remember(&app, pane.tab_ref().path());
                println!("{}", chooser::answer(&paths, filter));
                self.exit_code = 0;
            }
            chooser::Outcome::Cancelled => self.exit_code = 1,
        }
        self.chooser_prefs.save();
        event_loop.exit();
        true
    }

    fn init(&mut self, event_loop: &ActiveEventLoop) -> Result<Gfx> {
        // No titlebar, no min/max/close under Hyprland, which draws the border and owns window
        // management. Elsewhere the compositor's decorations are asked for, so the window can
        // still be moved and closed.
        // The chooser is its own kind of window, so a compositor rule can float it, with the
        // title the asking program gave and the size it was last left at.
        let (title, app_id, size) = match &self.chooser {
            Some(c) => (
                if c.request.title.is_empty() { "Choose a file".to_string() } else { c.request.title.clone() },
                "vantaexplorer-chooser",
                (self.chooser_prefs.width, self.chooser_prefs.height),
            ),
            None => ("Vanta Explorer".to_string(), "vantaexplorer", (1280.0, 800.0)),
        };
        let attrs = Window::default_attributes()
            .with_title(title)
            .with_decorations(std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE").is_none())
            // Transparent matters even though we composite our own alpha: without it winit
            // marks the whole wl_surface as an OPAQUE REGION, and Hyprland then skips the
            // blur pass behind the window while still alpha-blending the buffer, the
            // background reads see-through but razor sharp instead of frosted like every
            // other translucent window (kitty, Dolphin/Kvantum).
            .with_transparent(true)
            .with_inner_size(winit::dpi::LogicalSize::new(size.0, size.1));
        #[cfg(target_os = "linux")]
        let attrs = {
            use winit::platform::wayland::WindowAttributesExtWayland;
            attrs.with_name(app_id, app_id)
        };
        let window = Arc::new(event_loop.create_window(attrs).context("create window")?);
        let (display_ptr, surface_ptr) = wayland_handles(&window)?;

        let mut hdr = hdrmon::HdrMonitor::new();
        let output = window.current_monitor().and_then(|m| m.name());
        hdr.set_output(output.clone());
        let want_hdr = hdr.query_now(output.as_deref()).hdr;

        let size = window.inner_size();
        let renderer = unsafe {
            vk::Renderer::new(
                display_ptr,
                surface_ptr,
                size.width.max(1),
                size.height.max(1),
                want_hdr,
            )?
        };

        if self.probe {
            let _ = renderer.report_formats();
            return Err(anyhow!("__probe_done__"));
        }

        // Best-effort: no drag and drop is a missing feature, not a reason to fail startup.
        // Made BEFORE egui-winit's state: Hyprland gives drags and the clipboard to the first
        // data device a program makes, and egui-winit may make one of its own (see dnd.rs).
        let dnd = match wayland_handles(&window).and_then(|(d, s)| dnd::Dnd::new(d, s)) {
            Ok(d) => Some(d),
            Err(e) => {
                log::warn!("drag and drop unavailable: {e:#}");
                None
            }
        };

        let egui_ctx = egui::Context::default();
        // Ctrl+plus/minus/0 zoom the icons (browser.rs), not the whole interface.
        egui_ctx.options_mut(|o| o.zoom_with_keyboard = false);
        let egui_winit = egui_winit::State::new(
            egui_ctx.clone(),
            egui::ViewportId::ROOT,
            &*window,
            None,
            None,
            None,
        );

        let egui_renderer = egui_ash_renderer::Renderer::with_default_allocator(
            renderer.instance_ref(),
            renderer.physical_device,
            renderer.device.clone(),
            RenderMode::RenderPass(renderer.ui_pass),
            Options {
                in_flight_frames: vk::FRAMES_IN_FLIGHT,
                // true = egui emits LINEAR values into our fp16 UI layer; the composite
                // shader then only applies ui_scale.
                srgb_framebuffer: true,
                ..Default::default()
            },
        )
        .map_err(|e| anyhow!("egui renderer: {e}"))?;

        // The chooser belongs to the window that asked for it, when the portal named one.
        if let Some(c) = self.chooser.as_ref().filter(|c| !c.request.parent.is_empty()) {
            if let Err(e) = wayland_handles(&window).and_then(|(d, s)| chooser::adopt(d, s, &c.request.parent)) {
                log::info!("the chooser stands alone: {e:#}");
            }
        }

        Ok(Gfx {
            window,
            renderer,
            egui_ctx,
            egui_winit,
            egui_renderer,
            hdr,
            dnd,
            incoming_drag: false,
            incoming_payload: false,
            drop_cleanup: false,
            restore_modifiers: None,
            pending_paste: None,
            release_after_drag: None,
        })
    }
}

/// A drag from another window over ours, handed to egui as if it were a drag between panes:
/// its paths as the drag payload, the pointer where the drag is, and on the drop a release
/// there. Folders light up under it, the "Move/Copy" note follows it, Ctrl copies, and the drop
/// lands in the same code. While the compositor runs a drag, winit sees no pointer events of
/// its own, so these are the only ones egui gets.
fn feed_incoming_drag(gfx: &mut Gfx, raw: &mut egui::RawInput) {
    let Some(d) = &mut gfx.dnd else { return };
    let id = egui::Id::new("dnd");
    // Surface-local logical pixels to egui points.
    let scale = gfx.window.scale_factor() as f32 / gfx.egui_ctx.pixels_per_point();
    let point = |(x, y): (f64, f64)| egui::pos2(x as f32 * scale, y as f32 * scale);

    if let Some(button) = gfx.release_after_drag.take() {
        let away = egui::pos2(-1.0, -1.0);
        let modifiers = gfx.egui_ctx.input(|i| i.modifiers);
        raw.events.push(egui::Event::PointerMoved(away));
        raw.events.push(egui::Event::PointerButton { pos: away, button, pressed: false, modifiers });
        raw.events.push(egui::Event::PointerGone);
    }

    if std::mem::take(&mut gfx.drop_cleanup) {
        gfx.egui_ctx.memory_mut(|m| m.data.remove_temp::<browser::DragPayload>(id));
        raw.events.push(egui::Event::PointerGone);
        if let Some(m) = gfx.restore_modifiers.take() {
            raw.events.push(egui::Event::ModifiersChanged(m));
        }
    }

    if let Some(drop) = d.take_drop() {
        gfx.incoming_drag = false;
        gfx.incoming_payload = false;
        gfx.drop_cleanup = true;
        if drop.paths.is_empty() {
            return;
        }
        let pos = point(drop.pos);
        gfx.egui_ctx.memory_mut(|m| m.data.insert_temp(id, browser::DragPayload { source_paths: drop.paths, secondary: drop.ask }));
        // A source that allows no move gets a copy, as Ctrl would ask for.
        let mut modifiers = gfx.egui_ctx.input(|i| i.modifiers);
        if drop.copy_only && !modifiers.ctrl {
            gfx.restore_modifiers = Some(modifiers);
            modifiers.ctrl = true;
            modifiers.command = true;
            raw.events.push(egui::Event::ModifiersChanged(modifiers));
        }
        raw.events.push(egui::Event::PointerMoved(pos));
        raw.events.push(egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed: false, modifiers });
        return;
    }

    if let Some(over) = d.incoming() {
        gfx.incoming_drag = true;
        raw.events.push(egui::Event::PointerMoved(point(over.pos)));
        let has_payload = gfx.egui_ctx.memory(|m| m.data.get_temp::<browser::DragPayload>(id).is_some());
        if let Some(paths) = over.paths.filter(|p| !p.is_empty() && !has_payload) {
            gfx.egui_ctx.memory_mut(|m| m.data.insert_temp(id, browser::DragPayload { source_paths: paths, secondary: over.ask }));
            gfx.incoming_payload = true;
        }
    } else if std::mem::take(&mut gfx.incoming_drag) {
        // It left without dropping.
        if std::mem::take(&mut gfx.incoming_payload) {
            gfx.egui_ctx.memory_mut(|m| m.data.remove_temp::<browser::DragPayload>(id));
        }
        raw.events.push(egui::Event::PointerGone);
    }
}

/// Resolve the theme font and hand it to egui. Re-applied on config reload.
fn apply_font(ctx: &egui::Context, cfg: &config::Config, current: &mut Option<String>) {
    if current.as_deref() == Some(cfg.font.family.as_str()) {
        return;
    }
    // The installed font, or the built-in Poppins when that is the family and it is missing.
    let Some(bytes) = vanta_ui::font::file_bytes(&cfg.font.family) else {
        log::warn!("font {} not found; keeping the built-in one", cfg.font.family);
        return;
    };
    let mut fonts = egui::FontDefinitions::default();
    fonts
        .font_data
        .insert("theme".into(), Arc::new(egui::FontData::from_owned(bytes)));
    fonts
        .families
        .entry(egui::FontFamily::Proportional)
        .or_default()
        .insert(0, "theme".into());
    ctx.set_fonts(fonts);
    *current = Some(cfg.font.family.clone());
    log::info!("font: {}", cfg.font.family);
}

/// Returns how long egui wants before the next frame.
#[allow(clippy::too_many_arguments)]
fn draw(
    gfx: &mut Gfx,
    cfg: &config::Config,
    theme: &browser::Theme,
    settings: &settings::Settings,
    ws: &mut panes::Workspace,
    drives: &mut drives::Drives,
    icons: &mut icons::IconCache,
    sidebar: &mut sidebar::Sidebar,
    preview: &mut preview::Preview,
    thumbs: &std::cell::RefCell<thumbs::Thumbs>,
    app_ops: &mut OpsState,
    dir_size: &mut crate::dir_size::DirSize,
    paste_key: &mut bool,
    chooser: &mut Option<chooser::State>,
) -> Result<Duration> {
    let mut raw_input = gfx.egui_winit.take_egui_input(&gfx.window);
    feed_incoming_drag(gfx, &mut raw_input);
    if let Some(text) = gfx.pending_paste.take() {
        raw_input.events.push(egui::Event::Paste(text));
    }
    let hdr_sc = gfx.renderer.hdr_active;
    let hdr_mon = gfx.hdr.is_hdr();

    let hdr_out = hdr_sc && hdr_mon;
    let mc = gfx.hdr.color();
    let (ui_scale, ui_sat) = if !hdr_out {
        (1.0, 1.0)
    } else if cfg.ui.match_desktop {
        (mc.desktop_ui_scale(), mc.sdr_saturation)
    } else {
        (cfg.ui_scale_hdr(), cfg.ui.saturation)
    };

    // The UI reports where the preview image goes and which file it wants; both are read
    // back after the pass so the HDR layer can draw underneath it next. File operations
    // likewise queue as actions and execute here, after all borrows are released.
    let hdr_rect = std::cell::Cell::new(None);
    let want_preview = std::cell::Cell::new(None);
    let actions = std::cell::RefCell::new(Vec::new());
    // Files on the system clipboard, from any window or file manager; our own state only when
    // there is no Wayland clipboard to use.
    let clipboard_filled = match gfx.dnd.as_mut() {
        Some(d) => d.clipboard_has_files(),
        None => app_ops.clipboard.is_some(),
    };
    // Files put on the clipboard this frame: their paths go with them as text, so the "copy
    // the paths" text the browser asks for alongside must not replace them.
    let mut files_on_clipboard = false;
    // Cut files are drawn faded, in every window, until they are pasted.
    let cut_paths: std::collections::HashSet<std::path::PathBuf> = match gfx.dnd.as_mut() {
        Some(d) => d.cut_files().into_iter().collect(),
        None => app_ops.clipboard.iter().filter(|c| c.1).flat_map(|c| c.0.clone()).collect(),
    };
    let mut output = gfx.egui_ctx.run_ui(raw_input, |root| {
        app_ui(
            root,
            theme,
            settings,
            ws,
            drives,
            icons,
            sidebar,
            preview,
            thumbs,
            &actions,
            clipboard_filled,
            &cut_paths,
            app_ops,
            dir_size,
            &hdr_rect,
            &want_preview,
            paste_key,
            chooser,
        );
    });

    for action in actions.into_inner() {
        match action {
            browser::Action::Submit(req) => {
                app_ops.ops.submit(req);
            }
            browser::Action::ConfirmDelete(paths) => {
                // Starts on Cancel: the destructive button should never be one blind Enter away.
                app_ops.pending_delete = Some((Confirm::Delete(paths), false));
            }
            browser::Action::Toast(msg) => {
                app_ops.toast = Some((msg, std::time::Instant::now()));
            }
            browser::Action::EmptyTrash => {
                app_ops.pending_delete = Some((Confirm::EmptyTrash, false));
            }
            browser::Action::Restore(paths) => {
                let items = fileops::trash_items_at(&paths);
                if items.is_empty() {
                    app_ops.toast = Some((
                        "Nothing to restore: no trash record for the selection".into(),
                        std::time::Instant::now(),
                    ));
                } else {
                    app_ops.ops.submit(fileops::Request {
                        kind: fileops::OpKind::RestoreTrash,
                        trash_items: items,
                        ..Default::default()
                    });
                }
            }
            browser::Action::SetClipboard { paths, cut } => {
                if let Some(d) = &mut gfx.dnd {
                    d.set_clipboard_files(&paths, cut);
                    files_on_clipboard = true;
                }
                app_ops.clipboard = Some((paths, cut));
            }
            browser::Action::Paste { dest } => {
                // The system clipboard, so files copied in another window (or in Nautilus or
                // Dolphin) paste here too.
                let files = match gfx.dnd.as_mut() {
                    Some(d) => d.clipboard_files(),
                    None => app_ops.clipboard.clone(),
                };
                if let Some((paths, cut)) = files {
                    app_ops.ops.submit(fileops::Request::new(if cut {
                            fileops::OpKind::Move
                        } else {
                            fileops::OpKind::Copy
                        }, paths, Some(dest)));
                    if cut {
                        // A cut pastes once.
                        app_ops.clipboard = None;
                        if let Some(d) = &mut gfx.dnd {
                            d.clear_clipboard();
                        }
                    }
                }
            }
            browser::Action::OpenDropMenu { sources, dest } => {
                if let Some(pos) = gfx.egui_ctx.pointer_interact_pos() {
                    app_ops.drop_menu = Some((sources, dest, pos));
                }
            }
            // Dropped on the trash: trashed, with the record that lets it be restored. Moving
            // it into the trash's folder would leave it there with no way back.
            browser::Action::Drop { sources, dest, .. } if fileops::in_trash(&dest) => {
                if !sources.iter().any(|s| s.starts_with(fileops::trash_files_dir())) {
                    app_ops.ops.submit(fileops::Request::new(fileops::OpKind::Trash, sources, None));
                }
            }
            browser::Action::Drop { sources, dest, copy } => {
                app_ops.ops.submit(fileops::Request::new(
                    if copy { fileops::OpKind::Copy } else { fileops::OpKind::Move },
                    sources,
                    Some(dest),
                ));
            }
            browser::Action::Properties(paths) => {
                app_ops.properties = Some(props::Properties::new(paths, &settings.date_format));
            }
            browser::Action::OpenWith(paths) => {
                app_ops.open_with = Some(open_with_state(paths));
            }
            browser::Action::RecordRename { from, to } => {
                app_ops.undo.push(fileops::UndoRecord {
                    action: fileops::UndoAction::Renamed { from, to },
                    displaced: Vec::new(),
                    emptied: Vec::new(),
                });
                app_ops.redo.clear();
            }
            browser::Action::TogglePreview => ws.preview_open = !ws.preview_open,
            browser::Action::Activate(paths) => match chooser {
                Some(c) => c.activated(paths),
                None => {
                    for p in &paths {
                        model::log_recent(p);
                        let _ = std::process::Command::new("xdg-open").arg(p).spawn();
                    }
                }
            },
            browser::Action::RecordCreated(path) => {
                // Same shape as a copy: undoing it puts the new entry in the trash.
                app_ops.undo.push(fileops::UndoRecord {
                    action: fileops::UndoAction::Copied(vec![path]),
                    displaced: Vec::new(),
                    emptied: Vec::new(),
                });
                app_ops.redo.clear();
            }
            browser::Action::AddToSidebar { index, path } => sidebar.add(index, &path),
            browser::Action::OpenTerminal(dir) => {
                let dir_s = dir.to_string_lossy().into_owned();
                let Some((terminal, args)) = settings.terminal_command() else {
                    let msg = "No terminal found; set \"terminal\" in explorer.local.jsonc".to_string();
                    log::error!("{msg}");
                    app_ops.toast = Some((msg, std::time::Instant::now()));
                    continue;
                };
                let args: Vec<String> = args.iter().map(|a| a.replace("{dir}", &dir_s)).collect();
                // current_dir as well as the flag: a terminal configured with no arguments
                // at all still lands in the right folder.
                match std::process::Command::new(&terminal)
                    .args(&args)
                    .current_dir(&dir)
                    .spawn()
                {
                    Ok(_) => {}
                    Err(e) => {
                        let msg = format!("Could not start {terminal}: {e}");
                        log::error!("{msg}");
                        app_ops.toast = Some((msg, std::time::Instant::now()));
                    }
                }
            }
            browser::Action::CalculateDirSize(path) => {
                dir_size.start(path);
            }
        }
    }

    match want_preview.take() {
        Some((path, page)) => preview.request(&path, page, &gfx.egui_ctx),
        None => {
            preview.clear();
            // Free the texture when the preview goes away, but only once, the guard keeps
            // the device_wait_idle inside clear_hdr off the per-frame path.
            if gfx.renderer.has_hdr_texture() {
                gfx.renderer.clear_hdr();
            }
        }
    }

    let hdr_draw = hdr_rect.get().map(|r| {
        // egui works in points; the swapchain is physical pixels.
        let s = output.pixels_per_point;
        vk::HdrDraw {
            x: r.min.x * s,
            y: r.min.y * s,
            w: r.width() * s,
            h: r.height() * s,
            primaries: preview::primaries_index(preview.primaries),
            exposure: 1.0,
        }
    });

    let repaint_after = output
        .viewport_output
        .get(&egui::ViewportId::ROOT)
        .map(|v| v.repaint_delay)
        .unwrap_or(Duration::MAX);

    // Copied text goes on the system clipboard through our own data device (see dnd.rs).
    if let Some(d) = &mut gfx.dnd {
        output.platform_output.commands.retain(|c| match c {
            egui::OutputCommand::CopyText(text) => {
                if !files_on_clipboard {
                    d.set_clipboard(text.clone());
                }
                false
            }
            _ => true,
        });
    }
    gfx.egui_winit
        .handle_platform_output(&gfx.window, output.platform_output);

    let mut deltas = std::mem::take(&mut output.textures_delta);
    for (id, per_id) in &deltas.set {
        for delta in per_id {
            gfx.egui_renderer
                .set_texture(gfx.renderer.queue, gfx.renderer.command_pool, *id, delta)
                .map_err(|e| anyhow!("egui set_texture: {e}"))?;
        }
    }

    let primitives = gfx
        .egui_ctx
        .tessellate(output.shapes, output.pixels_per_point);

    let bg_raw = cfg.background_linear();
    let bg_sat = config::apply_saturation([bg_raw[0], bg_raw[1], bg_raw[2]], ui_sat);
    let bg_lin = [bg_sat[0], bg_sat[1], bg_sat[2], bg_raw[3]];
    let a = bg_lin[3];
    let bg = if hdr_out {
        [
            bg_lin[0] * ui_scale,
            bg_lin[1] * ui_scale,
            bg_lin[2] * ui_scale,
            1.0,
        ]
    } else {
        [
            config::linear_to_srgb(bg_lin[0]),
            config::linear_to_srgb(bg_lin[1]),
            config::linear_to_srgb(bg_lin[2]),
            1.0,
        ]
    };
    let bg = [bg[0] * a, bg[1] * a, bg[2] * a, a];

    let egui_renderer = &mut gfx.egui_renderer;
    let ppp = output.pixels_per_point;
    gfx.renderer
        .render(bg, ui_scale, ui_sat, hdr_draw, |cmd, extent| {
            if let Err(e) = egui_renderer.cmd_draw(cmd, extent, ppp, &primitives) {
                log::error!("egui cmd_draw: {e}");
            }
        })?;

    for id in &deltas.free {
        gfx.egui_renderer
            .free_texture(*id)
            .map_err(|e| anyhow!("egui free_texture: {e}"))?;
    }
    deltas.clear();
    Ok(repaint_after)
}

#[allow(clippy::too_many_arguments)]
fn app_ui(
    root: &mut egui::Ui,
    theme: &browser::Theme,
    settings: &settings::Settings,
    ws: &mut panes::Workspace,
    drives: &mut drives::Drives,
    icons: &mut icons::IconCache,
    sidebar: &mut sidebar::Sidebar,
    preview: &preview::Preview,
    thumbs: &std::cell::RefCell<thumbs::Thumbs>,
    actions_out: &std::cell::RefCell<Vec<browser::Action>>,
    clipboard_filled_flag: bool,
    cut_paths: &std::collections::HashSet<std::path::PathBuf>,
    app_ops: &mut OpsState,
    dir_size: &crate::dir_size::DirSize,
    hdr_rect: &std::cell::Cell<Option<egui::Rect>>,
    want_preview: &std::cell::Cell<Option<(std::path::PathBuf, u32)>>,
    paste_key_flag: &mut bool,
    chooser: &mut Option<chooser::State>,
) {
    // Any modal up? Computed before anything reads the keyboard, so dialogs and panes cannot
    // both act on the same keystroke.
    help::handle_key(root.ctx());
    let modal_open = help::is_open(root.ctx())
        || app_ops.pending_delete.is_some()
        || app_ops.pending_conflict.is_some()
        || app_ops.pending_error.is_some()
        || app_ops.open_with.is_some()
        || app_ops.properties.is_some()
        || chooser.as_ref().is_some_and(|c| c.replace.is_some());

    // Escape says no to the program that asked, once there is nothing smaller for it to close:
    // a filter being typed, a rename, the path field or a question. Read before the pane, which
    // takes Escape for those.
    if let Some(c) = chooser.as_mut() {
        let tab = ws.active_pane().tab_ref();
        let idle = tab.typeahead.is_empty() && !tab.search_open && tab.renaming.is_none() && tab.path_edit.is_none();
        if idle && !modal_open && !root.ctx().text_edit_focused() && root.input(|i| i.key_pressed(egui::Key::Escape)) {
            c.cancel();
        }
    }

    // Taken, not borrowed: one keystroke must cause exactly one paste. A modal owns the
    // keyboard outright, so the press is swallowed rather than queued.
    let paste_key = std::mem::take(paste_key_flag) && !modal_open;

    // Undo/redo. Handled here rather than in the per-pane handler because it is app-global:
    // it must work whichever pane is active, and the per-pane handler is skipped entirely
    // when a pane is inactive. Suppressed while a text field has focus so Ctrl+Z inside a
    // rename box is not hijacked. See handle_shortcuts for why this tests keyboard input
    // rather than focus.
    if !root.ctx().text_edit_focused() && !modal_open && chooser.is_none() {
        let (undo_key, redo_key) = root.input(|i| {
            (
                i.modifiers.ctrl && !i.modifiers.shift && i.key_pressed(egui::Key::Z),
                (i.modifiers.ctrl && i.key_pressed(egui::Key::Y))
                    || (i.modifiers.ctrl && i.modifiers.shift && i.key_pressed(egui::Key::Z)),
            )
        });
        if undo_key {
            match app_ops.undo.pop() {
                Some(rec) => apply_reversal(app_ops, rec, Reversal::Undo),
                None => {
                    app_ops.toast = Some(("Nothing to undo".into(), std::time::Instant::now()))
                }
            }
        } else if redo_key {
            match app_ops.redo.pop() {
                Some(rec) => apply_reversal(app_ops, rec, Reversal::Redo),
                None => {
                    app_ops.toast = Some(("Nothing to redo".into(), std::time::Instant::now()))
                }
            }
        }
    }

    // Pane cycling: Ctrl+Tab forward, Ctrl+Shift+Tab back (F6/Shift+F6 as secondary).
    // Ctrl, not Alt, on purpose: a compositor binding commonly owns Alt+Tab (a
    // window switcher eats it before the app ever sees it), and plain Tab drives egui's widget
    // focus. Tabs cycle on Ctrl+PgUp/PgDn, so Ctrl+Tab is free for panes here.
    let (cycle_next, cycle_prev) = root.input_mut(|i| {
        let tab = i.key_pressed(egui::Key::Tab) && i.modifiers.ctrl && !i.modifiers.alt;
        let f6 = i.key_pressed(egui::Key::F6);
        let next = (tab && !i.modifiers.shift) || (f6 && !i.modifiers.shift);
        let prev = (tab && i.modifiers.shift) || (f6 && i.modifiers.shift);
        if next || prev {
            i.events.retain(|e| !matches!(e, egui::Event::Key { key: egui::Key::Tab, .. } | egui::Event::Key { key: egui::Key::F6, .. }));
        }
        (next, prev)
    });
    // F3 splits the pane side by side, and joins it again: with more than one pane it closes
    // the one you are in, which after an F3 is the new one. F9 hides and shows the sidebar.
    if !modal_open && chooser.is_none() {
        let (f3, f9) = root.input(|i| (i.key_pressed(egui::Key::F3), i.key_pressed(egui::Key::F9)));
        if f3 {
            if ws.panes.len() > 1 {
                ws.close(ws.active);
            } else {
                ws.split(ws.active, panes::SplitDir::Horizontal);
            }
        }
        if f9 {
            ws.sidebar_hidden = !ws.sidebar_hidden;
        }
    }
    if (cycle_next || cycle_prev) && ws.panes.len() > 1 {
        let order = ws.visual_order();
        if let Some(pos) = order.iter().position(|&x| x == ws.active) {
            let next_pos = if cycle_next {
                (pos + 1) % order.len()
            } else {
                (pos + order.len() - 1) % order.len()
            };
            ws.active = order[next_pos];
        }
    }

    let full = root.max_rect();
    const STATUS_H: f32 = 24.0;

    // --- status bar (bottom, global) ---
    let status_rect =
        egui::Rect::from_min_max(egui::pos2(full.min.x, full.max.y - STATUS_H), full.max);
    let mut sui = root.new_child(
        egui::UiBuilder::new()
            .max_rect(status_rect)
            .layout(egui::Layout::left_to_right(egui::Align::Center)),
    );
    // Toasts fade after a few seconds; expiry is checked here rather than on a timer.
    let toast = app_ops
        .toast
        .as_ref()
        .filter(|(_, t)| t.elapsed() < std::time::Duration::from_secs(4))
        .map(|(m, _)| m.clone());
    // A frame when it is due to go, or it stays up until something else draws.
    if let Some((_, t)) = &app_ops.toast {
        if let Some(left) = std::time::Duration::from_secs(4).checked_sub(t.elapsed()) {
            root.ctx().request_repaint_after(left);
        }
    }
    let toggle_preview = status_bar(&mut sui, theme, ws, drives, toast.as_deref(), dir_size, actions_out, chooser.as_ref());
    if toggle_preview {
        ws.preview_open = !ws.preview_open;
    }

    // --- the chooser's bar (above the status bar): what will be chosen, and the buttons ---
    let bar_h = chooser.as_ref().map_or(0.0, |c| c.bar_height());
    if let Some(c) = chooser.as_mut() {
        let bar_rect = egui::Rect::from_min_max(
            egui::pos2(full.min.x, full.max.y - STATUS_H - bar_h),
            egui::pos2(full.max.x, full.max.y - STATUS_H),
        );
        let mut bar_ui = root.new_child(egui::UiBuilder::new().max_rect(bar_rect).layout(egui::Layout::top_down(egui::Align::Min)));
        let tab = ws.active_pane().tab_ref();
        let marked: Vec<(std::path::PathBuf, bool)> =
            tab.listing.entries.iter().filter(|e| tab.selection.contains(&e.name)).map(|e| (e.path.clone(), e.is_dir)).collect();
        let here = tab.path().to_path_buf();
        c.show_bar(&mut bar_ui, theme, &here, &marked, modal_open);
    }

    let body_bottom = full.max.y - STATUS_H - 4.0 - bar_h;

    // --- sidebar (left, global) ---
    let side_w = if ws.sidebar_hidden { 0.0 } else { ws.sidebar_width };
    let side_rect =
        egui::Rect::from_min_max(full.min, egui::pos2(full.min.x + side_w, body_bottom));
    let mut side_ui = root.new_child(
        egui::UiBuilder::new()
            .max_rect(side_rect)
            .layout(egui::Layout::top_down(egui::Align::Min)),
    );
    let current_dir = ws.active_pane().tab_ref().path().to_path_buf();
    if ws.sidebar_hidden {
        // Nothing drawn, nothing to drag; F9 brings it back at the width it had.
    } else if let Some(path) = sidebar.show(&mut side_ui, drives, icons, theme, &current_dir) {
        // Per the spec, the sidebar navigates the ACTIVE pane.
        ws.active_pane().tab().navigate(path);
    }
    if sidebar.take_empty_trash_request() {
        // The same question the trash view's own Empty Trash asks.
        app_ops.pending_delete = Some((Confirm::EmptyTrash, false));
    }
    if let Some(device) = sidebar.take_eject_request() {
        app_ops.toast = Some((format!("Ejecting {device}..."), std::time::Instant::now()));
        drives.eject(device);
    }
    // Draggable divider between sidebar and workspace.
    let grip = egui::Rect::from_min_max(
        egui::pos2(side_rect.max.x - 3.0, side_rect.min.y),
        egui::pos2(side_rect.max.x + 3.0, side_rect.max.y),
    );
    let grip_resp = root.interact(grip, root.id().with("sidebar-grip"), if ws.sidebar_hidden { egui::Sense::hover() } else { egui::Sense::drag() });
    if grip_resp.hovered() || grip_resp.dragged() {
        root.ctx()
            .set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
    }
    if grip_resp.dragged() {
        ws.sidebar_width = (side_w + grip_resp.drag_delta().x).clamp(140.0, 600.0);
    }
    if !ws.sidebar_hidden { root.painter().line_segment(
        [
            egui::pos2(side_rect.max.x, side_rect.min.y),
            egui::pos2(side_rect.max.x, side_rect.max.y),
        ],
        egui::Stroke::new(
            1.0,
            if grip_resp.hovered() || grip_resp.dragged() {
                theme.accent
            } else {
                theme.text.gamma_multiply(0.15)
            },
        ),
    ); }

    let sidebar_targets = sidebar.targets();
    let view = browser::View {
        theme,
        settings,
        preview,
        hdr_rect,
        want_preview,
        thumbs,
        actions: actions_out,
        clipboard_filled: clipboard_filled_flag,
        cut: cut_paths,
        dir_size,
        modal_open,
        paste_key,
        chooser: chooser.as_ref(),
        sidebar_targets: &sidebar_targets,
    };

    // --- preview panel (right, window-level) ---
    // It shows the ACTIVE pane's selection, so one panel and one GPU texture serve every
    // pane. A panel per pane was tried first; one shared panel read better.
    use crate::panes::{MIN_LIST_W, MIN_PREVIEW_W};
    let body_right = full.max.x;
    let can_show = ws.preview_open && (body_right - side_rect.max.x) > MIN_PREVIEW_W + MIN_LIST_W;
    let preview_w = if can_show {
        ws.preview_w.clamp(
            MIN_PREVIEW_W,
            (body_right - side_rect.max.x - MIN_LIST_W).max(MIN_PREVIEW_W),
        )
    } else {
        0.0
    };

    let work_right = if can_show {
        body_right - preview_w - 6.0
    } else {
        body_right
    };
    let work_rect = egui::Rect::from_min_max(
        egui::pos2(side_rect.max.x + 6.0, full.min.y),
        egui::pos2(work_right, body_bottom),
    );
    browser::show_workspace(root, ws, icons, &view, work_rect);

    if can_show {
        let split_x = body_right - preview_w;
        let grip = egui::Rect::from_min_max(
            egui::pos2(split_x - 5.0, full.min.y),
            egui::pos2(split_x - 1.0, body_bottom),
        );
        let gr = root.interact(grip, root.id().with("preview-grip"), egui::Sense::drag());
        if gr.hovered() || gr.dragged() {
            root.ctx()
                .set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
            root.painter()
                .rect_filled(grip, 0.0, theme.accent.gamma_multiply(0.5));
        }
        if gr.dragged() {
            let max = (body_right - side_rect.max.x - MIN_LIST_W).max(MIN_PREVIEW_W);
            ws.preview_w = (preview_w - gr.drag_delta().x).clamp(MIN_PREVIEW_W, max);
        }

        let prect = egui::Rect::from_min_max(
            egui::pos2(split_x, full.min.y),
            egui::pos2(body_right, body_bottom),
        );
        let mut pui = root.new_child(
            egui::UiBuilder::new()
                .max_rect(prect)
                .layout(egui::Layout::top_down(egui::Align::Min)),
        );
        let active = &ws.panes[ws.active.min(ws.panes.len() - 1)];
        browser::show_preview(&mut pui, active, &view);
    }

    // --- Drag overlay ---
    if root.input(|i| i.key_pressed(egui::Key::Escape)) {
        root.ctx().memory_mut(|m| m.data.remove_temp::<browser::DragPayload>(egui::Id::new("dnd")));
    }
    
    // Where the drop would land, named by whatever is under the pointer this frame.
    let target = root.ctx().data_mut(|d| d.remove_temp::<std::path::PathBuf>(egui::Id::new(browser::DROP_TARGET)));
    if let Some(payload) = root.ctx().memory(|m| m.data.get_temp::<browser::DragPayload>(egui::Id::new("dnd"))) {
        if let Some(pos) = root.ctx().pointer_interact_pos() {
            let copy = root.input(|i| i.modifiers.ctrl);
            let n = payload.source_paths.len();
            let items = if n == 1 { "1 item".to_string() } else { format!("{n} items") };
            let to = target
                .as_ref()
                .map(|t| format!(" to {}", t.file_name().map_or_else(|| t.to_string_lossy().into_owned(), |f| f.to_string_lossy().into_owned())))
                .unwrap_or_default();
            let text = if payload.secondary {
                format!("☰ Options for {items}{to}")
            } else if copy {
                format!("➕ Copy {items}{to}")
            } else {
                format!("↗ Move {items}{to}")
            };
            
            egui::Area::new(egui::Id::new("drag_tooltip"))
                .fixed_pos(pos + egui::vec2(16.0, 16.0))
                .order(egui::Order::Tooltip)
                .show(root.ctx(), |ui| {
                    egui::Frame::window(ui.style())
                        .fill(ui.visuals().window_fill().gamma_multiply(0.85))
                        .stroke(egui::Stroke::new(1.0, theme.accent))
                        
                        .show(ui, |ui| {
                            ui.label(egui::RichText::new(text).color(theme.text).strong());
                        });
                });
        }
    }

    ops_windows(root.ctx(), app_ops, theme);
    help::show(root.ctx(), theme, &settings.date_format);
    if let Some(c) = chooser.as_mut() {
        c.show_replace(root.ctx(), theme);
    }
}

/// Returns true when the preview toggle was clicked. The toggle lives here because the
/// preview is window-level state and the status bar is the only window-level surface, a
/// dedicated toolbar would cost vertical space this layout deliberately avoids.
#[allow(clippy::too_many_arguments)]
fn status_bar(
    ui: &mut egui::Ui,
    theme: &browser::Theme,
    ws: &mut panes::Workspace,
    drives: &drives::Drives,
    toast: Option<&str>,
    dir_size: &crate::dir_size::DirSize,
    actions: &std::cell::RefCell<Vec<browser::Action>>,
    chooser: Option<&chooser::State>,
) -> bool {
    ui.add_space(8.0);
    // Undo/redo feedback: without it Ctrl+Z on a background pane looks like nothing happened.
    if let Some(msg) = toast {
        ui.label(egui::RichText::new(msg).color(theme.accent));
        ui.separator();
    }
    // Selection summary for the active pane.
    let pane = ws.active_pane();
    let tab = pane.tab_ref();
    let count = tab.selection.len();
    if count > 0 {
        let selected = || {
            tab.listing
                .entries
                .iter()
                .filter(|e| tab.selection.contains(&e.name))
        };
        let (_, dirs, file_bytes) = model::selection_totals(selected());
        // The folder walk is driven by the preview panel; the status bar just reads whatever
        // result is on hand for this same selection, so the two agree and only one walk runs.
        let roots: Vec<std::path::PathBuf> =
            selected().filter(|e| e.is_dir).map(|e| e.path.clone()).collect();
        // The status bar owns this request rather than the preview panel, which can be
        // closed, otherwise the total would sit at "..." forever waiting on a walk nobody
        // had asked for.
        if dirs > 0 && !dir_size.matches(&roots) {
            actions
                .borrow_mut()
                .push(browser::Action::CalculateDirSize(roots.clone()));
        }
        let (dir_bytes, settled) = if dirs == 0 {
            (0, true)
        } else if dir_size.matches(&roots) {
            (dir_size.bytes, dir_size.done)
        } else {
            (0, false)
        };
        // No repaint forced while the size is still being counted: the walk's progress wakes
        // the loop by itself (`dir_size.poll`), about ten times a second.
        ui.label(format!(
            "{count} selected ({}{})",
            model::human_size(file_bytes + dir_bytes),
            if settled { "" } else { "..." }
        ));
    } else {
        let visible = tab.listing.entries.iter().filter(|e| browser::entry_visible(e, pane.show_hidden, "", chooser)).count();
        ui.label(format!("{visible} items"));
    }

    // View mode + zoom for the active pane, so ctrl+scroll has visible feedback.
    let (is_grid, zoom) = {
        let p = ws.active_pane();
        (p.is_grid(), p.zoom)
    };
    let preview_open = ws.preview_open;
    let mut toggled = false;
    let mut toggle_view = false;
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        ui.add_space(8.0);
        if ui
            .selectable_label(preview_open, "Preview")
            .on_hover_text("Preview panel")
            .clicked()
        {
            toggled = true;
        }
        ui.separator();
        // Clickable, not just an indicator: ctrl+scroll alone was too hidden to discover.
        if ui
            .selectable_label(
                is_grid,
                egui::RichText::new(format!(
                    "{}  {:.0}px",
                    if is_grid { "Grid" } else { "List" },
                    zoom
                )),
            )
            .on_hover_text("Click to switch list/grid. ctrl+scroll in a pane zooms.")
            .clicked()
        {
            toggle_view = true;
        }
        ui.separator();
        // Drive usage for the active path: used / total (percent). In a narrow window the
        // longest form that fits the room left beside the item count, never drawn over it.
        if let Some(m) = drives.mount_for(ws.active_pane().tab_ref().path()) {
            let (used, total, pct) = (model::human_size(m.used), model::human_size(m.total), m.percent_used());
            let forms = [
                format!("{}: {used} / {total} ({pct:.0}%)", m.display_name()),
                format!("{used} / {total} ({pct:.0}%)"),
                format!("{pct:.0}% used"),
            ];
            let font = egui::TextStyle::Body.resolve(ui.style());
            let room = ui.available_width() - ui.spacing().item_spacing.x;
            let fits = |t: &String| ui.ctx().fonts_mut(|f| f.layout_no_wrap(t.clone(), font.clone(), egui::Color32::WHITE)).size().x <= room;
            if let Some(text) = forms.iter().find(|t| fits(t)) {
                ui.label(egui::RichText::new(text).color(theme.text.gamma_multiply(0.8)))
                    .on_hover_text(&forms[0]);
            }
        }
    });
    if toggle_view {
        use crate::panes::{DEFAULT_ZOOM, GRID_THRESHOLD};
        let p = ws.active_pane();
        // Jump to a comfortable grid size rather than the threshold itself, which would be a
        // grid of postage stamps.
        p.zoom = if p.is_grid() {
            DEFAULT_ZOOM
        } else {
            GRID_THRESHOLD * 2.4
        };
    }
    toggled
}

/// After an operation touches directories, reload every tab currently showing one of them.
/// (No inotify watch yet; our own operations refreshing immediately covers the common case,
/// external writers still need a manual reload.)
/// Turn a record of what happened into the operation that reverses it.
///
/// Copy is reversed by TRASHING the copies rather than deleting them: undo must never itself
/// be a destructive act you cannot walk back.
fn inverse_request(rec: &fileops::UndoRecord) -> Option<fileops::Request> {
    use fileops::{OpKind, Request, UndoAction};
    match &rec.action {
        UndoAction::Moved(pairs) if !pairs.is_empty() => Some(Request {
            kind: OpKind::MoveExact,
            pairs: pairs.clone(),
            sources: rec.emptied.clone(),
            ..Default::default()
        }),
        UndoAction::Copied(created) if !created.is_empty() => {
            Some(Request::new(OpKind::Trash, created.clone(), None))
        }
        UndoAction::Trashed(items) if !items.is_empty() => Some(Request {
            kind: OpKind::RestoreTrash,
            trash_items: items.clone(),
            ..Default::default()
        }),
        UndoAction::Restored(paths) if !paths.is_empty() => {
            Some(Request::new(OpKind::Trash, paths.clone(), None))
        },
        UndoAction::Renamed { from, to } => Some(Request {
            kind: OpKind::MoveExact,
            pairs: vec![(to.clone(), from.clone())],
            ..Default::default()
        }),
        _ => None,
    }
}

/// Human summary for the status bar.
fn describe(rec: &fileops::UndoRecord) -> String {
    use fileops::UndoAction;
    match &rec.action {
        UndoAction::Moved(p) => format!("move of {} item(s)", p.len()),
        UndoAction::Copied(c) => format!("copy of {} item(s)", c.len()),
        UndoAction::Trashed(t) => format!("trashing of {} item(s)", t.len()),
        UndoAction::Restored(r) => format!("restore of {} item(s)", r.len()),
        UndoAction::Renamed { .. } => "rename".to_string(),
    }
}

/// Run one undo (or redo) step: reverse the action, then put back anything an overwrite
/// displaced. The displaced restore is submitted second because its target paths are still
/// occupied until the main reversal has run, and the worker is strictly sequential.
fn apply_reversal(app_ops: &mut OpsState, rec: fileops::UndoRecord, dir: Reversal) {
    let verb = if dir == Reversal::Undo { "Undid" } else { "Redid" };
    let what = describe(&rec);

    if let Some(req) = inverse_request(&rec) {
        let id = app_ops.ops.submit(req);
        app_ops.reversing.insert(id, dir);
    }
    if !rec.displaced.is_empty() {
        let id = app_ops.ops.submit(fileops::Request {
            kind: fileops::OpKind::RestoreTrash,
            trash_items: rec.displaced.clone(),
            ..Default::default()
        });
        // The restore is part of the same reversal, but its own inverse is not what we want
        // on the opposite stack, the main action's inverse already covers the round trip.
        app_ops.reversing.insert(id, dir);
    }
    app_ops.toast = Some((format!("{verb} {what}"), std::time::Instant::now()));
}

fn reload_affected(ws: &mut panes::Workspace, affected: &[std::path::PathBuf]) {
    for pane in &mut ws.panes {
        for tab in &mut pane.tabs {
            if affected.iter().any(|d| d == tab.path()) {
                tab.reload();
            }
        }
    }
}

/// The Open with window for these files: every application, the relevant ones shown first.
fn open_with_state(paths: Vec<std::path::PathBuf>) -> OpenWithState {
    let mime = apps::common_mime(&paths).unwrap_or_default();
    // "image" for a JPEG and a PNG together: the half of the type they share.
    let majors: Vec<String> = paths
        .iter()
        .filter_map(|p| apps::mime_for(p))
        .filter_map(|m| m.split('/').next().map(str::to_string))
        .collect();
    let major = majors.first().filter(|m| !m.is_empty() && majors.len() == paths.len() && majors.iter().all(|x| x == *m)).cloned();
    OpenWithState {
        paths,
        search: String::new(),
        apps: apps::all_apps(),
        relevant_only: major.is_some(),
        major,
        mime,
        make_default: false,
    }
}

/// Progress + conflict windows, drawn on top of everything.
fn ops_windows(ctx: &egui::Context, app_ops: &mut OpsState, theme: &browser::Theme) {
    if let Some(mut p) = app_ops.properties.take() {
        let (open, wants) = p.show(ctx, theme.accent);
        if open {
            app_ops.properties = Some(p);
        }
        if let props::Wants::OpenWith(path) = wants {
            let mut ow = open_with_state(vec![path]);
            ow.make_default = true;
            app_ops.open_with = Some(ow);
        }
    }
    if let Some((sources, dest, pos)) = app_ops.drop_menu.clone() {
        let mut close = false;
        egui::Area::new(egui::Id::new("drop_menu_popup"))
            .fixed_pos(pos)
            .order(egui::Order::Tooltip)
            .show(ctx, |ui| {
                egui::Frame::menu(ui.style()).show(ui, |ui| {
                    if ui.button("Copy here").clicked() {
                        app_ops.ops.submit(fileops::Request::new(fileops::OpKind::Copy, sources.clone(), Some(dest.clone())));
                        close = true;
                    }
                    if ui.button("Move here").clicked() {
                        app_ops.ops.submit(fileops::Request::new(fileops::OpKind::Move, sources.clone(), Some(dest.clone())));
                        close = true;
                    }
                    ui.separator();
                    if ui.button("Cancel").clicked() {
                        close = true;
                    }
                });
            });
        
        if close || (ctx.input(|i| i.pointer.any_pressed()) && !ctx.egui_wants_pointer_input()) || ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            app_ops.drop_menu = None;
        }
    }

    if !app_ops.active.is_empty() {
        egui::Window::new("File operations")
            .anchor(egui::Align2::RIGHT_BOTTOM, egui::vec2(-12.0, -36.0))
            .collapsible(false)
            .resizable(false)
            .title_bar(false)
            .show(ctx, |ui| {
                let mut cancel: Option<u64> = None;
                let mut dismiss: Option<u64> = None;
                for op in &app_ops.active {
                    ui.horizontal(|ui| match &op.error {
                        Some(e) if op.finished => {
                            ui.colored_label(egui::Color32::from_rgb(240, 200, 90), e);
                            if ui.small_button("Dismiss").clicked() {
                                dismiss = Some(op.id);
                            }
                        }
                        Some(e) => {
                            ui.colored_label(
                                egui::Color32::LIGHT_RED,
                                format!("{} failed: {e}", op.kind.label()),
                            );
                            if ui.small_button("Dismiss").clicked() {
                                dismiss = Some(op.id);
                            }
                        }
                        None => {
                            ui.label(format!("{}...", op.kind.label()));
                            let frac = if op.total_bytes > 0 {
                                op.bytes_done as f32 / op.total_bytes as f32
                            } else if op.total_items > 0 {
                                op.items_done as f32 / op.total_items as f32
                            } else {
                                0.0
                            };
                            ui.add(
                                egui::ProgressBar::new(frac)
                                    .desired_width(180.0)
                                    .fill(theme.accent)
                                    .show_percentage(),
                            );
                            if ui.small_button("Cancel").clicked() {
                                cancel = Some(op.id);
                            }
                        }
                    });
                    if !op.current.is_empty() && op.error.is_none() {
                        ui.label(
                            egui::RichText::new(&op.current)
                                .small()
                                .color(theme.text.gamma_multiply(0.6)),
                        );
                    }
                }
                if let Some(id) = cancel {
                    app_ops.ops.cancel(id);
                }
                if let Some(id) = dismiss {
                    app_ops.active.retain(|o| o.id != id);
                }
            });
    }

    // Conflict prompt: the worker is blocked until it gets an answer, so this window keeps
    // Permanent deletion is the only thing no undo can reverse, so it is the only operation
    // that asks first. A real Modal, not a Window: the backdrop blocks the pointer, and
    // `modal_open` above blocks the keyboard, so the pane behind cannot act on the same keys.
    if let Some((what, mut delete_selected)) = app_ops.pending_delete.take() {
        // Read the keys BEFORE drawing, so a button cannot also consume them.
        let (left, right, activate, escape) = ctx.input(|i| {
            (
                i.key_pressed(egui::Key::ArrowLeft) || i.key_pressed(egui::Key::Tab),
                i.key_pressed(egui::Key::ArrowRight),
                i.key_pressed(egui::Key::Enter) || i.key_pressed(egui::Key::Space),
                i.key_pressed(egui::Key::Escape),
            )
        });
        if left || right {
            delete_selected = !delete_selected;
        }

        let mut decision: Option<bool> = None;
        if escape {
            decision = Some(false);
        } else if activate {
            decision = Some(delete_selected);
        }

        egui::Modal::new(egui::Id::new("confirm-delete")).show(ctx, |ui| {
            let (heading, question, verb) = match &what {
                Confirm::Delete(paths) => {
                    let target = match paths.len() {
                        1 => paths[0]
                            .file_name()
                            .map(|n| format!("'{}'", n.to_string_lossy()))
                            .unwrap_or_else(|| "1 item".into()),
                        n => format!("{n} items"),
                    };
                    ("Delete permanently?", format!("Permanently delete {target}?"), "Delete")
                }
                Confirm::EmptyTrash => (
                    "Empty the trash?",
                    "Permanently delete everything in the trash, on every drive?".to_string(),
                    "Empty",
                ),
            };
            ui.heading(heading);
            ui.add_space(4.0);
            ui.label(question);
            ui.label(egui::RichText::new("This cannot be undone.").color(egui::Color32::LIGHT_RED));
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                // The keyboard selection is drawn, not just held: a dialog you can drive with
                // Enter has to show what Enter would do.
                let button = |ui: &mut egui::Ui, text: egui::RichText, selected: bool| {
                    let btn = egui::Button::new(text);
                    let btn = if selected {
                        btn.fill(theme.accent.gamma_multiply(0.35))
                            .stroke(egui::Stroke::new(2.0, theme.accent))
                    } else {
                        btn.stroke(egui::Stroke::new(1.0, theme.text.gamma_multiply(0.3)))
                    };
                    ui.add(btn).clicked()
                };
                if button(
                    ui,
                    egui::RichText::new(verb).color(egui::Color32::LIGHT_RED),
                    delete_selected,
                ) {
                    decision = Some(true);
                }
                if button(ui, egui::RichText::new("Cancel"), !delete_selected) {
                    decision = Some(false);
                }
                ui.add_space(8.0);
                ui.label(
                    egui::RichText::new("←/→ choose · Enter confirm · Esc cancel")
                        .small()
                        .color(theme.text.gamma_multiply(0.5)),
                );
            });
        });

        match decision {
            Some(true) => match what {
                Confirm::Delete(paths) => {
                    app_ops.ops.submit(fileops::Request::new(fileops::OpKind::Delete, paths, None));
                }
                Confirm::EmptyTrash => {
                    app_ops.ops.submit(fileops::Request::new(fileops::OpKind::EmptyTrash, Vec::new(), None));
                }
            },
            Some(false) => {}
            // No answer yet: put it back, carrying the keyboard selection to the next frame.
            None => app_ops.pending_delete = Some((what, delete_selected)),
        }
    }

    // appearing until one of the buttons is pressed.
    let mut answer: Option<fileops::ConflictAnswer> = None;
    if let Some((_, name, folders, _)) = &app_ops.pending_conflict {
        let (name, folders) = (name.clone(), *folders);
        egui::Window::new("File exists")
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                ui.label(format!("'{name}' already exists in the destination."));
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if folders
                        && ui
                            .button("Merge")
                            .on_hover_text("Put the contents in the folder that is there; a file in both is asked about")
                            .clicked()
                    {
                        answer = Some(fileops::ConflictAnswer::Merge);
                    }
                    if ui
                        .button(if folders { "Replace" } else { "Overwrite" })
                        .on_hover_text("What is there goes to the trash first")
                        .clicked()
                    {
                        answer = Some(fileops::ConflictAnswer::Overwrite);
                    }
                    if ui.button("Skip").clicked() {
                        answer = Some(fileops::ConflictAnswer::Skip);
                    }
                    if ui
                        .button("Keep both")
                        .on_hover_text("The new one is written as 'name (1)'; the existing one is untouched")
                        .clicked()
                    {
                        answer = Some(fileops::ConflictAnswer::Rename);
                    }
                    ui.separator();
                    if folders && ui.button("Merge all").on_hover_text("Every folder that is there already").clicked() {
                        answer = Some(fileops::ConflictAnswer::MergeAll);
                    }
                    if ui.button(if folders { "Replace all" } else { "Overwrite all" }).clicked() {
                        answer = Some(fileops::ConflictAnswer::OverwriteAll);
                    }
                    if ui.button("Skip all").clicked() {
                        answer = Some(fileops::ConflictAnswer::SkipAll);
                    }
                    if ui.button("Keep both, all").clicked() {
                        answer = Some(fileops::ConflictAnswer::RenameAll);
                    }
                    ui.separator();
                    if ui
                        .button(egui::RichText::new("Cancel").color(egui::Color32::LIGHT_RED))
                        .clicked()
                    {
                        answer = Some(fileops::ConflictAnswer::Cancel);
                    }
                });
            });
    }
    if let Some(a) = answer {
        if let Some((.., reply)) = app_ops.pending_conflict.take() {
            let _ = reply.send(a);
        }
    }

    // One item of a copy or move failed; the rest wait for the answer.
    let mut answer: Option<fileops::ErrorAnswer> = None;
    if let Some((id, name, error, _)) = &app_ops.pending_error {
        let moving = app_ops.active.iter().any(|o| o.id == *id && o.kind == fileops::OpKind::Move);
        egui::Window::new(if moving { "Could not move" } else { "Could not copy" })
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                ui.label(format!("'{name}'"));
                ui.colored_label(egui::Color32::LIGHT_RED, error);
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button("Try again").clicked() {
                        answer = Some(fileops::ErrorAnswer::Retry);
                    }
                    if ui.button("Skip").clicked() {
                        answer = Some(fileops::ErrorAnswer::Skip);
                    }
                    if ui.button("Skip all").on_hover_text("Leave out every item that fails from here on").clicked() {
                        answer = Some(fileops::ErrorAnswer::SkipAll);
                    }
                    ui.separator();
                    if ui
                        .button(egui::RichText::new("Stop").color(egui::Color32::LIGHT_RED))
                        .on_hover_text("What is done so far stays, and undo can reverse it")
                        .clicked()
                    {
                        answer = Some(fileops::ErrorAnswer::Cancel);
                    }
                });
            });
    }
    if let Some(a) = answer {
        if let Some((.., reply)) = app_ops.pending_error.take() {
            let _ = reply.send(a);
        }
    }

    if let Some(mut ow) = app_ops.open_with.take() {
        let OpenWithState { paths, search, apps, relevant_only, major, mime, make_default } = &mut ow;
        let mut keep = true;
        let mut launched = false;
        egui::Window::new("Open with...")
            .collapsible(false)
            .resizable(true)
            .open(&mut keep)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label("Search:");
                    let resp = ui.text_edit_singleline(search);
                    resp.request_focus();
                });
                if let Some(m) = &major {
                    let label = match m.as_str() {
                        "image" => "Show only image applications",
                        "video" => "Show only video applications",
                        "audio" => "Show only audio applications",
                        "text" => "Show only text editors",
                        _ => "Show only related applications",
                    };
                    ui.checkbox(relevant_only, label);
                }
                // Writes mimeapps.list via xdg-mime, so a plain double-click (xdg-open) follows the
                // choice from now on, here and everywhere else. By type, which is what the system
                // goes by, so the type is named: the choice holds for every file of it.
                if mime.is_empty() {
                    *make_default = false;
                    ui.add_enabled(false, egui::Checkbox::new(make_default, "Set as default (the files are of different types)"));
                } else if mime.as_str() == apps::UNKNOWN {
                    *make_default = false;
                    ui.add_enabled(false, egui::Checkbox::new(make_default, "Set as default (the file's type is unknown)"));
                } else {
                    ui.checkbox(make_default, format!("Set as default for this file type ({mime})"));
                }
                ui.separator();
                egui::ScrollArea::vertical()
                    .max_height(400.0)
                    .show(ui, |ui| {
                        let q = search.to_lowercase();
                        for app in apps.iter() {
                            if !q.is_empty() && !app.name.to_lowercase().contains(&q) {
                                continue;
                            }
                            if *relevant_only {
                                if let Some(m) = major.as_ref() {
                                    if !app.mimes.iter().any(|mime| mime.starts_with(m)) {
                                        continue;
                                    }
                                }
                            }
                            if ui.button(&app.name).clicked() {
                                if *make_default {
                                    apps::set_default(app, &paths[0]);
                                }
                                for p in paths.iter() {
                                    model::log_recent(p);
                                }
                                apps::launch_many(app, paths);
                                launched = true;
                            }
                        }
                    });
                ui.separator();
                ui.horizontal(|ui| {
                    if ui.button("Cancel").clicked() {
                        launched = true; // reusing to close dialog
                    }
                });
            });
        if keep && !launched {
            app_ops.open_with = Some(ow);
        }
    }
}

fn args_probe(args: &[String], flag: &str) -> Option<usize> {
    args.iter().position(|a| a == flag)
}

fn wayland_handles(window: &Window) -> Result<(*mut std::ffi::c_void, *mut std::ffi::c_void)> {
    let display = match window.display_handle()?.as_raw() {
        RawDisplayHandle::Wayland(d) => d.display.as_ptr(),
        other => return Err(anyhow!("vantaexplorer requires Wayland, got {other:?}")),
    };
    let surface = match window.window_handle()?.as_raw() {
        RawWindowHandle::Wayland(w) => w.surface.as_ptr(),
        other => return Err(anyhow!("vantaexplorer requires a Wayland surface, got {other:?}")),
    };
    Ok((display, surface))
}
