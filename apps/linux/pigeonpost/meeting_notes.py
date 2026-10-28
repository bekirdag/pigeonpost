"""Phase 7 meeting notes, read natively (docs/meeting-summaries.md §3, site-inbox/meeting-notes.js).

The postbox keeps a brief only as ciphertext plus its content key wrapped for each notes key of
each consenting attendee. This app keeps one notes key per account on this computer:

* a P-256 key pair made here; its private half is sealed in the protected store (groupstore-style:
  sealed file, key in the desktop keyring, the account it belongs to checked on every open) and
  removed when the account signs out of this app;
* its public half is registered for the account (`PUT /v1/meetings/keys/{key_id}`, label
  "Linux app") while the account has `meeting_summaries`, so notes made after that can be opened
  here; notes made before were wrapped only for the account's other devices.

Opening a brief: ECDH(this key, wrap.epk) → HKDF-SHA-256 (salt = meeting id, info =
"pigeonpost meeting notes v1|" + key id) → AES-256-GCM unwrap of the content key with AAD
"meeting|key_id" → AES-256-GCM of the brief with AAD = meeting id. The result is untrusted data
and is only ever shown as plain text.
No GUI dependency; `cryptography` is imported lazily so the rest of the app runs without it.
"""

import base64
import binascii
import json
import re
import secrets

from .binding import Retired

MEETING_SUMMARIES = "meeting_summaries"
NOTES_INFO = "pigeonpost meeting notes v1|"
LABEL = "Linux app"
# The protected store's slot for the account's notes key: not a mailbox address, never shown.
KEY_SLOT = "/.meeting-notes-key"
MEETING_ID = re.compile(r"^mtg_[a-z0-9]{26}$")
KEY_ID = re.compile(r"^[A-Za-z0-9_-]{8,64}$")
MAX_CIPHERTEXT = 6 * 1024 * 1024  # the postbox accepts 4 MiB of base64

STATE = {"active": "Notes in progress", "processing": "Being prepared…", "ready": "Ready", "failed": "Couldn’t be made"}
FAILURE = {"no_speech": "No speech was captured.", "no_recipients": "No device could receive the notes.",
           "stt_unavailable": "Transcription was unavailable.", "timed_out": "The notes service didn’t finish in time."}
INTRO = ("Notes from calls where you agreed to them. They’re encrypted for your devices that were in the call or set up "
         "for notes; Pigeonpost’s servers keep only the encrypted copy, and delete it after the retention date.")
NO_KEY = ("These notes were encrypted for your other devices. Open them on a device that was in the call, or one that "
          "opened Meeting notes before the call ended.")
FOOTER = ("Transcribed and summarized automatically on Pigeonpost’s server{by}. Speaker names come from each person’s own "
          "microphone; a microphone can pick up other voices.")


class NotesError(Exception):
    def __init__(self, code, text="This computer couldn’t open these notes."):
        super().__init__(text)
        self.code = code


def crypto_available():
    try:
        from cryptography.hazmat.primitives.asymmetric import ec  # noqa: F401
        from cryptography.hazmat.primitives.ciphers.aead import AESGCM  # noqa: F401
        return True
    except Exception:
        return False


def _unb64(text, length=None, limit=None):
    if not isinstance(text, str) or (limit and len(text) > limit):
        raise NotesError("bad_brief")
    try:
        raw = base64.b64decode(text, validate=True)
    except (binascii.Error, ValueError):
        raise NotesError("bad_brief") from None
    if length is not None and len(raw) != length:
        raise NotesError("bad_brief")
    return raw


def _b64(raw):
    return base64.b64encode(raw).decode()


class NotesKey:
    """This computer's notes key for one account. The private half never leaves the app."""

    def __init__(self, key_id, private_key):
        from cryptography.hazmat.primitives import serialization
        self.key_id = key_id
        self.private_key = private_key
        self.public_key = _b64(private_key.public_key().public_bytes(serialization.Encoding.X962,
                                                                     serialization.PublicFormat.UncompressedPoint))

    @classmethod
    def generate(cls):
        from cryptography.hazmat.primitives.asymmetric import ec
        key_id = "nk" + base64.urlsafe_b64encode(secrets.token_bytes(12)).decode().rstrip("=")
        return cls(key_id, ec.generate_private_key(ec.SECP256R1()))

    def record(self):
        from cryptography.hazmat.primitives import serialization
        der = self.private_key.private_bytes(serialization.Encoding.DER, serialization.PrivateFormat.PKCS8,
                                             serialization.NoEncryption())
        return {"key_id": self.key_id, "private_pkcs8": _b64(der), "public_key": self.public_key}

    @classmethod
    def from_record(cls, record):
        from cryptography.hazmat.primitives import serialization
        from cryptography.hazmat.primitives.asymmetric import ec
        if not isinstance(record, dict) or not isinstance(record.get("key_id"), str) or not KEY_ID.match(record["key_id"]):
            raise NotesError("bad_key")
        try:
            private = serialization.load_der_private_key(_unb64(record.get("private_pkcs8"), limit=4096), password=None)
        except NotesError:
            raise
        except Exception:
            raise NotesError("bad_key") from None
        if not isinstance(private, ec.EllipticCurvePrivateKey) or not isinstance(private.curve, ec.SECP256R1):
            raise NotesError("bad_key")
        key = cls(record["key_id"], private)
        if record.get("public_key") not in (None, key.public_key):
            raise NotesError("bad_key")
        return key

    def __repr__(self):
        return "NotesKey(redacted)"


class NotesKeys:
    """The account's notes key in the protected store (protected.py), made on first use.

    Every read and write takes the account's `binding.Lease`, so a key is only ever opened, made
    or used for the login that owns it; sign-out removes it with the rest of the account's work
    (`ProtectedStore.forget_owner`)."""

    def __init__(self, store):
        self.store = store

    def get(self, lease):
        lease.require()

        def change(record):
            existing = record.get("notes_key")
            if existing:
                return existing
            record["notes_key"] = NotesKey.generate().record()
            return record["notes_key"]
        key = NotesKey.from_record(self.store.update(lease, KEY_SLOT, change))
        lease.require()
        return key

    def persistent(self):
        return bool(getattr(self.store, "available", False))


def register(api, key):
    """Register (idempotently) this computer's public notes key for the signed-in account."""
    return api.call("PUT", "/v1/meetings/keys/" + key.key_id, data={"public_key": key.public_key, "label": LABEL})


def meetings(body):
    """`GET /v1/meetings`, checked; newest first as the postbox orders them."""
    rows = body.get("meetings") if isinstance(body, dict) else None
    out = []
    for row in rows if isinstance(rows, list) else []:
        if not isinstance(row, dict) or not isinstance(row.get("id"), str) or not MEETING_ID.match(row["id"]):
            continue
        out.append({"id": row["id"], "state": row.get("state") if row.get("state") in STATE else "processing",
                    "created_at": row.get("created_at") if isinstance(row.get("created_at"), int) else 0,
                    "expires_at": row.get("expires_at") if isinstance(row.get("expires_at"), int) else 0,
                    "host": _clip(row.get("host"), 200), "failure": row.get("failure") if isinstance(row.get("failure"), str) else None})
    return out[:200]


def open_brief(meeting, key):
    """Decrypt `GET /v1/meetings/{id}` with this computer's key. Raises NotesError
    (`no_key_for_device` when the brief was not wrapped for this key)."""
    from cryptography.exceptions import InvalidTag
    from cryptography.hazmat.primitives import hashes
    from cryptography.hazmat.primitives.asymmetric import ec
    from cryptography.hazmat.primitives.ciphers.aead import AESGCM
    from cryptography.hazmat.primitives.kdf.hkdf import HKDF
    mid = meeting.get("id") if isinstance(meeting, dict) else None
    if not isinstance(mid, str) or not MEETING_ID.match(mid) or meeting.get("state") != "ready":
        raise NotesError("bad_brief")
    if meeting.get("alg") not in (None, "A256GCM"):
        raise NotesError("bad_brief")
    wrap = next((k for k in meeting.get("keys") or [] if isinstance(k, dict) and k.get("key_id") == key.key_id), None)
    if wrap is None:
        raise NotesError("no_key_for_device", NO_KEY)
    try:
        epk = ec.EllipticCurvePublicKey.from_encoded_point(ec.SECP256R1(), _unb64(wrap.get("epk"), 65))
        shared = key.private_key.exchange(ec.ECDH(), epk)
        kek = HKDF(algorithm=hashes.SHA256(), length=32, salt=mid.encode(),
                   info=(NOTES_INFO + key.key_id).encode()).derive(shared)
        cek = AESGCM(kek).decrypt(_unb64(wrap.get("iv"), 12), _unb64(wrap.get("wrapped"), 48),
                                  f"{mid}|{key.key_id}".encode())
        plain = AESGCM(cek).decrypt(_unb64(meeting.get("iv"), 12), _unb64(meeting.get("ciphertext"), limit=MAX_CIPHERTEXT),
                                    mid.encode())
    except NotesError:
        raise
    except (InvalidTag, ValueError, TypeError):
        raise NotesError("cannot_open") from None
    try:
        return brief_of(json.loads(plain.decode("utf-8")))
    except (ValueError, UnicodeError):
        raise NotesError("cannot_open") from None


def _clip(value, limit):
    if not isinstance(value, str):
        return ""
    # Plain text only: control characters (other than line breaks) are dropped.
    return "".join(c for c in value if c in "\n\t" or (c >= " " and c != "\x7f"))[:limit].strip()


def _strings(values, limit=60, size=600):
    return [s for s in (_clip(v, size) for v in (values if isinstance(values, list) else [])[:limit]) if s]


def brief_of(raw):
    """The decrypted brief, reduced to the fields and sizes the reader shows."""
    if not isinstance(raw, dict):
        raise NotesError("cannot_open")
    actions = []
    for a in (raw.get("action_items") if isinstance(raw.get("action_items"), list) else [])[:60]:
        if isinstance(a, dict) and _clip(a.get("task"), 600):
            actions.append({"owner": _clip(a.get("owner"), 120) or None, "task": _clip(a.get("task"), 600), "due": _clip(a.get("due"), 60) or None})
    transcript = []
    for t in (raw.get("transcript") if isinstance(raw.get("transcript"), list) else [])[:5000]:
        if isinstance(t, dict) and _clip(t.get("text"), 4000):
            transcript.append({"at": _clip(t.get("at"), 20), "speaker": _clip(t.get("speaker"), 120) or "Someone", "text": _clip(t.get("text"), 4000)})
    by = raw.get("generated_by") if isinstance(raw.get("generated_by"), dict) else {}
    duration = raw.get("duration_s")
    return {"title": _clip(raw.get("title"), 120) or None, "summary": _clip(raw.get("summary"), 8000) or None,
            "decisions": _strings(raw.get("decisions")), "action_items": actions, "open_questions": _strings(raw.get("open_questions")),
            "transcript": transcript, "participants": _strings(raw.get("participants"), 60, 120),
            "started_at": _clip(raw.get("started_at"), 40) or None,
            "duration_s": duration if isinstance(duration, (int, float)) and 0 <= duration < 10 ** 7 else None,
            "draft": raw.get("draft") is True, "stt": _clip(by.get("stt"), 80) or None, "llm": _clip(by.get("llm"), 80) or None}


def action_text(a):
    return " ".join(x for x in [a["owner"] + ":" if a.get("owner") else None, a["task"], f"(due {a['due']})" if a.get("due") else None] if x)


def footer(b):
    by = f" ({b['stt']}{', ' + b['llm'] if b.get('llm') else ''})" if b.get("stt") else ""
    return FOOTER.format(by=by)


def as_text(b):
    """Copy as text, the same shape as the web reader's."""
    out = [b.get("title") or "Meeting notes", ""]
    if b.get("summary"):
        out += [b["summary"], ""]

    def listing(title, items, fn):
        if items:
            out.append(title + ":")
            out.extend("- " + fn(x) for x in items)
            out.append("")
    listing("Decisions", b.get("decisions"), lambda x: x)
    listing("Action items", b.get("action_items"), action_text)
    listing("Open questions", b.get("open_questions"), lambda x: x)
    listing("Transcript", b.get("transcript"), lambda t: f"[{t['at']}] {t['speaker']}: {t['text']}")
    return "\n".join(out)


def keep_current(lease):
    """Raise Retired when the login a notes request began under is no longer the accepted one."""
    if lease is None or not lease.current:
        raise Retired()
    return lease
