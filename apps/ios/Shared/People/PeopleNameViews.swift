import SwiftUI

struct PeopleNotice: View {
    @Environment(PeopleStore.self) private var people
    var body: some View {
        if let error = people.state.error { Text(error).foregroundStyle(.red).accessibilityIdentifier("people-error") }
        if let notice = people.state.notice { Text(notice).foregroundStyle(.secondary).accessibilityIdentifier("people-notice") }
        if people.state.busy { ProgressView().accessibilityLabel("Updating people") }
    }
}

struct DisplayNamePage: View {
    var onboarding = false
    @Environment(PeopleStore.self) private var people
    @Environment(\.dismiss) private var dismiss
    @State private var name = ""
    @State private var visibility = "known"
    @State private var loaded = false
    var body: some View {
        let scope = people.scope
        List {
            Section {
                Text("Give your inbox a name").font(.title2.bold())
                Text("Your name and profile picture are free. A name is a label, not a reserved handle or a verified identity.").foregroundStyle(.secondary)
                #if os(iOS)
                TextField("Your name", text: $name).textContentType(.name).autocorrectionDisabled().accessibilityIdentifier("people-name")
                #else
                TextField("Your name", text: $name).accessibilityIdentifier("people-name")
                #endif
                Picker("Who can see your name", selection: $visibility) {
                    Text("People you know").tag("known")
                    Text("Everyone").tag("everyone")
                    Text("Only you").tag("nobody")
                }
                Button("Save name") {
                    Task { await people.saveName(scope, name: name, visibility: visibility)
                        if onboarding, people.scope == scope, people.state.ownName != nil, people.state.error == nil { people.dismissPrompt(scope); dismiss() }
                    }
                }.disabled(people.state.busy || (try? PeopleInput.name(name)) == nil).accessibilityIdentifier("people-save-name")
                if onboarding {
                    Button("Skip for now") { people.dismissPrompt(scope); dismiss() }.accessibilityIdentifier("people-skip-name")
                } else if people.state.ownName != nil {
                    Button("Remove name", role: .destructive) { Task { await people.removeName(scope); if people.scope == scope, people.state.error == nil { name = "" } } }
                }
            }
            Section { PeopleNotice() }
            Section { Text("Self-set names appear with ~. Paid handles remain unique addresses such as /yourname. You can also give a contact a private name that only this device uses.").font(.footnote).foregroundStyle(.secondary) }
        }.sheetSurface().navigationTitle(onboarding ? "Welcome to Pigeonpost" : "Your name")
            .task(id: scope) {
                await people.loadName(scope)
                guard people.scope == scope, !loaded, people.state.nameLoaded else { return }
                name = people.state.ownName?.displayName ?? ""; visibility = people.state.ownName?.visibility ?? "known"; loaded = true
            }
    }
}
