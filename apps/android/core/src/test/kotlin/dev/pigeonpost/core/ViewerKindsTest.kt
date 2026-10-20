package dev.pigeonpost.core

import org.junit.Assert.*
import org.junit.Test

class ViewerKindsTest {
    @Test fun browsableTypesOpenInApp() {
        assertEquals(ViewerKind.IMAGE, AttachmentKinds.viewer("photo.jpg", "image/jpeg"))
        assertEquals(ViewerKind.IMAGE, AttachmentKinds.viewer("photo.png", "application/octet-stream"))
        assertEquals(ViewerKind.VIDEO, AttachmentKinds.viewer("clip.mp4", "video/mp4"))
        assertEquals(ViewerKind.AUDIO, AttachmentKinds.viewer("note.m4a", ""))
        assertEquals(ViewerKind.PDF, AttachmentKinds.viewer("report.pdf", "application/pdf; charset=binary"))
        assertEquals(ViewerKind.TEXT, AttachmentKinds.viewer("build-notes.txt", "text/plain"))
        assertEquals(ViewerKind.TEXT, AttachmentKinds.viewer("data.json", "application/json"))
    }
    @Test fun markupAndUnknownFilesNeverRender() {
        assertNull(AttachmentKinds.viewer("logo.svg", "image/svg+xml"))
        assertNull(AttachmentKinds.viewer("page.html", "text/html"))
        assertNull(AttachmentKinds.viewer("page.html", "text/plain"))
        assertNull(AttachmentKinds.viewer("logo.svg", "application/octet-stream"))
        assertNull(AttachmentKinds.viewer("feed.xml", "application/xml"))
        assertNull(AttachmentKinds.viewer("archive.zip", "application/zip"))
        assertNull(AttachmentKinds.viewer("binary", "application/octet-stream"))
    }
    @Test fun galleryOnlyForPhotosAndVideos() {
        assertTrue(AttachmentKinds.savesToGallery("a.jpg", "image/jpeg"))
        assertTrue(AttachmentKinds.savesToGallery("a.mov", "video/quicktime"))
        assertFalse(AttachmentKinds.savesToGallery("a.pdf", "application/pdf"))
        assertFalse(AttachmentKinds.savesToGallery("a.mp3", "audio/mpeg"))
    }
    @Test fun peerFaceMatchesTheWebHash() {
        // JavaScript: [..."/bekir"].reduce((h, c) => (h * 31 + c.charCodeAt(0)) >>> 0, 0) % 6 + 1
        var h = 0L; for (ch in "/bekir") h = (h * 31 + ch.code) and 0xFFFFFFFFL
        assertEquals((h % 6).toInt() + 1, PeerFace.toneIndex("/bekir"))
        assertEquals("DO", PeerFace.initials("/bekir/docdex"))
        assertEquals("··", PeerFace.initials("/"))
    }
}
