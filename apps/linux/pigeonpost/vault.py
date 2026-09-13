"""Linux Secret Service storage. Never falls back to plaintext."""

import gi
gi.require_version("Secret", "1")
from gi.repository import Secret

from . import APP_ID


class Vault:
    def __init__(self):
        self.schema = Secret.Schema.new(APP_ID, Secret.SchemaFlags.NONE,
                                        {"application": Secret.SchemaAttributeType.STRING})
        self.attributes = {"application": APP_ID}

    def load(self):
        return Secret.password_lookup_sync(self.schema, self.attributes, None)

    def save(self, token):
        if not Secret.password_store_sync(self.schema, self.attributes, Secret.COLLECTION_DEFAULT,
                                          "Pigeonpost account", token, None):
            raise RuntimeError("Unlock your desktop keyring, then sign in again.")

    def clear(self):
        Secret.password_clear_sync(self.schema, self.attributes, None)
