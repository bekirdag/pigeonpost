"""Linux live streams and meeting notes against a real loopback postbox (tests/e2e-messages/linux.sh notes).

Streams: an MLS group made by the app's own group code; the streamer goes live through the real
routes; the member's view (streams.live_stream / banner / can_go_live) follows the stream, the stage
invitation and the "Who can go live" policy; stopping posts the `stream` notice, worded as the web.
Notes: each person's Linux notes key (meeting_notes.NotesKeys, registered with PUT /v1/meetings/keys),
the meeting opened and consented through the internal routes the calls service uses, the brief sealed
by meeting-intel's own code (tests/seal_brief.mjs) for the postbox's recipients, and opened by the
Linux reader; non-attendees, other accounts and a newer key cannot open it. Synthetic only.
"""
import base64
import json
import os
import subprocess
import tempfile
import time
import urllib.parse
import urllib.request

from live_support import Checks, StaticSession, fixture, postbox
from pigeonpost import groupcrypto
from pigeonpost import meeting_notes as mn
from pigeonpost import streams
from pigeonpost.api import APIError
from pigeonpost.binding import Lease
from pigeonpost.features import notice_text
from pigeonpost.groups_core import GroupClient
from pigeonpost.groupstore import GroupStorage, MemoryKeyring
from pigeonpost.protected import MemoryProtectedStore

HERE = os.path.dirname(os.path.abspath(__file__))


def internal(setup, token, method, path, body=None):
    parts = urllib.parse.urlsplit(setup["internal"])
    if parts.scheme != "http" or parts.hostname != "127.0.0.1":
        raise SystemExit("The internal fixture API must be loopback only.")
    request = urllib.request.Request(setup["internal"] + path, method=method,
                                     data=json.dumps(body).encode() if body is not None else None,
                                     headers={"Authorization": "Bearer " + token, "Content-Type": "application/json"})
    with urllib.request.urlopen(request, timeout=20) as response:
        return json.load(response)


def p256_public():
    from cryptography.hazmat.primitives import serialization
    from cryptography.hazmat.primitives.asymmetric import ec
    key = ec.generate_private_key(ec.SECP256R1()).public_key()
    # Streams take base64url without padding (docs/streaming.md §4), unlike notes keys.
    return base64.urlsafe_b64encode(key.public_bytes(serialization.Encoding.X962, serialization.PublicFormat.UncompressedPoint)).decode().rstrip("=")


def main():
    gc = groupcrypto.load()
    if gc is None:
        raise SystemExit("group crypto library missing: " + str(groupcrypto.reason()))
    if not mn.crypto_available():
        raise SystemExit("python3-cryptography missing")
    setup = fixture()
    check = Checks("Linux live streams and meeting notes (live)")
    people = setup["people"]
    names = ("alice", "bob", "carol")
    address = {n: people[n]["address"] for n in names}
    sessions = {n: StaticSession(people[n]["token"]) for n in names}
    apis = {n: postbox(setup, n, session=sessions[n])[0] for n in names}
    display = {address["alice"]: "Alice", address["bob"]: "Bob", address["carol"]: "Carol"}
    features = apis["bob"].capabilities()
    for flag in ("groups", "streaming", "meeting_summaries"):
        check.ok(flag in features, f"capability {flag} is on in the fixture")

    # ---- live streams ---------------------------------------------------------------------------
    folders = {n: tempfile.mkdtemp(prefix=f"pp-live-notes-{n}-") for n in ("alice", "bob")}

    def scoped(name):
        def call(method, path, body=None, device=None, raw=None, binary=False):
            return apis[name].scoped(method, path, address[name], body, device=device, raw=raw, binary=binary)
        return call

    clients = {}
    for n in ("alice", "bob"):
        c = GroupClient(gc, scoped(n), address[n], GroupStorage(gc, keyring=MemoryKeyring(), directory=folders[n]),
                        sleep=lambda s: None, jitter=lambda: 0)
        c.init()
        c.top_up_key_packages()
        clients[n] = c
    gid = clients["alice"].create_group({"name": "Linux live", "invite": [address["bob"]]})["group"]["id"]
    apis["bob"].scoped("POST", f"/v1/groups/{gid}/invitation", address["bob"], {"accept": True})
    clients["alice"].process_pending(gid, no_jitter=True)
    check.ok(clients["bob"].process_welcomes() == [gid], "bob joined the group through a verified Welcome")

    def view(name):
        rows = apis[name].scoped("GET", "/v1/groups?state=active,invited,keying", address[name]).get("groups") or []
        return next(g for g in rows if g["id"] == gid)

    def live_for(name):
        return streams.live_stream(apis[name].scoped("GET", f"/v1/groups/{gid}/streams", address[name]), gid)

    check.ok(view("bob").get("stream_policy") == "admins", "groups start with admins-only live streams")
    check.ok(streams.can_go_live(view("alice")) and not streams.can_go_live(view("bob")), "the owner may go live, a member may not")
    try:
        apis["bob"].scoped("POST", f"/v1/groups/{gid}/streams", address["bob"], {"title": "x", "sources": ["camera"], "public_key": p256_public()})
        check.ok(False, "the postbox refuses a member under the admins policy")
    except APIError as error:
        check.ok(error.code == "stream_restricted", "the postbox refuses a member under the admins policy too")
    check.ok(live_for("bob") is None, "no banner before anyone is live")
    started = apis["alice"].scoped("POST", f"/v1/groups/{gid}/streams", address["alice"],
                                   {"title": "Linux demo", "sources": ["camera"], "public_key": p256_public()})
    sid = started["stream"]["id"]
    live = live_for("bob")
    check.ok(live and live["id"] == sid, "the member sees the live stream")
    shown = streams.banner(live, address["bob"], display.get)
    check.ok(shown["text"] == "Alice is live: Linux demo" and shown["watch"] == "Watch in browser" and shown["stage"] is None,
             "the LIVE banner names the streamer and the title, with Watch in browser")
    check.ok(streams.banner(live_for("alice"), address["alice"], display.get)["text"] == "You are live: Linux demo", "the streamer reads You are live")
    apis["alice"].scoped("POST", f"/v1/streams/{sid}/stage", address["alice"], {"address": address["bob"], "on": True})
    check.ok(streams.stage_role(live_for("bob"), address["bob"]) == "invited" and "invited on stage" in streams.banner(live_for("bob"), address["bob"], display.get)["stage"],
             "a stage invitation is shown to the invited member")
    revision = view("alice")["revision"]
    apis["alice"].scoped("PATCH", f"/v1/groups/{gid}", address["alice"], {"expected_revision": revision, "stream_policy": "everyone"})
    check.ok(view("bob")["stream_policy"] == "everyone" and streams.can_go_live(view("bob")), "Who can go live: everyone lets the member go live")
    apis["alice"].scoped("DELETE", f"/v1/streams/{sid}", address["alice"])
    check.ok(live_for("bob") is None, "the banner goes when the stream stops")
    clients["bob"].sync(gid)
    notices = [notice_text(x, lambda a: display.get(a, a)) for x in clients["bob"].history(gid) if x.get("kind") == "notice" and x.get("notice") == "stream"]
    check.ok(len(notices) == 1 and notices[0].startswith("Alice streamed for 1 min · ") and notices[0].endswith("viewers"),
             "the stream notice reads as the web words it: " + (notices[0] if notices else "none"))

    # ---- meeting notes --------------------------------------------------------------------------
    store = MemoryProtectedStore()
    keys = mn.NotesKeys(store)
    leases = {n: Lease(sessions[n]) for n in names}
    mine = {}
    for n in names:
        mine[n] = keys.get(leases[n])
        mn.register(apis[n], mine[n])
    listed = apis["bob"].call("GET", "/v1/meetings/keys").get("keys") or []
    check.ok([(k["key_id"], k["public_key"], k["label"]) for k in listed] == [(mine["bob"].key_id, mine["bob"].public_key, "Linux app")],
             "the Linux key is registered for the account, public half only, labelled Linux app")
    check.ok(len({mine[n].key_id for n in names}) == 3, "every account has its own key on this computer")
    check.ok(keys.get(leases["bob"]).key_id == mine["bob"].key_id, "the key is reused, not remade")

    opened = internal(setup, setup["callsToken"], "POST", "/internal/v1/meetings", {"host": address["alice"], "room_ref": "abcdef0123456789", "kind": "room"})
    mid = opened["meeting_id"]
    for n in ("alice", "bob"):
        internal(setup, setup["callsToken"], "POST", f"/internal/v1/meetings/{mid}/attendees", {"address": address[n], "consent": "yes"})
    internal(setup, setup["callsToken"], "POST", f"/internal/v1/meetings/{mid}/end", {"reason": "host_stopped"})
    recipients = internal(setup, setup["intelToken"], "GET", f"/internal/v1/meetings/{mid}/recipients")["recipients"]
    check.ok(sorted(r["address"] for r in recipients) == sorted([address["alice"], address["bob"]]), "only consenting attendees are recipients")
    brief = {"title": "Linux notes check", "summary": "Synthetic meeting.", "decisions": ["Ship 1.0.11"],
             "action_items": [{"owner": "Bob", "task": "Check the Flatpak", "due": None}], "open_questions": [],
             "participants": ["Alice", "Bob"], "duration_s": 120, "transcript": [{"at": "00:00:02", "speaker": "Bob", "text": "Ready."}],
             "generated_by": {"stt": "fixture", "llm": None}}
    sealed = json.loads(subprocess.run(["node", os.path.join(HERE, "seal_brief.mjs")], input=json.dumps({"meeting_id": mid, "brief": brief, "recipients": recipients}),
                                       capture_output=True, text=True, check=True, timeout=30).stdout)
    internal(setup, setup["intelToken"], "POST", f"/internal/v1/meetings/{mid}/brief", {"status": "ready", **sealed})

    rows = mn.meetings(apis["bob"].call("GET", "/v1/meetings"))
    check.ok([(r["id"], r["state"]) for r in rows] == [(mid, "ready")], "the meeting is listed for the attendee, ready")
    got = mn.open_brief(apis["bob"].call("GET", f"/v1/meetings/{mid}"), keys.get(leases["bob"]))
    check.ok(got["title"] == "Linux notes check" and got["decisions"] == ["Ship 1.0.11"] and mn.action_text(got["action_items"][0]) == "Bob: Check the Flatpak",
             "the Linux reader opens the brief meeting-intel sealed")
    check.ok(mn.open_brief(apis["alice"].call("GET", f"/v1/meetings/{mid}"), keys.get(leases["alice"]))["title"] == "Linux notes check",
             "the host's Linux key opens it too")
    meeting = apis["alice"].call("GET", f"/v1/meetings/{mid}")
    try:
        mn.open_brief(meeting, keys.get(leases["carol"]))
        check.ok(False, "another account's key cannot open it")
    except mn.NotesError as error:
        check.ok(error.code == "no_key_for_device", "another account's key cannot open it (no wrap for it)")
    try:
        apis["carol"].call("GET", f"/v1/meetings/{mid}")
        check.ok(False, "a non-attendee cannot read it")
    except APIError as error:
        check.ok(error.status == 404, "a non-attendee gets not found")
    # Sign-out of bob: his key is forgotten here; a new sign-in makes a new key that older notes skip.
    store.forget_owner(sessions["bob"].owner)
    sessions["bob"].switch(people["bob"]["token"], owner=sessions["bob"].owner)
    check.ok(not leases["bob"].current, "the earlier login's lease is retired")
    fresh = keys.get(Lease(sessions["bob"]))
    check.ok(fresh.key_id != mine["bob"].key_id, "a new sign-in on this computer gets a new key")
    try:
        mn.open_brief(apis["bob"].call("GET", f"/v1/meetings/{mid}"), fresh)
        check.ok(False, "notes made before the new key are not readable with it")
    except mn.NotesError as error:
        check.ok(error.code == "no_key_for_device", "notes made before this key say they were encrypted for other devices")
    apis["bob"].call("DELETE", "/v1/meetings/keys/" + mine["bob"].key_id)
    check.ok(all(k["key_id"] != mine["bob"].key_id for k in apis["bob"].call("GET", "/v1/meetings/keys")["keys"]), "sign-out removes the old key from the account")
    apis["bob"].call("DELETE", f"/v1/meetings/{mid}")
    try:
        apis["bob"].call("GET", f"/v1/meetings/{mid}")
        check.ok(False, "deleted copy is gone")
    except APIError as error:
        check.ok(error.status == 404, "Delete my copy removes bob's copy")
    check.ok(mn.open_brief(apis["alice"].call("GET", f"/v1/meetings/{mid}"), keys.get(leases["alice"]))["title"] == "Linux notes check",
             "other attendees keep theirs")
    check.done()


if __name__ == "__main__":
    main()
