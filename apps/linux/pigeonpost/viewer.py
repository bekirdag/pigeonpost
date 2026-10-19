"""In-app attachment viewer: pictures, video, sound, PDF and plain text, each with a Save button.

Only passive formats are opened. Markup (HTML, SVG, XML) is never rendered: such a file is not
viewable at all and can only be saved. Pictures are decoded with the loader for their declared
type, so an SVG labelled as a PNG fails to decode rather than being sniffed into a renderer.
"""
import io
import shutil
import tempfile
from pathlib import Path

import gi
gi.require_version("Gtk", "4.0")
gi.require_version("Adw", "1")
gi.require_version("GdkPixbuf", "2.0")
from gi.repository import Adw, Gdk, GdkPixbuf, Gio, GLib, Gtk

from .previews import MAX_BYTES, media_type, viewer_kind

MAX_PAGES = 60
MAX_TEXT = 4 * 1024 * 1024


def decode_image(data, mime):
    loader = GdkPixbuf.PixbufLoader.new_with_mime_type(mime)
    valid = [True]

    def size(_, width, height):
        valid[0] = 0 < width * height <= 64 * 1024 * 1024
        if not valid[0]:
            loader.set_size(1, 1)

    loader.connect("size-prepared", size)
    loader.write(data)
    loader.close()
    pixbuf = loader.get_pixbuf()
    if not valid[0] or pixbuf is None:
        raise ValueError("unsupported picture")
    return pixbuf.apply_embedded_orientation()


def render_pdf(data, width=820):
    """PNG bytes per page, at most MAX_PAGES, and the total page count."""
    import cairo
    gi.require_version("Poppler", "0.18")
    from gi.repository import Poppler
    document = Poppler.Document.new_from_bytes(GLib.Bytes.new(data), None)
    pages = []
    for index in range(min(document.get_n_pages(), MAX_PAGES)):
        page = document.get_page(index)
        page_width, page_height = page.get_size()
        scale = width / max(1, page_width)
        surface = cairo.ImageSurface(cairo.FORMAT_ARGB32, max(1, int(page_width * scale)), max(1, int(page_height * scale)))
        context = cairo.Context(surface)
        context.set_source_rgb(1, 1, 1)
        context.paint()
        context.scale(scale, scale)
        page.render(context)
        output = io.BytesIO()
        surface.write_to_png(output)
        pages.append(output.getvalue())
    return pages, document.get_n_pages()


def prepare(kind, data, mime):
    """Worker-thread half: decode into something the main thread can show directly."""
    if len(data) > MAX_BYTES:
        raise ValueError("too large")
    if kind == "image":
        return decode_image(data, mime)
    if kind == "pdf":
        return render_pdf(data)
    if kind == "text":
        return data[:MAX_TEXT].decode("utf-8", errors="replace")
    return data


class AttachmentViewer:
    def __init__(self, owner, name, mime, read, save):
        self.owner, self.name, self.save = owner, name, save
        self.mime = media_type(name, mime)
        self.kind = viewer_kind(name, mime)
        self.data = None
        self.folder = None
        self.media = None
        self.window = Adw.Window(title=name, transient_for=owner, modal=False, default_width=900, default_height=700)
        self.window.attachment_viewer = self
        root = Gtk.Box(orientation=Gtk.Orientation.VERTICAL)
        header = Adw.HeaderBar()
        header.set_title_widget(Adw.WindowTitle(title=name, subtitle=self.mime))
        self.save_button = Gtk.Button(icon_name="document-save-symbolic")
        self.save_button.set_tooltip_text("Save " + name)
        self.save_button.update_property([Gtk.AccessibleProperty.LABEL], ["Save " + name])
        self.save_button.set_sensitive(False)
        self.save_button.connect("clicked", lambda _: self.data is not None and self.save(self.data))
        header.pack_end(self.save_button)
        root.append(header)
        self.body = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, vexpand=True, hexpand=True)
        self.body.add_css_class("pp-wash")
        self.status = Gtk.Label(label="Opening " + name + "…")
        self.status.add_css_class("pp-status")
        self.status.set_vexpand(True)
        self.body.append(self.status)
        root.append(self.body)
        self.window.set_content(root)
        keys = Gtk.EventControllerKey()
        keys.connect("key-pressed", lambda _c, key, _k, _s: (self.window.close(), True)[1] if key == Gdk.KEY_Escape else False)
        self.window.add_controller(keys)
        self.window.connect("close-request", self.closed)
        owner.dialogs.append(self.window)
        if self.kind is None:
            self.status.set_text("This file cannot be shown here. Save it to open it with another app.")
        else:
            kind, mime = self.kind, self.mime
            owner._work(lambda: self._load(read, kind, mime), self.loaded, self.failed)
        self.window.present()

    @staticmethod
    def _load(read, kind, mime):
        data = read()
        return data, prepare(kind, data, mime)

    def failed(self, _error):
        if self.window.get_visible():
            self.status.set_text("This file could not be shown. You can still save it.")

    def loaded(self, result):
        if not self.window.get_visible():
            return
        self.data, shown = result
        self.save_button.set_sensitive(True)
        try:
            child = getattr(self, "show_" + self.kind)(shown)
        except (GLib.Error, OSError, ValueError):
            self.failed(None)
            return
        self.body.remove(self.status)
        self.body.append(child)

    def show_image(self, pixbuf):
        picture = Gtk.Picture.new_for_paintable(Gdk.Texture.new_for_pixbuf(pixbuf))
        picture.set_can_shrink(True)
        picture.set_content_fit(Gtk.ContentFit.CONTAIN)
        picture.set_vexpand(True)
        picture.set_margin_top(16)
        picture.set_margin_bottom(16)
        picture.set_margin_start(16)
        picture.set_margin_end(16)
        picture.update_property([Gtk.AccessibleProperty.LABEL], [self.name])
        return picture

    def show_pdf(self, result):
        pages, total = result
        column = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=14)
        for edge in ("top", "bottom", "start", "end"):
            getattr(column, "set_margin_" + edge)(18)
        for number, png in enumerate(pages, 1):
            texture = Gdk.Texture.new_from_bytes(GLib.Bytes.new(png))
            picture = Gtk.Picture.new_for_paintable(texture)
            picture.set_can_shrink(False)
            picture.set_halign(Gtk.Align.CENTER)
            picture.add_css_class("pp-viewer-page")
            picture.update_property([Gtk.AccessibleProperty.LABEL], [f"Page {number}"])
            column.append(picture)
        if total > len(pages):
            note = Gtk.Label(label=f"Showing the first {len(pages)} of {total} pages. Save the file to read the rest.")
            note.add_css_class("pp-status")
            column.append(note)
        if not pages:
            raise ValueError("empty document")
        scrolled = self.scroller(column)
        scrolled.set_policy(Gtk.PolicyType.AUTOMATIC, Gtk.PolicyType.AUTOMATIC)
        return scrolled

    def show_text(self, text):
        view = Gtk.TextView(editable=False, cursor_visible=False, monospace=True, wrap_mode=Gtk.WrapMode.WORD_CHAR)
        view.add_css_class("pp-viewer-text")
        for side in ("top", "bottom", "left", "right"):
            getattr(view, f"set_{side}_margin")(16)
        view.get_buffer().set_text(text)  # A plain buffer: no markup is parsed.
        view.update_property([Gtk.AccessibleProperty.LABEL], [self.name])
        self.text_view = view
        return self.scroller(view)

    def show_video(self, data):
        self.folder = tempfile.mkdtemp(prefix="pigeonpost-view-")
        path = Path(self.folder) / ("attachment" + (Path(self.name).suffix[:12] or ".bin"))
        path.write_bytes(data)
        path.chmod(0o600)
        video = Gtk.Video.new_for_file(Gio.File.new_for_path(str(path)))
        video.set_autoplay(False)
        video.set_vexpand(True)
        video.update_property([Gtk.AccessibleProperty.LABEL], [self.name])
        self.media = video
        return video

    show_audio = show_video

    @staticmethod
    def scroller(child):
        scrolled = Gtk.ScrolledWindow(vexpand=True, hexpand=True)
        scrolled.set_policy(Gtk.PolicyType.NEVER, Gtk.PolicyType.AUTOMATIC)
        scrolled.set_child(child)
        return scrolled

    def closed(self, window):
        if self.media and self.media.get_media_stream():
            self.media.get_media_stream().pause()
            self.media.set_file(None)
        if self.folder:
            shutil.rmtree(self.folder, ignore_errors=True)
            self.folder = None
        if window in self.owner.dialogs:
            self.owner.dialogs.remove(window)
        return False
