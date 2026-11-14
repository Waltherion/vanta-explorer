// Vanta colours from the Omarchy theme in use. Omarchy fills in the double-brace values every time
// the theme changes and writes the result to ~/.local/state/omarchy/current/theme/, which is
// the folder Vanta Explorer reads its theme from (see docs/omarchy.md). Edit this file, not
// the copy Omarchy writes: that one is replaced at the next theme switch.
{
  "color": {
    // "#rrggbb" is an opaque window. "#aarrggbb" (alpha first, e.g. "#d1{{ background_strip }}")
    // makes it translucent, which only looks right with blur turned on in Hyprland.
    "background": "{{ background }}",
    "text": "{{ foreground }}",
    "accent": "{{ accent }}",
    "border": "{{ accent }}",
    "highlight": "{{ accent }}",
    "error": "{{ red }}",
  },
  // "monospace" is the font Omarchy sets with its font menu.
  "font": {
    "family": "monospace",
  },
  // Square corners, like the rest of Omarchy. Explorer's window corners are Hyprland's, which
  // Omarchy already keeps square; this is for the other Vanta apps, should they join it.
  "shape": {
    "cornerRadius": 0,
  },
  "rainbow": {
    "stops": ["{{ red }}", "{{ yellow }}", "{{ green }}", "{{ cyan }}", "{{ blue }}", "{{ magenta }}"],
  },
}
