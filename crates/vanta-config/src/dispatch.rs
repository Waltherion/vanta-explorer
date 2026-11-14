//! Hyprland dispatches in whichever language the running compositor speaks.
//!
//! Since Hyprland 0.56 a Lua config (`hyprland.lua`) makes `hyprctl dispatch` evaluate Lua
//! (`hl.dsp.focus({ monitor = 'DP-1' })`), while a classic `hyprland.conf` keeps the old
//! dispatcher names (`focusmonitor DP-1`). Neither understands the other, and `hyprctl` still
//! exits 0 on a rejected command, so the dialect is asked for once: `hyprctl eval` answers `ok`
//! only under a Lua config.

use std::process::Command;
use std::sync::OnceLock;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dialect {
    Lua,
    Classic,
}

/// The dialect of the running Hyprland, asked until it gives a clear answer and then
/// remembered: `ok` is a Lua config; a classic one says eval is Lua-only, and a Hyprland older
/// than the Lua config does not know the request at all. Anything else (hyprctl missing, the
/// compositor not answering yet) is taken as classic for now and asked again next time.
pub fn dialect() -> Dialect {
    static D: OnceLock<Dialect> = OnceLock::new();
    if let Some(d) = D.get() {
        return *d;
    }
    let reply = Command::new("hyprctl").args(["eval", "return 1"]).output().map(|o| String::from_utf8_lossy(&o.stdout).trim().to_lowercase());
    match reply.as_deref() {
        Ok("ok") => *D.get_or_init(|| Dialect::Lua),
        Ok(r) if r.contains("lua config") || r.contains("unknown request") => *D.get_or_init(|| Dialect::Classic),
        _ => Dialect::Classic,
    }
}

/// One thing to ask Hyprland to do. Workspaces are given as Hyprland names them: a number,
/// or `special:<name>`.
#[derive(Clone, Debug, PartialEq)]
pub enum Dispatch {
    FocusMonitor(String),
    FocusWorkspace(String),
    /// A window by its address, without the `0x`-prefix convention changing: pass what
    /// `hyprctl clients -j` reports.
    FocusWindow(String),
    /// Move a window to a workspace without following it.
    MoveWindow { window: String, workspace: String },
    CloseWindow(String),
    MoveCursor(i64, i64),
    /// Show a special workspace (scratchpad) by name on the focused monitor, or hide it there.
    ToggleSpecial(String),
    Exit,
}

impl Dispatch {
    pub fn lua(&self) -> String {
        match self {
            Dispatch::FocusMonitor(m) => format!("hl.dsp.focus({{ monitor = '{m}' }})"),
            Dispatch::FocusWorkspace(w) => format!("hl.dsp.focus({{ workspace = '{w}' }})"),
            Dispatch::FocusWindow(a) => format!("hl.dsp.focus({{ window = 'address:{a}' }})"),
            Dispatch::MoveWindow { window, workspace } => {
                format!("hl.dsp.window.move({{ workspace = '{workspace}', follow = false, window = 'address:{window}' }})")
            }
            Dispatch::CloseWindow(a) => format!("hl.dsp.window.close({{ window = 'address:{a}' }})"),
            Dispatch::MoveCursor(x, y) => format!("hl.dsp.cursor.move({{ x = {x}, y = {y} }})"),
            Dispatch::ToggleSpecial(n) => format!("hl.dsp.workspace.toggle_special('{n}')"),
            Dispatch::Exit => "hl.dsp.exit()".into(),
        }
    }

    pub fn classic(&self) -> String {
        match self {
            Dispatch::FocusMonitor(m) => format!("focusmonitor {m}"),
            Dispatch::FocusWorkspace(w) => format!("workspace {w}"),
            Dispatch::FocusWindow(a) => format!("focuswindow address:{a}"),
            Dispatch::MoveWindow { window, workspace } => format!("movetoworkspacesilent {workspace},address:{window}"),
            Dispatch::CloseWindow(a) => format!("closewindow address:{a}"),
            Dispatch::MoveCursor(x, y) => format!("movecursor {x} {y}"),
            Dispatch::ToggleSpecial(n) => format!("togglespecialworkspace {n}"),
            Dispatch::Exit => "exit".into(),
        }
    }

    pub fn text(&self, dialect: Dialect) -> String {
        match dialect {
            Dialect::Lua => self.lua(),
            Dialect::Classic => self.classic(),
        }
    }
}

/// Carry a dispatch out and wait for Hyprland's answer, which is also returned. Anything but `ok`
/// is written to stderr under `app`, since the command itself reports success either way.
pub fn run(app: &str, d: &Dispatch) -> Result<(), String> {
    let text = d.text(dialect());
    // Straight to the compositor's socket, as `hyprctl` itself would send it, which spares a
    // process per dispatch (the organizer's move dance is several in a row). `hyprctl` only
    // when the socket cannot be reached.
    if let Some(reply) = crate::hypr::request(&format!("dispatch {text}"), std::time::Duration::from_secs(2)) {
        let reply = String::from_utf8_lossy(&reply).trim().to_string();
        if reply == "ok" || reply.is_empty() {
            return Ok(());
        }
        eprintln!("{app}: dispatch {text}: {reply}");
        return Err(reply);
    }
    // The classic form is one argument per word; the Lua form is a single expression.
    let mut cmd = Command::new("hyprctl");
    cmd.arg("dispatch");
    match dialect() {
        Dialect::Lua => {
            cmd.arg(&text);
        }
        Dialect::Classic => {
            cmd.args(text.split(' '));
        }
    }
    let reply = match cmd.output() {
        Ok(out) => String::from_utf8_lossy(&out.stdout).trim().to_string(),
        Err(e) => format!("could not run hyprctl: {e}"),
    };
    if reply == "ok" || reply.is_empty() {
        return Ok(());
    }
    eprintln!("{app}: hyprctl dispatch {text}: {reply}");
    Err(reply)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_dialects_say_the_same_thing() {
        let mv = Dispatch::MoveWindow { window: "0x55aa".into(), workspace: "special:magic".into() };
        assert_eq!(mv.lua(), "hl.dsp.window.move({ workspace = 'special:magic', follow = false, window = 'address:0x55aa' })");
        assert_eq!(mv.classic(), "movetoworkspacesilent special:magic,address:0x55aa");
        let sp = Dispatch::ToggleSpecial("mail".into());
        assert_eq!((sp.lua(), sp.classic()), ("hl.dsp.workspace.toggle_special('mail')".to_string(), "togglespecialworkspace mail".to_string()));
        assert_eq!(Dispatch::FocusWorkspace("3".into()).classic(), "workspace 3");
        assert_eq!(Dispatch::FocusMonitor("DP-1".into()).lua(), "hl.dsp.focus({ monitor = 'DP-1' })");
        assert_eq!(Dispatch::MoveCursor(10, -4).classic(), "movecursor 10 -4");
        assert_eq!(Dispatch::Exit.text(Dialect::Lua), "hl.dsp.exit()");
    }

    /// Against the running compositor: focuses what already has focus, so nothing moves.
    #[test]
    #[ignore = "talks to the running Hyprland"]
    fn the_running_hyprland_takes_it() {
        let out = Command::new("hyprctl").args(["monitors", "-j"]).output().unwrap();
        let mons: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
        let m = mons.as_array().unwrap().iter().find(|m| m["focused"] == true).unwrap();
        let (name, ws) = (m["name"].as_str().unwrap().to_string(), m["activeWorkspace"]["id"].as_i64().unwrap());
        assert_eq!(run("test", &Dispatch::FocusMonitor(name)), Ok(()));
        assert_eq!(run("test", &Dispatch::FocusWorkspace(ws.to_string())), Ok(()));
        let bogus = run("test", &Dispatch::FocusWindow("0xdeadbeef".into()));
        eprintln!("dialect {:?}; a window that does not exist: {bogus:?}", dialect());
    }
}
