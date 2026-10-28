//  Group info: members and their roles, invitations, the invite link, settings, the timer,
//  notifications, the safety number and this device's fingerprint, and leaving or deleting.
//
//  The web's info sheet (site-inbox/groups.js `renderInfo`) as a native form. Who may do what is the
//  postbox's decision; this only offers what the contract says the person's role allows, and says
//  the postbox's reason when it refuses anyway.

import SwiftUI

struct GroupInfoSheet: View {
    let groupId: String
    /// Phase 6: where a phone offers Go live (its group bar has no room for a fourth button).
    var goLive: (() -> Void)? = nil

    @Environment(GroupStore.self) private var groups
    @Environment(Account.self) private var account
    @Environment(\.dismiss) private var dismiss

    @State private var name = ""
    @State private var about = ""
    @State private var adding = ""
    @State private var link: String?
    @State private var confirm: Confirm?
    @State private var transferTo = ""
    @State private var expanded: Set<String> = []
    /// Blocking a member (SDS §15.2): their direct mail and calls, and their group posts to you.
    @State private var blocking: BlockTarget?

    private struct Confirm: Identifiable {
        let id = UUID()
        let title: String
        let detail: String
        let action: String
        let run: () async -> Void
    }

    private var group: GroupSummary? { groups.group(groupId) }

    var body: some View {
        NavigationStack {
            Group {
                if let group {
                    form(group)
                } else {
                    ContentUnavailableView("This group is gone", systemImage: "person.3")
                }
            }
            .sheetSurface()
            .navigationTitle("Group info")
            .inlineTitle()
            .toolbar { ToolbarItem(placement: .confirmationAction) { Button("Done") { dismiss() } } }
        }
        .task {
            await groups.loadMembers(groupId)
            if let group { name = group.name; about = group.description ?? "" }
        }
        .sheet(item: $blocking) { target in BlockSheet(target: target) }
        .confirmationDialog(confirm?.title ?? "", isPresented: Binding(get: { confirm != nil }, set: { if !$0 { confirm = nil } }),
                            titleVisibility: .visible, presenting: confirm) { pending in
            Button(pending.action, role: .destructive) { Task { await pending.run() } }
            Button("Cancel", role: .cancel) {}
        } message: { pending in Text(pending.detail) }
    }

    @ViewBuilder
    private func form(_ group: GroupSummary) -> some View {
        let members = groups.members[group.id] ?? []
        List {
            Section {
                HStack(spacing: 12) {
                    GroupFace(id: group.id, name: group.name, size: 48)
                    VStack(alignment: .leading, spacing: 3) {
                        Text(group.name).font(.geist(17, .semibold)).foregroundStyle(Theme.ink)
                        if let text = group.description, !text.isEmpty {
                            Text(text).font(.geist(13)).foregroundStyle(Theme.secondary)
                        }
                        Text("\(group.members) members · \(group.isPublic ? "public" : "invite only")")
                            .font(.geist(12)).foregroundStyle(Theme.muted)
                    }
                }
            }

            if group.isAdmin {
                Section("Name and description") {
                    TextField("Name", text: $name).accessibilityIdentifier("groupNameField")
                    TextField("Description", text: $about, axis: .vertical).lineLimit(1...4)
                    Button("Save name") {
                        Task { _ = await groups.update(group, ["name": name.trimmingCharacters(in: .whitespaces),
                                                               "description": about.isEmpty ? NSNull() : about as Any]) }
                    }
                    .disabled(name.trimmingCharacters(in: .whitespaces).isEmpty)
                }
            }

            if let goLive, account.features.streaming, group.canGoLive, groups.streams[group.id] == nil {
                Section {
                    Button { goLive() } label: { Label("Go live", systemImage: "dot.radiowaves.left.and.right") }
                        .accessibilityIdentifier("groupInfoGoLive")
                } footer: {
                    Text("Stream your camera to everyone in the group, end-to-end encrypted.")
                }
            }

            membersSection(group, members: members)

            if group.isAdmin, group.isPublic { linkSection(group) }
            if group.isAdmin { settingsSection(group) }
            if account.features.disappearing { timerSection(group) }
            notificationsSection(group)
            encryptionSection(group)
            membershipSection(group, members: members)
        }
    }

    // ---- members ------------------------------------------------------------------------------------

    private func membersSection(_ group: GroupSummary, members: [GroupMember]) -> some View {
        let visible = members.filter {
            ["active", "keying"].contains($0.state ?? "") || (group.isAdmin && ["invited", "banned"].contains($0.state ?? ""))
        }
        return Section {
            ForEach(visible) { member in
                memberRow(group, member)
            }
            if group.isAdmin || (group.membersCanInvite == true && group.isActive) {
                HStack {
                    TextField("/alp/main", text: $adding)
                        .noAutocapitalize()
                        .autocorrectionDisabled()
                        .font(.geistMono(14))
                        .accessibilityLabel("Add people by handle or address")
                    Button("Add people") {
                        let raw = adding
                        adding = ""
                        Task { await groups.invite(group, raw: raw) }
                    }
                    .disabled(adding.trimmingCharacters(in: .whitespaces).isEmpty)
                }
            }
        } header: {
            Text("Members (\(members.filter { $0.state == "active" }.count))")
        }
    }

    private static let roleLabel = ["owner": "Owner", "admin": "Admin", "member": "Member", "restricted": "Restricted"]

    private func memberRow(_ group: GroupSummary, _ member: GroupMember) -> some View {
        let me = member.address == account.me?.address
        let state = member.state ?? "active"
        let label = state == "active" ? (Self.roleLabel[member.role ?? "member"] ?? "Member")
            : state == "invited" ? "Invited" : state == "banned" ? "Banned" : "Joining"
        return VStack(alignment: .leading, spacing: 6) {
            HStack(spacing: 10) {
                Avatar(peer: member.key, size: 30)
                Text(me ? PeerFace.displayName(member.key) + " (you)" : groups.name(member.address))
                    .font(.geist(14, .medium))
                    .foregroundStyle(Theme.ink)
                    .lineLimit(1)
                Spacer(minLength: 4)
                Text(label.uppercased())
                    .font(.geist(10.5, .semibold))
                    .foregroundStyle(Theme.secondary)
                    .padding(.horizontal, 6)
                    .padding(.vertical, 2)
                    .background(Theme.chip, in: RoundedRectangle(cornerRadius: 5))
                Menu {
                    memberActions(group, member, me: me)
                } label: {
                    Image(systemName: "ellipsis.circle").foregroundStyle(Theme.secondary)
                }
                .accessibilityLabel("Actions for \(groups.name(member.address))")
                .accessibilityIdentifier("memberActions:" + member.address)
            }
            if state == "active" {
                DisclosureGroup(isExpanded: Binding(get: { expanded.contains(member.address) }, set: { open in
                    if open {
                        expanded.insert(member.address)
                        Task { await groups.loadDevices(member.address) }
                    } else { expanded.remove(member.address) }
                })) {
                    let list = groups.devices[member.address] ?? []
                    if list.isEmpty {
                        Text("No devices visible.").font(.geist(12)).foregroundStyle(Theme.muted)
                    }
                    ForEach(list) { device in
                        VStack(alignment: .leading, spacing: 1) {
                            Text(device.deviceId + (device.deviceId == groups.identity?.deviceId ? " (this device)" : ""))
                                .font(.geistMono(11.5)).foregroundStyle(Theme.secondary)
                            Text(device.fingerprintDigits ?? device.fingerprint ?? "")
                                .font(.geistMono(12)).foregroundStyle(Theme.ink).textSelection(.enabled)
                        }
                    }
                } label: {
                    Text("Devices").font(.geist(12.5)).foregroundStyle(Theme.secondary)
                }
            }
        }
        .padding(.vertical, 2)
    }

    @ViewBuilder
    private func memberActions(_ group: GroupSummary, _ member: GroupMember, me: Bool) -> some View {
        let role = member.role ?? "member"
        let state = member.state ?? "active"
        if state == "invited", group.isAdmin {
            Button("Revoke invitation", role: .destructive) { Task { await groups.change(group, member: member, state: "removed") } }
        }
        if state == "banned", group.isAdmin {
            Button("Lift ban") { Task { await groups.liftBan(group, address: member.address) } }
        }
        if state == "active", !me, role != "owner" {
            if group.isAdmin, role != "admin" {
                Button("Make admin") { Task { await groups.change(group, member: member, role: "admin") } }
            }
            if group.isOwner, role == "admin" {
                Button("Remove admin") { Task { await groups.change(group, member: member, role: "member") } }
            }
            if group.isAdmin, role == "member" {
                Button("Restrict") { Task { await groups.change(group, member: member, role: "restricted") } }
            }
            if group.isAdmin, role == "restricted" {
                Button("Unrestrict") { Task { await groups.change(group, member: member, role: "member") } }
            }
            if group.isAdmin, role != "admin" || group.isOwner {
                Button("Remove from group", role: .destructive) {
                    confirm = Confirm(title: "Remove from the group?",
                                      detail: "\(groups.name(member.address)) loses access at once. Messages they already saw stay on their devices.",
                                      action: "Remove") { await groups.change(group, member: member, state: "removed") }
                }
                Button("Remove and ban", role: .destructive) {
                    confirm = Confirm(title: "Remove and ban?",
                                      detail: "\(groups.name(member.address)) loses access at once and cannot rejoin.",
                                      action: "Ban") { await groups.change(group, member: member, state: "banned") }
                }
            }
        }
        if me, role == "admin" {
            Button("Step down as admin") { Task { await groups.change(group, member: member, role: "member") } }
        }
        if !me {
            Button { Clipboard.copy(member.key) } label: { Label("Copy address", systemImage: "doc.on.doc") }
            Button(role: .destructive) {
                blocking = BlockTarget(peer: member.key, name: groups.name(member.address), origin: .group)
            } label: { Label("Block…", systemImage: "hand.raised") }
        }
    }

    // ---- link, settings, timer, notifications -------------------------------------------------------------

    private func linkSection(_ group: GroupSummary) -> some View {
        Section {
            Text(group.hasLink == true
                 ? "Anyone with the current link can join. Resetting makes the old link stop working."
                 : "Create a link anyone can use to join this public group.")
                .font(.geist(12.5)).foregroundStyle(Theme.secondary)
            if let link {
                Text(link).font(.geistMono(12)).textSelection(.enabled).accessibilityIdentifier("groupLink")
                HStack {
                    Button("Copy link") { Clipboard.copy(link); groups.say("Link copied.") }
                    ShareLink(item: link) { Text("Share") }
                }
                Text("Shown once. Anyone with this link can join while it is on.")
                    .font(.geist(12)).foregroundStyle(Theme.muted)
            }
            Button(group.hasLink == true ? "Reset link" : "Create link") {
                Task { link = await groups.link(group, enabled: true) }
            }
            if group.hasLink == true {
                Button("Turn off link", role: .destructive) {
                    Task { _ = await groups.link(group, enabled: false); link = nil }
                }
            }
        } header: { Text("Invite link") }
    }

    private func settingsSection(_ group: GroupSummary) -> some View {
        Section("Settings") {
            Picker("Who can join", selection: Binding(get: { group.joinPolicy ?? "private" }, set: { value in
                Task { _ = await groups.update(group, ["join_policy": value]) }
            })) {
                Text("Private — invite only").tag("private")
                Text("Public — anyone with the link").tag("public")
            }
            Picker("Who can post", selection: Binding(get: { group.postPolicy ?? "everyone" }, set: { value in
                Task { _ = await groups.update(group, ["post_policy": value]) }
            })) {
                Text("Everyone").tag("everyone")
                Text("Only admins").tag("admins")
            }
            .accessibilityIdentifier("groupPostPolicy")
            if account.features.streaming {
                Picker("Who can go live", selection: Binding(get: { group.streamPolicy ?? "admins" }, set: { value in
                    Task { _ = await groups.update(group, ["stream_policy": value]) }
                })) {
                    Text("Only admins").tag("admins")
                    Text("Everyone").tag("everyone")
                }
                .accessibilityIdentifier("groupStreamPolicy")
            }
            Toggle("Members can add people", isOn: Binding(get: { group.membersCanInvite == true }, set: { on in
                Task { _ = await groups.update(group, ["members_can_invite": on]) }
            }))
            if account.features.disappearing {
                Toggle("Members can change the timer", isOn: Binding(get: { group.membersCanChangeTimer == true }, set: { on in
                    Task { _ = await groups.update(group, ["members_can_change_timer": on]) }
                }))
            }
        }
    }

    private func timerSection(_ group: GroupSummary) -> some View {
        let may = group.isAdmin || (group.membersCanChangeTimer == true && group.isActive)
        return Section {
            TimerPicker(seconds: group.ephemeralSecs) { value in
                Task { _ = await groups.update(group, ["ephemeral_secs": value.map { $0 as Any } ?? NSNull()]) }
            }
            .disabled(!may)
        } header: {
            Text("Disappearing messages")
        } footer: {
            Text("The clock starts when a message is sent. " + GroupTimers.limits)
        }
    }

    private func notificationsSection(_ group: GroupSummary) -> some View {
        let notify = group.my?.notify ?? "all"
        return Section {
            Picker("Notify me about", selection: Binding(get: { notify }, set: { value in
                Task { await groups.setNotifications(group, notify: value, muted: group.muted) }
            })) {
                Text("All messages").tag("all")
                Text("Only mentions").tag("mentions")
                Text("Nothing").tag("none")
            }
            Toggle("Mute", isOn: Binding(get: { group.muted }, set: { on in
                Task { await groups.setNotifications(group, notify: notify, muted: on) }
            }))
        } header: {
            Text("Notifications")
        } footer: {
            Text("Notifications say only “New message”: the postbox cannot read group messages.")
        }
    }

    // ---- encryption -------------------------------------------------------------------------------------------

    private func encryptionSection(_ group: GroupSummary) -> some View {
        Section {
            if let number = groups.safetyNumbers[group.id] {
                Text(GroupText.spaced(number))
                    .font(.geistMono(15, .medium))
                    .foregroundStyle(Theme.ink)
                    .textSelection(.enabled)
                    .accessibilityIdentifier("groupSafetyNumber")
            } else {
                Text("Not available on this device yet.").foregroundStyle(Theme.muted)
            }
            if let identity = groups.identity {
                LabeledContent("This device", value: identity.fingerprint)
                    .font(.geist(12.5))
                    .textSelection(.enabled)
                    .accessibilityIdentifier("deviceFingerprint")
            }
            let devices = groups.memberDevices[group.id] ?? []
            if !devices.isEmpty {
                Text("\(devices.count) \(devices.count == 1 ? "device" : "devices") in this group's encryption")
                    .font(.geist(12.5)).foregroundStyle(Theme.secondary)
            }
        } header: {
            Text("Encryption")
        } footer: {
            Text("Compare this safety number with another member in person or on a call. If it matches, you both see the same devices in this group. Hosted mailboxes' keys are held by the postbox, so it could add a device to one. Every new device is shown here and in the conversation; check fingerprints with people you trust.")
        }
    }

    // ---- membership ---------------------------------------------------------------------------------------------

    @ViewBuilder
    private func membershipSection(_ group: GroupSummary, members: [GroupMember]) -> some View {
        Section("Membership") {
            if group.isOwner {
                let admins = members.filter { $0.role == "admin" && $0.state == "active" }
                if admins.isEmpty {
                    Text("To leave, first make another member an admin and transfer ownership to them.")
                        .font(.geist(12.5)).foregroundStyle(Theme.secondary)
                } else {
                    Picker("Transfer to", selection: $transferTo) {
                        Text("Choose an admin").tag("")
                        ForEach(admins) { admin in Text(groups.name(admin.address)).tag(admin.address) }
                    }
                    Button("Transfer ownership") {
                        let to = transferTo
                        confirm = Confirm(title: "Transfer ownership?", detail: "\(groups.name(to)) becomes the owner. You stay an admin.",
                                          action: "Transfer") { await groups.transfer(group, to: to) }
                    }
                    .disabled(transferTo.isEmpty)
                }
            }
            Button("Leave group", role: .destructive) {
                confirm = Confirm(title: "Leave this group?", detail: "You stop receiving its messages. To come back you need a new invitation.",
                                  action: "Leave") { if await groups.leave(group) { dismiss() } }
            }
            .accessibilityIdentifier("leaveGroup")
            if group.isOwner {
                Button("Delete group", role: .destructive) {
                    confirm = Confirm(title: "Delete this group for everyone?",
                                      detail: "Members see that it was deleted and its messages are removed from the postbox.",
                                      action: "Delete group") { if await groups.delete(group) { dismiss() } }
                }
                .accessibilityIdentifier("deleteGroup")
            }
        }
    }
}

/// The seven allowed timers as a picker. Shared by groups and direct conversations.
struct TimerPicker: View {
    let seconds: Int?
    let change: (Int?) -> Void

    var body: some View {
        Picker("New messages disappear after", selection: Binding(get: { seconds ?? 0 }, set: { value in
            change(value == 0 ? nil : value)
        })) {
            ForEach(GroupTimers.all, id: \.self) { value in
                Text(GroupTimers.label(value)).tag(value ?? 0)
            }
        }
        .accessibilityIdentifier("timerPicker")
    }
}
