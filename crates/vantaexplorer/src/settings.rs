//! Behaviour preferences: date format, terminal, icon themes. They belong to the machine, so
//! they are written in `explorer.local.jsonc`, which a theme switch never touches (the defaults
//! with their comments are in `config::DEFAULT_LOCAL`), and read from the same layered files as
//! the appearance.

use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    pub date_format: String,
    /// Program for "Open Terminal Here".
    pub terminal: String,
    /// Its arguments; "{dir}" is substituted with the folder.
    pub terminal_args: Vec<String>,
    /// Icon themes to look in, best first.
    pub icon_themes: Vec<String>,
    /// A copy keeps the original's modification time (as `cp -a`, Dolphin and Nautilus do);
    /// false: a copy is dated when it was made.
    pub keep_dates: bool,
    /// Folders sort before files, whatever the column; false: in among them.
    pub folders_first: bool,
    /// Document pages shown dark on a dark theme.
    #[serde(deserialize_with = "dark_pages")]
    pub dark_pages: DarkPages,
    /// Whether list or grid and its size belong to the folder or to the pane.
    #[serde(deserialize_with = "view_per")]
    pub view_per: ViewPer,
}

/// What the view (list or grid, and how large) is remembered for.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub enum ViewPer {
    /// Each folder keeps its own; see `views`.
    #[default]
    Folder,
    /// The pane keeps one, whatever folder it shows.
    Pane,
}

fn view_per<'de, D: serde::Deserializer<'de>>(d: D) -> Result<ViewPer, D::Error> {
    let v = serde_json::Value::deserialize(d)?;
    Ok(match v.as_str() {
        Some("folder") => ViewPer::Folder,
        Some("pane") => ViewPer::Pane,
        None if v.is_null() => ViewPer::Folder,
        _ => {
            log::warn!("settings: viewPer {v} is not \"folder\" or \"pane\"; folder");
            ViewPer::Folder
        }
    })
}

/// Which previews of documents are drawn dark, with light text, when the theme is dark.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub enum DarkPages {
    #[default]
    Off,
    /// Word and Writer documents, which are drawn by us: their pictures stay as they are.
    Documents,
    /// PDFs too, whose pages are pictures: everything on them is turned, photos included.
    All,
}

impl DarkPages {
    /// Whether a preview is drawn dark: `rendered` for a page that is a picture (a PDF's).
    pub fn applies(self, rendered: bool, dark_theme: bool) -> bool {
        dark_theme && (self == DarkPages::All || (self == DarkPages::Documents && !rendered))
    }
}

/// A word the setting does not know is left off, rather than losing every other setting.
fn dark_pages<'de, D: serde::Deserializer<'de>>(d: D) -> Result<DarkPages, D::Error> {
    let v = serde_json::Value::deserialize(d)?;
    Ok(match v.as_str() {
        Some("documents") => DarkPages::Documents,
        Some("all") => DarkPages::All,
        Some("off") => DarkPages::Off,
        None if v.is_null() => DarkPages::Off,
        _ => {
            log::warn!("settings: darkPages {v} is not \"off\", \"documents\" or \"all\"; off");
            DarkPages::Off
        }
    })
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            date_format: "%Y-%m-%d %H:%M".into(),
            terminal: String::new(),
            terminal_args: Vec::new(),
            icon_themes: Vec::new(),
            keep_dates: true,
            folders_first: true,
            dark_pages: DarkPages::Off,
            view_per: ViewPer::Folder,
        }
    }
}

impl Settings {
    /// The terminal for "Open Terminal Here" and its arguments, `{dir}` still in them: the
    /// configured one, else `$TERMINAL` or the first known terminal installed, with the flag
    /// that terminal takes for a starting folder.
    pub fn terminal_command(&self) -> Option<(String, Vec<String>)> {
        if !self.terminal.trim().is_empty() {
            return Some((self.terminal.clone(), self.terminal_args.clone()));
        }
        let program = vanta_config::terminal::program()?;
        let name = std::path::Path::new(&program).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let args: &[&str] = match name.as_str() {
            _ if !self.terminal_args.is_empty() => &[],
            "kitty" => &["--directory", "{dir}"],
            "foot" | "alacritty" | "gnome-terminal" => &["--working-directory", "{dir}"],
            "ghostty" => &["--working-directory={dir}"],
            "wezterm" => &["start", "--cwd", "{dir}"],
            "konsole" => &["--workdir", "{dir}"],
            _ => &[],
        };
        let args = if self.terminal_args.is_empty() { args.iter().map(|a| a.to_string()).collect() } else { self.terminal_args.clone() };
        Some((program, args))
    }

    /// The preferences from the layered files, defaults for whatever they leave out.
    pub fn from_value(v: &serde_json::Value) -> Self {
        serde_json::from_value(v.clone()).unwrap_or_else(|e| {
            log::warn!("settings: {e}; using defaults");
            Settings::default()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Written verbatim on first run: if it does not parse, the app is broken for everyone
    /// who never touched it.
    #[test]
    fn default_settings_parse() {
        let s = Settings::from_value(&vanta_config::jsonc::parse(&vanta_config::jsonc::uncommented(crate::config::DEFAULT_LOCAL)).expect("DEFAULT_LOCAL must parse"));
        assert_eq!(s.terminal, "", "found on the machine, not named");
        assert!(s.icon_themes.is_empty(), "the desktop's own icon theme");
    }

    /// Older settings files predate the terminal keys; they must keep loading.
    #[test]
    fn missing_keys_fall_back_to_defaults() {
        let s = Settings::from_value(&vanta_config::jsonc::parse(r##"{ "dateFormat": "%Y", "color": { "text": "#fff" } }"##).unwrap());
        assert_eq!(s.date_format, "%Y");
        assert_eq!(s.terminal, "");

        let named = Settings { terminal: "foot".into(), terminal_args: vec!["-D".into(), "{dir}".into()], ..Default::default() };
        assert_eq!(named.terminal_command(), Some(("foot".to_string(), vec!["-D".to_string(), "{dir}".to_string()])), "a named terminal is taken as written");
    }

    /// The files written on first run set nothing: every option in them is commented out with
    /// its built-in default, so they show what can be turned and what it is now. Turned on all
    /// together they must give exactly what the app has with nothing set, or a file would show
    /// a default the app does not have. EXAMPLES are the options with no value of their own
    /// until set, which the file shows with an example.
    #[test]
    fn every_commented_option_is_the_default() {
        const EXAMPLES: &[&str] = &[];
        let on = |t: &str| vanta_config::jsonc::parse(&vanta_config::jsonc::uncommented(t)).unwrap();
        let mut v = on(crate::config::DEFAULT_LOCAL);
        
        for path in EXAMPLES {
            vanta_config::jsonc::remove(&mut v, path);
        }
        assert_eq!(format!("{:?}", Settings::from_value(&v)), format!("{:?}", Settings::from_value(&serde_json::json!({}))));
    }
}
