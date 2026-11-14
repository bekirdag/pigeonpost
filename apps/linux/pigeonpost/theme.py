"""The Pigeonpost Design Kit for GTK: palette, type and the person's appearance choice.

Colours come from the kit's tokens (design_tokens.json is a copy of the kit's generated tokens.json,
tokens v1.0): each name below maps onto a kit semantic role, with its light and dark values, so the
Linux app shares one palette with the web, Apple, Android and Windows clients. Accent is Signal Blue.
Geist and Geist Mono (SIL OFL 1.1, see fonts/Geist-OFL.txt) are registered with fontconfig for
this process only; if that fails, the system sans and monospace faces stand in.
"""
import ctypes
import ctypes.util
import json
import os
import re
from pathlib import Path

import gi
gi.require_version("Gtk", "4.0")
gi.require_version("Adw", "1")
from gi.repository import Adw, Gdk, GLib, Gtk

FONTS = Path(__file__).resolve().parent / "fonts"
TOKENS = Path(__file__).resolve().parent / "design_tokens.json"
CHOICES = ("system", "light", "dark")

# GTK name -> kit semantic role (css/tokens.css --pp-<role>). A (role, opacity) pair is that role's
# colour at that opacity over the role named third (how the kit's color-mix() reads on its fill).
ROLES = {
    "accent": "accent",
    "accent_hover": "accent-hover",
    "on_accent": "on-accent",
    "link": "text-link",
    "focus_ring": "focus-ring",
    "green": "success",
    "amber": "warning",
    "rose": "danger",
    "ink": "text",
    "body": "text-body",
    "secondary": "text-muted",
    "muted": "text-faint",
    "quiet": "text-body",
    "app": "bg",
    "sidebar": "bg-subtle",
    "column": "bg-subtle",
    "wash": "bg",
    "ground": "surface",
    "raised": "surface-raised",
    "field": "surface-sunken",
    "composer": "surface",
    "hover": "surface-hover",
    "selected": "surface-selected",
    "chip": "surface-hover",
    "tile": "surface-sunken",
    "rule": "border",
    "stroke": "border",
    "stroke_strong": "border-strong",
    "theirs_fill": "bubble-theirs",
    "theirs_stroke": "bubble-theirs-border",
    "theirs_text": "bubble-theirs-text",
    "mine_fill": "bubble-mine",
    "mine_stroke": "bubble-mine-border",
    "mine_text": "bubble-mine-text",
    "mine_meta": ("bubble-mine-text", 0.62, "bubble-mine"),
    "agent_rail": "bubble-agent-rail",
    "tick_sent": "tick-sent",
    "tick_seen": "tick-seen",
    "tick_working": "tick-working",
    "held_text": "warning",
    "held_fill": "warning-fill",
    "held_stroke": "warning-border",
    "auto_text": "success",
    "auto_fill": "success-fill",
    "auto_stroke": "success-border",
    "toast": "surface-inverse",
    "on_toast": "on-inverse",
}


def _mix(top, opacity, under):
    a = [int(top[i:i + 2], 16) for i in (1, 3, 5)]
    b = [int(under[i:i + 2], 16) for i in (1, 3, 5)]
    return "#" + "".join(f"{round(x * opacity + y * (1 - opacity)):02X}" for x, y in zip(a, b))


def load_palette(path=TOKENS):
    """{name: (light, dark)} from the kit's tokens.json."""
    semantic = json.loads(Path(path).read_text())["semantic"]
    palette = {}
    for name, role in ROLES.items():
        if isinstance(role, tuple):
            top, opacity, under = role
            palette[name] = tuple(_mix(semantic[top][m], opacity, semantic[under][m]) for m in ("light", "dark"))
        else:
            palette[name] = (semantic[role]["light"], semantic[role]["dark"])
    # GTK's CSS parser wants a leading zero on alpha values ("rgba(8, 108, 255, .35)" -> "0.35").
    return {name: tuple(re.sub(r"(?<![\d.])\.(\d)", r"0.\1", v) for v in pair) for name, pair in palette.items()}


# token: (light, dark)
PALETTE = load_palette()

STYLE = """
window, dialog, popover, .pp-root { font-family: "Geist", "Cantarell", "Inter", sans-serif; }
.pp-mono, .pp-peer, .pp-title, .pp-address { font-family: "Geist Mono", "DejaVu Sans Mono", monospace; }

window.background, .pp-root { background-color: @pp_app; color: @pp_ink; }
headerbar { background-color: @pp_ground; color: @pp_ink; box-shadow: none; border-bottom: 1px solid @pp_rule; min-height: 46px; }
headerbar .title { font-weight: 600; font-size: 14px; }
headerbar .subtitle { font-family: "Geist Mono", monospace; font-size: 11.5px; color: @pp_muted; opacity: 1; }
paned > separator { background-color: @pp_rule; min-width: 1px; min-height: 1px; background-image: none; }

.pp-sidebar { background-color: @pp_sidebar; }
.pp-column { background-color: @pp_column; }
.pp-wash { background-color: @pp_wash; }
.pp-toolbar { border-bottom: 1px solid @pp_rule; }

.pp-overline { font-size: 11.5px; font-weight: 600; letter-spacing: 0.06em; color: @pp_muted; }
.pp-title { font-size: 15px; font-weight: 600; color: @pp_ink; }
.pp-meta { font-size: 11.5px; color: @pp_muted; opacity: 1; font-feature-settings: "tnum"; }
.pp-status { font-size: 11.5px; color: @pp_secondary; opacity: 1; }

dropdown.pp-mailbox > button { background-color: @pp_ground; border: 1px solid @pp_stroke_strong; border-radius: 10px; padding: 8px 10px; box-shadow: none; }
dropdown.pp-mailbox > button label { font-family: "Geist Mono", monospace; font-weight: 600; font-size: 13px; }

entry.search, searchentry, .pp-sidebar entry, .pp-wash entry {
  background-color: @pp_field; color: @pp_ink; border: 1px solid transparent; border-radius: 10px;
  min-height: 36px; box-shadow: none; outline-color: @pp_focus_ring;
}
entry.search:focus-within, searchentry:focus-within, .pp-sidebar entry:focus-within, .pp-wash entry:focus-within {
  border-color: @pp_accent; background-color: @pp_ground;
}
searchentry > image, entry.search > image { color: @pp_muted; }
entry > text > placeholder, textview > text > placeholder { color: @pp_muted; }

button { border-radius: 10px; }
button.flat, .pp-icon-button { color: @pp_secondary; }
button.flat:hover { background-color: @pp_hover; color: @pp_ink; }
button.suggested-action { background-color: @pp_accent; color: @pp_on_accent; border-radius: 10px; font-weight: 600; box-shadow: none; }
button.suggested-action:hover { background-color: @pp_accent_hover; }
button.suggested-action:disabled { background-color: @pp_chip; color: @pp_muted; }
button.destructive-action { border-radius: 10px; }
.pp-pill-button { background-color: @pp_ground; border: 1px solid @pp_stroke_strong; color: @pp_quiet; border-radius: 8px; font-size: 12.5px; font-weight: 500; padding: 4px 10px; box-shadow: none; }
.pp-pill-button:hover { background-color: @pp_hover; color: @pp_ink; }

list.pp-list { background-color: transparent; }
list.pp-list > row { border-radius: 14px; margin: 1px 0; padding: 0; color: @pp_ink; }
list.pp-list > row:hover { background-color: @pp_hover; }
list.pp-list > row:selected { background-color: @pp_selected; color: @pp_ink; }
.pp-peer { font-size: 14px; font-weight: 600; color: @pp_ink; }
.pp-preview { font-size: 13.5px; color: @pp_muted; opacity: 1; }
.pp-subject { font-size: 13px; color: @pp_quiet; }
.pp-subject.heading { color: @pp_ink; font-weight: 600; }

/* Kit bubbles: 18 px radius, the corner toward the sender 6 px; theirs neutral, mine Signal Blue tinted. */
.pp-bubble { background-color: @pp_theirs_fill; border: 1px solid @pp_theirs_stroke; border-radius: 18px; border-bottom-left-radius: 6px; color: @pp_theirs_text; }
.pp-bubble.pp-mine { background-color: @pp_mine_fill; border-color: @pp_mine_stroke; color: @pp_mine_text; border-bottom-left-radius: 18px; border-bottom-right-radius: 6px; }
.pp-bubble .pp-meta { color: @pp_muted; }
.pp-bubble.pp-mine .pp-meta { color: @pp_mine_meta; }
.pp-body { font-size: 15px; line-height: 1.4667; }
.pp-body selection, label.pp-body > selection { background-color: alpha(@pp_accent, 0.32); color: inherit; }
.pp-verb { font-size: 12.5px; font-weight: 600; border-radius: 6px; padding: 2px 7px; }
.pp-verb.pp-held { color: @pp_held_text; background-color: @pp_held_fill; border: 1px solid @pp_held_stroke; }
.pp-verb.pp-auto { color: @pp_auto_text; background-color: @pp_auto_fill; border: 1px solid @pp_auto_stroke; }
.pp-bubble button.flat { min-height: 26px; min-width: 26px; padding: 2px; color: @pp_muted; }
.pp-bubble button.flat:hover { color: @pp_ink; background-color: alpha(@pp_chip, 0.7); }
.pp-file { background-color: @pp_tile; border: 1px solid @pp_stroke; color: @pp_quiet; border-radius: 8px; font-size: 12.5px; box-shadow: none; }
.pp-file:hover { background-color: @pp_chip; color: @pp_ink; }
.pp-preview-button { padding: 0; border-radius: 10px; background: none; }
.pp-system { font-size: 12px; color: @pp_muted; }
.pp-system.pp-security { color: @pp_ink; font-weight: 500; }
.pp-call-missed .title, .pp-call-missed .subtitle { color: @error_color; }
.pp-banner { background-color: alpha(@error_color, 0.08); border: 1px solid alpha(@error_color, 0.35); border-radius: 10px; margin-left: 16px; margin-right: 16px; padding: 4px 4px 4px 12px; }
.pp-badge { font-size: 11.5px; font-weight: 600; color: @pp_on_accent; background-color: @pp_accent; border-radius: 10px; padding: 0 6px; min-height: 20px; min-width: 8px; font-feature-settings: "tnum"; }
.pp-preview-button > frame, .pp-chip-preview { border-radius: 10px; border: 1px solid @pp_stroke; background-color: @pp_tile; }

/* Kit composer: a 24 px box that takes a Signal Blue edge while focused; the send button is a pill. */
.pp-composer { background-color: @pp_composer; border: 1px solid @pp_stroke; border-radius: 24px; padding: 6px; }
.pp-composer:hover { border-color: @pp_stroke_strong; }
.pp-composer:focus-within { border-color: @pp_accent; }
.pp-composer scrolledwindow, .pp-composer textview, .pp-composer textview > text { background-color: transparent; color: @pp_ink; }
.pp-composer textview { font-size: 15px; }
.pp-composer button.suggested-action { min-width: 40px; min-height: 40px; border-radius: 999px; padding: 0 14px; }

.pp-segmented { background-color: @pp_field; border: 1px solid @pp_stroke; border-radius: 10px; padding: 2px; }
.pp-segmented > button { background: none; border: none; box-shadow: none; border-radius: 7px; color: @pp_secondary; font-weight: 500; min-height: 28px; padding: 0 14px; }
.pp-segmented > button:checked { background-color: @pp_ground; color: @pp_ink; box-shadow: 0 1px 2px alpha(black, 0.08); }

.pp-viewer-text, .pp-viewer-text > text { background-color: @pp_ground; color: @pp_body; font-family: "Geist Mono", monospace; font-size: 13px; }
.pp-viewer-page { background-color: #FFFFFF; border: 1px solid @pp_stroke; }

toast { background-color: @pp_toast; color: @pp_on_toast; border-radius: 10px; }
"""

_registered = None

# Feature modules add their own rules here at import time (theme.add_css); they use the tokens above.
EXTRA_CSS = []


def add_css(text):
    if text not in EXTRA_CSS:
        EXTRA_CSS.append(text)


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
        "accent_color": tokens["link"], "accent_bg_color": tokens["accent"], "accent_fg_color": tokens["on_accent"],
        "window_bg_color": tokens["app"], "window_fg_color": tokens["ink"],
        "view_bg_color": tokens["wash"], "view_fg_color": tokens["body"],
        "headerbar_bg_color": tokens["app"], "headerbar_fg_color": tokens["ink"],
        "headerbar_backdrop_color": tokens["app"], "headerbar_border_color": tokens["rule"],
        "sidebar_bg_color": tokens["sidebar"], "sidebar_fg_color": tokens["ink"],
        "card_bg_color": tokens["ground"], "card_fg_color": tokens["body"],
        "dialog_bg_color": tokens["raised"], "dialog_fg_color": tokens["ink"],
        "popover_bg_color": tokens["raised"], "popover_fg_color": tokens["ink"],
        "success_color": tokens["green"], "warning_color": tokens["amber"], "error_color": tokens["rose"],
        "destructive_color": tokens["rose"],
    }
    defined += "".join(f"@define-color {name} {value};\n" for name, value in named.items())
    return defined + STYLE + "".join(EXTRA_CSS)


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
