#!/usr/bin/env python3
"""Cross-client group E2EE interop scenario.

Each language binding runs as a separate *device agent* process. The agents speak a JSON-lines
protocol on stdin/stdout (one request, one response; byte fields base64):

  init {seed, device}                       -> {mailbox, signature_key}
  key_packages {count}                      -> {key_packages: [...]}   (count single-use + 1 last-resort)
  create {group}                            -> {epoch}
  add {group, key_packages}                 -> commit
  remove_mailbox {group, mailbox}           -> commit
  self_update {group}                       -> commit
      commit = {epoch, commit_id, commit, welcome|null, signature}
  confirm {group} -> {epoch}      discard {group} -> {}
  join {welcome}                            -> {group, epoch, members}
  encrypt {group, id, body, attachment?}    -> {ciphertext, signature, epoch, sealed|null}
  process {group, message}                  -> {type: message|commit|own|own_commit_confirmed, ...}
  open_attachment {sealed, ref}             -> {plaintext}
  status {group}                            -> {epoch, epoch_authenticator, safety_number, members}
  persist {}                                -> export + re-import own state
  snapshot {} -> {state, key}     probe_snapshot {state, key, group, message} -> {code}
  quit
Failures: {"ok": false, "code": <stable error code>, "message": ...}.

This script is the delivery service. It enforces the postbox rules from docs/group-e2ee.md:
envelope signatures from the registered device key, cleartext header checks, one commit per
epoch (first valid wins, later ones get `revision_conflict`), application messages only in the
current epoch (`epoch_conflict`), idempotency on (sender, client id), one sequence per group,
Welcome routing by KeyPackageRef, and no echo of a device's own posts. Server-side checks run
in the native Rust agent (the code the postbox links).

Usage: run_interop.py --agent NAME=COMMAND [--agent ...] [--extra NAME]
The first agent creates the group; the last is removed later; --extra names the agent kind
used to spawn a *new device* of the first agent's mailbox (default: second agent's command).
"""

import argparse
import base64
import json
import shlex
import subprocess
import sys

GROUP = "grp_interop0000000000000000"


def b64(b: bytes) -> str:
    return base64.b64encode(b).decode()


def unb64(s: str) -> bytes:
    return base64.b64decode(s)


class AgentError(Exception):
    def __init__(self, code, message):
        super().__init__(f"{code}: {message}")
        self.code = code


class Agent:
    def __init__(self, name, command):
        self.name = name
        self.proc = subprocess.Popen(
            shlex.split(command), stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True, bufsize=1
        )
        self.cursor = 0
        self.mailbox = None
        self.signature_key = None
        self.device = None
        self.inbox = []

    def call(self, op, **args):
        args["op"] = op
        self.proc.stdin.write(json.dumps(args) + "\n")
        self.proc.stdin.flush()
        line = self.proc.stdout.readline()
        if not line:
            raise RuntimeError(f"{self.name} exited during {op}")
        resp = json.loads(line)
        if not resp.get("ok"):
            raise AgentError(resp.get("code"), resp.get("message"))
        return resp

    def expect_error(self, code, op, **args):
        try:
            self.call(op, **args)
        except AgentError as e:
            check(e.code == code, f"{self.name}: {op} failed with {e.code}, expected {code}")
            return
        raise AssertionError(f"{self.name}: {op} unexpectedly succeeded (expected {code})")

    def close(self):
        try:
            self.proc.stdin.write('{"op":"quit"}\n')
            self.proc.stdin.flush()
        except Exception:
            pass
        self.proc.wait(timeout=30)


def check(cond, what):
    if not cond:
        raise AssertionError(what)


class Postbox:
    """The delivery service, reduced to the rules that matter for the protocol."""

    def __init__(self, validator):
        self.v = validator
        self.epoch = 0
        self.log = []  # (sender agent name, message bytes)
        self.welcomes = {}  # kp ref (b64) -> welcome b64
        self.key_packages = {}  # agent name -> [kp b64]
        self.devices = {}  # (mailbox, device) -> signature key b64
        self.ids = {}  # (mailbox, client id) -> sha of message

    def register_key_packages(self, agent, kps):
        for kp in kps:
            info = self.v.call("server_key_package", key_package=kp)
            check(info["mailbox"] == agent.mailbox and info["device"] == agent.device, "key package identity")
            check(info["signature_key"] == agent.signature_key, "key package device key")
            self.devices[(info["mailbox"], info["device"])] = info["signature_key"]
        self.key_packages[agent.name] = [kp for kp in kps]

    def claim_key_package(self, agent_name):
        # Hand out single-use packages first; the last one is the last-resort package.
        kps = self.key_packages[agent_name]
        return kps.pop(0) if len(kps) > 1 else kps[0]

    def _envelope(self, agent, message, signature):
        key = self.devices[(agent.mailbox, agent.device)]
        info = self.v.call("server_envelope", message=message, signature=signature, signature_key=key)
        check(info["sender_mailbox"] == agent.mailbox and info["sender_device"] == agent.device, "envelope sender")
        check(info["group"] == GROUP, "envelope group")
        return info

    def _idempotent(self, agent, info, message):
        k = (agent.mailbox, info["client_message_id"])
        if k in self.ids:
            raise AgentError("duplicate" if self.ids[k] == message else "conflict", "client id reused")
        self.ids[k] = message

    def post_commit(self, agent, bundle):
        info = self._envelope(agent, bundle["commit"], bundle["signature"])
        check(info["kind"] == "commit", "commit kind")
        if info["epoch"] != self.epoch:
            raise AgentError("revision_conflict", f"commit for epoch {info['epoch']}, group at {self.epoch}")
        self._idempotent(agent, info, bundle["commit"])
        self.epoch += 1
        self.log.append((agent.name, bundle["commit"]))
        if bundle.get("welcome"):
            for r in self.v.call("server_welcome", welcome=bundle["welcome"])["recipients"]:
                self.welcomes[r] = bundle["welcome"]
        return len(self.log)

    def post_message(self, agent, sent):
        info = self._envelope(agent, sent["ciphertext"], sent["signature"])
        check(info["kind"] == "application", "message kind")
        if info["epoch"] != self.epoch:
            raise AgentError("epoch_conflict", f"message for epoch {info['epoch']}, group at {self.epoch}")
        self._idempotent(agent, info, sent["ciphertext"])
        self.log.append((agent.name, sent["ciphertext"]))
        return len(self.log)

    def welcome_for(self, kp):
        ref = self.v.call("server_key_package", key_package=kp)["ref"]
        return self.welcomes[ref]

    def sync(self, agent):
        """Deliver the backlog in sequence order, skipping the agent's own posts."""
        results = []
        while agent.cursor < len(self.log):
            sender, msg = self.log[agent.cursor]
            agent.cursor += 1
            if sender == agent.name:
                continue
            r = agent.call("process", group=GROUP, message=msg)
            if r["type"] == "message":
                agent.inbox.append(r)
            results.append(r)
        return results


def commit_and_confirm(postbox, agent, bundle):
    postbox.post_commit(agent, bundle)
    agent.call("confirm", group=GROUP)


def converge(postbox, members):
    for a in members:
        postbox.sync(a)
    states = {a.name: a.call("status", group=GROUP) for a in members}
    first = next(iter(states.values()))
    for name, s in states.items():
        check(s["epoch"] == postbox.epoch, f"{name} at epoch {s['epoch']}, postbox at {postbox.epoch}")
        check(s["epoch_authenticator"] == first["epoch_authenticator"], f"{name} epoch authenticator differs")
        check(s["safety_number"] == first["safety_number"], f"{name} safety number differs")
        check(s["members"] == len(members), f"{name} sees {s['members']} members, expected {len(members)}")
    return first


def send_and_check(postbox, sender, members, text, msg_id, attachment=None):
    postbox.sync(sender)
    sent = sender.call("encrypt", group=GROUP, id=msg_id, body=b64(text.encode()),
                       attachment=b64(attachment) if attachment else None)
    postbox.post_message(sender, sent)
    for a in members:
        if a is sender:
            continue
        postbox.sync(a)
        got = a.inbox[-1]
        check(unb64(got["body"]).decode() == text, f"{a.name} decrypted {got['body']!r} for {msg_id}")
        check(got["sender_mailbox"] == sender.mailbox and got["sender_device"] == sender.device,
              f"{a.name} saw wrong sender for {msg_id}")
        check(got["client_message_id"] == msg_id, f"{a.name} saw wrong id")
        if attachment:
            plain = a.call("open_attachment", sealed=sent["sealed"], ref=got["attachments"][0])["plaintext"]
            check(unb64(plain) == attachment, f"{a.name} attachment mismatch")
    return sent


def log(step):
    print(f"--- {step}", flush=True)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--agent", action="append", required=True, help="NAME=COMMAND")
    ap.add_argument("--validator", required=True, help="command for the native server-side validator agent")
    ap.add_argument("--extra", help="NAME=COMMAND used for the new-device step")
    args = ap.parse_args()
    specs = [a.split("=", 1) for a in args.agent]
    check(len(specs) >= 4, "need at least four agents")

    validator = Agent("validator", args.validator)
    agents = [Agent(n, c) for n, c in specs]
    spawned = [validator] + list(agents)
    try:
        postbox = Postbox(validator)

        log("1. enrol devices and publish key packages")
        for i, a in enumerate(agents):
            r = a.call("init", seed=b64(bytes([i + 1]) * 32), device=a.name)
            a.mailbox, a.signature_key, a.device = r["mailbox"], r["signature_key"], a.name
            postbox.register_key_packages(a, a.call("key_packages", count=2)["key_packages"])
            print(f"    {a.name}: {a.mailbox}/{a.device}")

        log("2. create group and activate it with one commit adding every other device")
        creator, rest = agents[0], agents[1:]
        creator.call("create", group=GROUP)
        kps = {a.name: postbox.claim_key_package(a.name) for a in rest}
        bundle = creator.call("add", group=GROUP, key_packages=list(kps.values()))
        commit_and_confirm(postbox, creator, bundle)
        creator.cursor = len(postbox.log)
        for a in rest:
            j = a.call("join", welcome=postbox.welcome_for(kps[a.name]))
            check(j["group"] == GROUP and j["epoch"] == 1 and j["members"] == len(agents), f"{a.name} join")
            a.cursor = len(postbox.log)
        members = list(agents)
        converge(postbox, members)

        log("3. every client sends; every other client decrypts (one with an attachment)")
        for i, a in enumerate(members):
            att = b"attachment from " + a.name.encode() if i == 1 else None
            send_and_check(postbox, a, members, f"hello from {a.name}", f"hello-{a.name}", att)

        log("4. remove a member; the removed device cannot decrypt later messages")
        victim, remover = members[-1], members[2]
        snap = victim.call("snapshot")
        bundle = remover.call("remove_mailbox", group=GROUP, mailbox=victim.mailbox)
        commit_and_confirm(postbox, remover, bundle)
        removal_commit = postbox.log[-1][1]
        r = postbox.sync(victim)
        check(r[-1]["type"] == "commit" and r[-1]["self_removed"], "victim sees its own removal")
        members = members[:-1]
        sent = send_and_check(postbox, members[1], members, "after removal", "after-removal")
        victim.expect_error("unknown_group", "process", group=GROUP, message=sent["ciphertext"])
        probe = victim.call("probe_snapshot", state=snap["state"], key=snap["key"], group=GROUP,
                            message=sent["ciphertext"])
        check(probe["code"] == "future_epoch", f"pre-removal state must not decrypt: {probe}")
        probe = victim.call("probe_snapshot", state=snap["state"], key=snap["key"], group=GROUP,
                            message=removal_commit)
        check(probe["code"] == "ok" and probe["result"]["self_removed"], "old state learns only its removal")
        converge(postbox, members)

        log("5. add a new device of the creator's mailbox; it gets no history")
        extra_name, extra_cmd = (args.extra or f"{specs[1][0]}={specs[1][1]}").split("=", 1)
        newdev = Agent(f"{extra_name}-new", extra_cmd)
        spawned.append(newdev)
        r = newdev.call("init", seed=b64(bytes([1]) * 32), device="newdevice")
        newdev.mailbox, newdev.signature_key, newdev.device = r["mailbox"], r["signature_key"], "newdevice"
        check(newdev.mailbox == creator.mailbox, "same mailbox as the creator")
        postbox.register_key_packages(newdev, newdev.call("key_packages", count=1)["key_packages"])
        adder = members[3] if len(members) > 3 else members[1]
        kp = postbox.claim_key_package(newdev.name)
        commit_and_confirm(postbox, adder, adder.call("add", group=GROUP, key_packages=[kp]))
        newdev.call("join", welcome=postbox.welcome_for(kp))
        newdev.cursor = len(postbox.log)
        old_message = postbox.log[1][1]
        newdev.expect_error("stale_epoch", "process", group=GROUP, message=old_message)
        members = members + [newdev]
        converge(postbox, members)
        send_and_check(postbox, newdev, members, "hello from the new device", "new-device-hello")

        log("6. concurrent commits: first valid commit for the epoch wins, the loser retries")
        offline = members[3] if len(members) > 4 else None  # stays offline through steps 6-7
        online = [a for a in members if a is not offline]
        winner, loser, bystander = online[1], online[2], online[0]
        pending_msg = bystander.call("encrypt", group=GROUP, id="pending-1", body=b64(b"queued before commits"))
        w = winner.call("self_update", group=GROUP)
        l = loser.call("self_update", group=GROUP)
        commit_and_confirm(postbox, winner, w)
        try:
            postbox.post_commit(loser, l)
            raise AssertionError("second commit for the same epoch was accepted")
        except AgentError as e:
            check(e.code == "revision_conflict", f"loser got {e.code}")
        loser.call("discard", group=GROUP)
        for a in online:
            postbox.sync(a)
        retry = loser.call("self_update", group=GROUP)
        commit_and_confirm(postbox, loser, retry)
        # The message encrypted before both commits is now two epochs stale.
        try:
            postbox.post_message(bystander, pending_msg)
            raise AssertionError("stale application message was accepted")
        except AgentError as e:
            check(e.code == "epoch_conflict", f"stale message got {e.code}")
        for a in online:
            postbox.sync(a)
        # Same client id, fresh encryption in the current epoch.
        resent = bystander.call("encrypt", group=GROUP, id="pending-1", body=b64(b"queued before commits"))
        postbox.post_message(bystander, resent)

        log("7. every online device rotates its keys in turn, persists, and sends")
        for a in online:
            postbox.sync(a)
            commit_and_confirm(postbox, a, a.call("self_update", group=GROUP))
        for a in online:
            postbox.sync(a)
            a.call("persist")
        for a in online:
            send_and_check(postbox, a, online, f"after restore {a.name}", f"restored-{a.name}")

        if offline:
            log(f"8. offline replay: {offline.name} catches up on the whole backlog in order")
            before = len(offline.inbox)
            postbox.sync(offline)
            texts = [unb64(m["body"]).decode() for m in offline.inbox[before:]]
            check("queued before commits" in texts, "offline device decrypts the re-sent message")
            check(sum(t.startswith("after restore") for t in texts) == len(online), "offline device decrypts all")
        final = converge(postbox, members)
        print(f"PASS: {len(members)} devices in {len({a.mailbox for a in members})} mailboxes, "
              f"epoch {final['epoch']}, safety number {final['safety_number']}")
        print("agents: " + ", ".join(sorted({a.name for a in spawned if a is not validator})))
    finally:
        for a in spawned:
            try:
                a.close()
            except Exception:
                a.proc.kill()


if __name__ == "__main__":
    try:
        main()
    except (AssertionError, AgentError) as e:
        print(f"FAIL: {e}", file=sys.stderr)
        sys.exit(1)
