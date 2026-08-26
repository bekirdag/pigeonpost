//  The thread model, against the postbox's real response shapes.
//
//  The fixtures are the web app's (site-inbox/test/app.test.mjs), which are in turn copied from
//  `do_inbox` / `do_list_identities` / `whoami` / `do_list_contacts`. Both clients assemble the same
//  conversations from the same bytes, and that is the property worth holding on to: a person reading
//  a thread in the browser and on their phone must see the same thread.
//
//  Deliberately not an XCTest target. It tests pure functions over their inputs — no view, no
//  network, no simulator — so it runs anywhere Swift does:
//
//      cd apps/ios && ./Tests/run.sh

import Foundation

let now = Int(Date().timeIntervalSince1970)

var failures = 0
func check(_ condition: Bool, _ what: String) {
    if condition {
        print("  ok   \(what)")
    } else {
        failures += 1
        print("  FAIL \(what)")
    }
}

func equal<T: Equatable>(_ actual: T, _ expected: T, _ what: String) {
    if actual == expected {
        print("  ok   \(what)")
    } else {
        failures += 1
        print("  FAIL \(what) — got \(actual), wanted \(expected)")
    }
}

// A namespace owner's fleet: the acting mailbox, two named sub-agents, one never named.
let me = Mailbox(address: "/k/cz6900v2h90vnwefj7g7ezvbh4", handle: "/bekir/su_iam", label: "su_iam")
let docdex = Mailbox(address: "/k/zz1111v2h90vnwefj7g7ezvbh9", handle: "/bekir/docdex", label: "docdex box")
let scratch = Mailbox(address: "/k/qq2222v2h90vnwefj7g7ezvbh7", handle: nil, label: "scratch")

let inboxJSON = """
{"messages":[
 {"message_id":"m1","from":"/k/aaaa1111bbbb2222cccc3333dd","body":"the build is green",
  "sender_standing":"unproven","sender_tier":"handle","untrusted":true,"sender_known":true,
  "alias":null,"matched_contact":"/bekir/*","sender_handle":"/bekir/agent1","peer":"/bekir/agent1",
  "peer_handle":"/bekir/agent1","thread_id":"t-agent1","direction":"in","autonomy":"review",
  "verb":null,"held_because":"not_a_request","received_at":\(now - 7200),"read":true},

 {"message_id":"m2","from":"/k/aaaa1111bbbb2222cccc3333dd",
  "body":"{\\"v\\":1,\\"verb\\":\\"run_tests\\",\\"args\\":{\\"suite\\":\\"unit\\"},\\"note\\":\\"before the tag\\"}",
  "sender_standing":"unproven","sender_tier":"handle","untrusted":true,"sender_known":true,
  "alias":null,"matched_contact":"/bekir/*","sender_handle":"/bekir/agent1","peer":"/bekir/agent1",
  "peer_handle":"/bekir/agent1","thread_id":"t-agent1","direction":"in","autonomy":"review",
  "verb":"run_tests","held_because":"verb_denied","received_at":\(now - 600),"read":false},

 {"message_id":"m3","from":"/k/eeee5555ffff6666gggg7777hh","body":"hello from a stranger",
  "sender_standing":"unproven","sender_tier":"anonymous","untrusted":true,"sender_known":false,
  "alias":null,"matched_contact":null,"sender_handle":null,"peer":"/k/eeee5555ffff6666gggg7777hh",
  "peer_handle":null,"thread_id":"t-stranger","direction":"in","autonomy":"review","verb":null,
  "held_because":"sender_not_auto","received_at":\(now - 60),"read":false},

 {"message_id":"m_docdex","from":"/k/zz1111v2h90vnwefj7g7ezvbh9","body":"index rebuilt",
  "sender_standing":"unproven","sender_tier":"handle","untrusted":true,"sender_known":true,
  "alias":null,"matched_contact":"/bekir/*","sender_handle":"/bekir/docdex","peer":"/bekir/docdex",
  "peer_handle":"/bekir/docdex","direction":"in","autonomy":"review","verb":null,
  "held_because":"not_a_request","received_at":\(now - 10800),"read":true},

 {"message_id":"m_out1","direction":"out","from":"/k/cz6900v2h90vnwefj7g7ezvbh4",
  "to":"/bekir/agent1","peer":"/bekir/agent1","peer_handle":"/bekir/agent1","thread_id":"t-agent1",
  "body":"on it, running now","untrusted":false,"autonomy":null,
  "sent_at":\(now - 3600),"received_at":\(now - 3600),"read":true}
],"policy":{"accept_all":true,"auto_accept_known":false}}
"""

let contactsJSON = """
{"contacts":[{"peer":"/bekir/*","alias":"my fleet","admission":"allow","autonomy":"review","allowed_verbs":[]}],
 "policy":{"accept_all":true,"auto_accept_known":false},
 "vocabulary":{"grantable":["report_status","answer_question","read_file","run_tests"],
 "never_auto":["git_push","deploy","read_credentials","spend","delete_files","run_shell"]}}
"""

let threadsJSON = """
{"threads":[
 {"thread_id":"t-agent1","peer":"/bekir/agent1","title":null,"is_default":true,
  "created_at":\(now - 9000),"last_at":\(now - 600),"archived":false},
 {"thread_id":"t-agent1-deploy","peer":"/bekir/agent1","title":"deploy","is_default":false,
  "created_at":\(now - 8000),"last_at":\(now - 8000),"archived":false},
 {"thread_id":"t-stranger","peer":"/k/eeee5555ffff6666gggg7777hh","title":null,"is_default":true,
  "created_at":\(now - 60),"last_at":\(now - 60),"archived":false}]}
"""

let decoder = JSONDecoder()
decoder.keyDecodingStrategy = .convertFromSnakeCase

print("decoding")
let inbox = try decoder.decode(InboxResponse.self, from: Data(inboxJSON.utf8))
let contactsBody = try decoder.decode(ContactsResponse.self, from: Data(contactsJSON.utf8))
let threadsBody = try decoder.decode(ThreadsResponse.self, from: Data(threadsJSON.utf8))
let messages = inbox.messages ?? []
let contacts = contactsBody.contacts ?? []
equal(messages.count, 5, "every message decodes, including the one with no thread_id")
equal(contactsBody.vocabulary?.neverAuto?.count, 6, "the server's never-auto list survives decoding")

print("\nthreads")
var conversations = ConversationBuilder.build(
    messages: messages, pending: [], contacts: contacts,
    ownAgents: [docdex, scratch], acting: me
)
equal(conversations.map(\.peer),
      ["/k/eeee5555ffff6666gggg7777hh", "/bekir/agent1", "/bekir/docdex"],
      "newest first, and `scratch` — which has never corresponded — is not a row at all")

let agent1 = conversations.first { $0.peer == "/bekir/agent1" }!
equal(agent1.messages.count, 3, "both halves of the conversation are in it")
equal(agent1.messages.map(\.kind), [.incoming, .outgoing, .incoming], "and in the order they happened")
equal(agent1.unread, 1, "unread counts received mail only")
equal(agent1.held, 1, "a held request is counted as held")
equal(agent1.contact?.peer, "/bekir/*", "the namespace wildcard is matched when there is no exact row")
equal(agent1.name, "my fleet", "and its alias is what the row is called")

let mine = conversations.first { $0.peer == "/bekir/docdex" }!
check(mine.mine, "an own mailbox that has written is marked as yours")
equal(mine.name, "docdex", "and named from its handle, not from the contact wildcard")

print("\na request reads as a request")
let request = agent1.messages.first { $0.isRequest }!
let envelope = request.envelope!
equal(envelope.verb, "run_tests", "the verb is lifted out of the envelope")
equal(envelope.args["suite"], "unit", "with its arguments")
equal(envelope.note, "before the tag", "and its note")
equal(ConversationBuilder.preview(request), "asks to run tests", "the list says what it asks for")
equal(ConversationBuilder.heldReason(request.heldBecause!), "that verb was not granted to this sender",
      "and why it is being held, in words")
check(RequestEnvelope(body: "{ this is prose that starts with a brace }") == nil,
      "prose that happens to start with a brace is prose")

print("\nsending")
let pending = PendingMessage(
    id: "local_1", mailbox: me.address, to: "/k/aaaa1111bbbb2222cccc3333dd",
    body: "typed to the key address", at: now, status: .sending, threadId: "t-agent1"
)
conversations = ConversationBuilder.build(
    messages: messages, pending: [pending], contacts: contacts,
    ownAgents: [docdex, scratch], acting: me
)
equal(conversations.first?.peer, "/bekir/agent1",
      "a message addressed to /k/… joins the conversation the handle already has")
equal(conversations.first?.messages.count, 4, "and is shown immediately, before any poll")

// The optimistic row is retired by id, so a repeated message is never mistaken for its own echo.
let echoed = ConversationBuilder.build(
    messages: messages + messages, pending: [], contacts: contacts,
    ownAgents: [docdex, scratch], acting: me
)
equal(echoed.first { $0.peer == "/bekir/agent1" }?.messages.count, 3,
      "a listing that repeats itself still shows each message once")

print("\nsubjects within one peer")
let subs = ConversationBuilder.subthreads(
    of: agent1, serverThreads: threadsBody.threads ?? [],
    peer: "/bekir/agent1", messages: messages
)
equal(subs.count, 2, "a thread opened and never written in is still a thread")
equal(subs.map(\.id), ["t-agent1", "t-agent1-deploy"], "most recently active first")
equal(subs.last?.name, "deploy", "named by its title")

let strangerSubs = ConversationBuilder.subthreads(
    of: conversations.first { $0.peer == "/k/eeee5555ffff6666gggg7777hh" },
    serverThreads: threadsBody.threads ?? [],
    peer: "/k/eeee5555ffff6666gggg7777hh", messages: messages
)
equal(strangerSubs.count, 1, "one conversation with a peer shows no thread list at all")

print("\nreplying to a peer with one conversation")
// The bug this covers, seen on a phone: one reply to the stranger — who has a single thread —
// put the question in one "General" and the answer in a second one beside it.
let stranger = "/k/eeee5555ffff6666gggg7777hh"
let strangerConversation = ConversationBuilder.build(
    messages: messages, pending: [], contacts: contacts,
    ownAgents: [docdex, scratch], acting: me
).first { $0.peer == stranger }
let strangerThreads = ConversationBuilder.subthreads(
    of: strangerConversation, serverThreads: threadsBody.threads ?? [],
    peer: stranger, messages: messages
)
equal(ConversationBuilder.targetThread(subthreads: strangerThreads, selected: nil), "t-stranger",
      "a reply goes to the one thread that exists, not to no thread at all")
equal(ConversationBuilder.targetThread(subthreads: subs, selected: "t-agent1-deploy"), "t-agent1-deploy",
      "and to the subject on screen when there is a choice")
equal(ConversationBuilder.targetThread(subthreads: [], selected: nil), nil,
      "nil only when there is no thread to name — a postbox with no thread routes")

// Belt as well as braces: a message that arrives with no thread id at all still belongs to the
// default conversation rather than beside it.
let orphaned = PendingMessage(
    id: "local_2", mailbox: me.address, to: stranger,
    body: "Yes. Who is this?", at: now, status: .sending, threadId: nil
)
let withOrphan = ConversationBuilder.build(
    messages: messages, pending: [orphaned], contacts: contacts,
    ownAgents: [docdex, scratch], acting: me
).first { $0.peer == stranger }
let foldedThreads = ConversationBuilder.subthreads(
    of: withOrphan, serverThreads: threadsBody.threads ?? [],
    peer: stranger, messages: messages
)
equal(foldedThreads.count, 1, "an id-less message joins the default thread instead of forming a second one")
equal(foldedThreads.first?.messages.count, 2, "and both halves are in it")

print("\nfaces")
// The tone hash is the web app's. These are the values app.js produces for the same strings —
// regenerate with:  node -e 'let h=0;for(const c of "/bekir/agent1")h=(h*31+c.charCodeAt(0))>>>0;console.log(h%6+1)'
equal(PeerFace.displayName("/bekir/agent1"), "agent1", "a handle reads as a name")
equal(PeerFace.displayName("/k/aaaa1111bbbb2222cccc3333dd"), "/k/aaaa1111b…", "a key address is truncated, not pretended to be a word")
equal(PeerFace.initials("/bekir/agent1"), "AG", "initials come from the name")
equal(PeerFace.toneIndex("/bekir/agent1"), 6, "/bekir/agent1 keeps the tone the web app gives it")
equal(PeerFace.toneIndex("/bekir/docdex"), 1, "/bekir/docdex too")
equal(PeerFace.toneIndex("/k/eeee5555ffff6666gggg7777hh"), 6, "and a key address")

print("\nmarkdown")
// What agents actually send, and the shapes that must not be swallowed.
equal(Markdown.blocks(of: "## Heading").first, .heading(level: 2, text: "Heading"),
      "a hash and a space is a heading")
equal(Markdown.blocks(of: "#nothashtag").first, .paragraph("#nothashtag"),
      "a hash without a space is not a heading")
equal(Markdown.blocks(of: "- one\n- two").first, .bullets(["one", "two"]),
      "consecutive dashes are one list")
equal(Markdown.blocks(of: "1. one\n2. two").first, .numbered(["one", "two"]),
      "numbered items are one list")
equal(Markdown.blocks(of: "```rust\nlet x = 1;\n```").first, .code("let x = 1;", language: "rust"),
      "a fence keeps its language")
// The case that matters most: markup inside a fence is literal, not markup.
equal(Markdown.blocks(of: "```\n# not a heading\n- not a bullet\n```").first,
      .code("# not a heading\n- not a bullet", language: nil),
      "a fence protects what is inside it")
equal(Markdown.blocks(of: "```\nunterminated").first, .code("unterminated", language: nil),
      "an unterminated fence keeps the rest of the message rather than losing it")
equal(Markdown.blocks(of: "> quoted").first, .quote("quoted"), "a quote is a quote")
equal(Markdown.blocks(of: "---").first, .rule, "a rule on its own line")
equal(Markdown.blocks(of: "a\n\nb").count, 2, "a blank line ends a paragraph")
equal(Markdown.blocks(of: "plain words").first, .paragraph("plain words"),
      "prose survives unchanged")
equal(Markdown.blocks(of: "").count, 0, "an empty body is no blocks, not an empty paragraph")
// Nothing may vanish: every non-blank line has to end up somewhere.
let messy = "# Title\n\nSome text\n- a\n- b\n\n```\ncode\n```\n\nmore"
equal(Markdown.blocks(of: messy).count, 5, "a mixed document keeps all five of its blocks")

print("\ntables")
// The same rule the web app applies, so a table read on the phone is the table read in the browser.
let table = "| Host | Port | State |\n| --- | ---: | :---: |\n| wodomini | 34251 | up |\n| postbox | 22 | up |"
equal(Markdown.blocks(of: table).first,
      .table(header: ["Host", "Port", "State"],
             alignments: [.leading, .trailing, .center],
             rows: [["wodomini", "34251", "up"], ["postbox", "22", "up"]]),
      "pipes under an underline are a table, alignments and all")
// The underline is what makes it a table. Without one the pipes were punctuation.
equal(Markdown.blocks(of: "not a table | just a sentence").first,
      .paragraph("not a table | just a sentence"),
      "a pipe with no underline beneath it is a sentence")
// Not a table, and neither line is lost: they are two ordinary lines of one paragraph.
equal(Markdown.blocks(of: "| a | b |\n| --- | oops |").first,
      .paragraph("| a | b |\n| --- | oops |"),
      "an underline that is not all dashes does not make a table")
// A blank line ends it, and what follows is its own block rather than another row.
equal(Markdown.blocks(of: "| a |\n| - |\n| x |\n\nafter").count, 2,
      "a blank line ends the table and the prose after it survives")
equal(Markdown.blocks(of: "| a | b |\n| --- | --- |\n| only one |").first,
      .table(header: ["a", "b"], alignments: [.leading, .leading], rows: [["only one"]]),
      "a short row is kept as it was rather than dropped for not fitting")
equal(Markdown.tableCells(of: "| a | b |"), ["a", "b"],
      "the fencing pipes are a fence, not two empty cells")

print("\npreviews")
// Everything these clients send is full_access, so a list of them previewing the verb would be a
// list of one repeated line. The words are the information there.
equal(ConversationBuilder.preview(ThreadMessage(
        id: "p1", kind: .incoming, at: 0,
        body: "{\"v\":1,\"verb\":\"full_access\",\"args\":{\"task\":\"ship the fix\"},\"note\":\"ship the fix\"}",
        threadId: nil)),
      "ship the fix", "a full-permissions request previews its words")
// A narrow verb is still the information: "asks to run tests" says what a peer wants.
equal(ConversationBuilder.preview(ThreadMessage(
        id: "p2", kind: .incoming, at: 0,
        body: "{\"v\":1,\"verb\":\"run_tests\",\"args\":{},\"note\":\"before the tag\"}",
        threadId: nil)),
      "asks to run tests", "a narrow verb still previews as the verb")
// Markdown punctuation is noise at a glance, and costs characters the sentence needed.
equal(ConversationBuilder.plain("## Pinned it\n\nThe **listener** was `non-blocking`."),
      "Pinned it The listener was non-blocking.", "a preview drops the markup")
equal(ConversationBuilder.plain("prose\n```\ncode()\n```\nmore"), "prose more",
      "and skips code, which summarises nothing")

print("\ncopy")
equal(ThreadMessage(id: "1", kind: .incoming, at: 0,
                    body: "{\"v\":1,\"verb\":\"full_access\",\"args\":{\"task\":\"ship it\"},\"note\":\"ship it\"}",
                    threadId: nil).copyText,
      "ship it", "copying a request gives the words, not the envelope")
equal(RequestEnvelope(body: "{\"v\":1,\"verb\":\"full_access\",\"args\":{},\"note\":\"x\"}")?.title,
      "Full permissions", "the verb reads as what it asks for")

print("")
if failures == 0 {
    print("all good")
} else {
    print("\(failures) failed")
    exit(1)
}
