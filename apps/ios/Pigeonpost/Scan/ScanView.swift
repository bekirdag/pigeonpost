//  Signing another machine in, by pointing this one at it.
//
//  `pigeonpost login` on a box prints a URL, a short code, and a QR of the two together. This reads
//  the QR and hands the URL to the realm in a web session, where the sign-in is completed by
//  somebody already signed in — which is the point. The phone is not passing its own token to the
//  box: it is approving the box's own request, and the code printed beside the QR is what stops a
//  photograph of somebody's screen from being enough on its own.

import AVFoundation
import AudioToolbox
import AuthenticationServices
import SwiftUI

struct ScanView: View {
    @Environment(\.dismiss) private var dismiss
    @State private var failure: String?
    @State private var handled = false

    var body: some View {
        NavigationStack {
            ZStack {
                Color.black.ignoresSafeArea()
                CameraPreview { scanned in
                    guard !handled else { return }
                    guard let url = Self.acceptable(scanned) else {
                        failure = "That code is not a Pigeonpost sign-in."
                        return
                    }
                    handled = true
                    open(url)
                }
                .ignoresSafeArea()

                VStack {
                    Spacer()
                    Text(failure ?? "Point this at the code the terminal printed.")
                        .font(.system(size: 14))
                        .foregroundStyle(.white)
                        .multilineTextAlignment(.center)
                        .padding(14)
                        .background(.black.opacity(0.55), in: RoundedRectangle(cornerRadius: 10))
                        .padding(.horizontal, 24)
                        .padding(.bottom, 40)
                }
            }
            .navigationTitle("Sign in a machine")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar { ToolbarItem(placement: .topBarLeading) { Button("Cancel") { dismiss() } } }
        }
    }

    /// Only the realm's own sign-in pages, and only over TLS.
    ///
    /// A QR code is a URL somebody else chose. Opening whatever it says in a session that carries
    /// this person's realm cookies is how a scanner becomes a phishing tool, so the host has to be
    /// the issuer this app was built against and nothing else.
    static func acceptable(_ text: String) -> URL? {
        guard let url = URL(string: text),
              url.scheme?.lowercased() == "https",
              let host = url.host?.lowercased(),
              let issuerHost = Config.OIDC.issuer.host?.lowercased(),
              host == issuerHost
        else { return nil }
        return url
    }

    private func open(_ url: URL) {
        let session = ASWebAuthenticationSession(url: url, callbackURLScheme: nil) { _, _ in
            dismiss()
        }
        session.presentationContextProvider = ScanAnchor.shared
        // Not ephemeral: the whole flow depends on the realm recognising the person already signed
        // in on this phone. An ephemeral session would ask them to sign in again to approve a
        // sign-in, which is a joke at their expense.
        session.prefersEphemeralWebBrowserSession = false
        if !session.start() { failure = "Could not open the sign-in page." }
    }
}

private final class ScanAnchor: NSObject, ASWebAuthenticationPresentationContextProviding {
    static let shared = ScanAnchor()
    func presentationAnchor(for session: ASWebAuthenticationSession) -> ASPresentationAnchor {
        MainActor.assumeIsolated {
            UIApplication.shared.connectedScenes
                .compactMap { $0 as? UIWindowScene }
                .flatMap(\.windows)
                .first { $0.isKeyWindow } ?? ASPresentationAnchor()
        }
    }
}

/// The camera, with one job.
private struct CameraPreview: UIViewControllerRepresentable {
    let onScan: (String) -> Void

    func makeUIViewController(context: Context) -> ScannerController {
        let controller = ScannerController()
        controller.onScan = onScan
        return controller
    }

    func updateUIViewController(_ controller: ScannerController, context: Context) {}
}

final class ScannerController: UIViewController, AVCaptureMetadataOutputObjectsDelegate {
    var onScan: ((String) -> Void)?
    private let session = AVCaptureSession()
    private var preview: AVCaptureVideoPreviewLayer?

    override func viewDidLoad() {
        super.viewDidLoad()
        view.backgroundColor = .black
        guard let device = AVCaptureDevice.default(for: .video),
              let input = try? AVCaptureDeviceInput(device: device),
              session.canAddInput(input)
        else { return }
        session.addInput(input)

        let output = AVCaptureMetadataOutput()
        guard session.canAddOutput(output) else { return }
        session.addOutput(output)
        output.setMetadataObjectsDelegate(self, queue: .main)
        // Only QR. A barcode on a cereal box is not a sign-in.
        output.metadataObjectTypes = [.qr]

        let layer = AVCaptureVideoPreviewLayer(session: session)
        layer.videoGravity = .resizeAspectFill
        layer.frame = view.bounds
        view.layer.addSublayer(layer)
        preview = layer
    }

    override func viewDidAppear(_ animated: Bool) {
        super.viewDidAppear(animated)
        guard !session.isRunning else { return }
        // Off the main thread: starting a capture session blocks, and blocking here is a visible
        // stutter on the way into the screen.
        Task.detached(priority: .userInitiated) { [session] in session.startRunning() }
    }

    override func viewWillDisappear(_ animated: Bool) {
        super.viewWillDisappear(animated)
        if session.isRunning { session.stopRunning() }
    }

    override func viewDidLayoutSubviews() {
        super.viewDidLayoutSubviews()
        preview?.frame = view.bounds
    }

    func metadataOutput(
        _ output: AVCaptureMetadataOutput,
        didOutput objects: [AVMetadataObject],
        from connection: AVCaptureConnection
    ) {
        guard let code = objects.compactMap({ $0 as? AVMetadataMachineReadableCodeObject }).first,
              let text = code.stringValue
        else { return }
        AudioServicesPlaySystemSound(kSystemSoundID_Vibrate)
        onScan?(text)
    }
}
