//! Vanta Explorer as the file chooser other programs get.
//!
//! A program that asks the desktop for a file ("open", "save as", "choose a folder") asks
//! xdg-desktop-portal, which hands the question to a backend: this. It draws nothing itself. Each
//! question starts `vantaexplorer --choose` with the question as JSON on its standard input, and
//! the answer it prints goes back to the program. So the chooser is the file manager, with its
//! places, theme, thumbnails and preview, and this is only the part that speaks D-Bus.
//!
//! Started by D-Bus when first asked (`org.freedesktop.impl.portal.desktop.vanta.service`) and
//! chosen in `~/.config/xdg-desktop-portal/portals.conf`:
//!
//!     [preferred]
//!     org.freedesktop.impl.portal.FileChooser=vanta;gtk

use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};

use serde_json::json;
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};

const NAME: &str = "org.freedesktop.impl.portal.desktop.vanta";
const PATH: &str = "/org/freedesktop/portal/desktop";

/// The portal's three answers.
const CHOSEN: u32 = 0;
const CANCELLED: u32 = 1;
const FAILED: u32 = 2;

type Options = HashMap<String, OwnedValue>;
/// A filter as the portal writes it: a name, and patterns each marked 0 (glob) or 1 (type).
type PortalFilter = (String, Vec<(u32, String)>);
/// The choosers open right now, by the request they answer, so one can be taken back.
type Open = Arc<Mutex<HashMap<String, u32>>>;

fn log(msg: impl AsRef<str>) {
    eprintln!("vantaportal: {}", msg.as_ref());
}

struct FileChooser {
    open: Open,
}

/// The object the portal calls to take a request back, at the request's own path.
struct Request {
    handle: String,
    open: Open,
}

#[zbus::interface(name = "org.freedesktop.impl.portal.Request")]
impl Request {
    /// The program no longer wants an answer (its window closed, say): the chooser goes too.
    fn close(&self) {
        if let Some(pid) = self.open.lock().unwrap().remove(&self.handle) {
            unsafe { libc::kill(pid as i32, libc::SIGTERM) };
        }
    }
}

#[zbus::interface(name = "org.freedesktop.impl.portal.FileChooser")]
impl FileChooser {
    async fn open_file(
        &self,
        #[zbus(object_server)] server: &zbus::ObjectServer,
        handle: OwnedObjectPath,
        app_id: String,
        parent_window: String,
        title: String,
        options: Options,
    ) -> (u32, Options) {
        let mode = match (flag(&options, "directory"), flag(&options, "multiple")) {
            (true, _) => "folder",
            (false, true) => "open-many",
            (false, false) => "open",
        };
        self.ask(server, handle, question(mode, &app_id, &parent_window, &title, &options), &options).await
    }

    async fn save_file(
        &self,
        #[zbus(object_server)] server: &zbus::ObjectServer,
        handle: OwnedObjectPath,
        app_id: String,
        parent_window: String,
        title: String,
        options: Options,
    ) -> (u32, Options) {
        self.ask(server, handle, question("save", &app_id, &parent_window, &title, &options), &options).await
    }

    async fn save_files(
        &self,
        #[zbus(object_server)] server: &zbus::ObjectServer,
        handle: OwnedObjectPath,
        app_id: String,
        parent_window: String,
        title: String,
        options: Options,
    ) -> (u32, Options) {
        self.ask(server, handle, question("save-many", &app_id, &parent_window, &title, &options), &options).await
    }
}

impl FileChooser {
    /// Shows the chooser for one question and waits for its answer, with the request's own
    /// object up for as long as that takes.
    async fn ask(&self, server: &zbus::ObjectServer, handle: OwnedObjectPath, question: serde_json::Value, options: &Options) -> (u32, Options) {
        let key = handle.to_string();
        let mut child = match Command::new(explorer()).arg("--choose").stdin(Stdio::piped()).stdout(Stdio::piped()).spawn() {
            Ok(c) => c,
            Err(e) => {
                log(format!("could not start the chooser ({}): {e}", explorer().display()));
                return (FAILED, Options::new());
            }
        };
        // The whole question, then the end of input, which is what the chooser reads up to.
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(question.to_string().as_bytes());
        }
        self.open.lock().unwrap().insert(key.clone(), child.id());
        let _ = server.at(handle.clone(), Request { handle: key.clone(), open: self.open.clone() }).await;

        let output = blocking::unblock(move || child.wait_with_output()).await;

        let _ = server.remove::<Request, _>(handle).await;
        self.open.lock().unwrap().remove(&key);
        match output {
            Ok(out) if out.status.success() => answer(&String::from_utf8_lossy(&out.stdout), options),
            Ok(out) if out.status.code() == Some(1) => (CANCELLED, Options::new()),
            // Killed by `Close`, or it fell over: either way there is no choice to report.
            Ok(out) => {
                log(format!("the chooser ended with {}", out.status));
                (if out.status.code().is_none() { CANCELLED } else { FAILED }, Options::new())
            }
            Err(e) => {
                log(format!("waiting for the chooser: {e}"));
                (FAILED, Options::new())
            }
        }
    }
}

/// `vantaexplorer` beside this program when it is there (a build tree, `~/.local/bin`), else
/// whichever the path finds.
fn explorer() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("vantaexplorer")))
        .filter(|p| p.is_file())
        .unwrap_or_else(|| PathBuf::from("vantaexplorer"))
}

fn get<T: TryFrom<OwnedValue>>(options: &Options, key: &str) -> Option<T> {
    options.get(key).and_then(|v| v.try_clone().ok()).and_then(|v| T::try_from(v).ok())
}

fn flag(options: &Options, key: &str) -> bool {
    get::<bool>(options, key).unwrap_or(false)
}

/// A path the portal sends as bytes, which it ends with a zero.
fn path_of(bytes: Vec<u8>) -> Option<PathBuf> {
    use std::os::unix::ffi::OsStringExt;
    let end = bytes.iter().position(|b| *b == 0).unwrap_or(bytes.len());
    (end > 0).then(|| PathBuf::from(std::ffi::OsString::from_vec(bytes[..end].to_vec())))
}

fn filter_json((name, patterns): &PortalFilter) -> serde_json::Value {
    let of = |kind: u32| patterns.iter().filter(|(k, _)| *k == kind).map(|(_, p)| p.clone()).collect::<Vec<_>>();
    json!({ "name": name, "globs": of(0), "mimes": of(1) })
}

/// The portal's question as the chooser reads it (see `chooser::Request` in Vanta Explorer).
fn question(mode: &str, app_id: &str, parent_window: &str, title: &str, options: &Options) -> serde_json::Value {
    let filters: Vec<PortalFilter> = get(options, "filters").unwrap_or_default();
    let current: Option<PortalFilter> = get(options, "current_filter");
    let mut folder = get::<Vec<u8>>(options, "current_folder").and_then(path_of);
    let mut name: String = get(options, "current_name").unwrap_or_default();
    // A save that names the file it is saving over starts on that file.
    if let Some(file) = get::<Vec<u8>>(options, "current_file").and_then(path_of) {
        if name.is_empty() {
            name = file.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        }
        folder = folder.or_else(|| file.parent().map(Path::to_path_buf));
    }
    let files: Vec<String> = get::<Vec<Vec<u8>>>(options, "files")
        .unwrap_or_default()
        .into_iter()
        .filter_map(path_of)
        .map(|p| p.to_string_lossy().into_owned())
        .collect();
    json!({
        "mode": mode,
        "title": title,
        "accept_label": get::<String>(options, "accept_label").unwrap_or_default(),
        "folder": folder.map(|p| p.to_string_lossy().into_owned()),
        "name": name,
        "filters": filters.iter().map(filter_json).collect::<Vec<_>>(),
        "filter": current.and_then(|c| filters.iter().position(|f| *f == c)),
        "files": files,
        "parent": parent_window,
        "app": app_id,
    })
}

/// The chooser's printed answer as the portal wants it: the addresses chosen, and the filter
/// that was in use when it has one.
fn answer(printed: &str, options: &Options) -> (u32, Options) {
    let Ok(said) = serde_json::from_str::<serde_json::Value>(printed.trim()) else {
        log(format!("the chooser's answer could not be read: {printed:?}"));
        return (FAILED, Options::new());
    };
    let uris: Vec<String> = said["uris"].as_array().into_iter().flatten().filter_map(|u| u.as_str().map(str::to_string)).collect();
    if uris.is_empty() {
        return (CANCELLED, Options::new());
    }
    let mut results = Options::new();
    if let Ok(v) = OwnedValue::try_from(Value::from(uris)) {
        results.insert("uris".into(), v);
    }
    let filters: Vec<PortalFilter> = get(options, "filters").unwrap_or_default();
    if let Some(filter) = said["filter"].as_u64().and_then(|i| filters.get(i as usize)) {
        if let Ok(v) = OwnedValue::try_from(Value::from(filter.clone())) {
            results.insert("current_filter".into(), v);
        }
    }
    (CHOSEN, results)
}

/// "Show in folder" from other programs: a browser's downloads, a chat program's files. They ask
/// `org.freedesktop.FileManager1`, and with `--file-manager` (started for that name by its own
/// D-Bus entry, which is opt-in: Dolphin and Nautilus answer to the same name) this answers by
/// opening Vanta Explorer there.
struct FileManager;

#[zbus::interface(name = "org.freedesktop.FileManager1")]
impl FileManager {
    /// Each folder in a window of its own.
    fn show_folders(&self, uris: Vec<String>, _startup_id: String) {
        for path in uris.iter().filter_map(|u| local_path(u)) {
            open_explorer(&path);
        }
    }

    /// The folder each item is in, with the item selected: one window per folder.
    fn show_items(&self, uris: Vec<String>, _startup_id: String) {
        let mut seen: Vec<PathBuf> = Vec::new();
        for path in uris.iter().filter_map(|u| local_path(u)) {
            let parent = path.parent().map(Path::to_path_buf).unwrap_or_default();
            if !seen.contains(&parent) {
                seen.push(parent);
                open_explorer(&path);
            }
        }
    }

    /// No properties window yet: the item is shown instead, which is what the asker can use.
    fn show_item_properties(&self, uris: Vec<String>, startup_id: String) {
        self.show_items(uris, startup_id);
    }
}

/// Vanta Explorer on `path`: a folder opens, a file opens its folder with it selected.
fn open_explorer(path: &Path) {
    match Command::new(explorer()).arg(path).stdin(Stdio::null()).spawn() {
        // Waited for on a thread of its own, so it does not linger as a zombie.
        Ok(mut child) => {
            std::thread::spawn(move || child.wait());
        }
        Err(e) => log(format!("could not start Vanta Explorer: {e}")),
    }
}

/// The path of a `file://` URI (percent-decoded, any host part dropped), or a plain path as is.
fn local_path(uri: &str) -> Option<PathBuf> {
    use std::os::unix::ffi::OsStringExt;
    let rest = match uri.strip_prefix("file://") {
        Some(rest) => &rest[rest.find('/')?..],
        None if uri.starts_with('/') => uri,
        None => return None,
    };
    let bytes = rest.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = |b: u8| (b as char).to_digit(16);
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(a), Some(b)) = (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                out.push((a * 16 + b) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    Some(PathBuf::from(std::ffi::OsString::from_vec(out)))
}

fn main() {
    if std::env::args().any(|a| a == "--file-manager") {
        let built = zbus::blocking::connection::Builder::session()
            .and_then(|b| b.name("org.freedesktop.FileManager1"))
            .and_then(|b| b.serve_at("/org/freedesktop/FileManager1", FileManager))
            .and_then(|b| b.build());
        match built {
            Ok(_conn) => {
                log("answering org.freedesktop.FileManager1");
                loop {
                    std::thread::park();
                }
            }
            Err(e) => {
                log(format!("could not take org.freedesktop.FileManager1 (another file manager has it?): {e}"));
                std::process::exit(1);
            }
        }
    }
    let open: Open = Arc::default();
    let built = zbus::blocking::connection::Builder::session()
        .and_then(|b| b.name(NAME))
        .and_then(|b| b.serve_at(PATH, FileChooser { open }))
        .and_then(|b| b.build());
    match built {
        Ok(_conn) => {
            log("ready");
            // Everything happens on the connection's own thread from here.
            loop {
                std::thread::park();
            }
        }
        Err(e) => {
            log(format!("could not take {NAME} on the session bus: {e}"));
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn owned<'a>(v: impl Into<Value<'a>>) -> OwnedValue {
        OwnedValue::try_from(v.into()).unwrap()
    }

    #[test]
    fn show_in_folder_takes_file_uris() {
        assert_eq!(local_path("file:///home/u/My%20Files/a%C3%A6.txt"), Some(PathBuf::from("/home/u/My Files/aæ.txt")));
        assert_eq!(local_path("file://localhost/tmp/x"), Some(PathBuf::from("/tmp/x")));
        assert_eq!(local_path("/tmp/plain"), Some(PathBuf::from("/tmp/plain")));
        assert_eq!(local_path("file:///tmp/100%"), Some(PathBuf::from("/tmp/100%")));
        assert_eq!(local_path("https://example.com/a"), None);
    }

    #[test]
    fn a_question_carries_everything_the_program_said() {
        let images: PortalFilter = ("Images".into(), vec![(0, "*.png".into()), (1, "image/jpeg".into())]);
        let all: PortalFilter = ("All".into(), vec![(0, "*".into())]);
        let mut o = Options::new();
        o.insert("accept_label".into(), owned("_Upload"));
        o.insert("filters".into(), owned(vec![images.clone(), all.clone()]));
        o.insert("current_filter".into(), owned(all));
        o.insert("current_folder".into(), owned(b"/home/u/Pictures\0".to_vec()));
        o.insert("current_name".into(), owned("a.png"));
        let q = question("save", "org.example.App", "wayland:abc", "Save image", &o);
        assert_eq!(q["mode"], "save");
        assert_eq!(q["folder"], "/home/u/Pictures", "the zero the portal ends a path with is not part of it");
        assert_eq!(q["name"], "a.png");
        assert_eq!(q["accept_label"], "_Upload");
        assert_eq!(q["filters"][0], json!({ "name": "Images", "globs": ["*.png"], "mimes": ["image/jpeg"] }));
        assert_eq!(q["filter"], 1);
        assert_eq!((q["app"].as_str(), q["parent"].as_str()), (Some("org.example.App"), Some("wayland:abc")));
    }

    #[test]
    fn a_save_over_a_named_file_starts_on_it() {
        let mut o = Options::new();
        o.insert("current_file".into(), owned(b"/home/u/notes/todo.md\0".to_vec()));
        let q = question("save", "", "", "", &o);
        assert_eq!((q["folder"].as_str(), q["name"].as_str()), (Some("/home/u/notes"), Some("todo.md")));
        assert_eq!(q["filter"], serde_json::Value::Null);
    }

    #[test]
    fn the_names_to_save_are_passed_on() {
        let mut o = Options::new();
        o.insert("files".into(), owned(vec![b"a.txt\0".to_vec(), b"b.txt\0".to_vec()]));
        assert_eq!(question("save-many", "", "", "", &o)["files"], json!(["a.txt", "b.txt"]));
    }

    #[test]
    fn an_answer_goes_back_as_addresses_and_the_filter_used() {
        let images: PortalFilter = ("Images".into(), vec![(0, "*.png".into())]);
        let mut o = Options::new();
        o.insert("filters".into(), owned(vec![images.clone()]));
        let (code, results) = answer(r#"{"uris":["file:///a%20b.png"],"filter":0}"#, &o);
        assert_eq!(code, CHOSEN);
        assert_eq!(get::<Vec<String>>(&results, "uris"), Some(vec!["file:///a%20b.png".to_string()]));
        assert_eq!(get::<PortalFilter>(&results, "current_filter"), Some(images));
        assert_eq!(answer("", &o).0, FAILED, "no answer at all is a failure, not a choice");
        assert_eq!(answer(r#"{"uris":[]}"#, &o).0, CANCELLED);
    }
}
