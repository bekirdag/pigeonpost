"""Small GTK building blocks shared by the window and its pages."""

import gi
gi.require_version("Gtk", "4.0")
gi.require_version("Adw", "1")
from gi.repository import Adw, Gdk, GLib, Gtk, Pango


def box(spacing=8, horizontal=False, margin=0):
    widget = Gtk.Box(orientation=Gtk.Orientation.HORIZONTAL if horizontal else Gtk.Orientation.VERTICAL, spacing=spacing)
    for edge in ("top", "bottom", "start", "end"):
        getattr(widget, "set_margin_" + edge)(margin)
    return widget


def label(text, style=None, wrap=False):
    widget = Gtk.Label(label=str(text), xalign=0)
    if style:
        widget.add_css_class(style)
    if wrap:
        widget.set_wrap(True)
        widget.set_wrap_mode(Pango.WrapMode.WORD_CHAR)
    else:
        widget.set_ellipsize(Pango.EllipsizeMode.END)
    return widget


def button(text, callback, icon=None, style=None):
    widget = Gtk.Button(label=text) if not icon else Gtk.Button(icon_name=icon)
    widget.set_tooltip_text(text)
    widget.update_property([Gtk.AccessibleProperty.LABEL], [text])
    if style:
        widget.add_css_class(style)
    widget.connect("clicked", lambda _: callback())
    return widget


def clear(widget):
    while widget.get_first_child():
        widget.remove(widget.get_first_child())


def address_row(address):
    row = box(4, horizontal=True)
    text = label(address, "dim-label", wrap=True)
    text.set_selectable(True)
    text.set_hexpand(True)
    row.append(text)

    def copied():
        copy.get_clipboard().set(address)
        copy.set_icon_name("emblem-ok-symbolic")
        copy.set_tooltip_text("Address copied")
        copy.update_property([Gtk.AccessibleProperty.LABEL], ["Address copied"])
        if copy.reset_source:
            GLib.source_remove(copy.reset_source)
        copy.reset_source = GLib.timeout_add_seconds(2, reset)

    def reset():
        copy.set_icon_name("edit-copy-symbolic")
        copy.set_tooltip_text("Copy address " + address)
        copy.update_property([Gtk.AccessibleProperty.LABEL], ["Copy address " + address])
        copy.reset_source = None
        return False

    copy = button("Copy address " + address, copied, "edit-copy-symbolic", "flat")
    copy.reset_source = None
    copy.set_sensitive(bool(address))
    copy.set_valign(Gtk.Align.CENTER)
    row.append(copy)
    return row


def scroll(child):
    widget = Gtk.ScrolledWindow(hexpand=True, vexpand=True)
    widget.set_policy(Gtk.PolicyType.NEVER, Gtk.PolicyType.AUTOMATIC)
    widget.set_child(child)
    return widget


def plain_row(row):
    """Rows show names people and servers chose: never parse them as Pango markup."""
    if hasattr(row, "set_use_markup"):
        row.set_use_markup(False)
    return row


def action_row(title="", subtitle=None, **props):
    row = plain_row(Adw.ActionRow(**props))
    row.set_title(title)
    if subtitle is not None:
        row.set_subtitle(subtitle)
    return row


def settings_row(title, detail, icon, callback):
    row = action_row(title=title, subtitle=detail, activatable=True)
    row.set_title_lines(0)
    row.set_subtitle_lines(0)
    row.add_prefix(Gtk.Image(icon_name=icon))
    row.add_suffix(Gtk.Image(icon_name="go-next-symbolic"))
    row.connect("activated", lambda _: callback())
    return row


def settings_group(content, *rows):
    group = Gtk.ListBox(selection_mode=Gtk.SelectionMode.NONE)
    group.add_css_class("boxed-list")
    for row in rows:
        group.append(row)
    content.append(group)


class SettingsNavigation:
    """One native window with a back stack, compatible with libadwaita 1.2."""
    def __init__(self, owner, title="Settings"):
        self.window = Adw.Window(title=title, transient_for=owner, modal=True,
                                 default_width=560, default_height=600)
        self.window.settings_navigation = self
        self.pages = []
        root = box(0)
        header = Adw.HeaderBar()
        self.heading = Adw.WindowTitle(title=title)
        header.set_title_widget(self.heading)
        self.back_button = button("Back", self.back, "go-previous-symbolic")
        header.pack_start(self.back_button)
        root.append(header)
        self.stack = Gtk.Stack(vexpand=True, transition_type=Gtk.StackTransitionType.SLIDE_LEFT_RIGHT)
        root.append(self.stack)
        self.window.set_content(root)
        keys = Gtk.EventControllerKey()
        keys.connect("key-pressed", self.key_pressed)
        self.window.add_controller(keys)
        owner.dialogs.append(self.window)
        self.window.connect("close-request", lambda w: owner.dialogs.remove(w) if w in owner.dialogs else None)
        self.content = self.push(title)

    def push(self, title):
        focus = self.window.get_focus()
        content = box(18, margin=24)
        view = scroll(content)
        self.pages.append((title, view, focus))
        self.stack.add_child(view)
        self.stack.set_visible_child(view)
        self.heading.set_title(title)
        self.window.set_title(title)
        self.back_button.set_visible(len(self.pages) > 1)
        GLib.idle_add(lambda: (content.child_focus(Gtk.DirectionType.TAB_FORWARD), False)[1])
        return content

    def back(self):
        if len(self.pages) < 2:
            return
        _, old, focus = self.pages.pop()
        title, view, _ = self.pages[-1]
        self.stack.set_visible_child(view)
        self.stack.remove(old)
        self.heading.set_title(title)
        self.window.set_title(title)
        self.back_button.set_visible(len(self.pages) > 1)
        if focus:
            focus.grab_focus()

    def key_pressed(self, _, key, _code, _state):
        if key != Gdk.KEY_Escape:
            return False
        if len(self.pages) > 1:
            self.back()
        else:
            self.window.close()
        return True
