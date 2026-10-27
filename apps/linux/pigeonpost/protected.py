"""Protected local work that must survive a restart: unconfirmed deliveries, drafts and pending
calendar writes, kept per account and mailbox.

One record per (account, mailbox), written as a whole on every change, the same way as the group
store (groupstore.py):

* the record is serialized and sealed with the group library's XChaCha20-Poly1305 attachment
  sealing under a fresh random key on every save;
* that key, the sealed file's SHA-256 and its length live in the desktop keyring (Secret Service),
  never on disk; the keyring item carries the account and mailbox it belongs to;
* the file is written with fsync + rename, and older files are removed only after the keyring
  points at the new one.

Every read and write takes a `binding.Lease`: the record is only opened for the login that owns it,
and the lease is checked again before anything is written, so work from an earlier sign-in never
writes into (or reads from) a newer account's record. A record whose stored owner or mailbox does
not match its slot is refused, not repaired. Nothing here ever holds a token.

Without the group library or a keyring the store is unavailable (`available` is False) and callers
keep that work in memory only, saying so.
"""

import base64
import hashlib
import json
import os
import threading
from pathlib import Path

from . import APP_ID
from .binding import Retired

FILE_PREFIX = "work-"
VERSION = 1
MAX_PLAIN = 4 * 1024 * 1024


class ProtectedUnavailable(Exception):
    """The protected record could not be read or saved. Nothing was sent or changed."""

    def __init__(self, text="Pending work could not be read or saved. Unlock your desktop keyring and try again."):
        super().__init__(text)


def data_home():
    base = os.environ.get("XDG_DATA_HOME") or os.path.join(os.path.expanduser("~"), ".local", "share")
    return Path(base) / "pigeonpost-desktop" / "protected"


def digest(value):
    return hashlib.sha256(value.encode()).hexdigest()


class SecretKeyring:
    """Secret Service items `{application, kind: protected-work, owner, slot}`. No plaintext fallback."""

    def __init__(self):
        import gi
        gi.require_version("Secret", "1")
        from gi.repository import Secret
        self.Secret = Secret
        self.schema = Secret.Schema.new(APP_ID + ".ProtectedWork", Secret.SchemaFlags.NONE, {
            "application": Secret.SchemaAttributeType.STRING,
            "kind": Secret.SchemaAttributeType.STRING,
            "owner": Secret.SchemaAttributeType.STRING,
            "slot": Secret.SchemaAttributeType.STRING,
        })

    def _attributes(self, owner, slot=None):
        attributes = {"application": APP_ID, "kind": "protected-work", "owner": owner}
        if slot is not None:
            attributes["slot"] = slot
        return attributes

    def get(self, owner, slot):
        return self.Secret.password_lookup_sync(self.schema, self._attributes(owner, slot), None)

    def put(self, owner, slot, value):
        if not self.Secret.password_store_sync(self.schema, self._attributes(owner, slot), self.Secret.COLLECTION_DEFAULT,
                                               "Pigeonpost pending work", value, None):
            raise ProtectedUnavailable()

    def delete(self, owner, slot=None):
        self.Secret.password_clear_sync(self.schema, self._attributes(owner, slot), None)


class MemoryKeyring:
    """Tests only."""

    def __init__(self):
        self.items = {}

    def get(self, owner, slot):
        return self.items.get((owner, slot))

    def put(self, owner, slot, value):
        self.items[(owner, slot)] = value

    def delete(self, owner, slot=None):
        for key in [k for k in self.items if k[0] == owner and (slot is None or k[1] == slot)]:
            self.items.pop(key)


class GroupCryptoSealer:
    """Seals with the group library's attachment sealing (fresh key per call)."""

    def __init__(self, gc):
        self.gc = gc

    def seal(self, plain):
        sealed = self.gc.seal_attachment(plain)
        ref = sealed.reference
        return bytes(sealed.sealed), {"key": base64.b64encode(bytes(ref.key)).decode(),
                                      "sha": base64.b64encode(bytes(ref.sealed_sha256)).decode(),
                                      "len": int(ref.plaintext_len)}

    def open(self, sealed, ref):
        reference = self.gc.AttachmentRef(key=base64.b64decode(ref["key"]), sealed_sha256=base64.b64decode(ref["sha"]),
                                          plaintext_len=int(ref["len"]))
        return bytes(self.gc.open_attachment(sealed, reference))


class ProtectedStore:
    available = True

    def __init__(self, sealer, keyring=None, directory=None):
        self.sealer = sealer
        self.keyring = keyring or SecretKeyring()
        self.directory = Path(directory) if directory else data_home()
        self.lock = threading.RLock()

    @staticmethod
    def _owner(lease):
        lease.require()
        if not lease.owner:
            raise ProtectedUnavailable("Pending work can only be kept for an account that has finished signing in.")
        return lease.owner[:32]

    @staticmethod
    def _slot(mailbox):
        if not isinstance(mailbox, str) or not mailbox.startswith("/") or len(mailbox) > 512:
            raise ProtectedUnavailable()
        return digest(mailbox)[:32]

    def _folder(self, owner, slot):
        return self.directory / owner / slot

    def _empty(self, lease, mailbox):
        return {"v": VERSION, "owner": lease.owner, "mailbox": mailbox}

    def _load(self, lease, mailbox):
        owner, slot = self._owner(lease), self._slot(mailbox)
        try:
            raw = self.keyring.get(owner, slot)
        except Exception:
            raise ProtectedUnavailable() from None
        if not raw:
            return self._empty(lease, mailbox)
        try:
            ref = json.loads(raw)
            name = ref["file"]
            if not isinstance(name, str) or not name.startswith(FILE_PREFIX) or "/" in name or len(name) > 80:
                raise ValueError("file")
            sealed = (self._folder(owner, slot) / name).read_bytes()
            if len(sealed) > 2 * MAX_PLAIN:
                raise ValueError("size")
            record = json.loads(self.sealer.open(sealed, ref))
        except Exception:
            raise ProtectedUnavailable() from None
        if (not isinstance(record, dict) or record.get("v") != VERSION or record.get("owner") != lease.owner
                or record.get("mailbox") != mailbox):
            raise ProtectedUnavailable()
        return record

    def read(self, lease, mailbox):
        with self.lock:
            record = self._load(lease, mailbox)
            lease.require()
            return json.loads(json.dumps(record))

    def update(self, lease, mailbox, change):
        """Apply `change(record)` and save it, all under one lock and the same login.

        `change` may raise to abandon the update; its return value is returned."""
        with self.lock:
            record = self._load(lease, mailbox)
            result = change(record)
            lease.require()
            self._save(lease, mailbox, record)
            return result

    def _save(self, lease, mailbox, record):
        owner, slot = self._owner(lease), self._slot(mailbox)
        record.update(v=VERSION, owner=lease.owner, mailbox=mailbox)
        plain = json.dumps(record, separators=(",", ":"), ensure_ascii=False).encode()
        if len(plain) > MAX_PLAIN:
            raise ProtectedUnavailable("Too much pending work is stored for this inbox. Review it before adding more.")
        try:
            sealed, ref = self.sealer.seal(plain)
            name = FILE_PREFIX + hashlib.sha256(sealed).hexdigest()[:32] + ".bin"
            folder = self._folder(owner, slot)
            folder.mkdir(parents=True, exist_ok=True, mode=0o700)
            for path in (self.directory, self.directory / owner, folder):
                os.chmod(path, 0o700)
            temporary = folder / (name + ".tmp")
            fd = os.open(temporary, os.O_WRONLY | os.O_CREAT | os.O_TRUNC | getattr(os, "O_NOFOLLOW", 0), 0o600)
            try:
                os.write(fd, sealed)
                os.fsync(fd)
            finally:
                os.close(fd)
            os.replace(temporary, folder / name)
            try:
                directory = os.open(folder, os.O_RDONLY)
                try:
                    os.fsync(directory)
                finally:
                    os.close(directory)
            except OSError:
                pass
            lease.require()
            self.keyring.put(owner, slot, json.dumps({"file": name, **ref}))
        except (Retired, ProtectedUnavailable):
            raise
        except Exception:
            raise ProtectedUnavailable() from None
        for other in folder.glob(FILE_PREFIX + "*"):
            if other.name != name:
                try:
                    other.unlink()
                except OSError:
                    pass

    def forget_owner(self, owner):
        """Sign-out: this account's pending work on this computer is removed (key first)."""
        if not owner:
            return
        owner = owner[:32]
        with self.lock:
            try:
                self.keyring.delete(owner)
            finally:
                folder = self.directory / owner
                if folder.is_dir():
                    for path in sorted(folder.rglob("*"), reverse=True):
                        try:
                            path.unlink() if path.is_file() or path.is_symlink() else path.rmdir()
                        except OSError:
                            pass
                    try:
                        folder.rmdir()
                    except OSError:
                        pass


class MemoryProtectedStore(ProtectedStore):
    """Keeps the same record shape for this run only (no keyring or group library)."""
    available = False

    def __init__(self):
        self.rows = {}
        self.lock = threading.RLock()

    @staticmethod
    def _owner(lease):
        lease.require()
        return lease.owner or "-"

    def _load(self, lease, mailbox):
        self._owner(lease)
        row = self.rows.get((lease.owner, mailbox))
        return json.loads(row) if row else self._empty(lease, mailbox)

    def _save(self, lease, mailbox, record):
        lease.require()
        record.update(v=VERSION, owner=lease.owner, mailbox=mailbox)
        self.rows[(lease.owner, mailbox)] = json.dumps(record)

    def forget_owner(self, owner):
        for key in [k for k in self.rows if k[0] == owner]:
            self.rows.pop(key)


def open_store():
    """The protected store when the group library and a keyring are available, else memory only."""
    try:
        from . import groupcrypto
        gc = groupcrypto.load()
        if gc is None:
            return MemoryProtectedStore()
        return ProtectedStore(GroupCryptoSealer(gc))
    except Exception:
        return MemoryProtectedStore()
