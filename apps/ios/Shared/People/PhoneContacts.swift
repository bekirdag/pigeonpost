import Foundation
import Contacts
import PhoneNumberKit

/// Contacts and phone metadata are read off the main actor and never persisted by Pigeonpost.
actor PhoneContacts {
    static let shared = PhoneContacts()
    private lazy var utility = PhoneNumberUtility()
    struct Import: Sendable { let contacts: [PhoneContact]; let limited: Bool; let bounded: Bool }
    static var allowed: Bool {
        let value = CNContactStore.authorizationStatus(for: .contacts)
        if value == .authorized { return true }
        #if os(iOS)
        if #available(iOS 18, *), value == .limited { return true }
        #endif
        return false
    }
    func normalize(_ text: String, region: String) throws -> String {
        guard PeopleInput.matches(region, "^[A-Z]{2}$") else { throw PeopleInput.invalid("Use a two-letter country code, such as TR, GB or US.") }
        let parsed = try utility.parse(text, withRegion: region)
        guard parsed.numberExtension == nil || parsed.numberExtension?.isEmpty == true else { throw PeopleInput.invalid("Use a number without an extension.") }
        let phone = utility.format(parsed, toType: .e164)
        guard PeopleInput.phone(phone) else { throw PeopleInput.invalid("This phone number could not be confirmed.") }; return phone
    }
    func read(region: String) async throws -> Import {
        let store = CNContactStore()
        if CNContactStore.authorizationStatus(for: .contacts) == .notDetermined { _ = try await store.requestAccess(for: .contacts) }
        guard Self.allowed else { throw PeopleInput.invalid("Contacts access is off. You can change it in system Settings.") }
        guard PeopleInput.matches(region, "^[A-Z]{2}$") else { throw PeopleInput.invalid("Choose a two-letter country code.") }
        let request = CNContactFetchRequest(keysToFetch: [CNContactIdentifierKey, CNContactGivenNameKey, CNContactFamilyNameKey, CNContactPhoneNumbersKey] as [CNKeyDescriptor])
        var contacts: [PhoneContact] = []; var seen = Set<String>(); var rows = 0; var bounded = false
        try store.enumerateContacts(with: request) { contact, stop in
            rows += 1
            if rows > 5000 { bounded = true; stop.pointee = true; return }
            let name = [contact.givenName, contact.familyName].filter { !$0.isEmpty }.joined(separator: " ")
            for entry in contact.phoneNumbers.prefix(30) {
                guard let phone = try? normalize(entry.value.stringValue, region: region), seen.insert(phone).inserted,
                      let value = try? PhoneContact(phone: phone, name: name) else { continue }
                contacts.append(value)
                if contacts.count == 5000 { bounded = true; stop.pointee = true; break }
            }
        }
        var limited = false
        #if os(iOS)
        if #available(iOS 18, *) { limited = CNContactStore.authorizationStatus(for: .contacts) == .limited }
        #endif
        return Import(contacts: contacts, limited: limited, bounded: bounded)
    }
}
