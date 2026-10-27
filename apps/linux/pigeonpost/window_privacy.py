"""Blocking, Settings → Privacy & filtering, Settings → Blocked and Settings → Profile picture
(SDS §13, §15, §16). Mixed into the main window; the web inbox is the reference."""

import math

import cairo
from pathlib import Path
from urllib.parse import urlencode

from gi.repository import Adw, Gdk, GdkPixbuf, GLib, Gtk

from .api import APIError
from .actions import RECEIPTS
from .features import (AVATAR_VISIBILITY, BLOCK_BODY, CALL_ADMISSION, CALL_HISTORY, MESSAGE_FILTERING, avatar_error,
                       policy_rows, policy_value, square_crop, wildcard_for)
from .model import display_name, valid_peer
from .widgets import action_row, box, button, clear, label

AVATAR_UPLOAD_SIDE = 1024
AVATAR_SOURCE_BYTES = 50 * 1024 * 1024
AVATAR_SOURCE_PIXELS = 60_000_000


def segmented(options, active, changed, sensitive=True):
    strip = box(0, horizontal=True)
    strip.add_css_class("pp-segmented")
    strip.add_css_class("linked")
    strip.set_halign(Gtk.Align.START)
    first, toggles = None, {}
    for value, text in options:
        toggle = Gtk.ToggleButton(label=text, active=str(active) == str(value))
        toggle.set_sensitive(sensitive)
        if first:
            toggle.set_group(first)
        first = first or toggle
        toggle.connect("toggled", lambda b, v=value: changed(v) if b.get_active() else None)
        toggles[value] = toggle
        strip.append(toggle)
    strip.toggles = toggles
    return strip


class PrivacyMixin:
    # ---- blocking --------------------------------------------------------------------------

    def ask_block(self, peer, name=None, message_id=None, call_id=None, report=False, done=None):
        """The confirmation sheet of SDS §15.2, from every place Block appears."""
        if not peer or not self.identity:
            return
        name = name or display_name(peer)
        dialog = Adw.MessageDialog.new(self, f"Block {name}?", BLOCK_BODY)
        extra = box(6)
        report_check = Gtk.CheckButton(label="Also report as spam", active=report)
        extra.append(report_check)
        wide = wildcard_for(peer)
        wide_check = None
        if wide:
            wide_check = Gtk.CheckButton(label="Block everyone at " + wide[:-2])
            extra.append(wide_check)
        dialog.set_extra_child(extra)
        dialog.add_response("cancel", "Cancel")
        dialog.add_response("block", "Block")
        dialog.set_response_appearance("block", Adw.ResponseAppearance.DESTRUCTIVE)
        dialog.set_default_response("cancel")
        dialog.set_close_response("cancel")
        generation = self.generation
        dialog.block_controls = (report_check, wide_check)

        def answered(_, response):
            if response != "block" or generation != self.generation:
                return
            target = wide if wide_check and wide_check.get_active() else peer
            self.block_address(target, report=report_check.get_active(), peer=peer, message_id=message_id, call_id=call_id, done=done)
        dialog.connect("response", answered)
        dialog.present()
        return dialog

    def block_address(self, target, report=False, peer=None, message_id=None, call_id=None, done=None):
        identity = self.identity
        existing = next((c for c in self.contacts if c.get("peer") == target), {})
        row = {"peer": target, "alias": existing.get("alias") or None, "admission": "block", "autonomy": "review", "allowed_verbs": []}
        if report and not message_id and not call_id:
            message_id = next((m.get("message_id") for m in reversed(self.messages + self.filtered)
                               if m.get("direction") != "out" and peer in (m.get("peer_handle"), m.get("peer"), m.get("from"))), None)

        def work():
            self.api.call("PUT", "/v1/contacts", identity, data=row)
            reported = None
            if report and (message_id or call_id):
                try:
                    self.api.call("POST", "/v1/report-spam", identity, data={"call_id": call_id} if call_id else {"message_id": message_id})
                    reported = True
                except APIError:
                    reported = False
            return reported

        def blocked(reported):
            self.revision += 1
            self.contacts = [c for c in self.contacts if c.get("peer") != target] + [row]
            self.filtered = [m for m in self.filtered if not self.blocked(self.filtered_key(m))]
            self.toast("Blocked and reported as spam." if reported else "Blocked, but the spam report could not be sent."
                       if reported is False else "Blocked. They won’t be able to message or call you.")
            self.render_all()
            if done:
                done()
        self._work(work, blocked, lambda error: self.toast("Could not block. " + self.error_text(error)))

    def unblock(self, peer, done=None):
        identity = self.identity

        def unblocked(_):
            self.revision += 1
            self.contacts = [c for c in self.contacts if c.get("peer") != peer]
            self.toast("Unblocked " + peer + ".")
            self.render_all()
            if done:
                done()
        self._work(lambda: self.api.call("DELETE", "/v1/contacts", identity, query={"peer": peer}), unblocked,
                   lambda error: self.toast("Could not unblock " + peer + ". " + self.error_text(error)))

    def blocked(self, peer):
        from .features import is_blocked
        return is_blocked(peer, self.contacts)

    # ---- Settings → Privacy & filtering ------------------------------------------------------

    def privacy_available(self):
        return bool({CALL_ADMISSION, MESSAGE_FILTERING, CALL_HISTORY, RECEIPTS} & self.features)

    def privacy_settings(self, navigation):
        content = navigation.push("Privacy & filtering")
        content.append(label("These settings belong to the mailbox you are using now and apply on every device.", "dim-label", wrap=True))
        rows = box(18)
        content.append(rows)
        status = label("Loading…", "dim-label", wrap=True)
        content.append(status)
        identity = self.identity
        state = {"policy": None}

        def render():
            clear(rows)
            if RECEIPTS in self.features:
                rows.append(self.receipts_switch(state, save))
            for row in policy_rows(self.features):
                section = box(6)
                section.append(label(row["title"], "heading", wrap=True))
                current = state["policy"].get(row["field"]) if state["policy"] else None
                strip = segmented(row["options"], current, lambda value, field=row["field"]: save(field, value),
                                  sensitive=state["policy"] is not None)
                strip.set_name("policy-" + row["field"])
                section.append(strip)
                if row["note"]:
                    section.append(label(row["note"], "dim-label", wrap=True))
                rows.append(section)

        def loaded(policy):
            state["policy"] = policy
            status.set_text("")
            render()

        def save(field, raw):
            if not state["policy"] or str(state["policy"].get(field)) == str(raw):
                return
            value = policy_value(field, raw)
            previous = state["policy"].get(field)
            state["policy"] = {**state["policy"], field: value}

            def saved(body):
                if isinstance(body, dict) and field in body:
                    state["policy"] = {**state["policy"], **body}
                self.toast("Saved.")

            def failed(error):
                state["policy"] = {**state["policy"], field: previous}
                status.set_text("Could not save that setting. Please try again.")
                render()
            self._work(lambda: self.api.call("PUT", "/v1/policy", identity, data={field: value}), saved, failed)
        render()
        self._work(lambda: self.api.call("GET", "/v1/policy", identity), loaded,
                   lambda _: status.set_text("Could not load these settings. Go back and try again."))
        navigation.privacy_rows = rows
        return content

    # ---- Settings → Blocked ------------------------------------------------------------------

    def blocked_settings(self, navigation):
        content = navigation.push("Blocked")
        content.append(label("Blocked people can’t message or call this mailbox, and aren’t told.", "dim-label", wrap=True))
        add = box(6, horizontal=True)
        entry = Gtk.Entry(placeholder_text="/k/…, /spamco/agent or /spamco/*", hexpand=True)
        add.append(entry)
        content.append(add)
        error = label("", "error", wrap=True)
        content.append(error)
        listing = Gtk.ListBox(selection_mode=Gtk.SelectionMode.NONE)
        listing.add_css_class("boxed-list")
        content.append(listing)
        identity = self.identity

        def render(rows):
            clear(listing)
            if rows is None:
                listing.append(action_row(title="Loading…"))
                return
            if not rows:
                listing.append(action_row(title="Nobody is blocked."))
                return
            for contact in rows:
                peer = contact["peer"]
                row = action_row(title=peer, subtitle="Everyone at " + peer[:-2] if peer.endswith("/*") else (contact.get("alias") or "Blocked"))
                row.set_title_lines(0)
                undo = button("Unblock", lambda p=peer: self.unblock(p, reload))
                undo.set_valign(Gtk.Align.CENTER)
                undo.update_property([Gtk.AccessibleProperty.LABEL], ["Unblock " + peer])
                row.add_suffix(undo)
                listing.append(row)

        def reload():
            render(None)
            self._work(lambda: self.api.call("GET", "/v1/contacts", identity, query={"admission": "block"}),
                       lambda body: render([c for c in body.get("contacts") or [] if c.get("admission") == "block"]),
                       lambda _: (error.set_text("Could not refresh the list. Showing what was last loaded."),
                                  render([c for c in self.contacts if c.get("admission") == "block"])))

        def add_block():
            peer = entry.get_text().strip()
            if peer and not peer.startswith("/"):
                peer = "/" + peer
            current = next((r for r in self.mailboxes if r["address"] == identity), {})
            if not valid_peer(peer, wildcard=True):
                error.set_text("Enter an address such as /k/…, /spamco/agent or /spamco/*.")
                return
            if peer in (current.get("address"), current.get("handle")):
                error.set_text("You cannot block this mailbox itself.")
                return
            error.set_text("")
            entry.set_text("")
            self.block_address(peer, done=reload)
        entry.connect("activate", lambda _: add_block())
        add.append(button("Block", add_block, style="destructive-action"))
        reload()
        navigation.blocked_list = listing
        return content

    # ---- Settings → Profile picture ----------------------------------------------------------

    def profile_settings(self, navigation):
        content = navigation.push("Profile picture")
        identity = self.identity
        current = next((r for r in self.mailboxes if r["address"] == identity), {})
        key = current.get("handle") or identity
        state = {"avatar": None, "visibility": "known"}
        holder = box(0, horizontal=True)
        holder.set_halign(Gtk.Align.START)
        content.append(holder)
        note = label("Loading…", "dim-label", wrap=True)
        content.append(note)
        content.append(label("Who can see it", "heading"))
        visibility_holder = box(0)
        content.append(visibility_holder)
        all_mailboxes = Gtk.CheckButton(label="Use for all my mailboxes")
        all_mailboxes.set_visible(len(self.mailboxes) > 1)
        content.append(all_mailboxes)
        actions = box(6, horizontal=True)
        content.append(actions)
        remove = button("Remove picture", lambda: remove_avatar(), style="destructive-action")
        actions.append(button("Choose picture…", lambda: self.choose_avatar(state, all_mailboxes.get_active(), refresh), style="suggested-action"))
        actions.append(remove)
        content.append(label("People see your picture in conversations, groups and calls. Initials on a colour are shown "
                             "when there is none, or when they may not see it. Copies already downloaded cannot be recalled.",
                             "dim-label", wrap=True))

        def render():
            clear(holder)
            avatar = Adw.Avatar.new(96, display_name(key), True)
            texture = getattr(self, "own_avatar_texture", None)
            if state["avatar"] and texture is not None:
                avatar.set_custom_image(texture)
            holder.append(avatar)
            clear(visibility_holder)
            visibility_holder.append(segmented(AVATAR_VISIBILITY, state["visibility"], set_visibility))
            remove.set_visible(bool(state["avatar"]))

        def loaded(result):
            avatar, data = result
            state["avatar"] = avatar
            if avatar and avatar.get("visibility"):
                state["visibility"] = avatar["visibility"]
            self.own_avatar_texture = None
            if data:
                try:
                    self.own_avatar_texture = Gdk.Texture.new_from_bytes(GLib.Bytes.new(data))
                except GLib.Error:
                    pass
            note.set_text("This picture belongs to the mailbox you are using now." if avatar else "No picture yet. People see your initials.")
            render()

        def fetch():
            avatar = self.api.call("GET", "/v1/profile/avatar", identity).get("avatar")
            data = None
            from .features import avatar_url
            url = avatar_url(avatar)
            if url:
                data = self.api.call("GET", url, binary=True)
            return avatar, data

        def refresh():
            note.set_text("Loading…")
            self._work(fetch, loaded, lambda _: note.set_text("Could not load your picture. Try again later."))

        def set_visibility(value):
            if value == state["visibility"]:
                return
            previous = state["visibility"]
            state["visibility"] = value
            if not state["avatar"]:
                return  # applied with the next upload

            def saved(body):
                state["avatar"] = body.get("avatar") or state["avatar"]
                self.avatars.forget([current.get("handle"), identity])
                self.toast("Picture visibility saved.")

            def failed(_):
                state["visibility"] = previous
                self.toast("Could not change who can see your picture.")
                render()
            self._work(lambda: self.api.call("PATCH", "/v1/profile/avatar", identity, data={"visibility": value}), saved, failed)

        def remove_avatar():
            def removed(_):
                state["avatar"] = None
                self.own_avatar_texture = None
                self.avatars.forget([current.get("handle"), identity])
                note.set_text("Picture removed. People see your initials.")
                render()
            self._work(lambda: self.api.call("DELETE", "/v1/profile/avatar", identity), removed,
                       lambda _: self.toast("Could not remove your picture."))
        render()
        refresh()
        navigation.profile_state = state
        return content

    def choose_avatar(self, state, all_mailboxes, refresh):
        chooser = Gtk.FileChooserNative.new("Choose a picture", self, Gtk.FileChooserAction.OPEN, "Choose", "Cancel")
        pictures = Gtk.FileFilter()
        pictures.set_name("Pictures")
        for mime in ("image/jpeg", "image/png", "image/webp", "image/heic", "image/heif"):
            pictures.add_mime_type(mime)
        chooser.add_filter(pictures)

        def chosen(dialog, response):
            file = dialog.get_file() if response == Gtk.ResponseType.ACCEPT else None
            dialog.destroy()
            if not file:
                return
            path = file.get_path()

            def read():
                if not path or not Path(path).is_file():
                    raise OSError("not a local file")
                if Path(path).stat().st_size > AVATAR_SOURCE_BYTES:
                    raise ValueError("That picture is too large.")
                info = GdkPixbuf.Pixbuf.get_file_info(path)
                if not info or not info[0] or info[1] * info[2] > AVATAR_SOURCE_PIXELS:
                    raise ValueError("That picture could not be opened.")
                pixbuf = GdkPixbuf.Pixbuf.new_from_file(path)
                return pixbuf.apply_embedded_orientation() or pixbuf

            def opened(pixbuf):
                CropDialog(self, pixbuf, lambda jpeg: self.upload_avatar(jpeg, state["visibility"], all_mailboxes, refresh))

            def failed(error):
                heic = path and path.lower().endswith((".heic", ".heif"))
                self.toast("This system cannot open HEIC photos. Save it as JPEG first, then choose it again." if heic
                           else str(error) if isinstance(error, ValueError) else "That picture could not be opened.")
            self._work(read, opened, failed)
        chooser.connect("response", chosen)
        chooser.show()
        return chooser

    def upload_avatar(self, jpeg, visibility, all_mailboxes, refresh, done=None):
        identity = self.identity
        side = jpeg["side"]
        query = {"identity": identity, "crop": f"0,0,{side},{side}", "visibility": visibility}
        if all_mailboxes:
            query["all"] = "1"

        def put():
            return self.api.call("PUT", "/v1/profile/avatar?" + urlencode(query), raw=jpeg["bytes"], timeout=60,
                                 headers={"Content-Type": "image/jpeg", "x-pigeonpost-identity": identity})

        def saved(_):
            keys = [k for row in (self.mailboxes if all_mailboxes else [r for r in self.mailboxes if r["address"] == identity])
                    for k in (row.get("handle"), row["address"])]
            self.avatars.forget(keys)
            self.toast("Saved for all your mailboxes." if all_mailboxes else "Saved.")
            refresh()
            if done:
                done(None)

        def failed(error):
            message = avatar_error(getattr(error, "status", 0), getattr(error, "code", None))
            self.toast(message)
            if done:
                done(message)
        self._work(put, saved, failed)


class CropDialog:
    """Square crop with zoom: drag to move, the slider (or + and −) to zoom, arrows to nudge. The
    round mask people will see is drawn over it. Saves a JPEG of the square, at most 1024 px, so no
    metadata of the original file leaves the device."""

    VIEW = 300

    def __init__(self, window, pixbuf, save):
        self.window, self.pixbuf, self.save = window, pixbuf, save
        self.zoom, self.offset = 1.0, [0.0, 0.0]
        self.dialog = Adw.Window(title="Crop picture", transient_for=window, modal=True, default_width=380, default_height=520)
        root = box(0)
        header = Adw.HeaderBar()
        header.set_title_widget(Adw.WindowTitle(title="Crop picture"))
        root.append(header)
        content = box(14, margin=20)
        root.append(content)
        self.area = Gtk.DrawingArea(content_width=self.VIEW, content_height=self.VIEW, focusable=True)
        self.area.set_halign(Gtk.Align.CENTER)
        self.area.set_draw_func(self.draw)
        self.area.update_property([Gtk.AccessibleProperty.LABEL], ["Picture crop. Drag or use the arrow keys to move, plus and minus to zoom."])
        drag = Gtk.GestureDrag()
        drag.connect("drag-begin", lambda *_: setattr(self, "start", list(self.offset)))
        drag.connect("drag-update", self.dragged)
        self.area.add_controller(drag)
        keys = Gtk.EventControllerKey()
        keys.connect("key-pressed", self.key)
        self.area.add_controller(keys)
        content.append(self.area)
        self.scale = Gtk.Scale.new_with_range(Gtk.Orientation.HORIZONTAL, 1, 4, 0.05)
        self.scale.set_value(1)
        self.scale.update_property([Gtk.AccessibleProperty.LABEL], ["Zoom"])
        self.scale.connect("value-changed", lambda s: self.set_zoom(s.get_value()))
        content.append(self.scale)
        self.error = label("", "error", wrap=True)
        content.append(self.error)
        actions = box(8, horizontal=True)
        actions.set_halign(Gtk.Align.END)
        actions.append(button("Cancel", self.dialog.close))
        self.save_button = button("Save", self.submit, style="suggested-action")
        actions.append(self.save_button)
        content.append(actions)
        self.dialog.set_content(root)
        window.dialogs.append(self.dialog)
        self.dialog.connect("close-request", lambda w: window.dialogs.remove(w) if w in window.dialogs else None)
        window.crop_dialog = self
        self.dialog.present()
        self.area.grab_focus()

    def rect(self):
        return square_crop(self.pixbuf.get_width(), self.pixbuf.get_height(), self.zoom, *self.offset)

    def draw(self, _, cr, width, height):
        x, y, side = self.rect()
        scale = width / side
        cr.save()
        cr.scale(scale, scale)
        Gdk.cairo_set_source_pixbuf(cr, self.pixbuf, -x, -y)
        cr.rectangle(0, 0, side, side)
        cr.fill()
        cr.restore()
        cr.set_source_rgba(0, 0, 0, 0.45)
        cr.set_fill_rule(cairo.FillRule.EVEN_ODD)  # shade outside the circle
        cr.rectangle(0, 0, width, height)
        cr.arc(width / 2, height / 2, width / 2, 0, 2 * math.pi)
        cr.fill()

    def free(self):
        x, y, side = square_crop(self.pixbuf.get_width(), self.pixbuf.get_height(), self.zoom, 0, 0)
        return max(1e-6, x), max(1e-6, y), side

    def dragged(self, _, dx, dy):
        free_x, free_y, side = self.free()
        per_pixel = side / self.VIEW
        self.offset = [max(-1.0, min(1.0, self.start[0] - dx * per_pixel / free_x)),
                       max(-1.0, min(1.0, self.start[1] - dy * per_pixel / free_y))]
        self.area.queue_draw()

    def set_zoom(self, value):
        self.zoom = max(1.0, min(4.0, value))
        self.area.queue_draw()

    def key(self, _, keyval, _code, _state):
        step = 0.05
        moves = {Gdk.KEY_Left: (-step, 0), Gdk.KEY_Right: (step, 0), Gdk.KEY_Up: (0, -step), Gdk.KEY_Down: (0, step)}
        if keyval in moves:
            dx, dy = moves[keyval]
            self.offset = [max(-1.0, min(1.0, self.offset[0] + dx)), max(-1.0, min(1.0, self.offset[1] + dy))]
            self.area.queue_draw()
            return True
        if keyval in (Gdk.KEY_plus, Gdk.KEY_equal, Gdk.KEY_KP_Add, Gdk.KEY_minus, Gdk.KEY_KP_Subtract):
            delta = 0.1 if keyval in (Gdk.KEY_plus, Gdk.KEY_equal, Gdk.KEY_KP_Add) else -0.1
            self.scale.set_value(max(1.0, min(4.0, self.zoom + delta)))
            return True
        return False

    def encoded(self):
        x, y, side = self.rect()
        square = self.pixbuf.new_subpixbuf(x, y, min(side, self.pixbuf.get_width() - x), min(side, self.pixbuf.get_height() - y))
        target = min(AVATAR_UPLOAD_SIDE, square.get_width(), square.get_height())
        if square.get_width() != target or square.get_height() != target:
            square = square.scale_simple(target, target, GdkPixbuf.InterpType.BILINEAR)
        if square.get_has_alpha():
            flat = GdkPixbuf.Pixbuf.new(GdkPixbuf.Colorspace.RGB, False, 8, target, target)
            flat.fill(0xffffffff)
            square.composite(flat, 0, 0, target, target, 0, 0, 1, 1, GdkPixbuf.InterpType.BILINEAR, 255)
            square = flat
        ok, data = square.save_to_bufferv("jpeg", ["quality"], ["90"])
        if not ok:
            raise ValueError("That picture could not be prepared.")
        return {"bytes": bytes(data), "side": target}

    def submit(self):
        try:
            jpeg = self.encoded()
        except (GLib.Error, ValueError):
            self.error.set_text("That picture could not be prepared.")
            return
        self.save_button.set_sensitive(False)
        self.save(jpeg)
        self.dialog.close()
