//  Starting a thread about a new subject.
//
//  The phone's `NewThreadSheet` is a `NavigationStack` with a `Form`, `navigationBarTitleDisplayMode`
//  and presentation detents — four things that either do not exist on macOS or read wrong there. The
//  words and the call it makes are the same; only the frame around them differs.

import SwiftUI

struct MacNewThreadSheet: View {
    let peer: String
    let onOpened: (String) -> Void

    @Environment(Inbox.self) private var inbox
    @Environment(\.dismiss) private var dismiss

    @State private var title = ""
    @State private var error: String?
    @State private var working = false

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            Text("New thread")
                .font(.system(size: 15, weight: .semibold))
                .foregroundStyle(Theme.ink)

            VStack(alignment: .leading, spacing: 6) {
                Text("What is it about?")
                    .font(.system(size: 12, weight: .medium))
                    .foregroundStyle(Theme.muted)
                TextField("the deploy, the failing tests, next week's release", text: $title)
                    .textFieldStyle(.roundedBorder)
                    .onSubmit { if !title.trimmed.isEmpty { create() } }
                Text("A thread keeps one subject apart from the rest. Both sides see the same name.")
                    .font(.system(size: 11))
                    .foregroundStyle(Theme.muted)
            }

            if let error {
                Text(error)
                    .font(.system(size: 12))
                    .foregroundStyle(Theme.Pill.blockedText)
            }

            HStack {
                Spacer()
                Button("Cancel") { dismiss() }
                    .keyboardShortcut(.cancelAction)
                Button("Create") { create() }
                    .keyboardShortcut(.defaultAction)
                    .disabled(working || title.trimmed.isEmpty)
            }
        }
        .padding(18)
        .frame(width: 420)
    }

    private func create() {
        working = true
        error = nil
        Task {
            do {
                let id = try await inbox.openThread(with: peer, title: title.trimmed)
                onOpened(id)
                dismiss()
            } catch let failure as APIError {
                error = failure.errorDescription
            } catch {
                self.error = "Could not open the thread."
            }
            working = false
        }
    }
}
