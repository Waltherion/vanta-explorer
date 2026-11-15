# For an AI agent setting this up for someone

You are installing Vanta Explorer for a person who does not edit config files themselves. Read
these before you change anything, in full:

1. `README.md`: what this is and the install commands.
2. `docs/omarchy.md`: the whole setup on Omarchy, step by step. On Omarchy, follow it exactly.
3. `docs/config.md`, the `colors.jsonc` and "Vanta Explorer" sections: the settings, when the
   person asks for a change in look or behavior.
4. `docs/install.md`, the "Vanta Explorer" section: plain Hyprland without Omarchy.

## Rules

- **Check which system it is first.** On Omarchy, `omarchy-version` prints its version; the guide
  is written for Omarchy 4.5. On an older one, check that `omarchy-theme-refresh` exists and that
  `$OMARCHY_PATH/bin/omarchy-theme-set-templates` reads `~/.config/omarchy/themed` before relying
  on them.
- **Never edit anything under `$OMARCHY_PATH`** (`/usr/share/omarchy` on a normal install, or
  `~/.local/share/omarchy` on older ones). Omarchy owns it and an update replaces it.
  Everything you need goes in the person's own files: `~/.config/hypr/bindings.lua`,
  `~/.config/omarchy/themed/`, `~/.config/omarchy/hooks/`, `~/.config/vanta/`.
- **Never edit `~/.local/state/omarchy/current/theme/vanta-colors.jsonc`.** It is generated.
  Color and font changes go in `~/.config/omarchy/themed/vanta-colors.jsonc.tpl`, followed by
  `omarchy-theme-refresh`.
- **Copy a file before you edit it** (`cp file file.bak-$(date +%Y%m%d)`), and add to
  `bindings.lua` rather than rewriting it.
- **`bindings.lua` is Lua and Hyprland reads it on save.** One syntax error and keys stop working.
  After editing it, run `hyprctl configerrors`; it must print nothing (or "no errors").
- **Do not set up the file chooser** (`portals.conf`, `vantaportal`) unless the person asks for
  it. It is the newest and least tested part.
- **Do not remove Nautilus.** Omarchy uses it in places of its own.
- **Leave the icons alone** unless the person asks. Explorer brings its own and paints the
  folders in the theme's accent by itself. Do not set `"iconThemes"`, install icon themes or
  make one: the guide's "Omarchy's icons instead" section is the way to other icons.
- `makepkg -si` asks for the sudo password in the terminal. Let the person type it; never ask
  them to paste it to you.

## Check that it worked

Each of these should hold before you tell the person it is done:

```bash
pacman -Q vantaexplorer-git                                  # installed
cat ~/.local/state/vanta/theme                               # .../.local/state/omarchy/current/theme
grep -c '"#' ~/.local/state/omarchy/current/theme/vanta-colors.jsonc   # a number above 0: real colors
grep -c '{{' ~/.local/state/omarchy/current/theme/vanta-colors.jsonc   # 0: nothing left unfilled
xdg-mime query default inode/directory                       # vantaexplorer.desktop
hyprctl configerrors                                         # no errors
grep -s '^ *"iconThemes"' ~/.config/vanta/explorer.local.jsonc  # nothing, unless asked for
ls /usr/share/icons/vantaexplorer/index.theme               # Explorer's own icons are there
```

Then ask the person to press `Super + Shift + F` and to switch Omarchy theme once with the
Explorer window open: its colors should change within a second.

## When something is wrong

- **It does not start**: run `vantaexplorer` in a terminal and read what it prints. Errors about
  a missing `.so` library mean a system library was upgraded after the build: rebuild with
  `git pull && makepkg -si` in the repository folder.
- **Black or broken window**: `vantaexplorer --probe` prints what Vulkan offers and what was
  picked. A working Vulkan driver is required (`vulkan-radeon`, `vulkan-intel` or the NVIDIA
  driver).
- **Colors do not follow the theme**: check the four lines above about the theme, then run
  `omarchy-theme-refresh`.
- **More logging**: `RUST_LOG=debug vantaexplorer`.
