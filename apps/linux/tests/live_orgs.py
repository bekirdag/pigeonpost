"""Live: organization rooms and device approval with the Linux app's code against a disposable
loopback postbox (tests/e2e-messages/linux.sh … orgs). Synthetic accounts only."""
import base64
import hashlib
import json
import secrets
import subprocess

from live_support import Checks, StaticSession, attempts, fixture, loss, postbox
from pigeonpost import orgs_core as oc
from pigeonpost import pairing
from pigeonpost.api import APIError
from pigeonpost.binding import Lease, Retired
from pigeonpost.protected import MemoryProtectedStore


def ed25519_public_key():
    script = ("const {generateKeyPairSync}=require('crypto');const k=generateKeyPairSync('ed25519');"
              "process.stdout.write(k.publicKey.export({format:'der',type:'spki'}).subarray(-32).toString('base64'))")
    return subprocess.run(["node", "-e", script], check=True, capture_output=True, text=True, timeout=30).stdout


def new_pairing(api, handle, label="Loopback build box"):
    secret = secrets.token_bytes(32)
    body = {"handle": handle, "device_public_key": ed25519_public_key(),
            "claim_challenge": base64.b64encode(hashlib.sha256(secret).digest()).decode(), "label": label}
    result = api.transport.request("POST", api.base + "/v1/device-pairings", data=body)
    return result["user_code"], result["confirmation"]


def main():
    setup = fixture()
    org_id = setup["organizationId"]
    alice, bob = setup["people"]["alice"], setup["people"]["bob"]
    check = Checks("Linux organizations (live)")

    session = StaticSession(alice["token"])
    account = Lease(session)
    direct, _ = postbox(setup, "alice", session=session)
    api = oc.OrgsAPI(direct.bound(account))

    # ---- catalog: owned org inbox vs operated agents ---------------------------------------
    catalog = api.catalog()
    org = next(o for o in catalog["orgs"] if o["id"] == org_id)
    check.ok(org["mailbox"]["address"] == alice["address"], "catalog names alice's own org inbox")
    check.ok("owner" in org["roles"] and oc.readable(org), "owner role and readable status")
    identities = direct.bound(account).call("GET", "/v1/identities").get("identities", [])
    own = oc.owned_org_mailboxes(catalog, identities)
    check.ok(own.get(org_id) == alice["address"], "org inbox is one of this account's own mailboxes")
    check.ok(catalog["operated"] == [], "no agents are delegated yet; nothing is merged as owned")
    check.ok(oc.may_create(org, alice["address"]) and not oc.may_create(org, alice["personalAddress"]),
             "creation hint only for the org inbox")

    # ---- directory: org inbox only -------------------------------------------------------------
    rooms = api.rooms(alice["address"], org_id)
    check.ok(isinstance(rooms, list), "room directory as the org inbox")
    try:
        api.rooms(alice["personalAddress"], org_id)
        check.ok(False, "personal inbox must not browse the org directory")
    except APIError as error:
        check.ok(error.status == 403 and error.code == "not_org_member", "personal inbox is refused (not_org_member)")

    # ---- room creation custody with a lost answer -------------------------------------------
    store = MemoryProtectedStore()
    mailbox_lease = account.child(alice["address"])
    proxied, _ = postbox(setup, "alice", via_proxy=True, session=session)
    proxied_api = oc.OrgsAPI(proxied.bound(mailbox_lease))
    wire = oc.room_request(org_id, "Loopback design", "Synthetic room")
    key = json.loads(wire)["request_key"]
    store.update(mailbox_lease, alice["address"], lambda r: oc.save_pending(r, wire))
    loss(setup, "after")
    try:
        proxied_api.create_exact(alice["address"], wire)
        check.ok(False, "the answer should have been lost")
    except APIError as error:
        check.ok(oc.uncertain(error), "lost answer is uncertain")
    loss(setup, "off")
    saved = oc.pending_rooms(store.read(mailbox_lease, alice["address"]))
    check.ok(len(saved) == 1 and saved[0]["wire"] == wire, "exact request kept for an explicit retry")
    group = proxied_api.create_exact(alice["address"], saved[0]["wire"])
    seen = attempts(setup)
    check.ok(len(seen) == 2 and seen[0]["sha"] == seen[1]["sha"] and seen[0]["requestKey"] == seen[1]["requestKey"] == key,
             "retry sent identical bytes with the same request key")
    store.update(mailbox_lease, alice["address"], lambda r: oc.remove_pending(r, key))
    groups = direct.scoped("GET", "/v1/groups?state=active,invited,keying", alice["address"]).get("groups", [])
    named = [g for g in groups if g.get("name") == "Loopback design"]
    check.ok(len(named) == 1 and named[0]["id"] == group["id"] and named[0].get("org_id") == org_id,
             "exactly one room exists after the retry")
    changed = oc.room_request(org_id, "Loopback design 2", request_key=key)
    try:
        api.create_exact(alice["address"], changed)
        check.ok(False, "a changed body under the same key must be refused")
    except APIError as error:
        check.ok(error.status == 409 and not oc.uncertain(error), "same key with other bytes: definite 409")

    # ---- an earlier login's work stops -----------------------------------------------------
    old = Lease(session)
    old_api = oc.OrgsAPI(direct.bound(old))
    session.switch(bob["token"])
    try:
        old_api.catalog()
        check.ok(False, "a retired login must not borrow the newer token")
    except Retired:
        check.ok(True, "work of the earlier login stops before any request")
    session.switch(alice["token"])

    # ---- device approval -------------------------------------------------------------------
    session_a = StaticSession(alice["token"])
    lease_a = Lease(session_a)
    person, _ = postbox(setup, "alice", session=session_a)
    code_text, confirmation = new_pairing(person, "/fixture/build1")
    gated = pairing.ApprovalStore(lease_a, pairing.PairingAPI(person.bound(lease_a)), strong_available=False)
    gated.resolve(code_text.lower())
    snap = gated.snapshot()
    check.ok(snap["phase"] == pairing.REVIEW and snap["preview"]["creates_agent"], "resolve shows the agent it would create")
    check.ok(snap["preview"]["confirmation"].replace(" ", "") == confirmation.replace(" ", ""), "confirmation matches the terminal")
    check.ok(not snap["can_approve"] and snap["needs_browser"], "native approval disabled; browser offered")
    check.ok(pairing.browser_link(pairing.parse_code(code_text)).endswith("#" + snap["code"]), "browser link carries the code in its fragment")
    gated.deny(snap["proof"])
    check.ok(gated.snapshot()["phase"] == pairing.DENIED, "native deny, confirmed by resolving again")

    code_text, _ = new_pairing(person, "/fixture/build2")
    capable = pairing.ApprovalStore(lease_a, pairing.PairingAPI(person.bound(lease_a)), strong_available=True)
    capable.resolve(code_text)
    proof = capable.snapshot()["proof"]
    check.ok(capable.snapshot()["can_approve"], "an owner may approve (with a strong session)")
    capable.approve(proof)
    check.ok(capable.snapshot()["phase"] == pairing.APPROVED, "approval acknowledged for exactly the reviewed agent")
    check.ok(not capable.approve(proof) and not capable.snapshot()["can_approve"], "a decided request offers no other approval")
    capable.check_status()
    check.ok(capable.snapshot()["phase"] == pairing.APPROVED, "status read back as approved")

    code_text, _ = new_pairing(person, "/fixture/build3")
    session_b = StaticSession(bob["token"])
    lease_b = Lease(session_b)
    member, _ = postbox(setup, "bob", session=session_b)
    plain = pairing.ApprovalStore(lease_b, pairing.PairingAPI(member.bound(lease_b)), strong_available=True)
    plain.resolve(code_text)
    snap = plain.snapshot()
    check.ok(snap["phase"] == pairing.REVIEW and not snap["can_approve"] and not snap["can_deny"]
             and "operators" in (snap["message"] or ""), "a plain member sees why they cannot decide")
    # ---- an agent delegated to bob is listed apart, never as bob's own inbox -----------------
    members = person.call("GET", f"/v1/orgs/{org_id}/members")["members"]
    bob_member = next(m["id"] for m in members if m.get("address") == bob["address"])
    agent = capable.snapshot()["preview"]["address"] or next(
        a["address"] for a in person.call("GET", f"/v1/orgs/{org_id}/agents")["agents"] if a["handle"] == "/fixture/build2")
    person.call("POST", f"/v1/orgs/{org_id}/delegations", data={"member_id": bob_member, "target": "/fixture/build2",
                                                               "capabilities": ["read_inbox"]})
    bob_catalog = oc.OrgsAPI(member.bound(lease_b)).catalog()
    bob_identities = member.call("GET", "/v1/identities").get("identities", [])
    operated = oc.operated_agents(bob_catalog)
    check.ok([a["address"] for a in operated] == [agent] and operated[0]["capabilities"] == ["read_inbox"],
             "delegated agent listed under operated with its capabilities")
    check.ok(agent not in oc.owned_org_mailboxes(bob_catalog, bob_identities).values()
             and agent not in {r["address"] for r in bob_identities}, "operated agent is not one of bob's own inboxes")
    plain.resolve("ZZZZ-ZZZZ")
    check.ok(plain.snapshot()["phase"] == pairing.UNAVAILABLE, "unknown code: not found, nothing to approve")
    check.done()


if __name__ == "__main__":
    main()
