import AuthenticationServices
import SwiftUI

/// Apple's own control draws and localizes the branding. Authentication still uses our
/// existing Keycloak session so an Apple login reaches the same Pigeonpost account.
#if canImport(UIKit)
struct AppleSignInButton: UIViewRepresentable {
    var enabled = true
    let action: () -> Void

    func makeCoordinator() -> Coordinator { Coordinator(action) }
    func makeUIView(context: Context) -> ASAuthorizationAppleIDButton {
        let button = ASAuthorizationAppleIDButton(type: .signIn, style: .black)
        button.cornerRadius = 10
        button.accessibilityIdentifier = "signInWithApple"
        button.addTarget(context.coordinator, action: #selector(Coordinator.pressed), for: .touchUpInside)
        return button
    }
    func updateUIView(_ button: ASAuthorizationAppleIDButton, context: Context) {
        context.coordinator.action = action
        button.isEnabled = enabled
    }
    final class Coordinator: NSObject {
        var action: () -> Void
        init(_ action: @escaping () -> Void) { self.action = action }
        @objc func pressed() { action() }
    }
}
#else
struct AppleSignInButton: NSViewRepresentable {
    var enabled = true
    let action: () -> Void

    func makeCoordinator() -> Coordinator { Coordinator(action) }
    func makeNSView(context: Context) -> ASAuthorizationAppleIDButton {
        let button = ASAuthorizationAppleIDButton(type: .signIn, style: .black)
        button.cornerRadius = 10
        button.setAccessibilityIdentifier("signInWithApple")
        button.target = context.coordinator
        button.action = #selector(Coordinator.pressed)
        return button
    }
    func updateNSView(_ button: ASAuthorizationAppleIDButton, context: Context) {
        context.coordinator.action = action
        button.isEnabled = enabled
    }
    final class Coordinator: NSObject {
        var action: () -> Void
        init(_ action: @escaping () -> Void) { self.action = action }
        @objc func pressed() { action() }
    }
}
#endif
