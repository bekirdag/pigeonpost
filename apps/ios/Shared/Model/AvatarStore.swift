//  Profile pictures of the people on screen (SDS §16.1), fetched as their faces are drawn.
//
//  The web inbox's `Avatars`, in Swift: ask `GET /v1/avatars` for up to a hundred keys at once, keep
//  what each key maps to (a picture's hash, or nothing), and fetch each picture's bytes once per
//  hash however many mailboxes share it. The bytes are fetched with the bearer token — a "people I
//  know" picture is not public — and kept in the Caches directory under the hash, so a relaunch does
//  not fetch every face again. A changed picture has a new hash; the old file is simply never asked
//  for again.

import Foundation
import Observation
import SwiftUI

@MainActor
@Observable
final class AvatarStore {
    let account: Account

    /// key → the picture it shows, or `.none` for "no picture, or not one this account may see".
    private(set) var meta: [String: AvatarInfo?] = [:]
    /// hash → decoded image.
    private(set) var images: [String: PlatformImage] = [:]
    /// hash → the bytes, for handing on (call tiles, notifications).
    @ObservationIgnored private var bytes: [String: Data] = [:]

    @ObservationIgnored private var queue: Set<String> = []
    @ObservationIgnored private var flushing = false
    @ObservationIgnored private var loadingHashes: Set<String> = []
    @ObservationIgnored private var generation = 0

    init(account: Account) {
        self.account = account
    }

    var enabled: Bool { account.features.avatars }

    /// The face for `key`, if it has one that has arrived. Asking is what fetches it.
    func image(for key: String) -> PlatformImage? {
        guard enabled, key.hasPrefix("/"), !key.hasSuffix("/*") else { return nil }
        guard let known = meta[key] else {
            want(key)
            return nil
        }
        guard let info = known else { return nil }
        return images[info.key]
    }

    /// The bytes behind a face already shown, for the call page and notifications.
    func data(for key: String) -> Data? {
        guard let info = meta[key] ?? nil else { return nil }
        return bytes[info.key]
    }

    /// The lookup `Avatar` reads from the environment.
    var lookup: AvatarLookup {
        AvatarLookup { [weak self] key in
            self?.image(for: key).map(ImageTools.swiftUIImage)
        }
    }

    /// Look again: a picture changed (ours, after an upload), or a new session.
    func forget(_ keys: [String]) {
        for key in keys { meta[key] = nil }
    }

    func reset() {
        generation += 1
        meta = [:]
        images = [:]
        bytes = [:]
        queue = []
        loadingHashes = []
    }

    private func want(_ key: String) {
        guard !queue.contains(key) else { return }
        queue.insert(key)
        guard !flushing else { return }
        flushing = true
        // After this render, not during it: the lookup is called from a view's body.
        Task { @MainActor [weak self] in
            try? await Task.sleep(nanoseconds: 30_000_000)
            await self?.flush()
        }
    }

    private func flush() async {
        defer { flushing = false }
        while !queue.isEmpty {
            let keys = Array(queue.prefix(100))
            queue.subtract(keys)
            let generation = self.generation
            #if DEBUG
            if Fixtures.enabled {
                for key in keys { installFixtureFace(key) }
                continue
            }
            #endif
            let found: [String: AvatarInfo?]
            do {
                found = try await account.client.avatars(for: keys)
            } catch {
                // Treat as "no picture" for now; initials stay, and the next session asks again.
                for key in keys { meta[key] = .some(nil) }
                continue
            }
            guard generation == self.generation else { return }
            for key in keys {
                let info = found[key] ?? nil
                guard let info, let path = info.small, path.hasPrefix("/") else {
                    meta[key] = .some(nil)
                    continue
                }
                meta[key] = .some(info)
                Task { await self.fetch(hash: info.key, path: path, generation: generation) }
            }
        }
    }

    private func fetch(hash: String, path: String, generation: Int) async {
        guard images[hash] == nil, !loadingHashes.contains(hash) else { return }
        loadingHashes.insert(hash)
        defer { loadingHashes.remove(hash) }
        let file = Self.cacheFile(hash)
        var data = file.flatMap { try? Data(contentsOf: $0) }
        if data == nil {
            data = try? await account.client.avatarBlob(path: path)
            if let data, let file { try? data.write(to: file, options: .atomic) }
        }
        guard generation == self.generation, let data, let image = ImageTools.platformImage(data) else { return }
        bytes[hash] = data
        images[hash] = image
    }

    /// Caches/avatars/<hash>. A hash names the bytes, so the file never needs invalidating.
    private static func cacheFile(_ hash: String) -> URL? {
        let safe = hash.filter { $0.isLetter || $0.isNumber || $0 == "-" || $0 == "_" || $0 == ":" }
            .replacingOccurrences(of: ":", with: "_")
        guard !safe.isEmpty,
              let caches = FileManager.default.urls(for: .cachesDirectory, in: .userDomainMask).first else { return nil }
        let folder = caches.appendingPathComponent("avatars", isDirectory: true)
        try? FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
        return folder.appendingPathComponent(safe)
    }

    /// `{peerKey: "data:image/jpeg;base64,…"}` for faces already on hand, for the call page's tiles.
    func dataURLs(for keys: [String]) -> [String: String] {
        var out: [String: String] = [:]
        for key in keys {
            guard let data = data(for: key), let small = ImageTools.thumbnailJPEG(data) else { continue }
            out[key] = "data:image/jpeg;base64," + small.base64EncodedString()
        }
        return out
    }

    #if DEBUG
    /// Fixture faces: a few people have one, the rest keep their initials.
    private static let fixtureHues: [String: Double] = [
        "/bekir/agent1": 0.58, "/bekir/main": 0.08, "/olga/main": 0.85, "/bekir/docdex": 0.33,
    ]

    private func installFixtureFace(_ key: String) {
        guard let hue = Self.fixtureHues[key], let data = ImageTools.fixtureFace(String(key.suffix(1)), hue: hue),
              let image = ImageTools.platformImage(data) else {
            meta[key] = .some(nil)
            return
        }
        let hash = "fixture-" + key
        meta[key] = .some(AvatarInfo(id: "av_fixture", version: 1, hash: hash, urls: nil, visibility: "known"))
        bytes[hash] = data
        images[hash] = image
    }
    #endif
}
