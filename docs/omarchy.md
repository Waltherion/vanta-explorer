# Vanta Explorer on Omarchy

Written for Omarchy 4.5. `makepkg` needs `base-devel` (`sudo pacman -S --needed base-devel git`). Every step is a command to run as your own user, except
`makepkg -si`, which asks for your password itself. Nothing here edits a file Omarchy owns:
everything goes in your own config, so an Omarchy update leaves it alone.

## 1. Install

```bash
git clone https://github.com/Waltherion/vanta-explorer ~/.local/src/vanta-explorer
cd ~/.local/src/vanta-explorer
makepkg -si
```

`makepkg` fetches the build tools it needs (Rust, shaderc and so on), builds, runs the tests and
installs the package `vantaexplorer-git`. The first build takes a few minutes.

To update later: the same `cd`, then `git pull && makepkg -si`.

## 2. Follow the Omarchy theme

Omarchy writes a set of config files for every theme it switches to, from templates. Your own
templates go in `~/.config/omarchy/themed/`, and Vanta Explorer reads the folder they are written
to. Three commands:

```bash
mkdir -p ~/.config/omarchy/themed ~/.local/state/vanta
cp /usr/share/doc/vantaexplorer/omarchy/vanta-colors.jsonc.tpl ~/.config/omarchy/themed/
echo "$HOME/.local/state/omarchy/current/theme" > ~/.local/state/vanta/theme
omarchy-theme-refresh
```

- The template becomes `~/.local/state/omarchy/current/theme/vanta-colors.jsonc` at every theme
  switch, with the theme's background, foreground, accent and red.
- `~/.local/state/vanta/theme` names the folder every Vanta app takes its theme from. Pointing it
  at Omarchy's current theme makes Explorer follow every switch, within a second, without a
  restart.
- `omarchy-theme-refresh` writes the file once now, for the theme already in use.

The icons are Omarchy's own (Yaru, in the colour each theme picks), and follow the theme by
themselves; there is nothing to set up for them.

The font is `monospace`, the one Omarchy's font menu sets. Explorer picks up a new font at the
next theme switch. To have it change at once, add a hook that nudges Explorer when the font
changes:

```bash
mkdir -p ~/.config/omarchy/hooks/font-set.d
printf '#!/bin/bash\ntouch "$HOME/.local/state/vanta/theme"\n' > ~/.config/omarchy/hooks/font-set.d/vantaexplorer
```

To change colours or the font for Explorer, edit `~/.config/omarchy/themed/vanta-colors.jsonc.tpl`
and run `omarchy-theme-refresh`. Never edit the copy in `~/.local/state/omarchy/current/theme/`:
it is replaced at the next switch. The keys are in `docs/config.md`. A file of your own,
`~/.config/vanta/explorer.jsonc`, wins over the theme for every theme at once (for example a
fixed font size).

## 3. Use it instead of Nautilus

**The keys.** Omarchy opens Nautilus with `Super + Shift + F` and, in the folder of the terminal
you are in, `Super + Alt + Shift + F`. Add to `~/.config/hypr/bindings.lua`:

```lua
o.rebind("SUPER + SHIFT + F", "File manager", { launch = "vantaexplorer" })
o.rebind("SUPER + ALT + SHIFT + F", "File manager (cwd)", { launch = 'vantaexplorer "$(omarchy-cmd-terminal-cwd)"' })
```

Hyprland reloads the file on save.

**Opening folders.** So that "open folder" in other programs (a browser's downloads, for
instance) opens Explorer:

```bash
xdg-mime default vantaexplorer.desktop inode/directory
```

Nautilus stays installed. Omarchy uses it in a few places of its own (the Dropbox panel's
"show in folder", for one); removing it is not needed and not recommended.

## Optional

**Translucency.** Omarchy has blur turned off, so the template gives Explorer an opaque
background. With blur turned on in Hyprland, a background with alpha in front (`#d1` before the
colour, see the comment in the template) makes it translucent, and a window rule makes Hyprland
blur behind it, in `~/.config/hypr/hyprland.lua` (or any file it loads):

```lua
o.window("vantaexplorer", { opacity = 0.999 })
```

**Floating.** Omarchy tiles Nautilus, and Explorer tiles the same way. To have it float
instead:

```lua
o.window("vantaexplorer", { float = true })
o.window("vantaexplorer", { center = true })
o.window("vantaexplorer", { size = { 1600, 950 } })
```

**As the file chooser** other programs get: see "As the file chooser other programs get" in
`docs/install.md`. This is newer than the rest and less tested; leave it out until Explorer
itself has been in use for a while.

## Undo

```bash
sudo pacman -R vantaexplorer-git
rm ~/.config/omarchy/themed/vanta-colors.jsonc.tpl ~/.config/omarchy/hooks/font-set.d/vantaexplorer
rm -r ~/.local/state/vanta
xdg-mime default org.gnome.Nautilus.desktop inode/directory
```

and remove the lines added to `~/.config/hypr/bindings.lua`. Explorer's own settings and state
are in `~/.config/vanta/` and `~/.local/state/vantaexplorer/`.

## Light themes

Explorer is built for dark themes and has not been tried with a light one (Catppuccin Latte,
Flexoki Light, White). It will take the light background and dark text from the template, but
parts of it may stay dark.
