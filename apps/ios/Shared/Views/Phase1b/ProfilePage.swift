//  Settings → Profile (SDS §16): this mailbox's picture, who may see it, and the square it is cut to.
//
//  Everything that leaves the device is a JPEG made here: decoded (HEIC included), turned upright,
//  cropped to the square chosen and scaled to at most 1024 px. The original — its EXIF, its location,
//  its full resolution — never leaves. The postbox re-encodes it again in an isolated worker anyway;
//  not sending what it would throw away is better still.

import PhotosUI
import SwiftUI
import UniformTypeIdentifiers

struct ProfilePage: View {
    @Environment(Account.self) private var account
    @Environment(AvatarStore.self) private var avatars: AvatarStore?

    @State private var own: AvatarInfo?
    @State private var ownImage: PlatformImage?
    @State private var loaded = false
    @State private var visibility = "known"
    @State private var allMailboxes = false
    @State private var note: String?
    @State private var said: String?
    @State private var working = false

    @State private var photo: PhotosPickerItem?
    @State private var cropping: CropSubject?
    @State private var importing = false
    #if os(iOS)
    @State private var camera = false
    #endif

    struct CropSubject: Identifiable {
        let id = UUID()
        let image: CGImage
    }

    private var key: String { account.me?.key ?? "" }

    var body: some View {
        List {
            Section {
                HStack {
                    Spacer()
                    ZStack {
                        Avatar(peer: key, size: 112)
                        if let ownImage {
                            ImageTools.swiftUIImage(ownImage)
                                .resizable()
                                .scaledToFill()
                                .frame(width: 112, height: 112)
                                .clipShape(Circle())
                        }
                    }
                    .accessibilityElement()
                    .accessibilityLabel(own == nil ? "No picture" : "Your picture")
                    .accessibilityIdentifier("profilePicture")
                    Spacer()
                }
                .listRowBackground(Color.clear)
                .listRowSeparator(.hidden)
                if let note {
                    Text(note)
                        .font(.geist(13))
                        .foregroundStyle(Theme.muted)
                        .frame(maxWidth: .infinity)
                        .multilineTextAlignment(.center)
                        .listRowBackground(Color.clear)
                }
            }

            Section {
                PhotosPicker(selection: $photo, matching: .images, photoLibrary: .shared()) {
                    Label(own == nil ? "Choose a picture" : "Choose a new picture", systemImage: "photo.on.rectangle")
                }
                .disabled(working)
                .accessibilityIdentifier("profileChoose")
                #if os(iOS)
                if UIImagePickerController.isSourceTypeAvailable(.camera) {
                    Button { camera = true } label: { Label("Take a photo", systemImage: "camera") }
                        .disabled(working)
                }
                #else
                Button { importing = true } label: { Label("Choose a file…", systemImage: "folder") }
                    .disabled(working)
                #endif
                Toggle("Use for all my mailboxes", isOn: $allMailboxes)
                    .disabled(account.mailboxes.count < 2)
                    .accessibilityIdentifier("profileAll")
            } footer: {
                Text("A new picture replaces the one this mailbox has. With “all my mailboxes” on, every mailbox on this account gets it.")
            }

            Section {
                Picker("Who can see it", selection: Binding(get: { visibility }, set: setVisibility)) {
                    Text("Everyone").tag("everyone")
                    Text("People I know").tag("known")
                    Text("Nobody").tag("nobody")
                }
                .pickerStyle(.segmented)
                .labelsHidden()
                .disabled(working)
                .accessibilityIdentifier("profileVisibility")
            } header: {
                Text("Who can see it")
            } footer: {
                Text("People you know are your contacts and anyone you have written to. Everyone else sees your initials.")
            }

            if own != nil {
                Section {
                    Button("Remove picture", role: .destructive) { remove() }
                        .disabled(working)
                        .accessibilityIdentifier("profileRemove")
                }
            }
        }
        .sheetSurface()
        .task(id: account.me?.address) { await load() }
        .onChange(of: photo) { _, item in
            guard let item else { return }
            photo = nil
            Task {
                guard let data = try? await item.loadTransferable(type: Data.self) else {
                    said = "That picture could not be opened."
                    return
                }
                open(data)
            }
        }
        #if os(iOS)
        .fullScreenCover(isPresented: $camera) {
            CameraPicker { data in
                camera = false
                if let data { open(data) }
            }
            .ignoresSafeArea()
        }
        #endif
        .fileImporter(isPresented: $importing, allowedContentTypes: [.image]) { result in
            guard case let .success(url) = result else { return }
            let scoped = url.startAccessingSecurityScopedResource()
            defer { if scoped { url.stopAccessingSecurityScopedResource() } }
            if let data = try? Data(contentsOf: url) { open(data) } else { said = "That picture could not be opened." }
        }
        .sheet(item: $cropping) { subject in
            AvatarCropSheet(image: subject.image) { rect in
                cropping = nil
                if let rect { upload(subject.image, rect: rect) }
            }
        }
        .toast($said)
    }

    private func load() async {
        guard let address = account.me?.address else { return }
        #if DEBUG
        if Fixtures.enabled {
            if !loaded, let data = ImageTools.fixtureFace("B", hue: 0.08) {
                own = AvatarInfo(id: "av_fixture", version: 1, hash: "fixture-own", urls: nil, visibility: "known")
                ownImage = ImageTools.platformImage(data)
            }
            visibility = own?.visibility ?? visibility
            loaded = true
            note = own == nil ? "No picture yet. People see your initials." : "This picture belongs to the mailbox you are using now."
            return
        }
        #endif
        note = "Loading…"
        do {
            let info = try await account.client.ownAvatar(identity: address)
            guard address == account.me?.address else { return }
            own = info
            if let shown = info?.visibility { visibility = shown }
            note = info == nil ? "No picture yet. People see your initials." : "This picture belongs to the mailbox you are using now."
            ownImage = nil
            if let path = info?.large, let data = try? await account.client.avatarBlob(path: path) {
                ownImage = ImageTools.platformImage(data)
            }
        } catch {
            note = "Could not load your picture. Try again later."
        }
        loaded = true
    }

    private func open(_ data: Data) {
        guard let image = ImageTools.upright(data, maxSide: 4096) else {
            said = "That picture could not be opened."
            return
        }
        cropping = CropSubject(image: image)
    }

    private var keysToForget: [String] {
        let mailboxes = allMailboxes ? account.mailboxes : account.me.map { [$0] } ?? []
        return mailboxes.flatMap { [$0.address] + ($0.handle.map { [$0] } ?? []) }
    }

    private func upload(_ image: CGImage, rect: CGRect) {
        guard let address = account.me?.address else { return }
        guard let square = ImageTools.crop(image, to: rect, side: 1024), let jpeg = ImageTools.jpeg(square) else {
            said = "That picture could not be prepared."
            return
        }
        working = true
        Task {
            defer { working = false }
            #if DEBUG
            if Fixtures.enabled {
                own = AvatarInfo(id: "av_fixture", version: 2, hash: "fixture-new", urls: nil, visibility: visibility)
                ownImage = ImageTools.platformImage(jpeg)
                note = allMailboxes ? "Saved for all your mailboxes." : "Saved."
                return
            }
            #endif
            do {
                let side = square.width
                let saved = try await account.client.uploadAvatar(identity: address, jpeg: jpeg, crop: (0, 0, side, side),
                                                                  visibility: visibility, allMailboxes: allMailboxes)
                guard address == account.me?.address else { return }
                avatars?.forget(keysToForget)
                note = allMailboxes ? "Saved for all your mailboxes." : "Saved."
                if let saved { own = saved }
                ownImage = ImageTools.platformImage(jpeg)
            } catch let error as APIError {
                said = Self.message(status: error.status, code: error.code)
            } catch {
                said = "Could not reach your inbox. Please try again."
            }
        }
    }

    private func setVisibility(_ value: String) {
        let previous = visibility
        visibility = value
        guard own != nil, let address = account.me?.address else { return }   // applied with the next upload
        guard !Fixtures.enabled else { said = "Picture visibility saved."; return }
        working = true
        Task {
            defer { working = false }
            do {
                own = try await account.client.setAvatarVisibility(identity: address, visibility: value) ?? own
                avatars?.forget(keysToForget)
                said = "Picture visibility saved."
            } catch {
                visibility = previous
                said = "Could not change who can see your picture."
            }
        }
    }

    private func remove() {
        guard let address = account.me?.address else { return }
        working = true
        Task {
            defer { working = false }
            do {
                if !Fixtures.enabled { try await account.client.removeAvatar(identity: address) }
                own = nil
                ownImage = nil
                avatars?.forget(keysToForget)
                note = "Picture removed. People see your initials."
            } catch {
                said = "Could not remove your picture."
            }
        }
    }

    /// The web's `avatarError`, word for word.
    static func message(status: Int, code: String?) -> String {
        if status == 429 || code == "avatar_rate_limited" { return "You can change your picture five times a day. Try again tomorrow." }
        if status == 413 { return "That picture is too large. Choose one under 10 MB." }
        if status == 415 || status == 422 { return "That picture could not be used. Try a JPEG or PNG." }
        return "Could not save your picture. Please try again."
    }
}

/// Pan and zoom a picture under a round mask, and say which square of it to keep.
struct AvatarCropSheet: View {
    let image: CGImage
    /// The square in pixels, or `nil` for cancel.
    let done: (CGRect?) -> Void

    @State private var crop: AvatarCrop
    @State private var dragStart: CGSize?
    @State private var pinchStart: CGFloat?
    private let side: CGFloat = 280

    init(image: CGImage, done: @escaping (CGRect?) -> Void) {
        self.image = image
        self.done = done
        _crop = State(initialValue: AvatarCrop(image: CGSize(width: image.width, height: image.height), view: 280))
    }

    var body: some View {
        NavigationStack {
            VStack(spacing: 22) {
                ZStack {
                    ImageTools.swiftUIImage(image)
                        .resizable()
                        .frame(width: CGFloat(image.width) * crop.scale, height: CGFloat(image.height) * crop.scale)
                        .offset(crop.offset)
                    // The round face people will see; outside it, a shade.
                    Rectangle()
                        .fill(Color.black.opacity(0.45))
                        .mask {
                            Rectangle()
                                .overlay(Circle().blendMode(.destinationOut))
                                .compositingGroup()
                        }
                        .allowsHitTesting(false)
                }
                .frame(width: side, height: side)
                .clipped()
                .contentShape(Rectangle())
                .gesture(drag.simultaneously(with: pinch))
                .accessibilityElement()
                .accessibilityLabel("Picture crop. Drag to move, pinch to zoom.")
                .accessibilityIdentifier("cropArea")

                HStack(spacing: 10) {
                    Image(systemName: "minus.magnifyingglass").foregroundStyle(Theme.muted)
                    Slider(value: Binding(get: { Double(crop.zoom) }, set: { crop = crop.zoomed(to: CGFloat($0)) }),
                           in: 1...Double(AvatarCrop.maxZoom))
                        .accessibilityLabel("Zoom")
                        .accessibilityIdentifier("cropZoom")
                    Image(systemName: "plus.magnifyingglass").foregroundStyle(Theme.muted)
                }
                .frame(width: side)
                Spacer(minLength: 0)
            }
            .padding(.top, 24)
            .frame(maxWidth: .infinity)
            .background(Theme.app)
            .navigationTitle("Move and scale")
            .inlineTitle()
            .toolbar {
                ToolbarItem(placement: .cancellationAction) { Button("Cancel") { done(nil) } }
                ToolbarItem(placement: .confirmationAction) {
                    Button("Save") { done(crop.rect) }
                        .accessibilityIdentifier("cropSave")
                }
            }
        }
        #if os(macOS)
        .frame(width: 420, height: 460)
        #endif
    }

    private var drag: some Gesture {
        DragGesture()
            .onChanged { value in
                let start = dragStart ?? crop.offset
                dragStart = start
                crop.offset = crop.clamped(CGSize(width: start.width + value.translation.width,
                                                  height: start.height + value.translation.height))
            }
            .onEnded { _ in dragStart = nil }
    }

    private var pinch: some Gesture {
        MagnifyGesture()
            .onChanged { value in
                let start = pinchStart ?? crop.zoom
                pinchStart = start
                crop = crop.zoomed(to: start * value.magnification)
            }
            .onEnded { _ in pinchStart = nil }
    }
}

#if os(iOS)
/// The system camera, for a picture taken now. Returns JPEG bytes, or nil if cancelled.
struct CameraPicker: UIViewControllerRepresentable {
    let done: (Data?) -> Void

    func makeUIViewController(context: Context) -> UIImagePickerController {
        let picker = UIImagePickerController()
        picker.sourceType = .camera
        picker.cameraDevice = .front
        picker.delegate = context.coordinator
        return picker
    }

    func updateUIViewController(_ controller: UIImagePickerController, context: Context) {}

    func makeCoordinator() -> Coordinator { Coordinator(done: done) }

    final class Coordinator: NSObject, UIImagePickerControllerDelegate, UINavigationControllerDelegate {
        let done: (Data?) -> Void
        init(done: @escaping (Data?) -> Void) { self.done = done }

        func imagePickerController(_ picker: UIImagePickerController,
                                   didFinishPickingMediaWithInfo info: [UIImagePickerController.InfoKey: Any]) {
            done((info[.originalImage] as? UIImage)?.jpegData(compressionQuality: 0.95))
        }

        func imagePickerControllerDidCancel(_ picker: UIImagePickerController) { done(nil) }
    }
}
#endif
