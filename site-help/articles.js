// help.pigeonpost.dev content: categories, the home page's lists and the contact page. The
// articles themselves live in articles/<category>.js, each pushing onto PP_HELP.articles.
//
// Every statement must come from the product itself (the apps, the platforms, the public pages
// and docs/). When a fact can't be checked, leave it out rather than guess.
//
// Article shape: { slug, cat, title, summary, keywords?, aliases?, related?, body }
//   body blocks: "paragraph" · ["h", "Section"] · ["steps", …] · ["list", …] ·
//                ["note" | "tip" | "warn", "Title", "text"] · ["code", "line", …]
//   inline: **UI label**, `code`, [label](/a/<slug>) or [label](https://…). Text only, never HTML.
window.PP_HELP = {
  tagline: "Guides for people, organizations and partners, on every Pigeonpost app.",
  categories: [
    { key: "getting-started", title: "Getting started", icon: "rocket", text: "Addresses, handles, the apps and signing in." },
    { key: "messaging", title: "Messaging", icon: "message-circle", text: "Conversations, groups, replies, receipts, files and contacts." },
    { key: "calls", title: "Calls and meetings", icon: "video", text: "Voice and video calls, meeting links, live streams and meeting notes." },
    { key: "calendar", title: "Calendar", icon: "calendar", text: "Events, guests, invitations and calendar files." },
    { key: "agents", title: "AI agents", icon: "bot", text: "Inboxes for agents, connector keys, trust, the CLI, agentd and MCP." },
    { key: "nests", title: "Nests", icon: "egg", text: "Hosted AI agents: packages, files, approvals and privacy." },
    { key: "marketplace", title: "Marketplace and credits", icon: "store", text: "Running services, credits, holds and refunds." },
    { key: "account", title: "Account and billing", icon: "wallet", text: "Plans, payment, invoices, cancelling and deleting your account." },
    { key: "enterprise", title: "Organizations", icon: "building-2", text: "Set up your company, people and roles, agents, rooms and billing." },
    { key: "partners", title: "Partners and publishers", icon: "briefcase", text: "Apply, publish, set prices and get paid." },
    { key: "privacy", title: "Privacy and security", icon: "shield-check", text: "Encryption, filtering, blocking and your data." },
    { key: "troubleshooting", title: "Troubleshooting", icon: "alert-circle", text: "Fixes for each app: sign-in, notifications, calls and updates." },
  ],
  articles: [],
  popular: ["sign-in", "two-factor", "handles", "inboxes", "credits", "nests"],
  quick: [
    { q: "Is Pigeonpost free?", a: "Yes. Key addresses and messaging are free, and a signed-in inbox holds 20 MB. A handle ($8 a year) raises your inbox to 1 GB. See [Inbox storage](/a/inbox-storage)." },
    { q: "Why can't someone send me a message?", a: "Your inbox may be full: new messages are refused and nothing old is deleted to make room. Your filtering settings may also hold or block them. See [My inbox is full](/a/inbox-full)." },
    { q: "How many people can join a call?", a: "Up to 30 people with a meeting link. Calls started inside a group hold up to 6. See [Group calls](/a/group-calls)." },
    { q: "Do marketplace credits expire?", a: "No. Credits are kept in US dollars and don't expire. Unspent credit from a purchase can be refunded within 14 days. See [Credits and refunds](/a/credits)." },
    { q: "Can my company's admins read my messages?", a: "No. Organization admin roles never include reading anyone's messages. See [Roles in an organization](/a/org-people)." },
  ],
  contact: {
    title: "Still need help?",
    text: "Write to our support team. Partners can talk to us live from the partner console.",
    lede: "Pick the channel that fits. Each one reaches the Pigeonpost team.",
    channels: [
      { icon: "mail", title: "Support", who: "Everyone", text: "Questions about your account, the apps, billing or a problem you've hit. Write to support@pigeonpost.dev.", action: "Write to support", href: "mailto:support@pigeonpost.dev" },
      { icon: "building-2", title: "Organizations", who: "Admins of a company on Pigeonpost", text: "The **Support** page in your organization's admin app writes to support with your organization named, and we answer within one business day.", action: "Open enterprise support", href: "https://enterprise.pigeonpost.dev/app#/support" },
      { icon: "briefcase", title: "Partners", who: "Businesses with a partner application", text: "Start a conversation from **Support** in the partner console, even before you're approved. Replies arrive there.", action: "Open partner support", href: "https://partner.pigeonpost.dev/#/support" },
      { icon: "shield", title: "Privacy and abuse", who: "Data requests and reports", text: "Ask for a copy of your data at privacy@pigeonpost.dev. Report abuse to abuse@pigeonpost.dev.", action: "Write to privacy", href: "mailto:privacy@pigeonpost.dev" },
    ],
    tips: [
      "Check [service status](https://status.pigeonpost.dev/) first: if something is down, it's shown there.",
      "Say which app and version you use (web, iPhone, iPad, Mac, Android, Windows, Linux or the CLI).",
      "Include the address involved, such as your handle, and what you expected to happen.",
      "Never send a password, a sign-in code, a connector key or card details.",
    ],
  },
};
