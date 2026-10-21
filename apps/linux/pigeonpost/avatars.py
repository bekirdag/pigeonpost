"""Profile pictures (SDS §16): looked up in bulk, fetched with the account token (a "people I know"
picture is not public), cached by content hash for this run only, and laid over Adw.Avatar's
initials, which stay as the fallback. Nothing is written to disk."""

import threading
import urllib.parse
import weakref

from gi.repository import Adw, GLib, Gdk

from .features import avatar_hash, avatar_url

MAX_BYTES = 1024 * 1024
BATCH = 100


class AvatarCache:
    def __init__(self, window):
        self.window = window
        self.lock = threading.Lock()
        self.enabled = False
        self.meta = {}       # key → (hash, url) or None (no picture, or none this account may see)
        self.textures = {}   # hash → Gdk.Texture
        self.queue = set()
        self.loading = False
        self.widgets = []    # (weakref to Adw.Avatar, key)
        self.generation = 0

    def reset(self, enabled=False):
        with self.lock:
            self.meta.clear()
            self.textures.clear()
            self.queue.clear()
            self.widgets = []
            self.generation += 1
            self.loading = False
            self.enabled = enabled

    def widget(self, key, text, size=32):
        """An avatar with initials of `text`, showing `key`'s picture once it is known."""
        avatar = Adw.Avatar.new(size, text or "?", True)
        if not self.enabled or not key or not key.startswith("/") or key.endswith("/*"):
            return avatar
        self.widgets = [(ref, k) for ref, k in self.widgets if ref() is not None]
        self.widgets.append((weakref.ref(avatar), key))
        meta = self.meta.get(key, ...)
        if meta is ...:
            self.want(key)
        elif meta and meta[0] in self.textures:
            avatar.set_custom_image(self.textures[meta[0]])
        return avatar

    def texture_for(self, key):
        meta = self.meta.get(key)
        return self.textures.get(meta[0]) if meta else None

    def want(self, key):
        self.queue.add(key)
        if not self.loading:
            self.loading = True
            GLib.timeout_add(40, self.flush)

    def forget(self, keys):
        for key in keys:
            if key:
                self.meta.pop(key, None)
                self.want(key)

    def flush(self):
        keys = sorted(self.queue)[:BATCH]
        for key in keys:
            self.queue.discard(key)
            self.meta[key] = None
        generation = self.generation
        api = self.window.api

        def fetch():
            found = api.call("GET", "/v1/avatars", query={"addresses": ",".join(keys)}).get("avatars") or {}
            results = {}
            for key in keys:
                meta = found.get(key)
                url = avatar_url(meta)
                if not url:
                    results[key] = None
                    continue
                digest = avatar_hash(meta)
                data = None
                if digest not in self.textures:
                    data = api.call("GET", urllib.parse.quote(url, safe="/:._-"), binary=True)
                    if len(data) > MAX_BYTES:
                        data = None
                results[key] = (digest, url, data)
            return results

        def done(results):
            if generation != self.generation:
                return
            for key, value in results.items():
                if not value:
                    self.meta[key] = None
                    continue
                digest, url, data = value
                if data is not None and digest not in self.textures:
                    try:
                        self.textures[digest] = Gdk.Texture.new_from_bytes(GLib.Bytes.new(data))
                    except GLib.Error:
                        self.meta[key] = None
                        continue
                self.meta[key] = (digest, url)
            self.repaint(set(results))
            self.window.avatars_changed()

        def failed(_):
            self.loading = bool(self.queue)

        def finish(fn):
            def run(value):
                self.loading = False
                fn(value)
                if self.queue and not self.loading:
                    self.loading = True
                    GLib.timeout_add(40, self.flush)
            return run

        if keys:
            self.window._work(fetch, finish(done), finish(failed))
        else:
            self.loading = False
        return False

    def repaint(self, keys):
        alive = []
        for ref, key in self.widgets:
            avatar = ref()
            if avatar is None:
                continue
            alive.append((ref, key))
            if key in keys:
                texture = self.texture_for(key)
                avatar.set_custom_image(texture)
        self.widgets = alive
