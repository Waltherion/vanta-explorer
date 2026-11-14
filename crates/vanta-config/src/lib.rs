//! Config loading, theme colours, XDG paths and hyprctl queries shared by the vanta apps.

/// JSONC (`//` and `/* */` comments, trailing commas) parsed through json5, which accepts a
/// superset of that. Returns a `serde_json::Value` so callers can pick fields with defaults.
pub mod ctl;
pub mod dispatch;
pub mod layers;
pub mod palette;
pub mod sort;
pub mod theme;
pub mod watch;

pub mod jsonc {
    use anyhow::{Context, Result};

    pub fn parse(text: &str) -> Result<serde_json::Value> {
        json5::from_str::<serde_json::Value>(text).context("JSONC parse error")
    }

    /// A settings file with every commented-out option turned back on. The files the apps write
    /// on first run hold each option commented out with its built-in default, so nothing is set
    /// until somebody takes the `//` away; this is that file with all of them taken away, which
    /// is how the apps' tests check that the defaults a file shows are the ones the app has.
    ///
    /// Only the object is touched, not the comment above it. A commented option is `// "key":`
    /// followed by a value that ends the line (or a comment does), or a `}` or `]` closing one; a
    /// comment inside a commented block (`//   // text`) loses one level. Prose that happens to
    /// start with a quoted word ("hwdec" decodes on the GPU) is left as it is.
    pub fn uncommented(text: &str) -> String {
        let mut out = Vec::new();
        let mut body = false;
        for line in text.lines() {
            if !body {
                body = line.starts_with('{');
                out.push(line.to_string());
                continue;
            }
            let indent = &line[..line.len() - line.trim_start().len()];
            let Some(after) = line.trim_start().strip_prefix("//") else {
                out.push(line.to_string());
                continue;
            };
            let after = after.strip_prefix(' ').unwrap_or(after);
            let a = after.trim_start();
            if a.starts_with("//") || is_code(a) {
                out.push(format!("{indent}{after}"));
            } else {
                out.push(line.to_string());
            }
        }
        out.join("\n") + "\n"
    }

    /// Takes `a.b.c` out of a parsed file, for a test that turns every commented option on and
    /// has to leave out the few that show an example rather than the default.
    pub fn remove(v: &mut serde_json::Value, path: &str) {
        let mut parts: Vec<&str> = path.split('.').collect();
        let last = parts.pop().unwrap_or_default();
        let mut cur = v;
        for p in parts {
            match cur.get_mut(p) {
                Some(next) => cur = next,
                None => return,
            }
        }
        if let Some(o) = cur.as_object_mut() {
            o.remove(last);
        }
    }

    /// The rest of a line ends here: nothing, or a comment, after an optional comma.
    fn ends(rest: &str) -> bool {
        let r = rest.trim_start();
        let r = r.strip_prefix(',').unwrap_or(r).trim_start();
        r.is_empty() || r.starts_with("//")
    }

    fn is_code(a: &str) -> bool {
        if let Some(r) = a.strip_prefix('}').or_else(|| a.strip_prefix(']')) {
            return ends(r);
        }
        let Some(r) = a.strip_prefix('"') else { return false };
        let Some(close) = r.find('"') else { return false };
        if close == 0 || !r[..close].chars().all(|c| c.is_ascii_alphanumeric()) {
            return false;
        }
        let Some(v) = r[close + 1..].trim_start().strip_prefix(':') else { return false };
        let v = v.trim_start();
        if v.starts_with('{') || v.starts_with('[') {
            return true;
        }
        if let Some(s) = v.strip_prefix('"') {
            let mut escaped = false;
            for (i, c) in s.char_indices() {
                match c {
                    '\\' if !escaped => escaped = true,
                    '"' if !escaped => return ends(&s[i + 1..]),
                    _ => escaped = false,
                }
            }
            return false;
        }
        let n = v.find(|c: char| !(c.is_ascii_alphanumeric() || "-_.#".contains(c))).unwrap_or(v.len());
        n > 0 && ends(&v[n..])
    }

    #[cfg(test)]
    mod tests {
        #[test]
        fn commented_options_come_back_and_prose_stays() {
            let text = "// \"header\": 1,\n{\n  // The glow.\n  // \"bloom\": {\n  //   // How strong.\n  //   \"strength\": 1, // 0 to 1\n  // },\n  // \"hwdec\": \"auto\" decodes on the GPU\n  \"on\": true,\n}\n";
            let out = super::uncommented(text);
            assert!(out.starts_with("// \"header\": 1,"), "the comment above the object is left alone");
            assert!(out.contains("\n  \"bloom\": {\n    // How strong.\n    \"strength\": 1, // 0 to 1\n  },"), "{out}");
            assert!(out.contains("  // \"hwdec\": \"auto\" decodes"), "prose stays a comment");
            let v = super::parse(&out).unwrap();
            assert_eq!(v["bloom"]["strength"], 1);
            assert_eq!(v["on"], true);
        }
    }
}

/// Field access with defaults on a `serde_json::Value` object.
pub trait ValueExt {
    fn section(&self, name: &str) -> &serde_json::Value;
    fn f32_or(&self, key: &str, default: f32) -> f32;
    fn i32_or(&self, key: &str, default: i32) -> i32;
    fn bool_or(&self, key: &str, default: bool) -> bool;
    fn str_or(&self, key: &str, default: &str) -> String;
}

static NULL: serde_json::Value = serde_json::Value::Null;

impl ValueExt for serde_json::Value {
    fn section(&self, name: &str) -> &serde_json::Value {
        self.get(name).unwrap_or(&NULL)
    }
    fn f32_or(&self, key: &str, default: f32) -> f32 {
        self.get(key).and_then(|v| v.as_f64()).map_or(default, |v| v as f32)
    }
    fn i32_or(&self, key: &str, default: i32) -> i32 {
        self.get(key).and_then(|v| v.as_i64()).map_or(default, |v| v as i32)
    }
    fn bool_or(&self, key: &str, default: bool) -> bool {
        self.get(key).and_then(|v| v.as_bool()).unwrap_or(default)
    }
    fn str_or(&self, key: &str, default: &str) -> String {
        self.get(key).and_then(|v| v.as_str()).unwrap_or(default).to_string()
    }
}

pub mod paths {
    use std::path::PathBuf;

    pub fn home() -> PathBuf {
        PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/".into()))
    }
    pub fn config_home() -> PathBuf {
        std::env::var("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| home().join(".config"))
    }
    /// `$XDG_DATA_HOME`, or `~/.local/share`: where an app's own files live when they were not
    /// put there by a package.
    pub fn data_home() -> PathBuf {
        std::env::var("XDG_DATA_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| home().join(".local/share"))
    }
    pub fn state_home() -> PathBuf {
        std::env::var("XDG_STATE_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| home().join(".local/state"))
    }
    pub fn runtime_dir() -> PathBuf {
        std::env::var("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("/tmp"))
    }
    /// Expands a leading `~` and `$VAR` / `${VAR}` references in a path from a settings file.
    pub fn expand(p: &str) -> String {
        let home = home().to_string_lossy().into_owned();
        let p = if p == "~" {
            home.clone()
        } else if let Some(rest) = p.strip_prefix("~/") {
            format!("{home}/{rest}")
        } else {
            p.to_string()
        };
        let is_word = |c: char| c.is_alphanumeric() || c == '_';
        let mut out = String::new();
        let mut i = 0;
        while i < p.len() {
            let rest = &p[i..];
            if let Some(r) = rest.strip_prefix('$') {
                let (name, consumed) = if let Some(inner) = r.strip_prefix('{') {
                    match inner.find('}') {
                        Some(end) if end > 0 && inner[..end].chars().all(is_word) => (&inner[..end], end + 3),
                        _ => ("", 0),
                    }
                } else {
                    let n = r.chars().take_while(|c| is_word(*c)).map(char::len_utf8).sum::<usize>();
                    (&r[..n], n + 1)
                };
                if !name.is_empty() {
                    out.push_str(&std::env::var(name).unwrap_or_default());
                    i += consumed;
                    continue;
                }
            }
            let c = rest.chars().next().unwrap();
            out.push(c);
            i += c.len_utf8();
        }
        out
    }
    /// Where the wallpapers are when neither a setting nor a theme says: the ones a user put in
    /// their own data folder, else the ones the package installs, else the usual place for
    /// pictures. The first of these that exists wins.
    pub fn default_wallpapers() -> String {
        let candidates = [data_home().join("vanta/wallpapers"), PathBuf::from("/usr/share/vanta/wallpapers"), home().join("Pictures/wallpapers")];
        let pick = candidates.iter().find(|p| p.is_dir()).unwrap_or(&candidates[2]);
        pick.display().to_string()
    }
}

/// 8-bit RGBA colour as written in the config/theme files.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Color {
    pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Self {
        Color { r, g, b, a }
    }
    /// sRGB-encoded floats 0..1.
    pub fn to_f32(self) -> [f32; 4] {
        [self.r as f32 / 255.0, self.g as f32 / 255.0, self.b as f32 / 255.0, self.a as f32 / 255.0]
    }
    /// Accepts `RRGGBBAA`, `RRGGBB`, with optional `#`, or `rgba(RRGGBBAA)` / `rgb(RRGGBB)`.
    pub fn parse(text: &str) -> Option<Color> {
        let mut h = text.trim();
        if let Some(inner) = h.strip_prefix("rgba(").or_else(|| h.strip_prefix("rgb(")) {
            h = inner.trim_end_matches(')');
        }
        h = h.trim_start_matches('#');
        let owned;
        if h.len() == 6 {
            owned = format!("{h}ff");
            h = &owned;
        }
        if h.len() != 8 {
            return None;
        }
        let v = u32::from_str_radix(h, 16).ok()?;
        Some(Color::rgba((v >> 24) as u8, (v >> 16) as u8, (v >> 8) as u8, v as u8))
    }
}



pub mod hypr {
    use std::collections::HashMap;
    use std::process::Command;
    use std::time::Duration;

    #[derive(Clone, Copy, Debug, PartialEq)]
    pub struct Monitor {
        /// The output is in HDR: `colorManagementPreset` is `hdr` or `hdredid` (the same thing
        /// with the luminances taken from the screen's own EDID).
        pub hdr: bool,
        /// How Hyprland lifts ordinary SDR content on an HDR output. An app that draws in HDR
        /// itself is not lifted, so to sit beside everything else it has to do the same.
        pub sdr_brightness: f64,
        pub sdr_saturation: f64,
        pub sdr_max_luminance: f64,
        /// Configured scale (a good first guess before wp_fractional_scale reports).
        pub scale: f64,
        /// Where this monitor sits on the desktop, and how large it is, in LOGICAL pixels.
        /// Anything drawn across more than one screen needs this: without it each surface
        /// starts its own coordinates at zero, and a pattern spanning the desktop restarts at
        /// every boundary.
        pub x: f64,
        pub y: f64,
        pub w: f64,
        pub h: f64,
    }

    /// `hyprctl monitors -j` -> connector name -> HDR + scale. Bounded to 2 s (a stuck
    /// compositor must not stall a lock screen); failure means "everything SDR, scale 1", always
    /// safe.
    pub fn monitors() -> HashMap<String, Monitor> {
        let mut map = HashMap::new();
        let Some(stdout) = request("j/monitors", Duration::from_secs(2)).or_else(|| run_bounded(&["monitors", "-j"], Duration::from_secs(2))) else {
            return map;
        };
        let Ok(json) = serde_json::from_slice::<serde_json::Value>(&stdout) else { return map };
        for m in json.as_array().into_iter().flatten() {
            let name = m.get("name").and_then(|v| v.as_str()).unwrap_or_default().to_string();
            let hdr = m.get("colorManagementPreset").and_then(|v| v.as_str()).is_some_and(|p| p.starts_with("hdr"));
            let scale = m.get("scale").and_then(|v| v.as_f64()).filter(|s| *s > 0.0).unwrap_or(1.0);
            let num = |k: &str| m.get(k).and_then(|v| v.as_f64()).unwrap_or(0.0);
            // Hyprland reports position in logical pixels but size in physical ones.
            let (w, h) = (num("width") / scale, num("height") / scale);
            let or = |k: &str, d: f64| m.get(k).and_then(|v| v.as_f64()).filter(|v| *v > 0.0).unwrap_or(d);
            map.insert(
                name,
                Monitor {
                    hdr,
                    sdr_brightness: or("sdrBrightness", 1.0),
                    sdr_saturation: or("sdrSaturation", 1.0),
                    sdr_max_luminance: or("sdrMaxLuminance", 80.0),
                    scale,
                    x: num("x"),
                    y: num("y"),
                    w,
                    h,
                },
            );
        }
        map
    }

    impl Monitor {
        /// What Hyprland multiplies ordinary SDR content by on this output, in scRGB units
        /// (1.0 = 80 nits): its SDR brightness times the luminance it maps SDR white to.
        pub fn sdr_scale(&self) -> f64 {
            self.sdr_brightness * self.sdr_max_luminance / 80.0
        }
    }

    /// Where SDR white sits on an HDR screen: SDR pictures, and an app's own text and panels.
    ///
    /// HDR content never goes through this. A PQ file already says how bright each pixel is, and
    /// it is shown at that; this is only about content that has no brightness of its own, which
    /// has to be put somewhere. By default it goes where the compositor puts every other SDR
    /// program, so a vanta app sits at the same level as the rest of the desktop.
    #[derive(Clone, Copy, Debug, PartialEq, Default)]
    pub enum SdrWhite {
        /// The compositor's own level for SDR on that screen, with its saturation.
        #[default]
        Desktop,
        /// A fixed level, in nits, with no saturation boost.
        Nits(f32),
    }

    impl SdrWhite {
        /// Reads `sdrWhite` from a config: `"desktop"`, or a number of nits.
        pub fn parse(v: Option<&serde_json::Value>) -> SdrWhite {
            match v {
                Some(v) if v.is_number() => v.as_f64().filter(|n| *n > 0.0).map_or(SdrWhite::Desktop, |n| SdrWhite::Nits(n as f32)),
                _ => SdrWhite::Desktop,
            }
        }

        /// The scale (scRGB units, 1.0 = 80 nits) and saturation for SDR content on a screen.
        /// With the screen unknown, the reference level of 203 nits the stack otherwise uses.
        pub fn levels(self, m: Option<&Monitor>) -> (f32, f32) {
            match self {
                SdrWhite::Nits(n) => (n / 80.0, 1.0),
                SdrWhite::Desktop => m.map_or((203.0 / 80.0, 1.0), |m| (m.sdr_scale() as f32, m.sdr_saturation as f32)),
            }
        }
    }

    #[cfg(test)]
    mod sdr_tests {
        use super::*;

        #[test]
        fn sdr_white_follows_the_desktop_unless_told_a_level() {
            let m = Monitor { hdr: true, sdr_brightness: 5.0, sdr_saturation: 1.35, sdr_max_luminance: 80.0, scale: 1.25, x: 0.0, y: 0.0, w: 0.0, h: 0.0 };
            assert_eq!(SdrWhite::Desktop.levels(Some(&m)), (5.0, 1.35), "400 nits, as Hyprland puts SDR");
            assert_eq!(SdrWhite::Nits(203.0).levels(Some(&m)), (203.0 / 80.0, 1.0));
            assert_eq!(SdrWhite::parse(Some(&serde_json::json!(300))), SdrWhite::Nits(300.0));
            assert_eq!(SdrWhite::parse(Some(&serde_json::json!("desktop"))), SdrWhite::Desktop);
            assert_eq!(SdrWhite::parse(None), SdrWhite::Desktop);
        }
    }

    /// Hyprland's command socket, where `hyprctl` itself sends its requests.
    fn command_socket() -> Option<std::path::PathBuf> {
        let runtime = std::env::var_os("XDG_RUNTIME_DIR")?;
        let sig = std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE")?;
        Some(std::path::Path::new(&runtime).join("hypr").join(sig).join(".socket.sock"))
    }

    /// A request straight to the compositor, as `hyprctl` would send it (`j/monitors` for
    /// `hyprctl monitors -j`), and its whole answer. A fraction of a millisecond where running
    /// `hyprctl` costs a process, which adds up for something asked every couple of seconds.
    /// Nothing if the socket is not there, or the answer has not finished within `limit`.
    pub fn request(cmd: &str, limit: Duration) -> Option<Vec<u8>> {
        use std::io::{Read, Write};
        let deadline = std::time::Instant::now() + limit;
        let mut s = std::os::unix::net::UnixStream::connect(command_socket()?).ok()?;
        s.set_write_timeout(Some(limit)).ok()?;
        s.write_all(cmd.as_bytes()).ok()?;
        let mut out = Vec::new();
        let mut buf = [0u8; 16384];
        loop {
            let left = deadline.checked_duration_since(std::time::Instant::now()).filter(|d| !d.is_zero())?;
            s.set_read_timeout(Some(left)).ok()?;
            match s.read(&mut buf) {
                Ok(0) => return Some(out),
                Ok(n) => out.extend_from_slice(&buf[..n]),
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(_) => return None,
            }
        }
    }

    /// `request`, read as JSON.
    pub fn request_json(cmd: &str, limit: Duration) -> Option<serde_json::Value> {
        serde_json::from_slice(&request(cmd, limit)?).ok()
    }

    /// `hyprctl <args>`'s output, or nothing if it fails or takes longer than `limit`, in which
    /// case the process is killed rather than left behind (a caller polling every couple of
    /// seconds against a wedged Hyprland would otherwise pile them up).
    fn run_bounded(args: &[&str], limit: Duration) -> Option<Vec<u8>> {
        use std::io::Read;
        let mut child = Command::new("hyprctl").args(args).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::null()).spawn().ok()?;
        let mut stdout = child.stdout.take()?;
        let reader = std::thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = stdout.read_to_end(&mut buf);
            buf
        });
        let deadline = std::time::Instant::now() + limit;
        loop {
            match child.try_wait() {
                Ok(Some(_)) => return reader.join().ok(),
                Ok(None) if std::time::Instant::now() < deadline => std::thread::sleep(Duration::from_millis(5)),
                _ => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colour_forms() {
        assert_eq!(Color::parse("ff7a00ff"), Some(Color::rgba(255, 122, 0, 255)));
        assert_eq!(Color::parse("#ff7a00"), Some(Color::rgba(255, 122, 0, 255)));
        assert_eq!(Color::parse("rgba(ff1a3c80)"), Some(Color::rgba(255, 26, 60, 128)));
        assert_eq!(Color::parse(""), None);
        assert_eq!(Color::parse("nope"), None);
    }

    #[test]
    fn jsonc_with_comments_and_trailing_commas() {
        let v = jsonc::parse("{ // c\n \"a\": 1, /* x */ \"b\": [1,2,], }").unwrap();
        assert_eq!(v.i32_or("a", 0), 1);
        assert_eq!(v.section("missing").f32_or("x", 2.5), 2.5);
    }
}

/// The terminal to run a command in, for an app that has none of its own to start.
pub mod terminal {
    /// Terminals tried in order when `$TERMINAL` is not set; each takes `-e <command…>`.
    pub const KNOWN: &[&str] = &["kitty", "foot", "alacritty", "ghostty", "wezterm", "konsole", "gnome-terminal", "xterm"];

    /// Whether `program` is an executable on `$PATH` (or a path to one).
    pub fn on_path(program: &str) -> bool {
        if program.contains('/') {
            return std::path::Path::new(program).is_file();
        }
        std::env::var_os("PATH").is_some_and(|p| std::env::split_paths(&p).any(|d| d.join(program).is_file()))
    }

    /// `$TERMINAL`, else `xdg-terminal-exec`, else the first of [`KNOWN`] that is installed.
    pub fn program() -> Option<String> {
        if let Some(t) = std::env::var("TERMINAL").ok().filter(|t| !t.trim().is_empty()) {
            return Some(t);
        }
        std::iter::once("xdg-terminal-exec").chain(KNOWN.iter().copied()).find(|t| on_path(t)).map(str::to_string)
    }

    /// The words that start `argv` in a terminal, or `None` when there is no terminal to be found.
    pub fn wrap(argv: &[&str]) -> Option<Vec<String>> {
        let term = program()?;
        let name = std::path::Path::new(&term).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let mut out = vec![term];
        match name.as_str() {
            "xdg-terminal-exec" => {}
            "wezterm" => out.extend(["start".to_string(), "--".to_string()]),
            "gnome-terminal" => out.push("--".into()),
            _ => out.push("-e".into()),
        }
        out.extend(argv.iter().map(|a| a.to_string()));
        Some(out)
    }
}

/// The icon theme the desktop is set to use.
pub mod icon_theme {
    /// Common themes, tried in order when nothing names one.
    const COMMON: &[&str] = &["breeze-dark", "breeze", "Papirus-Dark", "Papirus", "Adwaita"];

    /// GTK 3's setting, GTK 4's, KDE's, then GNOME's through gsettings; else the first common
    /// theme installed; else `hicolor`, which every system has.
    pub fn desktop() -> String {
        named().unwrap_or_else(|| {
            COMMON.iter().find(|t| installed(t)).map_or_else(|| "hicolor".to_string(), |t| t.to_string())
        })
    }

    fn named() -> Option<String> {
        let cfg = crate::paths::config_home();
        for f in ["gtk-3.0/settings.ini", "gtk-4.0/settings.ini"] {
            let text = std::fs::read_to_string(cfg.join(f)).unwrap_or_default();
            let v = text.lines().find_map(|l| l.trim().strip_prefix("gtk-icon-theme-name")).and_then(|r| r.split_once('=')).map(|(_, v)| v.trim().to_string());
            if let Some(v) = v.filter(|v| !v.is_empty()) {
                return Some(v);
            }
        }
        let kde = std::fs::read_to_string(cfg.join("kdeglobals")).unwrap_or_default();
        let mut inside = false;
        for l in kde.lines() {
            if l.starts_with('[') {
                inside = l.trim() == "[Icons]";
            } else if inside {
                if let Some(v) = l.strip_prefix("Theme=").map(str::trim).filter(|v| !v.is_empty()) {
                    return Some(v.to_string());
                }
            }
        }
        let out = std::process::Command::new("gsettings").args(["get", "org.gnome.desktop.interface", "icon-theme"]).output().ok()?;
        let v = String::from_utf8_lossy(&out.stdout).trim().trim_matches('\'').to_string();
        (out.status.success() && !v.is_empty()).then_some(v)
    }

    /// The common themes that are installed, best first: somewhere to look for an icon the
    /// desktop's own theme does not have.
    pub fn fallbacks() -> Vec<String> {
        COMMON.iter().filter(|t| installed(t)).map(|t| t.to_string()).collect()
    }

    /// Whether a theme of that name is installed anywhere icon themes are looked for.
    pub fn installed(name: &str) -> bool {
        let mut roots = vec![crate::paths::home().join(".icons"), data_home().join("icons")];
        let dirs = std::env::var("XDG_DATA_DIRS").ok().filter(|d| !d.is_empty()).unwrap_or_else(|| "/usr/local/share:/usr/share".into());
        roots.extend(dirs.split(':').map(|d| std::path::Path::new(d).join("icons")));
        roots.push("/usr/share/pixmaps".into());
        roots.iter().any(|r| r.join(name).join("index.theme").exists())
    }

    fn data_home() -> std::path::PathBuf {
        std::env::var_os("XDG_DATA_HOME").map(Into::into).unwrap_or_else(|| crate::paths::home().join(".local/share"))
    }
}
