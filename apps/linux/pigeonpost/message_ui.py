"""Shared widgets for message actions (docs/message-actions.md §6): ticks, the action row outside
the bubble, the reply quote and the composer's reply/edit bar. Used by direct conversations and
groups alike."""

import gi
gi.require_version("Gtk", "4.0")
from gi.repository import Gtk

from .theme import add_css
from .widgets import box, label

add_css("""
.pp-actions button { opacity: 0.7; }
.pp-actions button:hover, .pp-actions button:focus, .pp-message:hover .pp-actions button { opacity: 1; }
.pp-actions button { min-height: 30px; min-width: 30px; padding: 0; border-radius: 8px; color: @pp_muted; }
.pp-actions button:hover { color: @pp_ink; background-color: alpha(@pp_chip, 0.7); }
.pp-actions button image { -gtk-icon-size: 18px; }
.pp-tick { font-size: 11.5px; font-weight: 600; color: @pp_muted; }
.pp-tick.pp-seen, .pp-tick.pp-working { color: @pp_green; }
.pp-tick.pp-failed { color: @pp_rose; }
@keyframes pp-spin { from { -gtk-icon-transform: rotate(0turn); } to { -gtk-icon-transform: rotate(1turn); } }
image.pp-gear { color: @pp_green; -gtk-icon-size: 12px; animation: pp-spin 3s linear infinite; }
image.pp-gear.pp-still { animation: none; }
.pp-edited { font-size: 11px; font-style: italic; color: @pp_muted; }
.pp-quote { border-left: 3px solid alpha(@pp_accent, 0.6); padding: 2px 8px; border-radius: 4px; background-color: alpha(@pp_chip, 0.45); }
.pp-quote .pp-quote-from { font-size: 11.5px; font-weight: 600; color: @pp_quiet; }
.pp-quote .pp-quote-text { font-size: 12.5px; color: @pp_secondary; }
.pp-highlight { outline: 2px solid alpha(@pp_accent, 0.7); outline-offset: 2px; }
.pp-modebar { background-color: alpha(@pp_accent, 0.08); border: 1px solid alpha(@pp_accent, 0.3); border-radius: 10px; padding: 4px 4px 4px 10px; margin-left: 16px; margin-right: 16px; }
.pp-pending { background-color: alpha(@pp_amber, 0.08); border: 1px solid alpha(@pp_amber, 0.4); border-radius: 10px; padding: 6px 6px 6px 12px; margin-left: 16px; margin-right: 16px; }
""")


def reduced_motion():
    settings = Gtk.Settings.get_default()
    return bool(settings) and not settings.props.gtk_enable_animations


TICK_TEXT = {"sending": "◷", "sent": "✓", "delivered": "✓✓", "seen": "✓✓", "failed": "⚠"}


def tick_widget(state, accessible):
    """Inside the bubble, right of the time, on sent messages only."""
    holder = box(4, horizontal=True)
    holder.set_valign(Gtk.Align.CENTER)
    holder.tick_state = state
    if state == "working":
        gear = Gtk.Image(icon_name="emblem-system-symbolic")
        gear.add_css_class("pp-gear")
        holder.append(gear)
        if reduced_motion():
            gear.add_css_class("pp-still")
            text = label("Working…", "pp-tick")
            text.add_css_class("pp-working")
            holder.append(text)
    else:
        mark = label(TICK_TEXT.get(state, "✓"), "pp-tick")
        mark.add_css_class("pp-" + state)
        holder.append(mark)
    holder.set_tooltip_text(accessible)
    holder.update_property([Gtk.AccessibleProperty.LABEL], [accessible])
    return holder


def action_button(text, icon, clicked):
    widget = Gtk.Button(icon_name=icon)
    widget.add_css_class("flat")
    widget.set_tooltip_text(text)
    widget.update_property([Gtk.AccessibleProperty.LABEL], [text])
    widget.connect("clicked", lambda _: clicked())
    widget.action_name = text
    return widget


def action_row(outgoing, buttons):
    """The row below and outside the bubble, aligned to its side. `buttons`: [(text, icon, fn)]."""
    row = box(2, horizontal=True)
    row.add_css_class("pp-actions")
    row.set_halign(Gtk.Align.END if outgoing else Gtk.Align.START)
    for text, icon, clicked in buttons:
        row.append(action_button(text, icon, clicked))
    return row


def quote_block(sender, text, jump=None):
    """A reply's parent above the text; clicking it scrolls to the parent and highlights it."""
    content = box(2)
    content.add_css_class("pp-quote")
    content.append(label(sender, "pp-quote-from"))
    line = label(text or "Message unavailable", "pp-quote-text", wrap=True)
    line.set_lines(2)
    content.append(line)
    if jump is None:
        return content
    widget = Gtk.Button(child=content)
    widget.add_css_class("flat")
    widget.set_halign(Gtk.Align.FILL)
    widget.set_tooltip_text("Show the original message")
    widget.update_property([Gtk.AccessibleProperty.LABEL], [f"Reply to {sender}: {text}. Show the original message"])
    widget.connect("clicked", lambda _: jump())
    return widget


def mode_bar(title, detail, cancel):
    """'Replying to …' / 'Editing message' above the composer, with ✕ (Esc cancels too)."""
    bar = box(8, horizontal=True)
    bar.add_css_class("pp-modebar")
    text = box(1)
    text.set_hexpand(True)
    text.append(label(title, "pp-quote-from"))
    if detail:
        line = label(detail, "pp-quote-text")
        line.set_ellipsize(3)
        line.set_max_width_chars(60)
        text.append(line)
    bar.append(text)
    close = Gtk.Button(icon_name="window-close-symbolic")
    close.add_css_class("flat")
    close.set_tooltip_text("Cancel")
    close.update_property([Gtk.AccessibleProperty.LABEL], ["Cancel " + title.lower()])
    close.connect("clicked", lambda _: cancel())
    bar.append(close)
    return bar


def highlight(widget, seconds=2):
    from gi.repository import GLib
    widget.add_css_class("pp-highlight")
    GLib.timeout_add(int(seconds * 1000), lambda: (widget.remove_css_class("pp-highlight"), False)[1])
