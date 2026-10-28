#if DEBUG
//  The store-screenshot world: `-fixtures -marketing`.
//
//  Everything here is invented and mirrors `marketing/store/seed/demo-world.json`: /maya and her
//  friends, her agents, two groups, a week of meetings and the Fernhill Studio organization. No real
//  account, person or message. Debug only; the release build never compiles this file's contents.

import Foundation

extension Fixtures {
    /// `-marketing` (with `-fixtures`) swaps every fixture world for the store-screenshot one.
    static var marketing: Bool { enabled && CommandLine.arguments.contains("-marketing") }
}

@MainActor
enum MarketingFixtures {
    static let me = "/k/maya7demo0fixture0store0shots"
    static let leo = "/k/leo00demo0fixture0store0shot", ana = "/k/ana00demo0fixture0store0shot"
    static let kai = "/k/kai00demo0fixture0store0shot", noor = "/k/noor0demo0fixture0store0shot"
    static let priya = "/k/priya0demo0fixture0store0sho", sam = "/k/sam00demo0fixture0store0shot"
    static let tomas = "/k/tomas0demo0fixture0store0sho", research = "/k/research0agent0fixture0store"

    static let handles: [String: String] = [
        leo: "/leo", ana: "/ana", kai: "/kai", noor: "/noor", priya: "/priya", sam: "/sam",
        tomas: "/tomas", research: "/maya/research-agent",
    ]

    // ---- inbox ---------------------------------------------------------------------------------

    /// Today at 09:41, the time the status bar is pinned to, so message times agree with it.
    nonisolated static var clock: Int {
        let today = Foundation.Calendar.current.date(bySettingHour: 9, minute: 41, second: 0, of: Date()) ?? Date()
        return Int(today.timeIntervalSince1970)
    }

    static func apply(session: Session, account: Account, inbox: Inbox) {
        let now = clock
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        let mine = Mailbox(address: me, handle: "/maya", label: "Personal")
        let fleet = [mine]
        let rows = messages(now)
        let data = try! JSONSerialization.data(withJSONObject: rows)
        let list = (try? decoder.decode([Message].self, from: data)) ?? []
        let contacts = (try? decoder.decode(ContactsResponse.self, from: Data(contactsJSON.utf8)))
        let threads = (try? decoder.decode(ThreadsResponse.self, from: Data(threadsJSON(now).utf8)))?.threads ?? []
        session.installFixtureSession()
        account.installFixtures(mailboxes: fleet, me: mine)
        inbox.installFixtures(messages: list.sorted { $0.at < $1.at }, contacts: contacts?.contacts ?? [],
                              vocabulary: contacts?.vocabulary, threads: threads)
    }

    private static func incoming(_ id: String, _ peer: String, _ body: String, _ minutes: Int, now: Int,
                                 read: Bool = true, thread: String? = nil, autonomy: String = "auto") -> [String: Any] {
        var row: [String: Any] = ["message_id": id, "from": peer, "body": body, "direction": "in",
            "peer": peer, "peer_handle": peer, "sender_handle": peer, "sender_known": true, "sender_tier": "handle",
            "sender_standing": "good", "matched_contact": peer, "autonomy": autonomy,
            "received_at": now + minutes * 60, "read": read]
        if let thread { row["thread_id"] = thread }
        return row
    }

    private static func outgoing(_ id: String, _ peer: String, _ body: String, _ minutes: Int, now: Int,
                                 receipt: String, thread: String? = nil, working: Bool = false) -> [String: Any] {
        let at = now + minutes * 60
        var row: [String: Any] = ["message_id": id, "from": me, "to": peer, "body": body, "direction": "out",
            "peer": peer, "peer_handle": peer, "sent_at": at, "received_at": at, "read": true,
            "receipt": ["state": receipt, "delivered_at": at + 2, "seen_at": receipt == "seen" ? at + 30 : NSNull()] as [String: Any]]
        if let thread { row["thread_id"] = thread }
        if working { row["agent_status"] = ["state": "working", "at": at + 20] }
        return row
    }

    private static func messages(_ now: Int) -> [[String: Any]] {
        let agent = "/maya/research-agent", planner = "/maya/trip-planner"
        let table = """
        ## Venue shortlist

        | Venue | Seats | Per day | Transit |
        | --- | ---: | ---: | --- |
        | **Harbor Hall** | 24 | €1,450 | 5 min walk |
        | Mill House | 30 | €1,900 | 20 min bus |
        | The Glasshouse | 18 | €1,200 | 10 min walk |

        **Harbor Hall** is the best fit: room for everyone, quiet breakout spaces and lunch included.
        """
        var leoReply = incoming("leo-3", "/leo", "Dinner Friday? I found a great ramen place 🍜", -6, now: now, read: false, thread: "t-leo")
        leoReply["reply_to"] = ["message_id": "leo-2", "direction": "out", "excerpt": "Yes! 7:30 works for me."]
        return [
            outgoing("ra-1", agent, "Can you compare the three venues for the team offsite in May? 20 people, two days.", -26, now: now, receipt: "seen", thread: "t-research"),
            incoming("ra-2", agent, table, -19, now: now, thread: "t-research"),
            outgoing("ra-3", agent, "Great — hold Harbor Hall for May 14–15 and send Leo the details.", -3, now: now, receipt: "seen", thread: "t-research", working: true),

            incoming("leo-1", "/leo", "Hey! Are we still on for Friday?", -40, now: now, thread: "t-leo"),
            outgoing("leo-2", "/leo", "Yes! 7:30 works for me.", -38, now: now, receipt: "seen", thread: "t-leo"),
            incoming("leo-2b", "/leo", "Amazing. Bring an appetite 😄", -7, now: now, read: false, thread: "t-leo"),
            leoReply,

            incoming("ana-1", "/ana", "Photos from Saturday are in the shared album", -35, now: now, thread: "t-ana"),
            incoming("tp-1", planner, "Your train to Porto is booked: Fri 17:05, seat 42.", -95, now: now, thread: "t-planner"),
            outgoing("pr-1", "/priya", "Sounds perfect, see you at 10!", -180, now: now, receipt: "seen", thread: "t-priya"),
            incoming("sam-1", "/sam", "Thanks for the intro 🙏", -1440, now: now, thread: "t-sam"),
        ]
    }

    private static let contactsJSON = """
    {"contacts":[
     {"peer":"/maya/*","alias":"My agents","admission":"allow","autonomy":"auto","allowed_verbs":["report_status","answer_question","read_file"]},
     {"peer":"/maya/research-agent","alias":"Research agent","admission":"allow","autonomy":"auto","allowed_verbs":["report_status","answer_question","read_file"]},
     {"peer":"/maya/trip-planner","alias":"Trip planner","admission":"allow","autonomy":"auto","allowed_verbs":["report_status","answer_question"]},
     {"peer":"/leo","alias":"Leo","admission":"allow","autonomy":"review","allowed_verbs":[]},
     {"peer":"/ana","alias":"Ana","admission":"allow","autonomy":"review","allowed_verbs":[]},
     {"peer":"/priya","alias":"Priya","admission":"allow","autonomy":"review","allowed_verbs":[]},
     {"peer":"/sam","alias":"Sam","admission":"allow","autonomy":"review","allowed_verbs":[]}],
     "policy":{"accept_all":true,"auto_accept_known":true},
     "vocabulary":{"grantable":["report_status","answer_question","read_file","run_tests"],
     "never_auto":["git_push","deploy","read_credentials","spend","delete_files","run_shell"]}}
    """

    private static func threadsJSON(_ now: Int) -> String {
        func row(_ id: String, _ peer: String, _ minutes: Int, title: String? = nil, main: Bool = true) -> String {
            let name = title.map { "\"\($0)\"" } ?? "null"
            return "{\"thread_id\":\"\(id)\",\"peer\":\"\(peer)\",\"title\":\(name),\"is_default\":\(main),\"created_at\":\(now - 90000),\"last_at\":\(now + minutes * 60),\"archived\":false}"
        }
        let rows = [row("t-research", "/maya/research-agent", -3),
                    row("t-leo", "/leo", -6), row("t-ana", "/ana", -35), row("t-planner", "/maya/trip-planner", -95),
                    row("t-priya", "/priya", -180), row("t-sam", "/sam", -1440)]
        return "{\"threads\":[" + rows.joined(separator: ",") + "]}"
    }

    // ---- groups --------------------------------------------------------------------------------

    struct GroupSeed {
        var groups: [GroupSummary]
        var histories: [String: [GroupItem]]
        var members: [String: [GroupMember]]
        var handles: [String: String]
        var inGroup: Set<String>
        var safetyNumbers: [String: String]
    }

    static let hikers = "grp_marketinghikers00000000000", design = "grp_marketingdesign00000000000"

    /// The groups GroupStore installs instead of its own fixtures under `-marketing`.
    static func groupSeed(me mine: String, myHandle: String?, now _: Int) -> GroupSeed? {
        guard Fixtures.marketing else { return nil }
        let now = clock
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        let json = """
        {"groups":[
         {"id":"\(hikers)","name":"Weekend hikers","description":"Trails, coffee and summit lunches",
          "join_policy":"private","post_policy":"everyone","visibility":"unlisted","members_can_invite":true,
          "members_can_change_timer":true,"ephemeral_secs":604800,"revision":4,"epoch":3,"activation_state":"active",
          "member_count":5,"created_by":"\(kai)","created_at":\(now - 900000),"last_at":\(now - 480),"last_seq":14,
          "my":{"role":"member","state":"active","read_cursor":14,"unread":3,"notify":"all","revision":2}},
         {"id":"\(design)","name":"Design team","description":"Reviews, critiques and Friday demos",
          "join_policy":"private","post_policy":"everyone","visibility":"unlisted","revision":3,"epoch":2,"activation_state":"active",
          "member_count":6,"created_by":"\(priya)","created_at":\(now - 1900000),"last_at":\(now - 840),"last_seq":40,
          "my":{"role":"admin","state":"active","read_cursor":40,"unread":0,"notify":"all","revision":2}}
        ]}
        """
        let groups = (try? decoder.decode(GroupsResponse.self, from: Data(json.utf8)))?.groups ?? []
        let week = 604800
        func say(_ seq: Int, _ who: String, _ text: String, _ minutes: Int) -> GroupItem {
            GroupItem(kind: .message, seq: seq, createdAt: now + minutes * 60, expiresAt: now + minutes * 60 + week, sender: who, text: text)
        }
        let hikeHistory = [
            GroupItem(kind: .notice, seq: 8, createdAt: now - 4000, notice: "timer_changed", actor: kai, metadata: NoticeMeta(ephemeralSecs: week)),
            say(9, kai, "Trailhead at 8 on Saturday? Forecast says clear skies ☀️", -52),
            say(10, ana, "In! I'll bring the good coffee.", -47),
            say(11, leo, "Can we do the ridge loop this time? 14 km, great views.", -40),
            say(12, mine, "Ridge loop it is. I'll drive — room for three.", -33),
            say(13, noor, "Seat for me please 🙋", -21),
            say(14, kai, "Packed lunch at the summit, then the lake on the way down?", -8),
        ]
        func post(_ seq: Int, _ who: String, _ text: String, _ minutes: Int) -> GroupItem {
            GroupItem(kind: .message, seq: seq, createdAt: now + minutes * 60, sender: who, text: text)
        }
        let designHistory = [
            post(35, sam, "Morning! Coffee's on in the studio ☕️", -95),
            post(36, tomas, "Icon set v2 is in the shared folder.", -70),
            post(37, research, "I pulled the onboarding drop-off numbers: step 3 loses 18% of new people.", -52),
            post(38, mine, "Thanks! That's the screen we're redesigning.", -45),
            post(39, leo, "Can we see it before the Friday demo?", -30),
            post(40, priya, "New onboarding screens are up for review 🎨", -14),
        ]
        var handles = Self.handles
        handles[mine] = myHandle ?? "/maya"
        func member(_ address: String, _ role: String) -> GroupMember {
            GroupMember(address: address, handle: handles[address], role: role, state: "active", revision: 1)
        }
        return GroupSeed(
            groups: groups,
            histories: [hikers: hikeHistory, design: designHistory],
            members: [hikers: [member(kai, "owner"), member(mine, "member"), member(leo, "member"), member(ana, "member"), member(noor, "member")],
                      design: [member(priya, "owner"), member(mine, "admin"), member(sam, "member"), member(tomas, "member"),
                               member(leo, "member"), member(research, "member")]],
            handles: handles,
            inGroup: [hikers, design],
            safetyNumbers: [hikers: "418207 559130 662841 097356 314772 805193 220648"]
        )
    }

    // ---- calendar ------------------------------------------------------------------------------

    /// Thursday 29 October 2026, 09:41 in Istanbul — the morning of the launch sync.
    static let calendarNow = ISO8601DateFormatter().date(from: "2026-10-29T06:41:00Z")!

    nonisolated static func calendarEvents(mailbox: String) -> [String: [String: Any]] {
        func person(_ handle: String, _ name: String, _ rsvp: String = "accepted") -> [String: Any] {
            ["attendee_id": "att_" + name.lowercased(), "kind": "handle", "handle": handle, "name": name, "invited": true, "rsvp": rsvp]
        }
        func event(_ id: String, _ title: String, _ day: String, _ start: String, _ end: String, calendar: String = "cal_work",
                   location: String? = nil, people: [[String: Any]] = [], meeting: Bool = false, invitedBy: String? = nil) -> [String: Any] {
            var row: [String: Any] = ["id": id, "title": title, "description": "", "revision": 1, "sequence": 1,
                "calendar_id": calendar, "all_day": false, "start": "\(day)T\(start):00", "end": "\(day)T\(end):00",
                "tzid": "Europe/Istanbul", "status": "confirmed", "can_edit": invitedBy == nil,
                "organizer": ["handle": invitedBy ?? "/maya"],
                "my": invitedBy == nil ? ["role": "organizer"] : ["role": "attendee", "attendee_id": "att_self", "rsvp": "needs_action"],
                "attendees": people, "reminders": [["minutes_before": 10, "channel": "push"]],
                "_invited": invitedBy != nil, "_filtered": false]
            if let location { row["location"] = location }
            if meeting { row["meeting"] = ["enabled": true, "code": "LAUNCHSYNC", "transport": "sfu"] }
            return row
        }
        let today = "2026-10-29", friday = "2026-10-30", saturday = "2026-10-31"
        return [
            "evt_launch": event("evt_launch", "Launch sync", today, "10:00", "10:45",
                people: [person("/leo", "Leo"), person("/ana", "Ana"), person("/priya", "Priya"), person("/maya/research-agent", "Research agent")], meeting: true),
            "evt_lunch": event("evt_lunch", "Lunch with Ana", today, "12:30", "13:30", calendar: "cal_personal", location: "Harbor Café", people: [person("/ana", "Ana")]),
            "evt_design": event("evt_design", "Design review", today, "14:00", "15:00", people: [person("/priya", "Priya"), person("/sam", "Sam")], meeting: true),
            "evt_one": event("evt_one", "1:1 with Leo", friday, "09:30", "10:00", people: [person("/leo", "Leo")], meeting: true),
            "evt_ramen": event("evt_ramen", "Ramen with Leo 🍜", friday, "19:30", "21:00", calendar: "cal_personal", location: "Kōji Ramen", people: [person("/leo", "Leo")]),
            "evt_hike": event("evt_hike", "Weekend hike", saturday, "08:00", "14:00", calendar: "cal_personal", location: "Ridge loop trailhead",
                people: [person("/kai", "Kai"), person("/ana", "Ana"), person("/leo", "Leo"), person("/noor", "Noor")]),
            "evt_offsite": event("evt_offsite", "Offsite planning", "2026-11-02", "11:00", "11:30",
                people: [person("/priya", "Priya")], meeting: true, invitedBy: "/priya"),
        ]
    }

    // ---- organization rooms --------------------------------------------------------------------

    static let organizationName = "Fernhill Studio", organizationNamespace = "fernhill"
    static let rooms: [String: [String: Any]] = [
        "grp_room_launch": ["name": "Launch room", "description": "Product launch: people and two agents", "join_policy": "public", "my_state": "active"],
        "grp_room_reviews": ["name": "Design reviews", "description": "Weekly critiques and handoffs", "join_policy": "public", "my_state": NSNull()],
        "grp_room_support": ["name": "Support desk", "description": "Customer questions, with /fernhill/support-agent", "join_policy": "public", "my_state": NSNull()],
        "grp_room_leads": ["name": "Studio leads", "description": "Planning and hiring", "join_policy": "private", "my_state": "invited"],
    ]
}
#endif
