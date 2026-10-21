"""Encrypted local storage for a mailbox's group device: MLS state, cursors and decrypted history.

A device cannot decrypt a group message twice, so what it decrypted once is the only copy it will
ever have; it lives here and nowhere else.

One record per mailbox, written as a whole after every change:

* the record (device id, MLS state blob and the key that blob is exported under, per-group cursors,
  decrypted history) is serialized and sealed with the group library's own
  XChaCha20-Poly1305 attachment sealing, under a fresh random key every save;
* that key, the sealed file's SHA-256 and its length are stored in the desktop keyring (Secret
  Service), never on disk;
* the sealed file is named after its hash and written with fsync + rename; older files are deleted
  only after the keyring points at the new one. A crash in between leaves either the old or the new
  record readable, never a mix of an old MLS state with newer cursors.

Copying the files without the keyring gets nothing. Deleting the keyring item destroys the device
(it is re-enrolled and re-added to its groups without history).
"""

import base64
import hashlib
import json
import os
import threading
from pathlib import Path

from . import APP_ID

FILE_PREFIX = "store-"


def data_home():
    base = os.environ.get("XDG_DATA_HOME") or os.path.join(os.path.expanduser("~"), ".local", "share")
    return Path(base) / "pigeonpost-desktop" / "groups"


def mailbox_key(mailbox):
    return hashlib.sha256(mailbox.encode()).hexdigest()[:32]


class SecretKeyring:
    """Secret Service items `{application, kind: group-store, mailbox}`. No plaintext fallback."""

    def __init__(self):
        import gi
        gi.require_version("Secret", "1")
        from gi.repository import Secret
        self.Secret = Secret
        self.schema = Secret.Schema.new(APP_ID + ".GroupStore", Secret.SchemaFlags.NONE, {
            "application": Secret.SchemaAttributeType.STRING,
            "kind": Secret.SchemaAttributeType.STRING,
            "mailbox": Secret.SchemaAttributeType.STRING,
        })

    def _attributes(self, mailbox):
        return {"application": APP_ID, "kind": "group-store", "mailbox": mailbox}

    def get(self, mailbox):
        return self.Secret.password_lookup_sync(self.schema, self._attributes(mailbox), None)

    def put(self, mailbox, value):
        if not self.Secret.password_store_sync(self.schema, self._attributes(mailbox), self.Secret.COLLECTION_DEFAULT,
                                               "Pigeonpost group keys for " + mailbox, value, None):
            raise RuntimeError("Unlock your desktop keyring to use groups.")

    def delete(self, mailbox):
        self.Secret.password_clear_sync(self.schema, self._attributes(mailbox), None)


class MemoryKeyring:
    """Tests only."""

    def __init__(self):
        self.items = {}

    def get(self, mailbox):
        return self.items.get(mailbox)

    def put(self, mailbox, value):
        self.items[mailbox] = value

    def delete(self, mailbox):
        self.items.pop(mailbox, None)


class GroupStorage:
    persistent = True

    def __init__(self, gc, keyring=None, directory=None):
        self.gc = gc
        self.keyring = keyring or SecretKeyring()
        self.directory = Path(directory) if directory else data_home()
        self.lock = threading.Lock()

    def _folder(self, mailbox):
        return self.directory / mailbox_key(mailbox)

    def load(self, mailbox):
        with self.lock:
            raw = self.keyring.get(mailbox)
            if not raw:
                return None
            try:
                ref = json.loads(raw)
                sealed = (self._folder(mailbox) / ref["file"]).read_bytes()
                reference = self.gc.AttachmentRef(key=base64.b64decode(ref["key"]), sealed_sha256=base64.b64decode(ref["sha"]),
                                                  plaintext_len=int(ref["len"]))
                record = json.loads(bytes(self.gc.open_attachment(sealed, reference)))
            except Exception:
                return None  # unreadable: this device is gone, start again
            return record if isinstance(record, dict) else None

    def save(self, mailbox, record):
        plain = json.dumps(record, separators=(",", ":")).encode()
        sealed = self.gc.seal_attachment(plain)
        digest = bytes(sealed.reference.sealed_sha256)
        name = FILE_PREFIX + digest.hex()[:32] + ".bin"
        with self.lock:
            folder = self._folder(mailbox)
            folder.mkdir(parents=True, exist_ok=True, mode=0o700)
            os.chmod(folder, 0o700)
            temporary = folder / (name + ".tmp")
            fd = os.open(temporary, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
            try:
                os.write(fd, bytes(sealed.sealed))
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
            self.keyring.put(mailbox, json.dumps({
                "file": name, "key": base64.b64encode(bytes(sealed.reference.key)).decode(),
                "sha": base64.b64encode(digest).decode(), "len": int(sealed.reference.plaintext_len),
            }))
            for other in folder.glob(FILE_PREFIX + "*"):
                if other.name != name:
                    try:
                        other.unlink()
                    except OSError:
                        pass

    def remove(self, mailbox):
        with self.lock:
            self.keyring.delete(mailbox)
            folder = self._folder(mailbox)
            for other in folder.glob(FILE_PREFIX + "*"):
                try:
                    other.unlink()
                except OSError:
                    pass
            try:
                folder.rmdir()
            except OSError:
                pass


class MemoryStorage:
    """Tests, and the record shape without any persistence."""
    persistent = False

    def __init__(self):
        self.rows = {}

    def load(self, mailbox):
        row = self.rows.get(mailbox)
        return json.loads(row) if row else None

    def save(self, mailbox, record):
        self.rows[mailbox] = json.dumps(record)

    def remove(self, mailbox):
        self.rows.pop(mailbox, None)
