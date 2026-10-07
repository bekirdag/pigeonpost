"""Bounded native thumbnails. Untrusted documents never become HTML or executable content."""
from collections import OrderedDict
import mimetypes
from pathlib import Path
import tempfile
import threading

import gi
gi.require_version("GdkPixbuf", "2.0")
from gi.repository import GdkPixbuf, GLib

MAX_BYTES = 25 * 1024 * 1024


def media_type(name, mime):
    mime = (mime or "").split(";")[0].strip().lower()
    return (mimetypes.guess_type(name)[0] or "") if mime in ("", "application/octet-stream") else mime


def supports(name, mime):
    mime = media_type(name, mime)
    return mime in {"image/png", "image/jpeg", "image/gif", "image/webp", "image/bmp", "image/avif",
                    "application/pdf", "text/plain", "text/csv", "text/markdown", "application/json"} or mime.startswith(("video/", "audio/"))


class ThumbnailCache:
    def __init__(self):
        self.entries = OrderedDict()
        self.lock = threading.Lock()
        self.slots = threading.BoundedSemaphore(2)
        self.generation = 0

    def clear(self):
        with self.lock:
            self.generation += 1
            self.entries.clear()

    def load(self, key, name, mime, read):
        with self.slots:
            with self.lock:
                generation = self.generation
                if key in self.entries:
                    self.entries.move_to_end(key)
                    return self.entries[key]
            data = read()
            if not data or len(data) > MAX_BYTES:
                return None
            result = render(data, name, media_type(name, mime))
            with self.lock:
                if generation == self.generation:
                    self.entries[key] = result
                    while len(self.entries) > 32:
                        self.entries.popitem(last=False)
            return result


def render(data, name, mime):
    if mime.startswith("image/"):
        loader = GdkPixbuf.PixbufLoader.new()
        valid = [True]

        def size(_, width, height):
            valid[0] = 0 < width * height <= 64 * 1024 * 1024
            scale = min(1, 440 / max(1, width), 300 / max(1, height)) if valid[0] else 0
            loader.set_size(max(1, int(width * scale)), max(1, int(height * scale)))

        loader.connect("size-prepared", size)
        loader.write(data)
        loader.close()
        return loader.get_pixbuf().apply_embedded_orientation() if valid[0] else None
    if mime.startswith("text/") or mime == "application/json":
        return data[:4096].decode("utf-8", errors="replace")[:1200]
    if mime.startswith("audio/"):
        return "♫ " + name
    with tempfile.TemporaryDirectory(prefix="pigeonpost-preview-") as folder:
        path = Path(folder) / ("preview" + (mimetypes.guess_extension(mime) or ".bin"))
        path.write_bytes(data)
        if mime == "application/pdf":
            import cairo
            gi.require_version("Poppler", "0.18")
            from gi.repository import Poppler
            document = Poppler.Document.new_from_file(path.as_uri(), None)
            if document.get_n_pages() == 0:
                return None
            page = document.get_page(0)
            width, height = page.get_size()
            scale = min(440 / width, 300 / height)
            surface = cairo.ImageSurface(cairo.FORMAT_ARGB32, max(1, int(width * scale)), max(1, int(height * scale)))
            context = cairo.Context(surface)
            context.set_source_rgb(1, 1, 1)
            context.paint()
            context.scale(scale, scale)
            page.render(context)
            import io
            output = io.BytesIO()
            surface.write_to_png(output)
            return render(output.getvalue(), "preview.png", "image/png")
        if mime.startswith("video/"):
            gi.require_version("Gst", "1.0")
            gi.require_version("GstApp", "1.0")
            from gi.repository import Gst, GstApp
            Gst.init(None)
            pipeline = Gst.parse_launch("uridecodebin name=source ! videoconvert ! videoscale add-borders=true ! "
                                        "video/x-raw,format=RGB,width=440,height=300,pixel-aspect-ratio=1/1 ! "
                                        "appsink name=preview sync=false max-buffers=1 drop=true")
            pipeline.get_by_name("source").set_property("uri", path.as_uri())
            try:
                pipeline.set_state(Gst.State.PAUSED)
                sample = pipeline.get_by_name("preview").try_pull_preroll(8 * Gst.SECOND)
                if sample is None:
                    return None
                buffer = sample.get_buffer()
                pixels = buffer.extract_dup(0, buffer.get_size())
                return GdkPixbuf.Pixbuf.new_from_bytes(GLib.Bytes.new(pixels), GdkPixbuf.Colorspace.RGB, False, 8, 440, 300, 440 * 3)
            finally:
                pipeline.set_state(Gst.State.NULL)
    return None
