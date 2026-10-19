"""The Pigeonpost conversation design for GTK: palette, type and the person's appearance choice.

Dark is the conversation design's own values; light is the same language re-derived for paper.
The values are apps/ios/Shared/Design/Theme.swift's, so every platform shares one palette.
Geist and Geist Mono (SIL OFL 1.1, see fonts/Geist-OFL.txt) are registered with fontconfig for
this process only; if that fails, the system sans and monospace faces stand in.
"""
import ctypes
import ctypes.util
import json
import os
from pathlib import Path

import gi
gi.require_version("Gtk", "4.0")
gi.require_version("Adw", "1")
from gi.repository import Adw, Gdk, GLib, Gtk

FONTS = Path(__file__).resolve().parent / "fonts"
CHOICES = ("system", "light", "dark")

# token: (light, dark)
PALETTE = {
    "accent": ("#2F6BE0", "#8AB4FF"),
    "on_accent": ("#FFFFFF", "#0D0F12"),
    "green": ("#1E8A50", "#7DD3A0"),
    "amber": ("#A35F00", "#F0B860"),
    "rose": ("#B0284B", "#F29AAE"),
    "ink": ("#14181F", "#E8EAED"),
    "body": ("#2E353F", "#D7DCE3"),
    "secondary": ("#5B6470", "#9AA3AE"),
    "muted": ("#737C88", "#7D8590"),
    "quiet": ("#3E4651", "#C3C9D1"),
    "app": ("#F3F4F6", "#0D0F12"),
    "sidebar": ("#F7F8FA", "#111419"),
    "column": ("#F4F5F8", "#13161B"),
    "wash": ("#FAFBFC", "#0F1216"),
    "ground": ("#FFFFFF", "#171B21"),
    "field": ("#ECEEF2", "#171B21"),
    "composer": ("#FFFFFF", "#151920"),
    "selected": ("#E6EAF0", "#1C2129"),
    "chip": ("#DFE3EA", "#252B34"),
    "tile": ("#E9ECF1", "#22272F"),
    "rule": ("#E3E6EB", "#1F242B"),
    "stroke": ("#DFE3E8", "#262B33"),
    "stroke_strong": ("#D3D8DF", "#2A3039"),
    "mine_fill": ("#EDF3FF", "#131C2E"),
    "mine_stroke": ("#C9D8F6", "#263556"),
    "mine_text": ("#14213D", "#E6ECF8"),
    "mine_meta": ("#5B6B8E", "#8C9AB8"),
    "held_text": ("#A35F00", "#F0B860"),
    "held_fill": ("#FCEFD3", "#2A2213"),
    "held_stroke": ("#F4DDAE", "#45361A"),
    "auto_text": ("#1E7A47", "#7DD3A0"),
    "auto_fill": ("#E2F4E9", "#142A1F"),
    "auto_stroke": ("#C4E8D2", "#1F4030"),
    "toast": ("#14181F", "#2A3039"),
}

STYLE = """
window, dialog, popover, .pp-root { font-family: "Geist", "Cantarell", "Inter", sans-serif; }
.pp-mono, .pp-peer, .pp-title, .pp-address { font-family: "Geist Mono", "DejaVu Sans Mono", monospace; }

window.background, .pp-root { background-color: @pp_app; color: @pp_ink; }
headerbar { background-color: @pp_app; color: @pp_ink; box-shadow: none; border-bottom: 1px solid @pp_rule; min-height: 46px; }
headerbar .title { font-weight: 600; font-size: 14px; }
headerbar .subtitle { font-family: "Geist Mono", monospace; font-size: 11.5px; color: @pp_muted; opacity: 1; }
paned > separator { background-color: @pp_rule; min-width: 1px; min-height: 1px; background-image: none; }

.pp-sidebar { background-color: @pp_sidebar; }
.pp-column { background-color: @pp_column; }
.pp-wash { background-color: @pp_wash; }
.pp-toolbar { border-bottom: 1px solid @pp_rule; }

.pp-overline { font-size: 11px; font-weight: 600; letter-spacing: 0.06em; color: @pp_muted; }
.pp-title { font-size: 15px; font-weight: 600; color: @pp_ink; }
.pp-meta { font-size: 11.5px; color: @pp_muted; opacity: 1; }
.pp-status { font-size: 11.5px; color: @pp_secondary; opacity: 1; }

dropdown.pp-mailbox > button { background-color: @pp_field; border: 1px solid @pp_stroke; border-radius: 10px; padding: 8px 10px; box-shadow: none; }
dropdown.pp-mailbox > button label { font-family: "Geist Mono", monospace; font-weight: 600; font-size: 13px; }

entry.search, searchentry, .pp-sidebar entry, .pp-wash entry {
  background-color: @pp_field; color: @pp_ink; border: 1px solid @pp_stroke; border-radius: 9px;
  min-height: 34px; box-shadow: none; outline-color: alpha(@pp_accent, 0.5);
}
searchentry > image, entry.search > image { color: @pp_muted; }

button { border-radius: 8px; }
button.flat, .pp-icon-button { color: @pp_secondary; }
button.flat:hover { background-color: @pp_selected; color: @pp_ink; }
button.suggested-action { background-color: @pp_accent; color: @pp_on_accent; border-radius: 9px; font-weight: 600; box-shadow: none; }
button.suggested-action:disabled { background-color: @pp_chip; color: @pp_muted; }
button.destructive-action { border-radius: 9px; }
.pp-pill-button { background-color: @pp_ground; border: 1px solid @pp_stroke_strong; color: @pp_quiet; border-radius: 8px; font-size: 12.5px; font-weight: 500; padding: 4px 10px; box-shadow: none; }
.pp-pill-button:hover { background-color: @pp_selected; color: @pp_ink; }

list.pp-list { background-color: transparent; }
list.pp-list > row { border-radius: 10px; margin: 1px 0; padding: 0; color: @pp_ink; }
list.pp-list > row:hover { background-color: alpha(@pp_selected, 0.7); }
list.pp-list > row:selected { background-color: @pp_selected; color: @pp_ink; }
.pp-peer { font-size: 13.5px; font-weight: 600; color: @pp_ink; }
.pp-preview { font-size: 12.5px; color: @pp_secondary; opacity: 1; }
.pp-subject { font-size: 13px; color: @pp_quiet; }
.pp-subject.heading { color: @pp_ink; font-weight: 600; }

.pp-bubble { background-color: @pp_ground; border: 1px solid @pp_stroke; border-radius: 14px; color: @pp_body; }
.pp-bubble.pp-mine { background-color: @pp_mine_fill; border-color: @pp_mine_stroke; color: @pp_mine_text; }
.pp-bubble .pp-meta { color: @pp_muted; }
.pp-bubble.pp-mine .pp-meta { color: @pp_mine_meta; }
.pp-body { font-size: 14.5px; line-height: 1.45; }
.pp-body selection, label.pp-body > selection { background-color: alpha(@pp_accent, 0.32); color: inherit; }
.pp-verb { font-size: 12.5px; font-weight: 600; border-radius: 6px; padding: 2px 7px; }
.pp-verb.pp-held { color: @pp_held_text; background-color: @pp_held_fill; border: 1px solid @pp_held_stroke; }
.pp-verb.pp-auto { color: @pp_auto_text; background-color: @pp_auto_fill; border: 1px solid @pp_auto_stroke; }
.pp-bubble button.flat { min-height: 26px; min-width: 26px; padding: 2px; color: @pp_muted; }
.pp-bubble button.flat:hover { color: @pp_ink; background-color: alpha(@pp_chip, 0.7); }
.pp-file { background-color: @pp_tile; border: 1px solid @pp_stroke; color: @pp_quiet; border-radius: 8px; font-size: 12.5px; box-shadow: none; }
.pp-file:hover { background-color: @pp_chip; color: @pp_ink; }
.pp-preview-button { padding: 0; border-radius: 10px; background: none; }
.pp-preview-button > frame, .pp-chip-preview { border-radius: 10px; border: 1px solid @pp_stroke; background-color: @pp_tile; }

.pp-composer { background-color: @pp_composer; border: 1px solid @pp_stroke_strong; border-radius: 14px; padding: 6px; }
.pp-composer scrolledwindow, .pp-composer textview, .pp-composer textview > text { background-color: transparent; color: @pp_ink; }
.pp-composer textview { font-size: 14px; }
.pp-composer button.suggested-action { min-width: 38px; min-height: 34px; }

.pp-segmented { background-color: @pp_field; border: 1px solid @pp_stroke; border-radius: 9px; padding: 2px; }
.pp-segmented > button { background: none; border: none; box-shadow: none; border-radius: 7px; color: @pp_secondary; font-weight: 500; min-height: 28px; padding: 0 14px; }
.pp-segmented > button:checked { background-color: @pp_chip; color: @pp_ink; }

.pp-viewer-text, .pp-viewer-text > text { background-color: @pp_ground; color: @pp_body; font-family: "Geist Mono", monospace; font-size: 13px; }
.pp-viewer-page { background-color: #FFFFFF; border: 1px solid @pp_stroke; }

toast { background-color: @pp_toast; color: #FFFFFF; border-radius: 10px; }
"""

_registered = None


def register_fonts():
    """Add the bundled faces to this process's fontconfig set. Safe to call more than once."""
    global _registered
    if _registered is not None:
        return _registered
    _registered = False
    try:
        library = ctypes.CDLL(ctypes.util.find_library("fontconfig") or "libfontconfig.so.1")
        library.FcConfigGetCurrent.restype = ctypes.c_void_p
        library.FcConfigAppFontAddFile.argtypes = [ctypes.c_void_p, ctypes.c_char_p]
        config = library.FcConfigGetCurrent()
        added = [library.FcConfigAppFontAddFile(config, str(path).encode()) for path in sorted(FONTS.glob("*.ttf"))]
        _registered = bool(added) and all(added)
    except (OSError, AttributeError):
        pass
    return _registered


def colours(dark):
    return {name: pair[1 if dark else 0] for name, pair in PALETTE.items()}


def stylesheet(dark):
    tokens = colours(dark)
    defined = "".join(f"@define-color pp_{name} {value};\n" for name, value in tokens.items())
    # libadwaita's own named colours, so stock widgets (dialogs, popovers, settings lists) match.
    named = {
        "accent_color": tokens["accent"], "accent_bg_color": tokens["accent"], "accent_fg_color": tokens["on_accent"],
        "window_bg_color": tokens["app"], "window_fg_color": tokens["ink"],
        "view_bg_color": tokens["wash"], "view_fg_color": tokens["body"],
        "headerbar_bg_color": tokens["app"], "headerbar_fg_color": tokens["ink"],
        "headerbar_backdrop_color": tokens["app"], "headerbar_border_color": tokens["rule"],
        "sidebar_bg_color": tokens["sidebar"], "sidebar_fg_color": tokens["ink"],
        "card_bg_color": tokens["ground"], "card_fg_color": tokens["body"],
        "dialog_bg_color": tokens["ground"], "dialog_fg_color": tokens["ink"],
        "popover_bg_color": tokens["ground"], "popover_fg_color": tokens["ink"],
        "success_color": tokens["green"], "warning_color": tokens["amber"],
    }
    defined += "".join(f"@define-color {name} {value};\n" for name, value in named.items())
    return defined + STYLE


class Design:
    """Install the stylesheet on the display and swap palettes when the appearance changes."""
    def __init__(self, display=None):
        register_fonts()
        self.display = display or Gdk.Display.get_default()
        self.provider = Gtk.CssProvider()
        self.manager = Adw.StyleManager.get_default()
        self.dark = None
        if self.display:
            Gtk.StyleContext.add_provider_for_display(self.display, self.provider, Gtk.STYLE_PROVIDER_PRIORITY_APPLICATION)
        self.manager.connect("notify::dark", lambda *_: self.refresh())
        self.refresh()

    def refresh(self):
        dark = self.manager.get_dark()
        if dark == self.dark:
            return
        self.dark = dark
        css = stylesheet(dark)
        try:
            self.provider.load_from_string(css)
        except AttributeError:  # GTK < 4.12
            self.provider.load_from_data(css, -1)


def config_path():
    return Path(GLib.get_user_config_dir()) / "pigeonpost-desktop" / "preferences.json"


class Preferences:
    """Appearance only. Never tokens, messages or drafts."""
    def __init__(self, path=None):
        self.path = Path(path) if path else config_path()
        self.values = {}
        try:
            loaded = json.loads(self.path.read_text())
            if isinstance(loaded, dict):
                self.values = loaded
        except (OSError, ValueError):
            pass

    @property
    def appearance(self):
        value = self.values.get("appearance")
        return value if value in CHOICES else "system"

    @appearance.setter
    def appearance(self, value):
        if value not in CHOICES:
            raise ValueError(value)
        self.values["appearance"] = value
        try:
            self.path.parent.mkdir(parents=True, exist_ok=True)
            temporary = self.path.with_suffix(".tmp")
            temporary.write_text(json.dumps({"appearance": value}))
            os.replace(temporary, self.path)
        except OSError:
            pass  # The choice still applies for this session.


def apply_appearance(choice):
    scheme = {"light": Adw.ColorScheme.FORCE_LIGHT, "dark": Adw.ColorScheme.FORCE_DARK}.get(choice, Adw.ColorScheme.DEFAULT)
    Adw.StyleManager.get_default().set_color_scheme(scheme)
