# Setting the apps up

What to put in a Hyprland config so each app starts, gets its keys and draws where it should.
Both config languages are shown: `hyprland.conf` first, then the `hyprland.lua` form for anyone
on the Lua fork. Pick the apps you installed; nothing here depends on the rest of the stack.

Every app writes its own settings file with a comment on each key the first time it starts, so
there is nothing to copy before the first run. The files land in `~/.config/vanta/`, and
[config.md](config.md) is the reference for what is in them.

## Vanta Explorer

A normal window. It is meant to float, and it asks for a hair of transparency so Hyprland blurs
what is behind it:

    windowrule = float, class:(vantaexplorer)
    windowrule = size 1840 1040, class:(vantaexplorer)
    windowrule = center, class:(vantaexplorer)
    windowrule = opacity 0.999, class:(vantaexplorer)

    bind = $mainMod, E, exec, vantaexplorer

```lua
hl.window_rule({
    name = "vantaexplorer",
    match = { class = "(vantaexplorer)" },
    float = true, size = { 1840, 1040 }, center = true, opacity = 0.999,
})

hl.bind(mainMod .. " + E", hl.dsp.exec_cmd("vantaexplorer"))
```

The package makes it a handler for folders. To make it the one that opens them:

    xdg-mime default vantaexplorer.desktop inode/directory

### As the file chooser other programs get

When a program asks the desktop for a file, a folder or a place to save, it can be Vanta Explorer
that comes up: the same places in the sidebar, the same theme, thumbnails and preview, cut down
to one pane with a bar along the bottom that says exactly what will be chosen. The package
installs what is needed (`vantaportal`, and its entries for xdg-desktop-portal and D-Bus); what
is left is to choose it, in `~/.config/xdg-desktop-portal/portals.conf`:

    [preferred]
    default=hyprland;gtk
    org.freedesktop.impl.portal.FileChooser=vanta;gtk

and then `systemctl --user restart xdg-desktop-portal`. `gtk` after `vanta` is the fallback, so
nothing is lost if Vanta Explorer is removed. The chooser is its own kind of window, which a
rule can float:

    windowrule = float, class:(vantaexplorer-chooser)
    windowrule = center, class:(vantaexplorer-chooser)
    windowrule = opacity 0.999, class:(vantaexplorer-chooser)

```lua
hl.window_rule({
    name = "vantaexplorer-chooser",
    match = { class = "(vantaexplorer-chooser)" },
    float = true, center = true, opacity = 0.999,
})
```

It remembers its own size, and the folder each program last chose from. Which programs ask the
portal is up to them: Flatpak apps always do, as do most Chromium and Electron programs; Firefox
does with `widget.use-xdg-desktop-portal.file-picker` set to 1, GTK programs with
`GTK_USE_PORTAL=1`, and Qt programs keep their own dialog unless their platform theme sends them
to the portal.

## Where everything lives

| What | Where |
|---|---|
| Settings | `~/.config/vanta/<app>.jsonc`, with `colors.jsonc` shared and `<app>.local.jsonc` on top |
| State | `~/.local/state/<app>/` |
| Caches | `~/.cache/<app>/` |
| Settings reference | `/usr/share/doc/<app>/config.md` |
| The theme in use | `~/.local/state/vanta/theme` |

A theme is a folder in `~/.config/vanta/themes/` with any of those same files in it, laid under
your own. Every app re-reads its files while it runs, so a theme switch is not a restart.
