import SwiftUI
import Observation

struct DeviceApprovalRequest: Identifiable, Equatable {
    let id = UUID()
    let code: DevicePairingCode?
}

@MainActor @Observable final class DeviceApprovalRouter {
    var pending: DeviceApprovalRequest?
    var scanned: DeviceApprovalRequest?
    func receive(_ code: DevicePairingCode) { pending = DeviceApprovalRequest(code: code) }
    @discardableResult func finishScanner() -> Bool {
        guard let scanned else { return false }
        self.scanned = nil; pending = scanned
        return true
    }
    func clear() { pending = nil; scanned = nil }
}

private struct DeviceApprovalLinks: ViewModifier {
    @Environment(Account.self) private var account
    @Environment(Inbox.self) private var inbox
    @Environment(CallHost.self) private var calls: CallHost?
    @Environment(DeviceApprovalRouter.self) private var router: DeviceApprovalRouter?
    let canPresent: Bool
    let open: (DeviceApprovalRequest) -> Void
    private struct Ready: Equatable {
        let request: DeviceApprovalRequest?
        let loaded: Bool
        let signedIn: Bool
    }
    func body(content: Content) -> some View {
        let ready = Ready(request: router?.pending, loaded: account.features.loaded,
                          signedIn: account.session.status == .signedIn)
        content.onChange(of: ready, initial: true) { _, state in
            guard state.signedIn, state.loaded, let request = state.request else { return }
            router?.pending = nil
            guard canPresent else { inbox.toast = "Close the open screen and scan or enter the machine code again."; return }
            guard account.features.orgs else { inbox.toast = "Machine sign-in is unavailable for this account."; return }
            guard calls?.mode == .hidden, calls?.preparingScheduled != true else {
                inbox.toast = "Finish the call before signing in a machine."; return
            }
            open(request)
        }
    }
}

extension View {
    func deviceApprovalLinks(canPresent: Bool, open: @escaping (DeviceApprovalRequest) -> Void) -> some View {
        modifier(DeviceApprovalLinks(canPresent: canPresent, open: open))
    }
}
