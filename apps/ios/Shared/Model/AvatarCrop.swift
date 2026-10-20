//  The geometry of choosing a square out of a picture: pan, zoom, and the rectangle that results.
//
//  Pure, so the tests can check it without a screen. The picture always covers the square — at
//  zoom 1 its shorter side fits exactly — and the offset is clamped so no edge of it can be dragged
//  inside the frame, which is what would otherwise upload a picture with a blank strip in it.

import CoreGraphics

struct AvatarCrop: Equatable {
    /// The picture's size in pixels.
    var image: CGSize
    /// The square on screen, in points.
    var view: CGFloat
    /// 1...4.
    var zoom: CGFloat = 1
    /// Where the picture's centre is, relative to the square's centre, in points.
    var offset: CGSize = .zero

    static let maxZoom: CGFloat = 4

    /// Points per pixel.
    var scale: CGFloat {
        let short = min(image.width, image.height)
        guard short > 0 else { return 1 }
        return view / short * zoom
    }

    /// The offset nearest to `wanted` that keeps the square covered.
    func clamped(_ wanted: CGSize) -> CGSize {
        let spareX = max(0, (image.width * scale - view) / 2)
        let spareY = max(0, (image.height * scale - view) / 2)
        return CGSize(width: min(spareX, max(-spareX, wanted.width)),
                      height: min(spareY, max(-spareY, wanted.height)))
    }

    /// Zoom, keeping the point under the square's centre where it is.
    func zoomed(to next: CGFloat) -> AvatarCrop {
        var copy = self
        let target = min(Self.maxZoom, max(1, next))
        let ratio = target / zoom
        copy.zoom = target
        copy.offset = copy.clamped(CGSize(width: offset.width * ratio, height: offset.height * ratio))
        return copy
    }

    /// The square, in the picture's pixels, that is inside the frame.
    var rect: CGRect {
        let s = scale
        let left = view / 2 - image.width * s / 2 + offset.width
        let top = view / 2 - image.height * s / 2 + offset.height
        let side = min(view / s, min(image.width, image.height))
        let x = min(max(0, -left / s), image.width - side)
        let y = min(max(0, -top / s), image.height - side)
        return CGRect(x: x.rounded(), y: y.rounded(), width: side.rounded(), height: side.rounded())
    }
}
