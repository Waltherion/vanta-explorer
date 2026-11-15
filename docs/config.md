# Configuration

Every Vanta app keeps its settings in one folder, `~/.config/vanta/`, and writes its files there
with their defaults and a comment on every setting the first time it starts. Nothing needs to be
written by hand to get going; everything below is what those files say.

## How the files are read

Each app reads, in this order, the later file winning key by key (an object is merged key by key,
anything else, a list included, is replaced whole):

1. its **built-in defaults**, the Vanta look;
2. the **theme** in use, if there is one (see below): its palette, then its file for the app;
3. **`colors.jsonc`**, the palette all of them share: the sections `color`, `font`, `rainbow`
   and `shape`, and nothing else;
4. **`<app>.jsonc`**: the app's own look and behaviour. It may also override any of the palette
   for that app alone;
5. **`<app>.local.jsonc`**: what belongs to the machine rather than the look (commands, actions,
   screen order, folders, the terminal).

A theme is where everything starts from, and whatever you write in your own files wins over it,
through every theme switch. The files the apps write on first run have every line commented out,
so a theme shows through them untouched until you turn a line on.

Beside them:

| File | What | Read by |
|---|---|---|
| `display.jsonc` | where SDR content sits on an HDR screen (`sdrWhite`) | every app; an app's own `sdrWhite` wins |
| `viewer-keys.jsonc` | the viewer's keybindings | Vanta Viewer |
| `explorer-sidebar.jsonc` | Explorer's Places sections; the app edits it too | Vanta Explorer |
| `calendar.jsonc`, `weather.jsonc` | the bar's calendar and weather panel | Vanta Bar |

A file that does not parse is left out and reported on stderr; the others still apply. Every app
picks up a change within a couple of seconds without a restart (Vanta Lock at the next lock,
Vanta Organizer the next time the board opens).

## Themes

A theme is a folder in `~/.config/vanta/themes/` (`selector.themesDir` in `bar.local.jsonc` moves
it) holding any of:

- the same files as `~/.config/vanta/`: `colors.jsonc`, `bar.jsonc`, `lock.jsonc`, `paper.jsonc`,
  `organizer.jsonc`, `explorer.jsonc`, `viewer.jsonc`. The same names with `vanta` in front
  (`vanta-colors.jsonc`, `vantabar.jsonc`, ...) work too, for a theme folder shared with other
  programs. A theme holds only what it changes; the rest comes from the defaults;
- a `wallpapers` folder, which Vanta Paper plays from while the theme is in use (unless a `path`
  is set in `paper.jsonc`);
- or nothing but pictures: a folder of wallpapers is a theme that changes the wallpaper and
  nothing else;
- optionally a `meta.toml` with `name = "..."` for the name the selector shows, and
  `status = "..."` other than `"active"` to keep a theme out of the selector.

Switch in the selector in the bar, or with `vantabar ctl theme <name>` (the folder's name or the
theme's). Every app follows by itself, and Vanta Paper moves to the new wallpapers with its own
transition. `vantabar ctl theme` on its own answers with the theme in use.

The theme in use is one line in `~/.local/state/vanta/theme`: the folder's full path. Anything
that writes a new line there switches the Vanta apps too, which is how a theme script of your own
can switch them; `selector.applyTheme` is the other way round, a command run after every switch
the bar makes, for whatever else on the desktop should follow.

A palette from a wallpaper only has to write `colors.jsonc`; [templates/](../templates/README.md) has
ready ones for matugen and wallust.

## Values

- Colours are `"#RRGGBB"` or `"#AARRGGBB"`, **alpha first**.
- `~` and `$HOME` are expanded in paths and commands.
- JSONC: `//` and `/* */` comments and trailing commas are fine.
- Fonts are named as fontconfig knows them (`"Poppins"`, `"Inter"`). A family the machine does
  not have is not replaced by something unrelated: the app keeps its own face instead. Poppins
  is built in, in weights 300, 400, 500 and 600, for machines that do not have it installed.

## Environment

| Variable | Effect |
|---|---|
| `VANTA_CONFIG_DIR` | another folder in place of `~/.config/vanta` |
| `VANTA_THEME` | another theme folder in place of the one in use, for one program |
| `VANTABAR_CONFIG`, `VANTALOCK_CONFIG`, `VANTAPAPER_CONFIG`, `VANTAORGANIZER_CONFIG`, `VANTAEXPLORER_CONFIG`, `VANTAVIEWER_CONFIG` | another file in place of that app's `<app>.jsonc` |
| `VANTA_HDR=off` | every app in SDR |

Each app's own test switches (`VANTALOCK_TIMEOUT`, `VANTAPAPER_DEV` and the like) are listed at
the top of its `main.rs`.

## The palette: `colors.jsonc`

```jsonc
// The palette every Vanta app paints with (JSONC: comments and trailing commas are fine).
// A theme usually makes this file a link into its own folder. Any app's own file can override
// any of it for that app alone. Colours are "#RRGGBB" or "#AARRGGBB" (alpha first).
//
// Everything below is commented out: as long as it is, each app uses its own built-in look.
// Turn on what you want every app to share.
{
  // "color": {
  //   "background": "#80000000", // panels and the bar's pill
  //   "text": "#bd97f7",
  //   "accent": "#bd97f7",       // outlines, active marks, the password field
  //   "border": "#bd97f7",
  //   "highlight": "#bd97f7",    // hover and focus
  //   "error": "#ff3d7f",        // a wrong password, a failed operation
  // },
  // "font": {
  //   "family": "Poppins", // any installed family; resolved through fontconfig
  //   "size": 16,
  //   "weight": 300,
  // },
  // // A band of colours running through text and outlines.
  // "rainbow": {
  //   "enabled": true,
  //   "stops": ["#6a2cc4", "#a23be0", "#e04fd0", "#ff7ab8"], // two or more
  //   "period": 3000, // pixels of desktop per trip through the stops
  //   "speed": 3,     // how fast it rolls; 0 holds it still, negative runs it backwards
  // },
  // "shape": {
  //   "cornerRadius": 24,
  //   "borderWidth": 0,
  // },
}
```

`rainbow` also takes `brightness` (a multiplier on the band's colours; Vanta Lock uses it to lift
the band on HDR screens).

## `display.jsonc`

```jsonc
// Shared by every Vanta app: how they meet the screens.
//
// Lines starting with // are options left at their default: remove the // to use one.
{
  // Where SDR content (text, panels, SDR pictures) sits on an HDR screen: "desktop" puts it
  // where the compositor puts every other SDR program, a number puts SDR white at that many
  // nits. HDR pictures are always shown as mastered.
  // "sdrWhite": "desktop",
}
```

## Vanta Bar

### `bar.jsonc`

```jsonc
// Vanta Bar (JSONC: comments and trailing commas are fine).
//
// Colours, font, the rainbow band and corner/border shape come from colors.jsonc, shared by
// every Vanta app. Any of those sections written here overrides it for the bar only, e.g.
//   "shape": { "cornerRadius": 12 },
// Commands, actions and the selector's commands belong to this machine and are in
// bar.local.jsonc. Changes are picked up within a second, without a restart.
//
// Lines starting with // are options left at their default: remove the // to use one.
{
  // The pill's opacity, 0 to 1.
  // "opacity": 1,
  // Blur the desktop behind the pill, the panels and the notifications wherever they can be seen
  // through (an opacity or a background colour below full). An opaque pill gets none, since
  // nothing would show it. Asked of the compositor by the bar itself, no compositor rule needed.
  // "blur": true,
  // The glow around the pill's outline and text. strength 0 to 1 (0 costs nothing);
  // mode "glow" (tight and bright) or "blur" (wide and soft, the one that can glow dark on a
  // light theme); spread 0.1 to 2 reaches nearer or further without changing the brightness.
  // target "all" lets everything in the pill glow, "visualizer" only the visualizer. reach is
  // how many points from the visualizer's line its glow (with "visualizer") and its HDR peaks
  // may go, and nothing beyond that; 0 lets the spread decide. intensity 0.25 to 4 makes the
  // glow denser or fainter without spreading it further: a black line on a light pill gives off
  // a shadow that reads as one at 2 to 3 (with mode "blur", which keeps it black).
  // "bloom": {
  //   "strength": 1,
  //   "mode": "glow",
  //   "spread": 0.35,
  //   "target": "visualizer",
  //   "reach": 10,
  //   "intensity": 1,
  // },
  // Which way the band runs through the bar, when colors.jsonc turns it on: "diagonal" (45
  // degrees across the desktop) or "horizontal".
  // "rainbow": {
  //   "axis": "diagonal",
  // },
  // The visualizer's own band, so it can be the one thing in colour (turn the band off in
  // colors.jsonc and on here). Unset, it is the bar's band, or the text colour without one.
  // mode "position" runs the band along the curve; "level" colours it by how loud it is, the
  // first stop at rest and the last at the peaks; "fit" lays the stops once across the curve,
  // left to right in the order they are written, standing still.
  // hdr: on an HDR screen, how many times brighter than the rest of the bar its peaks may get
  // (1 to 6; 1 keeps them level with the bar). SDR screens are not affected.
  // fill: how strongly the area under the curve takes its colour, 0 (the line alone) to 1.
  // height: how far the curve rises, as a share of the room between its floor and the pill's
  // edge, so 1 is as tall as it can be without its peaks being cut off. Its floor is the foot of
  // the clock's own digits, whatever the font and size, so the two line up.
  // "visualizer": {
  //   "rainbow": {
  //     "enabled": true,
  //     "mode": "level",
  //   },
  //   "hdr": 4,
  //   "fill": 0,
  //   "height": 0.9,
  // },
  // The fill of the power buttons; unset, the pill's own background, so only the outline shows.
  // The dot for the workspace you are on: pink, the band's crown, and "auto" makes it the band,
  // or the accent without one. Its own key so a theme can have one colour appear when something
  // happens and nowhere else.
  // "color": {
  //   "workspaceActive": "#ff7ab8",
  //   // The power panel's button fill; unset, the pill's own background.
  //   "powerButton": "#000000",
  // },
  // "bar": {
  //   // "shown" or "hidden" when the bar starts (Super+B toggles it).
  //   "startState": "shown",
  // },
  // What the pill holds, in order: "clock", "workspaces".
  // "modules": ["clock"],
  // What the system panel writes temperatures in: "celsius" or "fahrenheit".
  // "system": {
  //   "temperature": "celsius",
  // },
  // How the pill writes the time and the date (strftime): "%H:%M:%S" and "%d-%m-%Y" here,
  // "%I:%M:%S %p" and "%m/%d/%Y" for the American way. Leaving the seconds out lets the bar
  // sleep until the minute turns.
  // "clock": {
  //   "time": "%H:%M:%S",
  //   "date": "%Y-%m-%d",
  // },
  // "workspaces": {
  //   // Which field hands its place over to the workspace dots: "time" (the left one) or "date"
  //   // (the right one). "left" and "right" mean the same thing.
  //   "side": "date",
  //   // What brings them out: "change" (while switching workspace), "hover", "both" or "always".
  //   "trigger": "change",
  // },
  // "hub": {
  //   // The buttons in the row that folds out of the pill: "square", "circle" or "squircle".
  //   "buttonShape": "squircle",
  // },
  // "selector": {
  //   // How far the theme and wallpaper selector opens below the pill: points (576) or a share
  //   // of each screen ("50%"). Unset, half the screen, which is what it takes for the three
  //   // stacked pictures to be worth looking at. The panel is then as wide as they come out.
  //   "height": "50%",
  //   // Corners of the pictures: "square" or "rounded". Unset, they follow the hub buttons.
  //   "previewShape": "rounded",
  // },
}
```

### `bar.local.jsonc`

```jsonc
// Vanta Bar, this machine only: kept when the theme changes.
//
// Lines starting with // are options left at their default: remove the // to use one.
{
  // What the power panel runs, each handed to a shell as written ($HOME is expanded).
  // "commands": {
  //   "lock": "loginctl lock-session",
  //   "suspend": "systemctl suspend", // or "systemctl hibernate", with a matching sleepLabel
  //   "logout": "", // empty: leave the compositor
  //   "reboot": "systemctl reboot",
  //   "poweroff": "systemctl poweroff",
  //   "audioSettings": "pavucontrol",
  //   // The gears in the network panel, for what the bar leaves to a settings program (company
  //   // networks, VPN, a fixed address). Shown only when the program is installed.
  //   "wifiSettings": "nm-connection-editor",
  //   "bluetoothSettings": "blueman-manager",
  //   "brightness": "brightnessctl set {pct}%", // {pct} is the percentage
  // },
  // What the suspend button is called.
  // "sleepLabel": "Sleep",
  // The actions panel: a label, and either a shell line ("cmd") or a hub panel to open ("panel").
  // "actions": [
  //   // { "label": "Screenshot", "cmd": "grim ~/Pictures/screenshot.png" },
  //   // { "label": "Sound", "panel": "audio" },
  // ],
  // "selector": {
  //   // The folder of themes, one folder each: the files of ~/.config/vanta it wants to change
  //   // (or the same with vanta in front: vantabar.jsonc), a wallpapers folder, or only pictures.
  //   // With none, the selector is a wallpaper picker for the folder Vanta Paper shows from.
  //   "themesDir": "~/.config/vanta/themes",
  //   // Run with `sh -c` after a theme switch, for whatever else on the desktop should follow;
  //   // {dir} is the theme's folder name. Empty: only the Vanta apps switch.
  //   "applyTheme": "",
  //   // Makes a thumbnail: {src}, {dest} and {size} are replaced. Empty: the selector's own.
  //   "thumbCommand": "",
  //   // Run for each theme before its folder is read, {dir} replaced (e.g. to mirror its
  //   // wallpapers from elsewhere). Empty: nothing is run.
  //   "syncCommand": "",
  // },
  // How the network panel finds out whether the internet is there, while it is open: a ping to
  // checkAddress (a number, so it works without names), and a plain http request to checkUrl,
  // whose answer must contain checkText (empty: any answer that is not a redirect). A name that
  // does not resolve means DNS is down; another answer means a login page is in the way.
  // "network": {
  //   "checkAddress": "1.1.1.1",
  //   "checkUrl": "http://ping.archlinux.org/nm-check.txt",
  //   "checkText": "NetworkManager is online",
  // },
}
```

### `weather.jsonc` and `calendar.jsonc`

Not written on first run; the panel says what is missing. `weather.jsonc`: `latitude`,
`longitude` (or `location` and `country` to look them up), `units` (`"metric"` or `"imperial"`),
`windUnit` (`"ms"`, `"kmh"`, `"mph"` or `"kn"`), `hourFormat` (`12` or `24`), `model`, `refreshMinutes`. `calendar.jsonc`: `weekStart`
(`"monday"` or `"sunday"`), `showWeekNumbers`, `dateFormat`, `events` (a list of `{ "date":
"YYYY-MM-DD", "time", "title", "location" }`), and `graph: { "clientId", "refreshMinutes" }` to
read a Microsoft calendar.

## Vanta Lock

### `lock.jsonc`

```jsonc
// Vanta Lock (JSONC: // and /* */ comments + trailing commas OK).
//
// The text, accent and error colours, the font family and the rainbow band come from
// colors.jsonc, shared by every Vanta app. Any of those written here overrides them for the
// lock screen only, e.g.
//   "color": { "text": "#ffffff", "accent": "#ffffff", "error": "#ff1a3c" },
//   "font": { "family": "Poppins" },
//   "rainbow": {
//     "enabled": true,
//     "stops": ["#ff5555", "#ffb86c", "#50fa7b", "#8be9fd", "#bd93f9"], // >= 2 colours
//     "period": 0,       // px per cycle on the 1920-wide reference (0 = span the width once)
//     "brightness": 2.5, // band luminance (1 = faithful; >1 lifts it on HDR screens)
//     "speed": 0,        // cycles per second (0 = still, near-zero cost; negative = reverse);
//                        // >0 renders continuously while a screen is on
//   },
// Colours are "#RRGGBB" or "#AARRGGBB" (alpha first). Sizes are points and positions are
// fractions (0..1) of a canvas 1080 points tall and as wide as the screen's shape; text
// rasterises at the screen's own resolution, so it stays sharp on 4K.
// Things that belong to this machine rather than a theme go in lock.local.jsonc.
//
// Lines starting with // are options left at their default: remove the // to use one.
{
  // ---- Background: the blurred, dimmed wallpaper behind everything ----
  // "background": {
  //   "blur": 0.008, // blur radius in uv units (0 = sharp / off)
  //   // Milliseconds for the blur and the dimming to come and go: the wallpaper goes soft as the
  //   // screen locks and comes back sharp as it unlocks. 0 = no animation, soft from the first
  //   // frame. The unlock waits this long before it is sent, so keep it short; 1000 is the most.
  //   "focusMs": 500,
  //   "blurType": "gaussian", // "gaussian" | "frosted" (sparse-tap glass) | "box" | "pixelate" | "none"
  //   "dim": 0.6, // dim multiplier in linear light (1 = none, 0 = black)
  // },
  // ---- Thumbnail: the sharp, framed copy of the wallpaper ----
  // "thumbnail": {
  //   "show": false, // true = a sharp, framed copy of the wallpaper as well
  //   "height": 0.24, // height as a fraction of screen height
  //   "y": 0.55, // vertical centre as a fraction
  //   "radius": 0, // corner rounding, fraction of thumbnail height (0 = square)
  //   "border": 1, // border thickness in points (0 = off); rides the rainbow band
  // },
  // ---- Clock and date: size in points, vertical centre as a fraction of screen height ----
  // "clock": {
  //   "time": {
  //     "size": 52,
  //     "y": 0.541,
  //   },
  //   "weekday": {
  //     "size": 18,
  //     "y": 0.616,
  //   },
  //   "date": {
  //     "size": 14,
  //     "y": 0.651,
  //   },
  //   "format": "24h", // "24h" (14:05) or "12h" (2:05 PM)
  //   "locale": "", // "" = system locale; else e.g. "da_DK", "en_US"
  //   "dateFormat": "", // "" = "d MMMM yyyy" (localised); else e.g. "yyyy-MM-dd"
  // },
  // ---- Password field ----
  // "field": {
  //   "width": 200,
  //   "height": 35,
  //   "y": 0.78, // field top as a fraction of screen height
  //   "radius": 0.5, // corner rounding as a fraction of field height (0 = sharp, 0.5 = pill)
  //   "fontSize": 12, // status text ("Enter password", "Wrong password")
  //   "symbol": "", // what hides each typed character: "" = a dot, or any glyph e.g. "*"
  //   "symbolSize": 0, // points for that glyph; 0 = scales with the field height
  //   "symbolFont": "", // font for the glyph; "" = the main family (set it if that family has no
  //                     // such character: many display fonts lack hearts and stars)
  // },
  // ---- Text shadow ----
  // "shadow": {
  //   "color": "#96000000", // #AARRGGBB
  //   "offset": 0, // points (0 = none)
  //   "strength": 0.5, // alpha multiplier (0..1)
  // },
  // ---- Bloom: a soft glow around the text (rainbow colour, or the text colour). 0 = off ----
  // "bloom": {
  //   "strength": 1.5,
  // },

  // Environment overrides for quick tests: VANTALOCK_BLUR, VANTALOCK_DIM, VANTALOCK_BLOOM;
  // VANTALOCK_TIMEOUT=<seconds> unlocks by itself (testing only).
}
```

### `lock.local.jsonc`

```jsonc
// Vanta Lock, this machine only: kept when the theme changes.
//
// Lines starting with // are options left at their default: remove the // to use one.
{
  // A picture to lock over; "" = the one Vanta Paper is showing.
  // "image": "",
}
```

## Vanta Paper

### `paper.jsonc`

```jsonc
// Vanta Paper: the wallpaper (JSONC: // and /* */ comments are allowed).
// Things that belong to this machine rather than a theme go in paper.local.jsonc; where SDR
// sits on an HDR screen is shared by every Vanta app, in display.jsonc.
//
// Lines starting with // are options left at their default: remove the // to use one.
{
  // --- Source ---
  // Folder to read wallpapers from. "~" and $ENV are expanded.
  // Left out, the active theme's wallpapers are used, and without a theme the ones installed
  // (~/.local/share/vanta/wallpapers or /usr/share/vanta/wallpapers), failing those
  // ~/Pictures/wallpapers.
  // "path": "",

  // --- Autorotation ---
  // "durationSecs": 300, // seconds between automatic wallpaper changes
  // "startPaused": false, // start with autorotation paused (toggle with the keybind)

  // --- Playback order ---
  // "ascending" -> alphabetical by filename. "random" -> shuffle-bag: every image
  // shows once before any repeats, never repeating across a reshuffle.
  // "sort": "ascending",

  // --- Video wallpapers ---
  // Videos (.mp4/.mkv/.webm/.mov/.m4v) rotate alongside stills: the first frame
  // transitions in, then the video plays and loops (hard cut) until the next
  // rotation. HDR video (PQ/HLG, e.g. 10-bit BT.2020 AV1) renders natively on HDR
  // monitors and tone-maps on SDR ones, exactly like still wallpapers.
  // "hwdec": "auto" decodes on the GPU with NVDEC (NVIDIA cards) when possible, falling back
  // to software automatically (other cards decode in software); "off" forces software
  // decode (dav1d etc.).
  // "video": {
  //   "hwdec": "auto",
  // },

  // --- Transitions ---
  // A random one is chosen from "enabled" on each change. Available: "fade", "wipe",
  // "grow", "shrink", "slide", "wave", "pixelate", "blinds", and "radial" (alias
  // "clock"). Slide may be plain ("slide" = random side) or directional:
  // "slide-left"/"slide-right"/"slide-up"/"slide-down" name the side the new image
  // enters from. Use [] or ["none"] to switch instantly with no animation.
  //
  // "series" ties next/previous (and autorotation) to a directional slide, so a run of
  // images reads like a filmstrip:
  //   "none"        -> next/previous use the random pool above
  //   "horizontal"  -> next enters from the right, previous from the left
  //   "vertical"    -> next enters from the bottom, previous from the top
  // "transition": {
  //   "enabled": ["fade"],
  //   "series": "none",
  //   "durationMs": 1200,
  // },
}
```

### `paper.local.jsonc`

```jsonc
// Vanta Paper, this machine only: kept when the theme changes.
//
// Lines starting with // are options left at their default: remove the // to use one.
{
  // Give specific monitors their own source folder; any monitor not listed uses "path" from
  // paper.jsonc. Keys are connector names (see `hyprctl monitors`). Each output rotates its own
  // playlist, so monitors show different wallpapers; with "sort": "random" even outputs that
  // share a folder draw independently.
  // "outputs": {
  // },
}
```

## Vanta Organizer

### `organizer.jsonc`

```jsonc
// Vanta Organizer: the workspace board (JSONC: comments and trailing commas are fine).
//
// Colours, font, the rainbow band and corner/border shape come from colors.jsonc, shared by
// every Vanta app. Any of those sections written here overrides it for the organizer only, e.g.
//   "shape": { "borderWidth": 2 },
// The workspace you are on has a colour of its own, pink unless a theme says otherwise, so the
// board marks it without every hover and outline taking the same colour ("auto" to leave it to
// the band and the accent):
//   "color": { "active": "#ff7ab8" },
// Things that belong to this machine rather than a theme go in organizer.local.jsonc.
//
// Lines starting with // are options left at their default: remove the // to use one.
{
  // "overview": {
  //   // Workspaces per monitor shown on the board.
  //   "workspaceCount": 10,
  //   // How workspace ids map onto monitors: "auto" (per monitor when every monitor owns its own
  //   // block of ids), "perMonitor" (1..count, count+1..2*count, ...) or "shared" (one set of ids).
  //   "workspaceLayout": "auto",
  //   // Workspaces per row of a monitor's grid.
  //   "columns": 5,
  //   // Monitors stacked "vertical"ly or side by side ("horizontal").
  //   "direction": "vertical",
  //   // Space between monitors' grids, and between workspaces, in logical pixels.
  //   "spacing": 28,
  //   "cellGap": 10,
  //   // Size of a workspace tile against the monitor it stands for (0.18 = 18 %).
  //   "scale": 0.18,
  //   // How dark the desktop behind the board goes, 0 to 1.
  //   "backdropOpacity": 0.45,
  //   // Live window thumbnails; false shows each window as its icon.
  //   "livePreviews": true,
  //   // Scratchpads beside each monitor's grid, this many side by side before the next row: 1
  //   // stacks them one above the other.
  //   "specialColumns": 1,
  //   // Scratchpads (special workspaces) on the board.
  //   "showSpecial": true,
  //   // Scratchpads laid out in rows by name, e.g. [["scratchtopleft", "scratchtopright"]].
  //   // Empty, each sits with the monitor its rule opens it on.
  //   "specialRows": [],
  //   // Monitor names above each grid.
  //   "headers": true,
  // },
  // The workspace you are on, in a colour of its own; "auto" to leave it to the band and accent.
  // "color": {
  //   "active": "#ff7ab8",
  // },
}
```

### `organizer.local.jsonc`

```jsonc
// Vanta Organizer, this machine only: kept when the theme changes.
//
// Lines starting with // are options left at their default: remove the // to use one.
{
  // "overview": {
  //   // Monitors in this order on the board, by connector name (e.g. ["DP-3", "DP-1"]).
  //   // Empty: the compositor's own order.
  //   "monitorOrder": [],
  // },
}
```

## Vanta Explorer

### `explorer.jsonc`

```jsonc
// Vanta Explorer (JSONC: comments and trailing commas are fine).
//
// Colours and font come from colors.jsonc, shared by every Vanta app. Written here, they apply
// to Explorer alone. Behaviour (date format, terminal, icon themes) belongs to this machine and
// is in explorer.local.jsonc. Where the interface puts SDR white on an HDR screen is shared by
// every Vanta app, in display.jsonc. Changes are picked up within a second, without a restart.
//
// Lines starting with // are options left at their default: remove the // to use one.
{
  // "color": {
  //   // Window background: "#rrggbb", or "#aarrggbb" where the alpha makes the window
  //   // translucent and the compositor blurs what is behind it. Content and text stay opaque.
  //   "background": "#d1000000",
  //   "text": "#bd97f7",
  //   "accent": "#bd97f7",
  //   // The folders in Explorer's own icons. Unset: the accent.
  //   "folder": "#bd97f7",
  // },
  // Resolved through fontconfig; "sans-serif" asks for the system's own.
  // "font": {
  //   "family": "Poppins",
  //   "size": 15,
  // },
}
```

### `explorer.local.jsonc`

```jsonc
// Vanta Explorer, this machine only: kept when the theme changes.
//
// Lines starting with // are options left at their default: remove the // to use one.
{
  // Date/time format for the Modified column, strftime-style. For example:
  //   "%Y-%m-%d %H:%M"  ->  2026-08-22 21:30  (ISO 8601)
  //   "%d-%m-%Y %H:%M"  ->  22-08-2026 21:30
  //   "%m/%d/%Y %I:%M %p" -> 08/22/2026 09:30 PM
  // Always an absolute date, no "3 d ago".
  // "dateFormat": "%Y-%m-%d %H:%M",

  // "Open Terminal Here" (context menu, or F4). The program, and the arguments it is
  // started with; "{dir}" is replaced by the folder. Terminals disagree on the flag:
  //   kitty      ["--directory", "{dir}"]
  //   foot       ["--working-directory", "{dir}"]
  //   alacritty  ["--working-directory", "{dir}"]
  //   wezterm    ["start", "--cwd", "{dir}"]
  // The process is also started WITH that folder as its working directory, so a terminal
  // that takes no flag at all works with "terminalArgs": [].
  // Empty: $TERMINAL, else the first known terminal installed, with its own flag.
  // "terminal": "",
  // "terminalArgs": [],

  // Icon themes to look in, best first. Empty: Explorer's own icons (Breeze, with folders in
  // the accent or color.folder), then the desktop's theme for anything they lack.
  // "desktop" stands for the desktop's own theme (GTK, KDE or GNOME setting), followed when it
  // changes: ["desktop"] uses the desktop's icons only. Name a theme yourself and the last one
  // should stay "hicolor", which every desktop ships. A theme that is repainted in place (some
  // desktops recolor their icons when the look changes) is picked up live.
  // "iconThemes": [],
}
```

## Vanta Viewer

### `viewer.jsonc`

```jsonc
// Vanta Viewer (JSONC: comments and trailing commas are fine). Keybindings are in
// viewer-keys.jsonc.
//
// Lines starting with // are options left at their default: remove the // to use one.
{
  // Area behind and around the image: "black" (true 0 nits on OLED), "transparent"
  // (see the wallpaper through letterbox bars), or a hex colour like "#101014".
  // "background": "transparent",

  // The info card, keys bar, crop caption and toasts take their accent and text colours and
  // their font family from colors.jsonc, shared by every Vanta app. To set them for the viewer
  // alone:
  // "color": {
  //   "accent": "#bd97f7",
  //   "text": "#bd97f7",
  // },
  // "font": {
  //   "family": "Poppins",
  // },
  // "overlayOpacity": 0.7, // 0..1, the card background opacity
  // "fontSize": 11.0, // overlay base point size

  // Behaviour.
  // "infoOverlay": true, // show the info card and keys bar by default
  // "windowWidth": 1280,
  // "windowHeight": 800,
  // "fullscreen": false,
}
```

### `viewer.local.jsonc`

```jsonc
// Vanta Viewer, this machine only. Any key from viewer.jsonc can go here too; this file wins.
{
}
```

### `viewer-keys.jsonc`

```jsonc
// Vanta Viewer keybindings. Each action takes a chord or a list of them, like "Ctrl+S"
// or ["0", "="]; an action left out keeps its default. Letters are written as they
// are typed on your layout ("ø", "æ" on a Danish keyboard).
//
// Lines starting with // are options left at their default: remove the // to use one.
{
  // "next":           ["Right", "space"],
  // "prev":           "Left",
  // "fit":            ["0", "="],
  // "oneToOne":       "1",
  // "rotateCW":       "]",
  // "rotateCCW":      "[",
  // "rotate180":      "r",
  // "exposureUp":     ".",
  // "exposureDown":   ",",
  // "exposureReset":  "/",
  // "crop":           "c",
  // "cropRatio":      "x",
  // "cropRatioPrev":  "z",
  // "save":           "Ctrl+S",
  // "saveAs":         "Ctrl+Shift+S",
  // "info":           "i",
  // "fullscreen":     "f",
  // "quit":           ["q", "Escape"],
}
```
