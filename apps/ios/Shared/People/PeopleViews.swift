import SwiftUI
import Contacts
#if os(iOS)
import ContactsUI
#endif

struct PhoneNumbersPage: View {
    @Environment(PeopleStore.self) private var people
    @State private var phone = ""
    @State private var region = Locale.current.region?.identifier ?? "US"
    @State private var code = ""
    @State private var discoverable = false
    @State private var localError: String?
    @State private var sending = false
    @State private var removing: PPPhoneRecord?
    var body: some View {
        let scope = people.scope
        List {
            Section {
                Text("Associate a phone number with this inbox. Your number stays hidden from other users. People can find you only when you turn discovery on.")
                PhoneRegionPicker(region: $region).accessibilityIdentifier("people-region")
                #if os(iOS)
                TextField("Phone number", text: $phone).textContentType(.telephoneNumber).keyboardType(.phonePad).accessibilityIdentifier("people-phone")
                #else
                TextField("Phone number", text: $phone).accessibilityIdentifier("people-phone")
                #endif
                TimelineView(.periodic(from: .now, by: 1)) { timeline in
                    let wait = max(0, (people.state.challenge?.resendAfter ?? 0) - Int(timeline.date.timeIntervalSince1970))
                    Button(wait > 0 ? "Send again in \(wait)s" : (people.state.challenge == nil ? "Send verification code" : "Send another code")) {
                        sending = true; localError = nil
                        let input = phone; let country = region.uppercased()
                        Task {
                            defer { sending = false }
                            do { let normalized = try await PhoneContacts.shared.normalize(input, region: country)
                                guard people.scope == scope else { return }; await people.sendCode(scope, phone: normalized)
                            } catch { if people.scope == scope { localError = "Check the number and country code." } }
                        }
                    }.disabled(wait > 0 || sending || people.state.busy || phone.isEmpty).accessibilityIdentifier("people-send-code")
                }
            } header: { Text("Your phone number") } footer: { Text("A verification SMS will be sent when you press the button. This does not invite anyone.") }
            if people.state.challenge != nil {
                Section("Verification code") {
                    Text("Enter the code sent to \(people.state.challengePhone ?? "your phone").").foregroundStyle(.secondary)
                    #if os(iOS)
                    TextField("Six-digit code", text: $code).textContentType(.oneTimeCode).keyboardType(.numberPad).accessibilityIdentifier("people-code")
                    #else
                    TextField("Six-digit code", text: $code).accessibilityIdentifier("people-code")
                    #endif
                    Toggle("Let people find this inbox by my number", isOn: $discoverable)
                    Button("Verify number") { Task { await people.verify(scope, code: code, discoverable: discoverable); if people.scope == scope, people.state.challenge == nil { code = ""; phone = "" } } }
                        .disabled(people.state.busy || !PeopleInput.matches(code, "^[0-9]{6}$")).accessibilityIdentifier("people-verify")
                }
            }
            Section("Verified numbers") {
                ForEach(people.state.phones) { number in
                    VStack(alignment: .leading, spacing: 8) {
                        Text(number.masked).font(.headline)
                        Text(number.identity).font(.caption.monospaced()).textSelection(.enabled)
                        Toggle("Discoverable", isOn: Binding(get: { number.discoverable }, set: { value in Task { await people.setDiscoverable(scope, phone: number, value: value) } }))
                            .disabled(people.state.busy)
                        Button("Unlink number", role: .destructive) { removing = number }.disabled(people.state.busy)
                    }
                }
                if people.state.phones.isEmpty { Text("No verified numbers yet.").foregroundStyle(.secondary) }
            }
            Section { if let localError { Text(localError).foregroundStyle(.red) }; PeopleNotice() }
        }.sheetSurface().task(id: scope) { await people.loadPhones(scope) }
            .confirmationDialog("Unlink this number?", isPresented: Binding(get: { removing != nil }, set: { if !$0 { removing = nil } }), titleVisibility: .visible) {
                if let number = removing { Button("Unlink number", role: .destructive) { removing = nil; Task { await people.unlink(scope, phone: number) } } }
            } message: { Text("People will no longer find the inbox through this number. You can verify it again later.") }
    }
}

struct FindPeoplePage: View {
    @Environment(PeopleStore.self) private var people
    @Environment(Inbox.self) private var inbox
    @Environment(\.dismiss) private var dismiss
    @Environment(\.openURL) private var openURL
    @Environment(\.scenePhase) private var phase
    @State private var disclosure = false
    @State private var picker = false
    @State private var importing = false
    @State private var limited = false
    @State private var localNotice: String?
    @State private var localError: String?
    @State private var region = Locale.current.region?.identifier ?? "US"
    var body: some View {
        let scope = people.scope
        List {
            Section {
                Text("Find people who have verified a number and chosen to be discoverable.")
                PhoneRegionPicker(region: $region).accessibilityIdentifier("contacts-region")
                Button("Choose contacts") { disclosure = true }.disabled(importing || people.state.busy).accessibilityIdentifier("contacts-choose")
                if limited {
                    Text("Only the contacts you shared with Pigeonpost are available.").foregroundStyle(.secondary)
                    #if os(iOS)
                    if #available(iOS 18, *) { Button("Choose more contacts") { picker = true }.accessibilityIdentifier("contacts-choose-more") }
                    #endif
                }
                if CNContactStore.authorizationStatus(for: .contacts) == .denied || CNContactStore.authorizationStatus(for: .contacts) == .restricted {
                    Button("Open contact permission settings") {
                        #if os(iOS)
                        if let url = URL(string: UIApplication.openSettingsURLString) { openURL(url) }
                        #else
                        if let url = URL(string: "x-apple.systempreferences:com.apple.preference.security?Privacy_Contacts") { openURL(url) }
                        #endif
                    }.accessibilityIdentifier("contacts-settings")
                }
                if importing { ProgressView("Reading shared contacts…") }
                if let localNotice { Text(localNotice).foregroundStyle(.secondary) }
                if let localError { Text(localError).foregroundStyle(.red) }
            }
            if !people.state.contacts.isEmpty {
                Section {
                    Text(people.state.contacts.count == 1 ? "1 phone number ready on this device." : "\(people.state.contacts.count) phone numbers ready on this device.")
                    Button("Find on Pigeonpost") { Task { await people.discover(scope) } }
                        .disabled(people.state.busy || people.state.remaining == 0).accessibilityIdentifier("contacts-find")
                    Button("Clear imported contacts", role: .destructive) { people.clearContacts(scope) }
                } footer: { Text("Only phone-number hashes are sent. Hashes can still identify a number; abuse-prevention marks expire after 7 days. Names and your address book stay on this device.") }
                Section("Contacts") {
                    ForEach(people.state.contacts) { contact in
                        VStack(alignment: .leading, spacing: 6) {
                            Text(contact.name.isEmpty ? contact.phone : contact.name).font(.headline)
                            if !contact.name.isEmpty { Text(contact.phone).font(.caption).foregroundStyle(.secondary) }
                            if let match = people.state.matches[contact.hash] {
                                Text(match.handle ?? match.displayName.map { "~" + $0 } ?? match.address)
                                Text(match.address).font(.caption.monospaced()).textSelection(.enabled)
                                Button("Start a conversation") {
                                    guard people.scope == scope else { return }
                                    people.requestConversation(scope, peer: match.handle ?? match.address)
                                    dismiss()
                                }
                                if !contact.name.isEmpty {
                                    Button("Use contact name on this device") { people.alias(scope, peer: match.handle ?? match.address, name: contact.name) }
                                }
                            } else if people.state.checked.contains(contact.hash) {
                                Text("No match found").foregroundStyle(.secondary)
                                if people.has("sms_invites") {
                                    Button("Invite by SMS") { people.reviewInvite(scope, contact: contact) }
                                        .disabled(people.state.busy || !people.state.phones.contains(where: { $0.identity == scope?.mailbox })).accessibilityIdentifier("contacts-invite-" + contact.hash)
                                }
                            } else { Text("Not checked yet").foregroundStyle(.secondary) }
                        }.padding(.vertical, 4)
                    }
                }
            }
            Section { PeopleNotice() }
        }.sheetSurface()
            .contactsAccessPicker(isPresented: $picker) { people.clearContacts(scope); localNotice = "Your shared contacts changed. Choose contacts again to refresh the list." }
            .task(id: scope) { await people.loadPhones(scope) }
            .onChange(of: phase) { _, phase in if phase == .active, !PhoneContacts.allowed { people.clearContacts(scope) } }
            .onReceive(NotificationCenter.default.publisher(for: .CNContactStoreDidChange)) { _ in
                people.clearContacts(scope); localNotice = "Contacts changed. Choose contacts again to refresh."
            }
            .alert("Find your contacts", isPresented: $disclosure) {
                Button("Continue to contacts") { read(scope) }.accessibilityIdentifier("contacts-disclosure-continue")
                Button("Cancel", role: .cancel) { }
            } message: { Text("Pigeonpost reads only contacts you allow. Their names and numbers stay in memory on this device. Press Find on Pigeonpost to send phone-number hashes for matching; abuse-prevention marks expire after 7 days. No SMS is sent without a separate confirmation.") }
            .alert("Send an invitation by SMS?", isPresented: Binding(get: { people.state.invitation != nil }, set: { if !$0 { people.cancelInvite() } })) {
                if let review = people.state.invitation {
                    Button("Send one SMS invitation") {
                        if let confirmed = people.prepareInviteConfirmation(review) { Task { await people.confirmInvite(confirmed) } }
                    }.accessibilityIdentifier("contacts-confirm-invite")
                    Button("Cancel", role: .cancel) { people.cancelInvite() }
                }
            } message: {
                Text("Pigeonpost will send a join link to \(people.state.invitation?.contact.phone ?? "this number"). Invite only someone who expects to hear from you. Your verified inbox is required; daily and monthly limits apply.")
            }
    }
    private func read(_ scope: PeopleScope?) {
        importing = true; localError = nil; let region = region.uppercased()
        Task {
            defer { importing = false }
            do {
                let result = try await PhoneContacts.shared.read(region: region)
                guard people.scope == scope, PhoneContacts.allowed else { return }
                people.importContacts(scope, contacts: result.contacts); limited = result.limited
                localNotice = result.bounded ? "The first 5,000 valid phone numbers are available. Narrow shared contacts to check others." : "\(result.contacts.count) valid phone numbers loaded. Nothing has been uploaded."
            } catch { if people.scope == scope { localError = "Could not read contacts. Check the country and Contacts permission in system Settings." } }
        }
    }
}

private extension View {
    @ViewBuilder func contactsAccessPicker(isPresented: Binding<Bool>, changed: @escaping () -> Void) -> some View {
        #if os(iOS)
        if #available(iOS 18, *) { self.contactAccessPicker(isPresented: isPresented) { _ in changed() } }
        else { self }
        #else
        self
        #endif
    }
}

struct LocalAliasSection: View {
    let peer: String
    @Environment(PeopleStore.self) private var people
    @State private var name = ""
    var body: some View {
        let scope = people.scope
        Section {
            TextField("Name on this device", text: $name).accessibilityIdentifier("people-local-alias")
            Button("Save local name") { people.alias(scope, peer: peer, name: name) }
                .disabled(!name.trimmingCharacters(in: .whitespaces).isEmpty && (try? PeopleInput.name(name, alias: true)) == nil)
            PeopleNotice()
        } header: { Text("Private contact name") } footer: { Text("Only this device and inbox use this name. It does not grant permissions or change their handle. Leave it empty to remove it.") }
            .task(id: scope) { name = people.localName(peer) ?? "" }
    }
}

private struct PhoneRegionPicker: View {
    @Binding var region: String
    private static let countries: [(code: String, name: String)] = Locale.isoRegionCodes.filter { $0.count == 2 }
        .map { ($0, Locale.current.localizedString(forRegionCode: $0) ?? $0) }
        .sorted { $0.1.localizedStandardCompare($1.1) == .orderedAscending }
    var body: some View {
        Picker("Country for local numbers", selection: $region) {
            ForEach(Self.countries, id: \.code) { country in Text(country.name).tag(country.code) }
        }
    }
}
