"""Contacts, phone numbers and names on Linux (SDS §14D, docs/contacts-and-phone.md §9 Linux).

Mixed into the main window. The web inbox (`site-inbox/app.js`, "Phase 10") is the reference:
- Settings → Display name (`display_names`): your mailbox's self-set name and who sees it.
- Settings → Phone number (`phone_numbers`): add with a 6-digit code, discoverable (off by default),
  move to another mailbox, unlink.
- Settings → Find people I know (`contact_discovery`): pick a .vcf/.csv export; only truncated
  hashes leave this computer. Matches open a conversation; with `sms_invites`, unmatched numbers can
  be invited one at a time after a confirmation that shows the exact text.
- Edit name on a conversation: a local alias kept in `$XDG_DATA_HOME/pigeonpost/aliases.json`.
Names are shown with the §6.3 precedence; a self-set display name is "~Name".
"""

import time

from gi.repository import GLib, Gtk

from . import contacts_core as C
from .model import contact_for, display_name
from .widgets import address_row, box, button, clear, label, settings_row

PROFILE_BATCH = 100


class ContactsMixin:
    # ---- state ------------------------------------------------------------------------------

    def reset_contacts(self, forget_aliases=False):
        if not hasattr(self, "aliases"):
            self.aliases = C.Aliases()
        self.profile_names, self.profile_queue = {}, set()
        self.profile_timer = None
        self.profile_generation = getattr(self, "profile_generation", 0) + 1
        self.own_display_name = None
        self.phone_last_country = None
        if forget_aliases:
            self.aliases.clear_all()

    def bind_aliases(self):
        owner = self.lease.owner if self.lease is not None else None
        if owner != self.aliases.owner:
            self.aliases.bind(owner)

    # ---- names (§6.3) -----------------------------------------------------------------------

    def peer_name(self, peer):
        """What to call `peer` in lists and headers: local alias, server contact alias, ~display
        name, then the name derived from the handle."""
        if not peer:
            return ""
        shown = None
        if C.DISPLAY_NAMES in self.features and peer.startswith("/") and not peer.endswith("/*"):
            if peer in self.profile_names:
                shown = self.profile_names[peer]
            else:
                self.want_profile(peer)
        return C.pick_name(self.aliases.get(peer), contact_for(peer, self.contacts).get("alias"), shown, display_name(peer))[0]

    def refresh_profiles(self):
        """Names change rarely and Linux has no event stream: ask again on the slow side-data cycle."""
        if self.profile_names:
            for key in list(self.profile_names):
                self.want_profile(key)

    def want_profile(self, peer):
        if peer in self.profile_queue:
            return
        self.profile_queue.add(peer)
        if self.profile_timer is None:
            self.profile_timer = GLib.timeout_add(30, self.flush_profiles)

    def flush_profiles(self):
        self.profile_timer = None
        keys = sorted(self.profile_queue)[:PROFILE_BATCH]
        for key in keys:
            self.profile_queue.discard(key)
            self.profile_names.setdefault(key, None)  # asked once; refresh_profiles asks again later
        if self.profile_queue:
            self.profile_timer = GLib.timeout_add(30, self.flush_profiles)
        if not keys or C.DISPLAY_NAMES not in self.features or self.lease is None:
            return False
        generation, api = self.profile_generation, self.api_for(self.lease)

        def fetch():
            return C.parse_profiles(api.call("GET", "/v1/profiles", query={"addresses": ",".join(keys)}), keys)

        def done(found):
            if generation != self.profile_generation:
                return
            changed = any(self.profile_names.get(k) != v for k, v in found.items())
            self.profile_names.update(found)
            if changed:
                self.render_peers()
                if self.peer in found:
                    self.render_messages()
        self.bound_work(self.lease, fetch, done, lambda _: None)
        return False

    def edit_alias(self, peer=None):
        """"Edit name": what you call this person, on this computer only."""
        peer = peer or self.peer
        if not peer:
            return
        window, content = self.dialog("Edit name")
        content.append(label("Only you see this name, and only on this computer. It is not sent to Pigeonpost.", "dim-label", wrap=True))
        content.append(address_row(peer))
        entry = Gtk.Entry(placeholder_text=display_name(peer), text=self.aliases.get(peer) or "")
        entry.update_property([Gtk.AccessibleProperty.LABEL], ["Name for " + peer])
        content.append(entry)
        error_label = label("", "error", wrap=True)
        content.append(error_label)

        def changed():
            self.render_peers()
            if peer == self.peer:
                self.render_messages()

        def save():
            text = entry.get_text()
            if not text.strip():
                self.aliases.remove(peer)
            else:
                try:
                    self.aliases.set(peer, text)
                except ValueError as error:
                    error_label.set_text(str(error))
                    return
            changed()
            window.close()

        def remove():
            self.aliases.remove(peer)
            changed()
            window.close()
        actions = box(6, horizontal=True)
        actions.append(button("Save name", save, style="suggested-action"))
        if self.aliases.get(peer):
            actions.append(button("Remove name", remove))
        content.append(actions)
        entry.connect("activate", lambda _: save())
        window.present()
        entry.grab_focus()
        return window

    # ---- Settings rows --------------------------------------------------------------------------

    def contacts_settings_rows(self, navigation):
        rows = []
        if not self.identity:
            return rows
        if C.DISPLAY_NAMES in self.features:
            rows.append(settings_row("Display name", "What people see instead of your address", "avatar-default-symbolic",
                                     lambda: self.name_settings(navigation)))
        if C.PHONE_NUMBERS in self.features:
            rows.append(settings_row("Phone number", "Verify a number and choose who can find you", "phone-symbolic",
                                     lambda: self.phone_settings(navigation)))
        if C.CONTACT_DISCOVERY in self.features:
            rows.append(settings_row("Find people I know", "Check a contacts file for people on Pigeonpost", "system-search-symbolic",
                                     lambda: self.find_people(navigation)))
        return rows

    # ---- Settings → Display name ----------------------------------------------------------------

    def name_settings(self, navigation):
        content = navigation.push("Display name")
        identity = self.identity
        state = {"name": None, "visibility": "known"}
        note = label("Loading…", "dim-label", wrap=True)
        entry = Gtk.Entry(placeholder_text="Your name", max_length=40)
        entry.update_property([Gtk.AccessibleProperty.LABEL], ["Display name"])
        content.append(entry)
        content.append(note)
        content.append(label("Who can see it", "heading"))
        from .window_privacy import segmented
        visibility_holder = box(0)
        content.append(visibility_holder)
        error_label = label("", "error", wrap=True)
        content.append(error_label)
        actions = box(6, horizontal=True)
        save_button = button("Save name", lambda: save(), style="suggested-action")
        remove_button = button("Remove name", lambda: remove(), style="destructive-action")
        actions.append(save_button)
        actions.append(remove_button)
        content.append(actions)
        content.append(label("People see this name with a ~ in front of it, beside your address. Anyone can choose any "
                             "name, so it does not prove who you are.", "dim-label", wrap=True))

        def current():
            return identity == self.identity

        def render():
            clear(visibility_holder)
            visibility_holder.append(segmented([("known", "People I know"), ("everyone", "Everyone"), ("nobody", "Nobody")],
                                               state["visibility"], set_visibility))
            remove_button.set_visible(bool(state["name"]))

        def loaded(raw):
            if not current():
                return
            state["name"] = C.parse_own_name(raw)
            if state["name"]:
                state["visibility"] = state["name"]["visibility"]
                entry.set_text(state["name"]["display_name"])
            self.own_display_name = state["name"]["display_name"] if state["name"] else None
            note.set_text("This name belongs to the mailbox you are using now." if state["name"] else "No name yet. People see your address.")
            render()

        def put(text, visibility, done):
            ok, value = C.validate_display_name(text)
            if not ok:
                error_label.set_text(value)
                return
            error_label.set_text("")
            save_button.set_sensitive(False)

            def finished(_):
                save_button.set_sensitive(True)
                if not current():
                    return
                state["name"] = {"display_name": value, "visibility": visibility}
                state["visibility"] = visibility
                self.own_display_name = value
                entry.set_text(value)
                note.set_text("Saved.")
                self.toast("Saved.")
                render()
                if done:
                    done()

            def failed(error):
                save_button.set_sensitive(True)
                if current():
                    error_label.set_text(C.name_error(error))
                    render()
            self._work(lambda: self.api.call("PUT", "/v1/profile/name", data={"display_name": value, "visibility": visibility},
                                             query={"identity": identity}), finished, failed)

        def save():
            put(entry.get_text(), state["visibility"], None)

        def set_visibility(value):
            if not state["name"]:
                state["visibility"] = value  # applied with the next save
                return
            put(state["name"]["display_name"], value, None)

        def remove():
            def done(_):
                if not current():
                    return
                state["name"] = None
                self.own_display_name = None
                entry.set_text("")
                note.set_text("Name removed. People see your address.")
                render()
            self._work(lambda: self.api.call("DELETE", "/v1/profile/name", query={"identity": identity}), done,
                       lambda _: self.toast("Could not remove your name."))
        entry.connect("activate", lambda _: save())
        render()
        self._work(lambda: self.api.call("GET", "/v1/profile/name", query={"identity": identity}), loaded,
                   lambda _: note.set_text("Could not load your name. Try again later.") if current() else None)
        return content

    # ---- Settings → Phone number ---------------------------------------------------------------

    def mailbox_label(self, address):
        row = next((r for r in self.mailboxes if r["address"] == address), None)
        return (row.get("handle") or row.get("label") or address) if row else address

    def phone_settings(self, navigation):
        content = navigation.push("Phone number")
        identity = self.identity
        state = {"numbers": [], "max": 3}
        note = label("Loading…", "dim-label", wrap=True)
        rows = box(10)
        content.append(rows)
        content.append(note)
        add = button("Add a phone number", lambda: self.phone_verify_dialog(load), style="suggested-action")
        content.append(add)
        content.append(label("Your number is never shown to anyone. When you let people find you, someone who already has "
                             "your number in their contacts can see that you are on Pigeonpost.", "dim-label", wrap=True))

        def current():
            return identity == self.identity

        def render():
            clear(rows)
            if not state["numbers"]:
                rows.append(label("No phone number yet.", "dim-label"))
            for number in state["numbers"]:
                rows.append(self.phone_row(number, render, load))
            here = any(n["identity"] == identity for n in state["numbers"])
            full = len(state["numbers"]) >= state["max"]
            add.set_visible(not here and not full)
            if full:
                note.set_text(f"An account can have {state['max']} numbers. Unlink one to add another.")
            elif here:
                note.set_text("This mailbox has a number. Switch mailbox to add one for another.")
            else:
                note.set_text("")

        def loaded(raw):
            if current():
                state["numbers"], state["max"] = C.parse_phones(raw)
                render()

        def load():
            self._work(lambda: self.api.call("GET", "/v1/phone"), loaded,
                       lambda _: note.set_text("Could not load your numbers. Try again later.") if current() else None)
        content.phone_state = state
        load()
        return content

    def phone_row(self, number, render, reload):
        row = box(6)
        row.add_css_class("card")
        for edge in ("top", "bottom", "start", "end"):
            getattr(row, "set_margin_" + edge)(2)
        row.append(label(number["masked"], "heading"))
        row.append(label("On " + self.mailbox_label(number["identity"]), "dim-label"))
        find = Gtk.CheckButton(label="People who have this number can find me", active=number["discoverable"])
        row.append(find)

        def patch(change, undo):
            def done(raw):
                updated = raw.get("number") if isinstance(raw, dict) else None
                if isinstance(updated, dict):
                    number.update({k: updated[k] for k in ("identity", "discoverable") if k in updated})
                self.toast("Saved.")
                render()

            def failed(error):
                undo()
                self.toast("That mailbox already has a number." if getattr(error, "code", "") == "mailbox_has_number"
                           else "Could not save. Please try again.")
                render()
            self._work(lambda: self.api.call("PATCH", "/v1/phone/" + number["id"], data=change), done, failed)

        def toggled(check):
            before = number["discoverable"]
            if check.get_active() == before:
                return
            number["discoverable"] = check.get_active()
            patch({"discoverable": number["discoverable"]}, lambda: number.update(discoverable=before))
        find.connect("toggled", toggled)
        actions = box(6, horizontal=True)
        if len(self.mailboxes) > 1:
            names = [self.mailbox_label(m["address"]) for m in self.mailboxes]
            move = Gtk.DropDown(model=Gtk.StringList.new(names))
            move.update_property([Gtk.AccessibleProperty.LABEL], ["Mailbox for " + number["masked"]])
            addresses = [m["address"] for m in self.mailboxes]
            if number["identity"] in addresses:
                move.set_selected(addresses.index(number["identity"]))

            def moved(*_):
                target = addresses[move.get_selected()] if move.get_selected() < len(addresses) else None
                before = number["identity"]
                if not target or target == before:
                    return
                number["identity"] = target
                patch({"identity": target}, lambda: number.update(identity=before))
            move.connect("notify::selected", moved)
            actions.append(move)

        def unlink():
            def done(_):
                self.toast("Number unlinked.")
                reload()
            self.confirm("Unlink this number?", f"Unlink {number['masked']} from {self.mailbox_label(number['identity'])}? "
                         "People will no longer find you by it, and you can add it again later.",
                         lambda: self._work(lambda: self.api.call("DELETE", "/v1/phone/" + number["id"]), done,
                                            lambda _: self.toast("Could not unlink the number. Please try again.")), "Unlink")
        actions.append(button("Unlink", unlink, style="destructive-action"))
        row.append(actions)
        return row

    def country_picker(self):
        """Calling codes, common countries first, then "Other: type +code"."""
        names = [f"{name} (+{code})" for _, name, code in C.COUNTRIES] + ["Other: type +code"]
        picker = Gtk.DropDown(model=Gtk.StringList.new(names))
        picker.update_property([Gtk.AccessibleProperty.LABEL], ["Country"])
        guess = self.phone_last_country or C.guess_calling_code()
        codes = [code for _, _, code in C.COUNTRIES]
        picker.set_selected(codes.index(guess) if guess in codes else 0)
        other = Gtk.Entry(placeholder_text="+358")
        other.update_property([Gtk.AccessibleProperty.LABEL], ["Country calling code"])
        other.set_visible(False)
        picker.connect("notify::selected", lambda *_: other.set_visible(picker.get_selected() >= len(codes)))

        def code():
            index = picker.get_selected()
            return codes[index] if index < len(codes) else "".join(c for c in other.get_text() if c.isdigit())
        return picker, other, code

    def phone_verify_dialog(self, verified=None):
        if not self.identity or C.PHONE_NUMBERS not in self.features:
            return None
        identity = self.identity
        window, content = self.dialog("Add a phone number")
        state = {"challenge": None, "number": None, "resend_after": 0, "timer": None}
        picker, other, calling_code = self.country_picker()
        number = Gtk.Entry(placeholder_text="Phone number", input_purpose=Gtk.InputPurpose.PHONE)
        number.update_property([Gtk.AccessibleProperty.LABEL], ["Phone number"])
        step_number = box(8)
        step_number.append(picker)
        step_number.append(other)
        step_number.append(number)
        content.append(step_number)
        step_code = box(8)
        sent = label("", wrap=True)
        code = Gtk.Entry(placeholder_text="6-digit code", input_purpose=Gtk.InputPurpose.DIGITS, max_length=6)
        code.update_property([Gtk.AccessibleProperty.LABEL], ["Verification code"])
        find = Gtk.CheckButton(label="Let people who have my number find me", active=False)  # off unless turned on
        resend = button("Resend code", lambda: send(True))
        step_code.append(sent)
        step_code.append(code)
        step_code.append(find)
        step_code.append(resend)
        step_code.set_visible(False)
        content.append(step_code)
        error_label = label("", "error", wrap=True)
        content.append(error_label)
        send_button = button("Send code", lambda: send(False), style="suggested-action")
        verify_button = button("Verify", lambda: verify(), style="suggested-action")
        verify_button.set_visible(False)
        content.append(send_button)
        content.append(verify_button)
        content.append(label("Standard text message rates may apply. The code expires in 10 minutes.", "dim-label", wrap=True))

        def current():
            return identity == self.identity and window in self.dialogs

        def tick():
            left = int(state["resend_after"] - time.time())
            if left > 0 and current():
                resend.set_sensitive(False)
                resend.set_label(f"Resend code in {left // 60}:{left % 60:02d}")
                return True
            resend.set_sensitive(True)
            resend.set_label("Resend code")
            state["timer"] = None
            return False

        def send(again):
            cc = calling_code()
            e164 = state["number"] if again else C.normalize_e164(number.get_text(), cc)
            if not e164:
                error_label.set_text("That doesn’t look like a phone number. Check the country and the number." if cc
                                     else "Type the country calling code, such as +358.")
                return
            error_label.set_text("")
            send_button.set_sensitive(False)

            def done(raw):
                send_button.set_sensitive(True)
                if not current():
                    return
                challenge = raw.get("challenge_id") if isinstance(raw, dict) else None
                if not isinstance(challenge, str) or not challenge:
                    error_label.set_text("Could not send the code. Please try again.")
                    return
                self.phone_last_country = cc
                state["challenge"], state["number"] = challenge, e164
                resend_after = raw.get("resend_after")
                state["resend_after"] = resend_after if isinstance(resend_after, (int, float)) else time.time() + 60
                length = raw.get("code_length") if isinstance(raw.get("code_length"), int) else 6
                sent.set_text(f"We sent a {length}-digit code to {e164}. It expires in 10 minutes.")
                step_number.set_visible(False)
                send_button.set_visible(False)
                step_code.set_visible(True)
                verify_button.set_visible(True)
                code.set_text("")
                code.grab_focus()
                if state["timer"] is None and tick():
                    state["timer"] = GLib.timeout_add_seconds(1, tick)

            def failed(error):
                send_button.set_sensitive(True)
                if current():
                    error_label.set_text(C.phone_error(error))
            self._work(lambda: self.api.call("POST", "/v1/phone/verify", data={"phone": e164, "identity": identity}), done, failed)

        def verify():
            digits = "".join(code.get_text().split())
            if not state["challenge"] or not (len(digits) == 6 and digits.isdigit()):
                error_label.set_text("Enter the 6-digit code from the text message.")
                return
            verify_button.set_sensitive(False)
            error_label.set_text("")

            def done(_):
                verify_button.set_sensitive(True)
                if not current():
                    return
                window.close()
                self.toast("Number verified.")
                if verified:
                    verified()

            def failed(error):
                verify_button.set_sensitive(True)
                if not current():
                    return
                error_label.set_text(C.phone_error(error))
                if getattr(error, "code", "") == "challenge_expired":
                    state["resend_after"] = 0
                    tick()
            self._work(lambda: self.api.call("POST", "/v1/phone/verify/" + state["challenge"],
                                             data={"code": digits, "discoverable": find.get_active()}), done, failed)
        number.connect("activate", lambda _: send(False))
        code.connect("activate", lambda _: verify())
        window.connect("close-request", lambda _: (GLib.source_remove(state["timer"]) if state["timer"] else None, False)[1])
        window.verify_state = state
        window.present()
        number.grab_focus()
        return window

    # ---- Find people I know ----------------------------------------------------------------------

    def find_people(self, navigation=None):
        if not self.identity or C.CONTACT_DISCOVERY not in self.features:
            return None
        window, content = (navigation.window, navigation.push("Find people I know")) if navigation else self.dialog("Find people I know")
        identity = self.identity
        content.append(label("Choose a contacts file exported from Google Contacts, iCloud or Outlook (.vcf or .csv). "
                             "Names and numbers stay on this computer: only scrambled numbers are sent to check who is on "
                             "Pigeonpost, and nothing from the file is kept.", wrap=True))
        picker, other, calling_code = self.country_picker()
        content.append(label("Numbers without a country code are from", "dim-label"))
        content.append(picker)
        content.append(other)
        choose = button("Choose a contacts file…", lambda: pick(), style="suggested-action")
        content.append(choose)
        status = label("", "dim-label", wrap=True)
        content.append(status)
        error_label = label("", "error", wrap=True)
        content.append(error_label)
        found_head = label("On Pigeonpost", "heading")
        found = box(6)
        missing_head = label("No match found", "heading")
        missing = box(6)
        for widget in (found_head, found, missing_head, missing):
            content.append(widget)
            widget.set_visible(False)
        run = {"entries": [], "checked": set(), "matches": {}, "remaining": None, "unreadable": 0, "token": 0}
        content.discovery = run

        def live(token):
            return token == run["token"] and identity == self.identity and window in self.dialogs

        def render():
            clear(found)
            clear(missing)
            matched = [e for e in run["entries"] if e["hash"] in run["matches"]]
            unmatched = [e for e in run["entries"] if e["hash"] in run["checked"] and e["hash"] not in run["matches"]]
            for entry in matched:
                match = run["matches"][entry["hash"]]
                row = box(8, horizontal=True)
                who = box(2)
                who.set_hexpand(True)
                shown = "~" + match["display_name"] if match["display_name"] else display_name(match["handle"] or match["address"])
                who.append(label(entry["name"] or shown))
                if match["display_name"] and entry["name"]:
                    who.append(label("~" + match["display_name"], "dim-label"))
                who.append(label(match["handle"] or match["address"], "dim-label"))
                row.append(who)
                target = match["handle"] or match["address"]
                row.append(button("Start chat", lambda t=target: start_chat(t), style="suggested-action"))
                found.append(row)
            invites = C.SMS_INVITES in self.features
            for entry in unmatched if invites else []:
                row = box(8, horizontal=True)
                who = box(2)
                who.set_hexpand(True)
                who.append(label(entry["name"] or entry["e164"]))
                if entry["name"]:
                    who.append(label(entry["e164"], "dim-label"))
                row.append(who)
                if entry["invite"]:
                    row.append(label(entry["invite"], "dim-label"))
                else:
                    invite = button("Invite", lambda e=entry: self.invite_dialog(e, render))
                    spoken = f"Invite {entry['name'] or entry['e164']} by text message"
                    invite.set_tooltip_text(spoken)
                    invite.update_property([Gtk.AccessibleProperty.LABEL], [spoken])
                    row.append(invite)
                missing.append(row)
            found_head.set_visible(bool(matched))
            found.set_visible(bool(matched))
            missing_head.set_visible(invites and bool(unmatched))
            missing.set_visible(invites and bool(unmatched))
            parts = [f"Checked {len(run['checked'])} of {len(run['entries'])} numbers.",
                     "1 person is on Pigeonpost." if len(matched) == 1 else f"{len(matched)} people are on Pigeonpost."]
            if run["unreadable"]:
                parts.append(f"{run['unreadable']} couldn’t be read.")
            if run["remaining"] is not None:
                parts.append(f"{run['remaining']} new checks left today.")
            status.set_text(" ".join(parts))

        def start_chat(target):
            window.close()
            self.open_conversation(target)

        def check(token, hashes, start):
            if not live(token):
                return
            batch = C.next_batch(hashes, start, run["remaining"])
            if not batch:
                if start < len(hashes):
                    error_label.set_text("Your daily allowance is used. The remaining numbers haven’t been checked. Try again tomorrow.")
                render()
                return

            def done(raw):
                if not live(token):
                    return
                matches, remaining = C.parse_discovery(raw, batch)
                run["checked"].update(batch)
                run["matches"].update(matches)
                if remaining is not None:
                    run["remaining"] = remaining
                render()
                check(token, hashes, start + len(batch))

            def failed(error):
                if live(token):
                    error_label.set_text(C.discover_error(error))
                    render()
            self._work(lambda: self.api.call("POST", "/v1/contacts/discover", data={"hashes": batch}), done, failed)

        def loaded(text, token):
            if not live(token):
                return
            entries, unreadable = C.numbers_from(C.parse_contacts_file(text), calling_code())
            run.update(entries=entries, unreadable=unreadable, checked=set(), matches={})
            if not entries:
                status.set_text("")
                error_label.set_text("No phone numbers found in that file." + (f" {unreadable} couldn’t be read." if unreadable else ""))
                return
            status.set_text(f"Checking {len(entries)} numbers…")
            check(token, [e["hash"] for e in entries], 0)

        def read(file):
            run["token"] += 1
            token = run["token"]
            error_label.set_text("")
            status.set_text("Reading the file…")

            def contents():
                ok, data, _ = file.load_contents(None)
                if not ok or len(data) > 20 * 1024 * 1024:
                    raise OSError("unreadable")
                return bytes(data).decode("utf-8", errors="replace")

            def failed(_):
                if live(token):
                    status.set_text("")
                    error_label.set_text("That file could not be read.")
            self._work(contents, lambda text: loaded(text, token), failed)
        content.read_contacts = read  # tests feed a Gio.File directly

        def pick():
            chooser = Gtk.FileChooserNative.new("Choose a contacts file", window, Gtk.FileChooserAction.OPEN, "Check", "Cancel")
            pattern = Gtk.FileFilter()
            pattern.set_name("Contacts (.vcf, .csv)")
            for glob in ("*.vcf", "*.VCF", "*.vcard", "*.csv", "*.CSV"):
                pattern.add_pattern(glob)
            chooser.add_filter(pattern)

            def chosen(dialog, response):
                if response == Gtk.ResponseType.ACCEPT and dialog.get_file():
                    read(dialog.get_file())
                dialog.destroy()
            chooser.connect("response", chosen)
            chooser.show()
        window.present()
        return content

    def invite_dialog(self, entry, changed=None):
        """One invitation, one confirmation: `confirm: true` records that a person pressed Send."""
        if C.SMS_INVITES not in self.features or not self.identity:
            return None
        identity = self.identity
        window, content = self.dialog("Invite by text message")
        content.append(label("To " + (f"{entry['name']} ({entry['e164']})" if entry["name"] else entry["e164"]), "heading", wrap=True))
        text = label(C.invite_text(self.own_display_name if C.DISPLAY_NAMES in self.features else None), wrap=True)
        text.set_selectable(True)
        content.append(text)
        content.append(label("Pigeonpost sends this text. Your number and address are not included.", "dim-label", wrap=True))
        error_label = label("", "error", wrap=True)
        content.append(error_label)
        verify = button("Verify your number", lambda: (window.close(), self.phone_verify_dialog()))
        verify.set_visible(False)
        content.append(verify)
        actions = box(6, horizontal=True)
        send = button("Send invitation", lambda: go(), style="suggested-action")
        actions.append(button("Cancel", window.close))
        actions.append(send)
        content.append(actions)

        def go():
            send.set_sensitive(False)
            error_label.set_text("")

            def done(raw):
                if identity != self.identity:
                    return
                _, entry["invite"], message = C.invite_outcome(raw)
                window.close()
                self.toast(message)
                if changed:
                    changed()

            def failed(error):
                if identity != self.identity:
                    return
                error_label.set_text(C.invite_error(error))
                verify.set_visible(getattr(error, "code", "") == "phone_required" and C.PHONE_NUMBERS in self.features)
                send.set_sensitive(True)
            self._work(lambda: self.api.call("POST", "/v1/contacts/invites",
                                             data={"phone": entry["e164"], "confirm": True, "identity": identity}), done, failed)
        window.invite_send = go

        def fetch_name():
            if C.DISPLAY_NAMES not in self.features or self.own_display_name is not None:
                return

            def named(raw):
                own = C.parse_own_name(raw)
                if own and identity == self.identity and window in self.dialogs:
                    self.own_display_name = own["display_name"]
                    text.set_text(C.invite_text(own["display_name"]))
            self._work(lambda: self.api.call("GET", "/v1/profile/name", query={"identity": identity}), named, lambda _: None)
        fetch_name()
        window.present()
        send.grab_focus()
        return window

    def open_conversation(self, peer):
        """Open a conversation with `peer` the way New conversation does: by address or handle."""
        if not peer:
            return
        self.group_open = None
        if self.page() != "conversation":
            self.main_stack.set_visible_child_name("conversation")
            self.sync_side_nav()
        self.peer, self.subject, self.message_limit = peer, None, 200
        self.render_peers()
        self.render_subjects()
        self.restore_draft()
        self.render_messages()
