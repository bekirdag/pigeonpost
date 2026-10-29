import Foundation
import Observation

struct PeopleScope: Equatable, Hashable {
    let subject: String
    let authorization: UUID
    let actor: UUID
    let mailbox: String
    let features: Set<String>
}

@MainActor protocol PeopleLocalStorage {
    func aliases(subject: String, mailbox: String) -> [String: String]
    func setAlias(subject: String, mailbox: String, peer: String, name: String?) throws
    func promptSeen(subject: String, mailbox: String) -> Bool
    func markPromptSeen(subject: String, mailbox: String) throws
    func clear(subject: String) throws
}

struct PeopleState {
    var phones: [PPPhoneRecord] = []
    var challenge: PPPhoneChallenge?
    var challengePhone: String?
    var ownName: PPDisplayName?
    var nameLoaded = false
    var promptSeen = false
    var contacts: [PhoneContact] = []
    var matches: [String: PPDiscoveryMatch] = [:]
    var checked: Set<String> = []
    var remaining: Int?
    var profiles: [String: PPProfile] = [:]
    var aliases: [String: String] = [:]
    var invitation: InviteReview?
    var conversationRequest: String?
    var busy = false
    var notice: String?
    var error: String?
    struct InviteReview: Identifiable, Equatable {
        let id = UUID()
        let scope: PeopleScope
        let contact: PhoneContact
    }
}

@MainActor @Observable final class PeopleStore {
    @ObservationIgnored private let current: () -> PeopleScope?
    @ObservationIgnored private let api: () -> PeopleAPI
    @ObservationIgnored private let storage: PeopleLocalStorage
    private var owner: PeopleScope?
    private var cached = PeopleState()
    private var operation = UUID()
    private var confirmedInvite: PeopleState.InviteReview?
    private var profileRefresh = Date.distantPast

    init(scope: @escaping () -> PeopleScope?, api: @escaping () -> PeopleAPI, storage: PeopleLocalStorage) {
        current = scope; self.api = api; self.storage = storage
    }
    var scope: PeopleScope? { current() }
    var state: PeopleState { owner == current() ? cached : PeopleState() }
    func has(_ feature: String) -> Bool { current()?.features.contains(feature) == true }
    func synchronize() {
        guard owner != current() else { return }
        owner = current(); operation = UUID(); confirmedInvite = nil; cached = PeopleState(); profileRefresh = .distantPast
        if let owner {
            cached.aliases = storage.aliases(subject: owner.subject, mailbox: owner.mailbox)
            cached.promptSeen = storage.promptSeen(subject: owner.subject, mailbox: owner.mailbox)
        }
    }
    private func valid(_ scope: PeopleScope?) -> Bool { scope != nil && scope == current() && scope == owner && !Task.isCancelled }
    /// The caller captures `scope` before creating its Task. A stale sheet cannot act as a new login.
    private func perform(_ scope: PeopleScope?, feature: String, action: (PeopleAPI, PeopleScope) async throws -> Void) async {
        synchronize()
        guard let scope, valid(scope), scope.features.contains(feature), !cached.busy else { return }
        let operation = UUID(); self.operation = operation; cached.busy = true; cached.error = nil; cached.notice = nil
        let client = api()
        defer { if self.operation == operation && valid(scope) { cached.busy = false } }
        do { try await action(client, scope) }
        catch is CancellationError { }
        catch { if valid(scope) { cached.error = error.localizedDescription } }
    }
    /// Initial page reads wait for background work; explicit mutations are never queued or replayed.
    private func readReady(_ scope: PeopleScope?) async -> Bool {
        synchronize()
        while valid(scope), cached.busy {
            do { try await Task.sleep(for: .milliseconds(25)) } catch { return false }
        }
        return valid(scope)
    }
    func loadName(_ scope: PeopleScope?) async {
        guard await readReady(scope), !state.nameLoaded else { return }
        await perform(scope, feature: "display_names") { client, scope in
            let value = try await client.ownName(scope.mailbox)
            guard valid(scope) else { return }; cached.ownName = value; cached.nameLoaded = true
        }
    }
    func saveName(_ scope: PeopleScope?, name: String, visibility: String) async {
        await perform(scope, feature: "display_names") { client, scope in
            let value = try await client.saveName(scope.mailbox, name: name, visibility: visibility)
            guard valid(scope) else { return }; cached.ownName = value; cached.nameLoaded = true; cached.notice = "Your name is saved."
        }
    }
    func removeName(_ scope: PeopleScope?) async {
        await perform(scope, feature: "display_names") { client, scope in
            try await client.removeName(scope.mailbox)
            guard valid(scope) else { return }; cached.ownName = nil; cached.nameLoaded = true
        }
    }
    func dismissPrompt(_ scope: PeopleScope?) {
        synchronize(); guard let scope, valid(scope) else { return }
        do { try storage.markPromptSeen(subject: scope.subject, mailbox: scope.mailbox); cached.promptSeen = true }
        catch { cached.error = "Could not save this preference on this device." }
    }
    func loadPhones(_ scope: PeopleScope?) async {
        guard await readReady(scope) else { return }
        await perform(scope, feature: "phone_numbers") { client, scope in
            let phones = try await client.phones(scope.mailbox)
            guard valid(scope) else { return }; cached.phones = phones
        }
    }
    func sendCode(_ scope: PeopleScope?, phone: String) async {
        guard state.challenge?.resendAfter ?? 0 <= Int(Date().timeIntervalSince1970) else { return }
        await perform(scope, feature: "phone_numbers") { client, scope in
            let challenge = try await client.sendCode(scope.mailbox, phone: phone)
            guard valid(scope) else { return }; cached.challenge = challenge; cached.challengePhone = phone
        }
    }
    func verify(_ scope: PeopleScope?, code: String, discoverable: Bool) async {
        let challenge = state.challenge
        await perform(scope, feature: "phone_numbers") { client, scope in
            guard let challenge else { return }
            let phone = try await client.verify(scope.mailbox, challenge: challenge.challengeId, code: code, discoverable: discoverable)
            guard valid(scope) else { return }; cached.phones.removeAll { $0.id == phone.id }; cached.phones.append(phone)
            cached.challenge = nil; cached.challengePhone = nil; cached.notice = "Your phone number is verified."
        }
    }
    func setDiscoverable(_ scope: PeopleScope?, phone: PPPhoneRecord, value: Bool) async {
        await perform(scope, feature: "phone_numbers") { client, scope in
            let updated = try await client.discoverable(scope.mailbox, id: phone.id, value: value)
            guard valid(scope) else { return }; cached.phones = cached.phones.map { $0.id == updated.id ? updated : $0 }
        }
    }
    func unlink(_ scope: PeopleScope?, phone: PPPhoneRecord) async {
        await perform(scope, feature: "phone_numbers") { client, scope in
            try await client.unlink(scope.mailbox, id: phone.id)
            guard valid(scope) else { return }; cached.phones.removeAll { $0.id == phone.id }
        }
    }
    func importContacts(_ scope: PeopleScope?, contacts: [PhoneContact]) {
        synchronize(); guard valid(scope), has("contact_discovery"), !cached.busy else { return }
        var seen = Set<String>()
        cached.contacts = Array(contacts.filter { seen.insert($0.hash).inserted }.prefix(5000))
        cached.checked = []; cached.matches = [:]; cached.remaining = nil; cached.invitation = nil; cached.error = nil; cached.notice = nil
    }
    func clearContacts(_ scope: PeopleScope?) {
        synchronize(); guard valid(scope) else { return }
        operation = UUID(); confirmedInvite = nil; cached.busy = false
        cached.contacts = []; cached.checked = []; cached.matches = [:]; cached.invitation = nil; cached.remaining = nil
    }
    func discover(_ scope: PeopleScope?) async {
        await perform(scope, feature: "contact_discovery") { client, scope in
            let operation = self.operation
            var pending = cached.contacts.map(\.hash).filter { !cached.checked.contains($0) }
            while !pending.isEmpty {
                let count = min(200, cached.remaining ?? 200, pending.count)
                guard count > 0, valid(scope), self.operation == operation else { break }
                let batch = Array(pending.prefix(count))
                let result = try await client.discover(scope.mailbox, hashes: batch)
                guard valid(scope), self.operation == operation else { return }
                cached.checked.formUnion(batch); cached.remaining = result.remainingToday
                for match in result.matches { cached.matches[match.hash] = match }
                pending.removeFirst(count)
            }
            guard valid(scope), self.operation == operation else { return }
            cached.notice = pending.isEmpty ? "Checked \(cached.checked.count) numbers." : "Checked \(cached.checked.count) numbers. The daily limit was reached; unchecked contacts cannot be invited."
        }
    }
    func reviewInvite(_ scope: PeopleScope?, contact: PhoneContact) {
        synchronize()
        guard let scope, valid(scope), has("sms_invites"), !cached.busy, cached.phones.contains(where: { $0.identity == scope.mailbox }),
              cached.checked.contains(contact.hash), cached.matches[contact.hash] == nil, cached.contacts.contains(contact) else { return }
        confirmedInvite = nil
        cached.invitation = .init(scope: scope, contact: contact)
    }
    func requestConversation(_ scope: PeopleScope?, peer: String) {
        synchronize(); guard valid(scope), PeopleInput.address(peer) else { return }
        cached.conversationRequest = peer
    }
    func takeConversationRequest() -> String? {
        let peer = state.conversationRequest; cached.conversationRequest = nil; return peer
    }
    func cancelInvite() { cached.invitation = nil }
    /// Consume the confirmation synchronously, before SwiftUI dismisses the dialog and clears its binding.
    func prepareInviteConfirmation(_ review: PeopleState.InviteReview) -> PeopleState.InviteReview? {
        guard state.invitation == review, valid(review.scope), !cached.busy else { return nil }
        cached.invitation = nil
        confirmedInvite = review
        return review
    }
    func confirmInvite(_ review: PeopleState.InviteReview) async {
        guard confirmedInvite == review else { return }
        confirmedInvite = nil // one confirmation, one attempt; never replay a failed SMS automatically
        await perform(review.scope, feature: "sms_invites") { client, scope in
            guard cached.checked.contains(review.contact.hash), cached.matches[review.contact.hash] == nil else { return }
            let result = try await client.invite(scope.mailbox, phone: review.contact.phone)
            guard valid(scope) else { return }
            cached.notice = result.status == "sent" ? "Invitation sent." : "No invitation was sent. This number may already have an invitation or be unavailable."
        }
    }
    func alias(_ scope: PeopleScope?, peer: String, name: String) {
        synchronize(); guard let scope, valid(scope), PeopleInput.address(peer) else { return }
        do {
            let name = name.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty ? nil : try PeopleInput.name(name, alias: true)
            try storage.setAlias(subject: scope.subject, mailbox: scope.mailbox, peer: peer, name: name)
            cached.aliases[peer] = name
        } catch { cached.error = error.localizedDescription }
    }
    func clearLocalAccount() {
        guard let owner else { return }
        do { try storage.clear(subject: owner.subject) } catch { cached.error = "Could not clear the local contact names." }
        cached = PeopleState(); self.owner = nil; operation = UUID(); confirmedInvite = nil
    }
    func refreshProfiles(_ scope: PeopleScope?, addresses: [String], now: Date = Date()) async {
        guard now.timeIntervalSince(profileRefresh) >= 60 else { return }
        await perform(scope, feature: "display_names") { client, scope in
            var profiles: [String: PPProfile] = [:]
            let addresses = Array(Set(addresses.filter(PeopleInput.address))).sorted().prefix(500)
            for offset in stride(from: 0, to: addresses.count, by: 100) {
                let batch = Array(addresses.dropFirst(offset).prefix(100))
                let result = try await client.profiles(scope.mailbox, addresses: batch)
                guard valid(scope) else { return }
                for (key, value) in result { profiles[key] = value }
            }
            guard valid(scope) else { return }; cached.profiles = profiles; profileRefresh = now
        }
    }
    func localName(_ peer: String) -> String? { state.aliases[peer] }
    func displayName(_ peer: String, legacyName: String? = nil) -> String? {
        let s = state
        if let name = s.aliases[peer] { return name }
        if let legacyName, !legacyName.isEmpty { return legacyName }
        guard has("display_names"), let p = s.profiles[peer] else { return nil }
        return p.displayName.map { "~" + $0 } ?? p.handle
    }
}
