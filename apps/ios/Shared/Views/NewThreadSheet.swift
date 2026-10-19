//  Starting a thread about a new subject.
//
//  One dialog for every client. The phone's used to be a `NavigationStack` around a `Form`, and the
//  Mac's a plain stack, because the phone's leaned on modifiers a Mac does not have; the words and
//  the call they make were always the same, and now so is the frame around them.

import SwiftUI

/// A thread keeps one subject apart from the rest, so an old request does not colour a new one.
/// Both sides see the same name.
struct NewThreadSheet: View {
    let peer: String
    let onOpened: (String) -> Void

    @Environment(Inbox.self) private var inbox
    @Environment(\.dismiss) private var dismiss

    @State private var title = ""
    @State private var error: String?
    @State private var working = false

    @FocusState private var focused: Bool

    private var empty: Bool { title.trimmed.isEmpty }

    var body: some View {
        VStack(alignment: .leading, spacing: 18) {
            Text("New thread")
                .font(.geist(17, .semibold))
                .kerning(-0.17)
                .foregroundStyle(Theme.ink)

            VStack(alignment: .leading, spacing: 8) {
                Text("What is it about?")
                    .font(.geist(13, .semibold))
                    .foregroundStyle(Theme.body)
                TextField("the deploy, the failing tests, next week's release", text: $title)
                    .textFieldStyle(.plain)
                    .font(.geist(14.5))
                    .foregroundStyle(Theme.ink)
                    .focused($focused)
                    .padding(.horizontal, 14)
                    .frame(height: 44)
                    .background(Theme.wash, in: RoundedRectangle(cornerRadius: 10))
                    .overlay(RoundedRectangle(cornerRadius: 10)
                        .stroke(empty ? Theme.strokeStrong : Theme.accent, lineWidth: 1))
                    .onSubmit { if !empty { create() } }
            }

            Text("A thread keeps one subject apart from the rest. Both sides see the same name.")
                .font(.geist(13))
                .foregroundStyle(Theme.secondary)
                .fixedSize(horizontal: false, vertical: true)

            if let error {
                Text(error)
                    .font(.geist(13))
                    .foregroundStyle(Theme.Pill.blockedText)
            }

            HStack(spacing: 8) {
                Spacer()
                Button { dismiss() } label: {
                    Text("Cancel")
                        .font(.geist(13.5, .medium))
                        .foregroundStyle(Theme.ink)
                        .padding(.horizontal, 16)
                        .frame(height: 38)
                        .overlay(RoundedRectangle(cornerRadius: 9).stroke(Theme.strokeStrong, lineWidth: 1))
                        .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .keyboardShortcut(.cancelAction)
                Button { create() } label: {
                    Text("Create")
                        .font(.geist(13.5, .semibold))
                        .foregroundStyle(empty || working ? Theme.muted : Theme.onAccent)
                        .padding(.horizontal, 18)
                        .frame(height: 38)
                        .background(empty || working ? Theme.chip : Theme.accent, in: RoundedRectangle(cornerRadius: 9))
                        .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .keyboardShortcut(.defaultAction)
                .disabled(working || empty)
            }
        }
        .padding(24)
        .background(Theme.ground)
        .onAppear { focused = true }
        #if os(macOS)
        .frame(width: 480)
        #else
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .top)
        .background(Theme.ground)
        .presentationDetents([.height(300)])
        .presentationCornerRadius(16)
        #endif
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
