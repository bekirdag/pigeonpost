//  Pictures, both platforms: decode anything ImageIO can (HEIC and WebP included), turn it upright,
//  crop it square and write it out as JPEG.
//
//  ImageIO and Core Graphics rather than UIImage/NSImage drawing, so one path serves the phone and
//  the Mac, and so nothing of the original container — EXIF, GPS, the maker's notes — survives into
//  what leaves the device. The postbox strips all of that again; not sending it is better still.

import CoreGraphics
import Foundation
import ImageIO
import SwiftUI
import UniformTypeIdentifiers
#if canImport(UIKit)
import UIKit
typealias PlatformImage = UIImage
#elseif canImport(AppKit)
import AppKit
typealias PlatformImage = NSImage
#endif

enum ImageTools {
    /// Decode, honour the EXIF orientation, and cap the longest side. `nil` when it is not a picture.
    static func upright(_ data: Data, maxSide: Int = 4096) -> CGImage? {
        guard let source = CGImageSourceCreateWithData(data as CFData, nil),
              CGImageSourceGetCount(source) > 0 else { return nil }
        let props = CGImageSourceCopyPropertiesAtIndex(source, 0, nil) as? [CFString: Any]
        let width = (props?[kCGImagePropertyPixelWidth] as? Int) ?? 0
        let height = (props?[kCGImagePropertyPixelHeight] as? Int) ?? 0
        let longest = max(width, height)
        let options: [CFString: Any] = [
            kCGImageSourceCreateThumbnailFromImageAlways: true,
            kCGImageSourceCreateThumbnailWithTransform: true,   // applies the orientation
            kCGImageSourceThumbnailMaxPixelSize: max(1, min(maxSide, longest > 0 ? longest : maxSide)),
            kCGImageSourceShouldCacheImmediately: true,
        ]
        return CGImageSourceCreateThumbnailAtIndex(source, 0, options as CFDictionary)
    }

    /// The square `rect` (in `image`'s pixels) of `image`, scaled to at most `side` pixels.
    static func crop(_ image: CGImage, to rect: CGRect, side: Int) -> CGImage? {
        let bounds = CGRect(x: 0, y: 0, width: image.width, height: image.height)
        let clipped = rect.integral.intersection(bounds)
        guard !clipped.isNull, clipped.width >= 1, let cut = image.cropping(to: clipped) else { return nil }
        let out = min(side, Int(clipped.width))
        guard out > 0, let context = CGContext(
            data: nil, width: out, height: out, bitsPerComponent: 8, bytesPerRow: 0,
            space: CGColorSpace(name: CGColorSpace.sRGB) ?? CGColorSpaceCreateDeviceRGB(),
            bitmapInfo: CGImageAlphaInfo.noneSkipLast.rawValue
        ) else { return nil }
        context.interpolationQuality = .high
        context.draw(cut, in: CGRect(x: 0, y: 0, width: out, height: out))
        return context.makeImage()
    }

    static func jpeg(_ image: CGImage, quality: Double = 0.9) -> Data? {
        let data = NSMutableData()
        guard let destination = CGImageDestinationCreateWithData(data, UTType.jpeg.identifier as CFString, 1, nil)
        else { return nil }
        CGImageDestinationAddImage(destination, image, [kCGImageDestinationLossyCompressionQuality: quality] as CFDictionary)
        guard CGImageDestinationFinalize(destination) else { return nil }
        return data as Data
    }

    static func platformImage(_ data: Data) -> PlatformImage? {
        #if canImport(UIKit)
        return UIImage(data: data)
        #else
        return NSImage(data: data)
        #endif
    }

    static func swiftUIImage(_ image: PlatformImage) -> Image {
        #if canImport(UIKit)
        return Image(uiImage: image)
        #else
        return Image(nsImage: image)
        #endif
    }

    static func swiftUIImage(_ image: CGImage) -> Image {
        Image(decorative: image, scale: 1)
    }

    /// A small JPEG of `data`, for handing a face to the call page as a `data:` URL.
    static func thumbnailJPEG(_ data: Data, side: Int = 96) -> Data? {
        guard let image = upright(data, maxSide: side * 2) else { return nil }
        let short = min(image.width, image.height)
        let square = CGRect(x: (image.width - short) / 2, y: (image.height - short) / 2, width: short, height: short)
        return crop(image, to: square, side: side).flatMap { jpeg($0, quality: 0.8) }
    }

    #if DEBUG
    /// A face for the fixture mailbox: a two-colour disc with a letter, drawn here so no picture
    /// has to be fetched or committed.
    static func fixtureFace(_ letter: String, hue: Double) -> Data? {
        let side = 256
        guard let context = CGContext(data: nil, width: side, height: side, bitsPerComponent: 8, bytesPerRow: 0,
                                      space: CGColorSpaceCreateDeviceRGB(),
                                      bitmapInfo: CGImageAlphaInfo.noneSkipLast.rawValue) else { return nil }
        func color(_ h: Double, _ s: Double, _ b: Double) -> CGColor {
            #if canImport(UIKit)
            return UIColor(hue: h, saturation: s, brightness: b, alpha: 1).cgColor
            #else
            return NSColor(hue: h, saturation: s, brightness: b, alpha: 1).cgColor
            #endif
        }
        context.setFillColor(color(hue, 0.55, 0.85))
        context.fill(CGRect(x: 0, y: 0, width: side, height: side))
        context.setFillColor(color(fmod(hue + 0.08, 1), 0.7, 0.55))
        context.fillEllipse(in: CGRect(x: 64, y: -40, width: 128, height: 150))
        context.fillEllipse(in: CGRect(x: 88, y: 120, width: 80, height: 80))
        _ = letter
        return context.makeImage().flatMap { jpeg($0) }
    }
    #endif
}
