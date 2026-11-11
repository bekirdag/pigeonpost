//! `authorize(actor, action, resource)` — the one place new authorization rules are written
//! (SDS §3.3).
//!
//! A decision is an **intersection** of mandatory constraints, never an early-return list of
//! alternative grants. In order:
//!
//! 1. the credential must be of a kind the action accepts (automation cannot perform human-only
//!    actions, mailbox tokens cannot act for the account, services only reach service actions);
//! 2. every mandatory-deny hook must pass (revocation, blocks, suspension — facts the caller
//!    resolved from server state, never from the request);
//! 3. an action-specific grant must exist for this actor on this resource (ownership today; org
//!    roles and delegations join here in later phases);
//! 4. anything not explicitly granted is denied.
//!
//! Existing handlers keep their own checks; new endpoints go through here.

use crate::principal::{require_human, require_step_up, AuthDenial, StepUpPolicy};
use crate::{AccountCredential, ApiError, Principal};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

/// Who is acting. A [`Principal`] for member-facing routes, or an internal service.
pub(crate) enum Actor<'a> {
    Principal(&'a Principal),
    Service(&'a str),
    /// No credential at all: only actions open to anyone (public profile pictures) accept it.
    Anonymous,
    /// An org API key (`pk_org_…`, SDS §18.2): automation bound to one organization. Never a
    /// person, never a mailbox; it acts only through `Action::OrgApi`.
    OrgKey {
        key_id: &'a str,
        org_id: &'a str,
    },
}

/// What they want to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Action {
    /// `GET /v1/capabilities`.
    ReadCapabilities,
    /// `GET /v1/events`.
    StreamAccountEvents,
    /// `GET /internal/v1/health`.
    ServiceHealth,
    /// `GET /internal/v1/features`.
    ReadFeatureFlags,
    /// `PUT /internal/v1/features/{name}`.
    WriteFeatureFlags,
    /// The billing bridge's entitlement and org endpoints (`/internal/v1/entitlements`, `/orgs`).
    ManageBilling,
    /// `POST /internal/v1/call-ring` and `/call-ring-cancel` (Phase 1c).
    PushCallRing,
    /// A sensitive member action (§3.4 list). Nothing in Phase 0 is gated on it yet; it exists so
    /// the requirement is expressed once, here, when the first such endpoint lands.
    #[cfg_attr(not(test), allow(dead_code))]
    SensitiveAccountChange,
    /// Read or change a mailbox's own call history, filtering settings or picture
    /// (`/v1/calls/log*`, `/v1/policy`, `/v1/profile/avatar`). Agents may act on their own mailbox.
    ManageOwnMailbox,
    /// Move a sender out of Filtered (`POST /v1/filtered/accept`): a trust raise, so not through a
    /// connector acting on the member's behalf. (MCP agents never reach the route at all.)
    RaiseTrust,
    /// Report a call or a picture as abuse. Lowering trust in someone else; anyone signed in.
    ReportAbuse,
    /// See a mailbox's picture.
    ViewAvatar,
    /// `POST /internal/v1/call-admission`.
    CallAdmission,
    /// `POST /internal/v1/call-events`.
    CallEvents,
    /// `/internal/v1/avatar-reports`, `/internal/v1/avatars/{id}/remove`.
    ModerateAvatars,
    /// The account's own keys, mailboxes and handles (`/v1/api-keys`, `/v1/identities`, …): the
    /// account holder's own credential (member session or API key).
    ManageOwnAccount,
    /// `POST /internal/v1/accounts/{id}/purge`: remove an empty account (ops only, Phase 9).
    PurgeEmptyAccount,
    /// `POST /internal/v1/account-deletions/{request_id}/cancel-calendar`: cancel what a person
    /// being erased organizes, so their guests are told (ops only, audit P2-17).
    PrepareAccountErasure,
    /// `PUT /internal/v1/namespaces`: bind a namespace to an account by hand (ops only, P3-10).
    GrantNamespace,
    /// `/internal/v1/nests*`: create, list, run and delete hosted agents (Nests Phase A, ops).
    ManageNests,
    /// See one's own Nests, Nest allowance and the Nest package catalog (`GET /v1/nests`,
    /// `/v1/nest-packages`): the account's own credential, never a mailbox token or a connector.
    NestView,
    /// Create, pause, resume or delete one's own Nest (build plan §4 `NestOwn`): a person.
    NestOwn,
    /// Start or claim a Nest package purchase, restore one, retry a paid setup (build plan §4
    /// `NestPurchase`): a person, never automation — buying is never automatic.
    NestPurchase,
    /// A Nest proposes a typed tool call for its owner to approve (`POST /v1/nest-approvals`):
    /// the Nest's own mailbox credential only, never an account or a connector.
    NestToolRequest,
    /// Approve or reject a Nest's proposed call: a person, never automation.
    NestApprove,
    /// Marketplace foundations (build plan §4, M0). Each variant exists so the requirement is
    /// expressed here before the route that needs it lands; the ones with a route today are
    /// `NestDiscover`, `PublisherApply`, `PublisherManage`, `NestReview` and `StaffRolesManage`.
    ///
    /// Use a Nest one operates: chat, files. Approvals stay `NestApprove`.
    #[cfg_attr(not(test), allow(dead_code))]
    NestOperate,
    /// See a marketplace listing. Discovery grants nothing beyond the listing's public metadata.
    NestDiscover,
    /// Install a release as a Nest for a buyer account or org.
    NestInstall,
    /// Change an installation's configuration.
    NestConfigure,
    /// Administer an installation: update, roll back, remove.
    NestManage,
    /// Submit a job to a Nest operation (`/v1/nest-jobs`): needs admission and, when the offer is
    /// paid, a valid payment for this job. M1 also uses it for the zero-priced quote and payment
    /// authorization that precede a job: no money moves, so no purchase action is involved.
    NestInvoke,
    /// Read one's own job (`GET /v1/nest-jobs/{job}`, M1): the submitting account only.
    NestJobRead,
    /// Pay for one marketplace purchase. There is no wallet and no stored balance (owner decision
    /// 2026-11-09), so this replaces the plan's top-up and spend-mandate actions and is never
    /// automatic: no mandate exists for automation to consume.
    #[cfg_attr(not(test), allow(dead_code))]
    NestMarketplacePurchase,
    /// Apply to the publisher programme as an account or an org.
    PublisherApply,
    /// Read or change one's own publisher profile.
    PublisherManage,
    /// Submit (or resubmit) a verified publisher registration: a person with step-up, so the
    /// developer account has two-factor sign-in (developer program plan §2).
    PublisherSubmit,
    /// Publish a release or listing: a person with step-up.
    NestPublish,
    /// Record a review decision on a release; also what a reviewer reads to get there.
    NestReview,
    /// Sign the attestation of an approved release.
    NestAttest,
    /// Quarantine or revoke a live release.
    NestQuarantine,
    /// Prepare a publisher payout batch. The separate preparer/approver rule is design only and is
    /// not implemented in M0.
    #[cfg_attr(not(test), allow(dead_code))]
    PayoutPrepare,
    /// Approve a publisher payout batch (see `PayoutPrepare`).
    #[cfg_attr(not(test), allow(dead_code))]
    PayoutApprove,
    /// Assign, list or revoke staff roles: the platform owner only.
    StaffRolesManage,
    /// Back office (`/v1/staff/me`, `/v1/staff/dashboard`): who one is as staff, and platform
    /// counts. No personal data, so a role and a person suffice; no step-up.
    StaffOverview,
    /// Back office people screens (accounts, handles, mailboxes, organizations): personal data,
    /// so a step-up no older than [`STAFF_READ_STEP_UP_SECS`].
    StaffPeopleRead,
    /// Back office plans screen (entitlements and store subscriptions): personal data.
    StaffPlansRead,
    /// Back office credit reads proxied to saas_be (balances, history, sales, refunds).
    StaffFinanceRead,
    /// Back office credit writes proxied to saas_be (refund, resolve a refund, adjust a balance):
    /// money moves, so the write step-up window.
    StaffFinanceWrite,
    /// The platform-wide audit log reader.
    StaffAuditRead,
    /// Marketplace operations: listings and their moderation, buyer reports, kill switches,
    /// publisher quotas, source builds and hosted jobs. Reviews, attestations and release
    /// security stay `NestReview`/`NestAttest`/`NestQuarantine` (reviewers only).
    MarketplaceOps,
    /// Take part in a group as one's own mailbox: read, post, leave, own keys (SDS §4.4). Agents
    /// may; a connector acting for the member may not reach E2EE content at all.
    GroupParticipate,
    /// Change a group's settings, roles or membership, accept a private invitation or enrol a
    /// further device: a person only (`human_required`, SDS §4.4 "Agents in groups").
    GroupManage,
    /// `POST /internal/v1/group-call-admission`.
    GroupCallAdmission,
    /// An organization action a person takes on the org's administrative surface (`/v1/orgs/…`):
    /// the org role matrix (SDS §6.4) is resolved into `Facts::org_permitted` beforehand.
    OrgAdminister,
    /// The §3.4 sensitive org actions: appoint/remove full admins, delete the org, transfer
    /// ownership, export the audit log, issue enrollment codes, approve a device login.
    OrgSensitive,
    /// Redeem a one-time enrollment code or claim an approved pairing: possession of the code or
    /// claim secret plus the device key is the credential (SDS §10.2).
    DeviceEnroll,
    /// Use one's own calendar as one's own mailbox (SDS §14B.5). Agents may (handle-only events);
    /// a connector acting for the member may not. Sending to contact addresses additionally needs
    /// a person with a verified sign-in address, checked by the calendar routes.
    CalendarUse,
    /// `POST /internal/v1/calendar/meeting-access` (SDS §14B.7).
    MeetingAccess,
    /// Read, delete and key one's own meeting notes (`/v1/meetings…`, SDS §14.7): the account's own
    /// credential — a person's session or the account key — never a mailbox token or a connector.
    MeetingNotes,
    /// Register a device's meeting-notes key (`PUT /v1/meetings/keys/{id}`): a person signed in
    /// interactively, never the account API key (audit 2026-11-03 P2-2). A notes key decides who
    /// can open every later brief, so only a person at a device may add one.
    MeetingNotesKey,
    /// The calls service opening meetings and recording consent (`/internal/v1/meetings…`).
    MeetingsCalls,
    /// meeting-intel reading recipients' public keys and storing the encrypted brief.
    MeetingsIntel,
    /// One `/org/v1` operation by an org API key (SDS §18). Granted only when the key holds the
    /// operation's scope (`Facts::scope_granted`) **and** the person who created the key still has
    /// the authority for it (`Facts::org_permitted`) — a key can never exceed either.
    OrgApi,
    /// Watch a group's live stream as one's own mailbox: list streams, register a receive key,
    /// get a subscribe-only media ticket (SDS §14A). Agents may; a connector acting for the member
    /// may not (the stream is end-to-end encrypted to the watching device).
    StreamWatch,
    /// Go live in a group, or accept an invitation onto a stream's stage: a person only, because it
    /// opens a camera, a microphone or a screen (SDS §14A.1).
    StreamPublish,
    /// Invite to or remove from a stream's stage, read a stream's audience keys, stop a stream: the
    /// streamer or a group owner/admin, as a person.
    StreamModerate,
    /// `POST /internal/v1/stream-admission`, `/stream-check`, `/stream-end` (the calls service).
    StreamAdmission,
    /// Open an SSH session to an agent's workspace (SDS §11.5), or collect its credentials after
    /// approval: a person, signed in recently and strongly, holding the agent (or the org owner
    /// role, or a live `ssh` delegation) — resolved into `Facts::shell_permitted`. Never an API
    /// key, an org key, a connector or another agent's mailbox (`human_required`).
    ShellOpen,
    /// Approve or deny someone else's pending session, approve a host-key rotation, or switch SSH
    /// on for an org agent: a person with authority over the agent, with step-up.
    ShellApprove,
    /// See an agent's shell status, list or end sessions: a person with a relation to the agent.
    ShellView,
    /// The agent machine's own side: hold the shell control stream and report its local opt-in
    /// and host key. Only the agent's own mailbox credential, never an account credential.
    ShellAgent,
    /// `POST /internal/v1/shell/*` (the tunnel broker).
    ShellBroker,
    /// Add, verify, move, unlink a phone number or change its discoverability (SDS §14D): a person
    /// only, because it sends an SMS that costs money and links a real-world identifier.
    ManagePhone,
    /// Match uploaded contact hashes against discoverable numbers (SDS §14D.3): a person only. API
    /// keys, mailbox tokens and connectors are the cheapest scraping credentials, so none may.
    DiscoverContacts,
    /// Send one SMS invitation (SDS §14D, D-14D-8): a person, on an explicit tap.
    SendSmsInvite,
    /// Set or remove the acting mailbox's self-chosen display name. The mailbox's own credential
    /// or the account's; never a connector acting for the member.
    SetDisplayName,
    /// See a mailbox's display name: any credential (never anonymous), subject to the name's
    /// visibility, decided exactly like a picture's.
    ViewProfile,
    /// Stop SMS invitations to a number, holding the invitation code that was texted to it
    /// (`POST /v1/sms/opt-out`). Anyone, signed in or not: the code is the proof.
    SmsOptOut,
}

/// The resource acted on, with bindings resolved from server state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Resource<'a> {
    /// An account, by id.
    Account(&'a str),
    /// The platform itself (flags, health): only services act on it.
    Platform,
    /// One mailbox, with its account binding as stored (never as requested).
    Mailbox {
        address: &'a str,
        account: Option<&'a str>,
    },
    /// An organization, by id. The grant is the caller's live role or delegation, resolved from
    /// server state into [`Facts::org_permitted`].
    Org(&'a str),
    /// A pending device pairing or enrollment, by id.
    DeviceGrant(&'a str),
    /// An agent's shell, by the agent's immutable mailbox address. A person's grant (owner, org
    /// owner, or a live `ssh` delegation, plus the org's and the agent's switches) is resolved
    /// from server state into [`Facts::shell_permitted`]; the agent itself acts with its own
    /// mailbox credential only.
    AgentShell { agent: &'a str },
    /// A group's live stream, acted on as `mailbox` (with its stored account binding). The group
    /// rules (membership, role, the group's `stream_policy`, the stage) are resolved from the rows
    /// the request changes into [`Facts::stream_permitted`].
    GroupStream {
        mailbox: &'a str,
        account: Option<&'a str>,
    },
    /// A mailbox's display name, with the facts its visibility is decided on (same rule as a
    /// picture).
    Profile {
        visibility: crate::store::Visibility,
        owner_account: Option<&'a str>,
        owner_address: &'a str,
        viewer_known: bool,
    },
    /// A mailbox's picture, with the facts its visibility is decided on.
    Avatar {
        visibility: crate::store::Visibility,
        owner_account: Option<&'a str>,
        owner_address: &'a str,
        /// The owner counts at least one of the viewer's mailboxes as known (SDS §13.1).
        viewer_known: bool,
    },
    /// A marketplace listing, by id. Whether this caller may see it is resolved into
    /// [`Facts::listing_visible`].
    NestListing(&'a str),
    /// An operation offered by a listing, by listing id. Admission is [`Facts::job_admitted`].
    NestOperation(&'a str),
    /// One job, by id. Whether this caller submitted it is [`Facts::job_visible`].
    NestJob(&'a str),
    /// One installed Nest, by id. The owner, org role or live delegation is
    /// [`Facts::nest_permitted`].
    #[cfg_attr(not(test), allow(dead_code))]
    InstalledNest(&'a str),
    /// A publisher profile, by id. The profile's owner or org role is
    /// [`Facts::publisher_permitted`].
    Publisher(&'a str),
    /// What a staff action is taken on: a release, a publisher's application or a payout batch.
    /// Only actions whose requirement is `Staff` reach it.
    StaffSubject(&'a str),
    /// The platform's staff role table.
    StaffRoles,
}

/// A role in `staff_roles`. Which actions each role covers is decided here, not in the routes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum StaffRole {
    /// The back office's owner screens: people, the platform audit log. Managing staff roles is
    /// still the configured platform owner only (`StaffRolesManage`), never a role row.
    Owner,
    /// People and plans: accounts, handles, mailboxes, organizations, entitlements.
    Support,
    /// Money: credits, and plans.
    Finance,
    /// Marketplace review: publishers, reviews, attestations, release security; and marketplace
    /// operations.
    Reviewer,
    /// Marketplace operations (listings, reports, kills, quotas, builds, hosted jobs).
    Ops,
}

impl StaffRole {
    pub(crate) const ALL: [StaffRole; 5] = [
        StaffRole::Owner,
        StaffRole::Support,
        StaffRole::Finance,
        StaffRole::Reviewer,
        StaffRole::Ops,
    ];

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            StaffRole::Owner => "owner",
            StaffRole::Support => "support",
            StaffRole::Finance => "finance",
            StaffRole::Reviewer => "reviewer",
            StaffRole::Ops => "ops",
        }
    }

    pub(crate) fn parse(text: &str) -> Option<Self> {
        StaffRole::ALL.into_iter().find(|r| r.as_str() == text)
    }
}

/// How fresh a strong sign-in must be to read personal data in the back office: at most an hour
/// (writes keep the configured, shorter window). Never looser than the write window.
pub(crate) const STAFF_READ_STEP_UP_SECS: u64 = 3600;

/// The kind of credential an action accepts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Requirement {
    /// Any member credential, API key, connector or mailbox token.
    AnyPrincipal,
    /// The account's own credential: member session or API key (not a mailbox token or a
    /// connector).
    AccountHolder,
    /// A person signed in interactively, recently and strongly.
    HumanStepUp,
    /// An internal service.
    Service,
    /// Any principal except a connector acting for the member.
    NotConnector,
    /// A person signed in interactively (no step-up).
    Human,
    /// Anyone, with or without a credential.
    Anyone,
    /// An org API key, and nothing else.
    OrgKey,
    /// One mailbox's own credential (capability token or device credential), and nothing else:
    /// not the account that owns it.
    Mailbox,
    /// A person holding the live staff role the action names, signed in recently and strongly.
    /// Staff is a principal kind of its own: never automation, and never an account by itself.
    Staff,
    /// The same, for reads of personal data: the strong sign-in may be up to
    /// [`STAFF_READ_STEP_UP_SECS`] old.
    StaffRead,
    /// A person holding one of the action's staff roles, without step-up: back office reads with
    /// no personal data in them.
    StaffView,
}

impl Action {
    fn requirement(self) -> Requirement {
        match self {
            Action::ReadCapabilities => Requirement::AnyPrincipal,
            Action::StreamAccountEvents => Requirement::AccountHolder,
            Action::ServiceHealth
            | Action::ReadFeatureFlags
            | Action::WriteFeatureFlags
            | Action::ManageBilling
            | Action::PushCallRing => Requirement::Service,
            Action::SensitiveAccountChange => Requirement::HumanStepUp,
            Action::ManageOwnMailbox | Action::ReportAbuse => Requirement::AnyPrincipal,
            Action::RaiseTrust => Requirement::NotConnector,
            Action::ManageOwnAccount => Requirement::AccountHolder,
            Action::ViewAvatar => Requirement::Anyone,
            Action::CallAdmission
            | Action::CallEvents
            | Action::ModerateAvatars
            | Action::PurgeEmptyAccount
            | Action::PrepareAccountErasure
            | Action::GrantNamespace
            | Action::ManageNests
            | Action::GroupCallAdmission => Requirement::Service,
            Action::GroupParticipate | Action::CalendarUse => Requirement::NotConnector,
            Action::MeetingAccess => Requirement::Service,
            Action::MeetingNotes => Requirement::AccountHolder,
            Action::MeetingNotesKey => Requirement::Human,
            Action::MeetingsCalls | Action::MeetingsIntel => Requirement::Service,
            Action::GroupManage | Action::OrgAdminister => Requirement::Human,
            Action::OrgSensitive => Requirement::HumanStepUp,
            Action::DeviceEnroll => Requirement::Anyone,
            Action::OrgApi => Requirement::OrgKey,
            Action::StreamWatch => Requirement::NotConnector,
            Action::StreamPublish | Action::StreamModerate => Requirement::Human,
            Action::StreamAdmission => Requirement::Service,
            Action::ShellOpen | Action::ShellApprove => Requirement::HumanStepUp,
            Action::ShellView => Requirement::Human,
            Action::ShellAgent => Requirement::Mailbox,
            Action::ShellBroker => Requirement::Service,
            Action::ManagePhone | Action::DiscoverContacts | Action::SendSmsInvite => {
                Requirement::Human
            }
            Action::SetDisplayName => Requirement::NotConnector,
            Action::ViewProfile => Requirement::AnyPrincipal,
            Action::SmsOptOut => Requirement::Anyone,
            Action::NestView => Requirement::AccountHolder,
            Action::NestOwn | Action::NestPurchase | Action::NestApprove => Requirement::Human,
            Action::NestToolRequest => Requirement::Mailbox,
            Action::NestOperate | Action::NestInvoke | Action::NestJobRead => {
                Requirement::NotConnector
            }
            Action::NestDiscover => Requirement::Anyone,
            Action::NestInstall
            | Action::NestConfigure
            | Action::NestManage
            | Action::NestMarketplacePurchase
            | Action::PublisherApply
            | Action::PublisherManage => Requirement::Human,
            Action::NestPublish | Action::PublisherSubmit | Action::StaffRolesManage => {
                Requirement::HumanStepUp
            }
            Action::NestReview
            | Action::NestAttest
            | Action::NestQuarantine
            | Action::PayoutPrepare
            | Action::PayoutApprove
            | Action::StaffFinanceWrite
            | Action::MarketplaceOps => Requirement::Staff,
            Action::StaffPeopleRead
            | Action::StaffPlansRead
            | Action::StaffFinanceRead
            | Action::StaffAuditRead => Requirement::StaffRead,
            Action::StaffOverview => Requirement::StaffView,
        }
    }

    /// The staff roles that cover a staff action (any one of them suffices). Empty for every
    /// other action.
    pub(crate) fn staff_roles(self) -> &'static [StaffRole] {
        use StaffRole::*;
        match self {
            Action::NestReview | Action::NestAttest | Action::NestQuarantine => &[Reviewer],
            Action::PayoutPrepare | Action::PayoutApprove => &[Finance],
            Action::StaffOverview => &StaffRole::ALL,
            Action::StaffPeopleRead => &[Owner, Support],
            Action::StaffPlansRead => &[Support, Finance],
            Action::StaffFinanceRead | Action::StaffFinanceWrite => &[Finance],
            Action::StaffAuditRead => &[Owner],
            Action::MarketplaceOps => &[Reviewer, Ops],
            _ => &[],
        }
    }
}

/// Server-side facts the caller resolved before asking. All default to "nothing against".
#[derive(Debug, Clone, Default)]
pub(crate) struct Facts {
    pub now: u64,
    pub step_up: StepUpPolicy,
    /// The credential was revoked after it was presented (e.g. a key deleted mid-stream).
    pub credential_revoked: bool,
    /// The account is suspended or being erased.
    pub account_suspended: bool,
    /// An explicit block or ban covers this actor and resource.
    pub blocked: bool,
    /// The caller's live org membership, role or delegation grants the requested org action
    /// (SDS §6.4), as resolved from server state. False by default: no grant.
    pub org_permitted: bool,
    /// The org or membership the action depends on is not live (suspended org, suspended or
    /// removed member, deleted agent).
    pub org_inactive: bool,
    /// The caller holds a live `read_inbox` delegation for this agent mailbox (SDS §6.5).
    pub delegated: bool,
    /// The device grant or code was verified (hash, expiry, single use, key proof) by the caller.
    pub device_proof: bool,
    /// The org API key holds the scope the operation needs (SDS §18.2).
    pub scope_granted: bool,
    /// The group's rules let this mailbox take this stream action (SDS §14A): an active member to
    /// watch; an active non-restricted member allowed by `stream_policy` to go live; the invited
    /// member to accept the stage; the streamer or a group admin to moderate.
    pub stream_permitted: bool,
    /// The person's authority over the agent's shell for this action (SDS §11.5): ownership, the
    /// org owner role or a live `ssh` delegation, with the org's `ssh_allowed` and the agent's own
    /// switch — or, for approvals, the org role that may approve.
    pub shell_permitted: bool,
    /// The acting person's live staff roles, from `staff_roles`. Empty for everyone else.
    pub staff_roles: Vec<StaffRole>,
    /// The acting staff member has a stake in what they are acting on (their own publisher or
    /// release), as resolved by `store::marketplace`. Refuses regardless of role.
    pub staff_conflict: bool,
    /// The acting person is the configured platform owner.
    pub platform_owner: bool,
    /// The listing's status and visibility let this caller see it.
    pub listing_visible: bool,
    /// The caller owns this publisher profile, or holds the org role that manages it.
    pub publisher_permitted: bool,
    /// The caller owns this installed Nest or holds the org role or delegation for the action.
    pub nest_permitted: bool,
    /// The job passed admission and any payment it needs is valid for it.
    pub job_admitted: bool,
    /// The caller's account submitted this job.
    pub job_visible: bool,
}

/// Why a request was refused. Stable; mapped to an HTTP status and error code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DenyReason {
    /// The credential kind cannot perform this action at all.
    CredentialNotAccepted,
    /// A human-only gate (see [`AuthDenial`]).
    Auth(AuthDenial),
    /// A mandatory-deny hook fired.
    Mandatory(&'static str),
    /// The action is for staff holding a role this person does not hold.
    StaffRequired,
    /// The actor holds no grant on this resource.
    NotPermitted,
}

impl DenyReason {
    pub(crate) fn code(&self) -> &'static str {
        match self {
            DenyReason::CredentialNotAccepted => "credential_not_accepted",
            DenyReason::StaffRequired => "staff_required",
            DenyReason::Auth(denial) => denial.code(),
            DenyReason::Mandatory(code) => code,
            DenyReason::NotPermitted => "forbidden",
        }
    }
}

impl IntoResponse for DenyReason {
    fn into_response(self) -> Response {
        match self {
            DenyReason::Auth(denial) => denial.into_response(),
            other => ApiError::new(
                StatusCode::FORBIDDEN,
                other.code(),
                "this credential may not perform this action",
            )
            .into_response(),
        }
    }
}

/// A mandatory-deny hook: `Some(code)` refuses regardless of any grant.
pub(crate) type MandatoryDeny = fn(&Actor, Action, &Resource, &Facts) -> Option<&'static str>;

fn deny_revoked(_: &Actor, _: Action, _: &Resource, f: &Facts) -> Option<&'static str> {
    f.credential_revoked.then_some("credential_revoked")
}

fn deny_suspended(actor: &Actor, _: Action, _: &Resource, f: &Facts) -> Option<&'static str> {
    // A suspended account cannot act; services acting on the platform are not the account.
    (f.account_suspended && matches!(actor, Actor::Principal(_))).then_some("account_suspended")
}

fn deny_blocked(_: &Actor, _: Action, _: &Resource, f: &Facts) -> Option<&'static str> {
    f.blocked.then_some("blocked")
}

fn deny_org_inactive(_: &Actor, _: Action, _: &Resource, f: &Facts) -> Option<&'static str> {
    f.org_inactive.then_some("org_inactive")
}

fn deny_staff_conflict(_: &Actor, _: Action, _: &Resource, f: &Facts) -> Option<&'static str> {
    f.staff_conflict.then_some("staff_conflict")
}

/// The hooks every decision runs.
pub(crate) const MANDATORY_DENIES: &[MandatoryDeny] = &[
    deny_revoked,
    deny_suspended,
    deny_blocked,
    deny_org_inactive,
    deny_staff_conflict,
];

/// Decide with the standard mandatory hooks.
pub(crate) fn authorize(
    actor: &Actor,
    action: Action,
    resource: &Resource,
    facts: &Facts,
) -> Result<(), DenyReason> {
    authorize_with(MANDATORY_DENIES, actor, action, resource, facts)
}

/// Decide with an explicit hook list (tests add their own).
pub(crate) fn authorize_with(
    hooks: &[MandatoryDeny],
    actor: &Actor,
    action: Action,
    resource: &Resource,
    facts: &Facts,
) -> Result<(), DenyReason> {
    // 1. Credential kind.
    match (action.requirement(), actor) {
        (Requirement::OrgKey, Actor::OrgKey { .. }) => {}
        // Re-authenticating cannot turn an org API key into a person (SDS §3.4).
        (
            Requirement::Human
            | Requirement::HumanStepUp
            | Requirement::Staff
            | Requirement::StaffRead
            | Requirement::StaffView,
            Actor::OrgKey { .. },
        ) => return Err(DenyReason::Auth(AuthDenial::HumanRequired)),
        (_, Actor::OrgKey { .. }) | (Requirement::OrgKey, _) => {
            return Err(DenyReason::CredentialNotAccepted)
        }
        (Requirement::Service, Actor::Service(_)) => {}
        (Requirement::Anyone, Actor::Principal(_) | Actor::Anonymous) => {}
        (Requirement::Service, Actor::Principal(_) | Actor::Anonymous)
        | (_, Actor::Service(_))
        | (_, Actor::Anonymous) => return Err(DenyReason::CredentialNotAccepted),
        (Requirement::AnyPrincipal, Actor::Principal(_)) => {}
        (Requirement::NotConnector, Actor::Principal(p)) => {
            if matches!(
                p,
                Principal::Account(_, AccountCredential::Connector { .. })
            ) {
                return Err(DenyReason::CredentialNotAccepted);
            }
        }
        (Requirement::Mailbox, Actor::Principal(p)) => {
            if !matches!(p, Principal::Identity(_)) {
                return Err(DenyReason::CredentialNotAccepted);
            }
        }
        (Requirement::AccountHolder, Actor::Principal(p)) => match p {
            Principal::Account(_, AccountCredential::ApiKey | AccountCredential::Member(_)) => {}
            _ => return Err(DenyReason::CredentialNotAccepted),
        },
        (Requirement::Human, Actor::Principal(p)) => {
            require_human(p).map_err(DenyReason::Auth)?;
        }
        (Requirement::HumanStepUp, Actor::Principal(p)) => {
            require_human(p).map_err(DenyReason::Auth)?;
            require_step_up(
                p,
                facts.step_up.max_age,
                &facts.step_up.acr_values,
                facts.now,
            )
            .map_err(DenyReason::Auth)?;
        }
        (
            requirement @ (Requirement::Staff | Requirement::StaffRead | Requirement::StaffView),
            Actor::Principal(p),
        ) => {
            require_human(p).map_err(DenyReason::Auth)?;
            // The role before step-up: re-authenticating cannot make someone staff, so a person
            // without the role is told so rather than sent through a sign-in that cannot help.
            if !action
                .staff_roles()
                .iter()
                .any(|role| facts.staff_roles.contains(role))
            {
                return Err(DenyReason::StaffRequired);
            }
            let max_age = match requirement {
                Requirement::StaffRead => facts.step_up.max_age.max(STAFF_READ_STEP_UP_SECS),
                _ => facts.step_up.max_age,
            };
            if requirement != Requirement::StaffView {
                require_step_up(p, max_age, &facts.step_up.acr_values, facts.now)
                    .map_err(DenyReason::Auth)?;
            }
        }
    }

    // 2. Mandatory denies.
    for hook in hooks {
        if let Some(code) = hook(actor, action, resource, facts) {
            return Err(DenyReason::Mandatory(code));
        }
    }

    // 3. An action-specific grant on this resource; 4. otherwise deny.
    let granted = match (actor, resource) {
        // These two are bound to their own resources. The general arms below grant on ownership,
        // and owning an account must never reach a staff action or the staff table.
        _ if !action.staff_roles().is_empty() => matches!(resource, Resource::StaffSubject(_)),
        _ if action == Action::StaffRolesManage => {
            matches!(resource, Resource::StaffRoles) && facts.platform_owner
        }
        (Actor::Service(_), Resource::Platform) => true,
        // The one public platform action: the invitation code was checked by the caller.
        (Actor::Principal(_) | Actor::Anonymous, Resource::Platform) => {
            action == Action::SmsOptOut && facts.device_proof
        }
        (Actor::Principal(p), Resource::Account(id)) => match p {
            Principal::Account(account, _) => account == id,
            Principal::Identity(identity) => identity.account_id.as_deref() == Some(*id),
        },
        (Actor::Principal(p), Resource::Mailbox { address, account }) => {
            // An operated agent mailbox (a live `read_inbox` delegation) is reachable by the
            // delegate's own account credential, never by a token bound to another mailbox.
            owns(p, address, *account) || (facts.delegated && matches!(p, Principal::Account(_, _)))
        }
        (Actor::Principal(_), Resource::Org(_)) => facts.org_permitted,
        // A stream action needs both: the credential holds the acting mailbox, and the group's
        // rules (resolved in the same transaction) allow that mailbox this action.
        (Actor::Principal(p), Resource::GroupStream { mailbox, account }) => {
            matches!(
                action,
                Action::StreamWatch | Action::StreamPublish | Action::StreamModerate
            ) && owns(p, mailbox, *account)
                && facts.stream_permitted
        }
        // A key acts on its own org only, within its scope and its creator's live authority.
        (Actor::OrgKey { org_id, .. }, Resource::Org(id)) => {
            org_id == id && facts.org_permitted && facts.scope_granted
        }
        // The agent's own side: only that agent's mailbox credential.
        (Actor::Principal(Principal::Identity(identity)), Resource::AgentShell { agent })
            if action == Action::ShellAgent =>
        {
            identity.address == *agent
        }
        // A person's side: the grant resolved from server state, for the person actions only.
        (Actor::Principal(Principal::Account(_, _)), Resource::AgentShell { .. }) => {
            matches!(
                action,
                Action::ShellOpen | Action::ShellApprove | Action::ShellView
            ) && facts.shell_permitted
        }
        (_, Resource::DeviceGrant(_)) => {
            matches!(action, Action::DeviceEnroll) && facts.device_proof
        }
        (
            actor,
            Resource::Avatar {
                visibility,
                owner_account,
                owner_address,
                viewer_known,
            },
        ) => {
            action == Action::ViewAvatar
                && visible_to(
                    actor,
                    *visibility,
                    *owner_account,
                    owner_address,
                    *viewer_known,
                )
        }
        (
            actor,
            Resource::Profile {
                visibility,
                owner_account,
                owner_address,
                viewer_known,
            },
        ) => {
            action == Action::ViewProfile
                && visible_to(
                    actor,
                    *visibility,
                    *owner_account,
                    owner_address,
                    *viewer_known,
                )
        }
        // Discovery is the one marketplace action open without a credential.
        (Actor::Principal(_) | Actor::Anonymous, Resource::NestListing(_)) => {
            action == Action::NestDiscover && facts.listing_visible
        }
        (Actor::Principal(_), Resource::NestOperation(_)) => {
            action == Action::NestInvoke && facts.job_admitted
        }
        (Actor::Principal(_), Resource::NestJob(_)) => {
            action == Action::NestJobRead && facts.job_visible
        }
        (Actor::Principal(_), Resource::InstalledNest(_)) => {
            matches!(
                action,
                Action::NestOperate | Action::NestConfigure | Action::NestManage
            ) && facts.nest_permitted
        }
        (Actor::Principal(_), Resource::Publisher(_)) => {
            matches!(action, Action::PublisherManage | Action::NestPublish)
                && facts.publisher_permitted
        }
        _ => false,
    };
    if granted {
        Ok(())
    } else {
        if let Actor::OrgKey { key_id, .. } = actor {
            tracing::debug!(key_id, ?action, "org API key denied");
        }
        if let Actor::Service(service) = actor {
            tracing::warn!(
                service,
                ?action,
                "service denied on a non-platform resource"
            );
        }
        Err(DenyReason::NotPermitted)
    }
}

/// The picture/name visibility rule (SDS §16.1): everyone; people the owner knows; only the owner.
fn visible_to(
    actor: &Actor,
    visibility: crate::store::Visibility,
    owner_account: Option<&str>,
    owner_address: &str,
    viewer_known: bool,
) -> bool {
    let own = match actor {
        Actor::Principal(p) => owns(p, owner_address, owner_account),
        _ => false,
    };
    match visibility {
        crate::store::Visibility::Everyone => true,
        // Being known needs someone to know: never the anonymous.
        crate::store::Visibility::Known => {
            own || (viewer_known && matches!(actor, Actor::Principal(_)))
        }
        crate::store::Visibility::Nobody => own,
    }
}

/// Whether a principal holds a mailbox: its own capability token, or the account it is bound to.
fn owns(p: &Principal, address: &str, account: Option<&str>) -> bool {
    match p {
        Principal::Identity(identity) => identity.address == address,
        Principal::Account(id, _) => account == Some(id.as_str()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::principal::{member_principal, MemberSession};

    const NOW: u64 = 1_800_000_000;

    fn facts() -> Facts {
        Facts {
            now: NOW,
            step_up: StepUpPolicy {
                acr_values: vec!["gold".into()],
                max_age: 600,
            },
            ..Facts::default()
        }
    }

    fn human(account: &str, acr: Option<&str>) -> Principal {
        member_principal(
            account,
            MemberSession {
                auth_time: Some(NOW - 10),
                acr: acr.map(str::to_string),
                ..MemberSession::default()
            },
        )
    }

    fn human_at(account: &str, acr: &str, auth_time: u64) -> Principal {
        member_principal(
            account,
            MemberSession {
                auth_time: Some(auth_time),
                acr: Some(acr.to_string()),
                ..MemberSession::default()
            },
        )
    }

    fn mailbox(account: Option<&str>) -> Principal {
        let mut identity = crate::store::StoredIdentity::test_fixture("/k/agent");
        identity.account_id = account.map(str::to_string);
        Principal::Identity(identity)
    }

    #[test]
    fn credential_type_is_checked_first() {
        let key = Principal::Account("a1".into(), AccountCredential::ApiKey);
        let connector = Principal::Account(
            "a1".into(),
            AccountCredential::Connector { client: "c".into() },
        );
        let mb = mailbox(Some("a1"));
        let me = human("a1", None);
        let own = Resource::Account("a1");

        // Account holders may stream; a mailbox token or connector may not, even for its own
        // account.
        for ok in [&key, &me] {
            assert!(authorize(
                &Actor::Principal(ok),
                Action::StreamAccountEvents,
                &own,
                &facts()
            )
            .is_ok());
        }
        for no in [&mb, &connector] {
            assert_eq!(
                authorize(
                    &Actor::Principal(no),
                    Action::StreamAccountEvents,
                    &own,
                    &facts()
                ),
                Err(DenyReason::CredentialNotAccepted)
            );
        }
        // Any principal reads capabilities for its own account.
        for any in [&key, &connector, &mb, &me] {
            assert!(authorize(
                &Actor::Principal(any),
                Action::ReadCapabilities,
                &own,
                &facts()
            )
            .is_ok());
        }
        // Members never reach service actions; services never reach member actions.
        assert_eq!(
            authorize(
                &Actor::Principal(&me),
                Action::WriteFeatureFlags,
                &Resource::Platform,
                &facts()
            ),
            Err(DenyReason::CredentialNotAccepted)
        );
        assert_eq!(
            authorize(
                &Actor::Service("ops"),
                Action::ReadCapabilities,
                &own,
                &facts()
            ),
            Err(DenyReason::CredentialNotAccepted)
        );
        assert!(authorize(
            &Actor::Service("ops"),
            Action::WriteFeatureFlags,
            &Resource::Platform,
            &facts()
        )
        .is_ok());
    }

    #[test]
    fn ownership_is_required() {
        let key = Principal::Account("a1".into(), AccountCredential::ApiKey);
        assert_eq!(
            authorize(
                &Actor::Principal(&key),
                Action::StreamAccountEvents,
                &Resource::Account("a2"),
                &facts()
            ),
            Err(DenyReason::NotPermitted)
        );
        // A mailbox's account binding comes from the stored identity, not the request.
        let orphan = mailbox(None);
        assert_eq!(
            authorize(
                &Actor::Principal(&orphan),
                Action::ReadCapabilities,
                &Resource::Account("a1"),
                &facts()
            ),
            Err(DenyReason::NotPermitted)
        );
        // A principal never acts on the platform, even for an action it may otherwise take.
        assert_eq!(
            authorize(
                &Actor::Principal(&key),
                Action::ReadCapabilities,
                &Resource::Platform,
                &facts()
            ),
            Err(DenyReason::NotPermitted)
        );
    }

    #[test]
    fn mandatory_denies_beat_any_grant() {
        let me = human("a1", Some("gold"));
        let own = Resource::Account("a1");
        let actor = Actor::Principal(&me);
        for (f, code) in [
            (
                Facts {
                    credential_revoked: true,
                    ..facts()
                },
                "credential_revoked",
            ),
            (
                Facts {
                    account_suspended: true,
                    ..facts()
                },
                "account_suspended",
            ),
            (
                Facts {
                    blocked: true,
                    ..facts()
                },
                "blocked",
            ),
        ] {
            assert_eq!(
                authorize(&actor, Action::ReadCapabilities, &own, &f),
                Err(DenyReason::Mandatory(code))
            );
        }
        // A custom hook list is honoured too.
        fn always(_: &Actor, _: Action, _: &Resource, _: &Facts) -> Option<&'static str> {
            Some("test_hook")
        }
        assert_eq!(
            authorize_with(&[always], &actor, Action::ReadCapabilities, &own, &facts()),
            Err(DenyReason::Mandatory("test_hook"))
        );
    }

    #[test]
    fn mailbox_actions_need_the_mailbox() {
        let key = Principal::Account("a1".into(), AccountCredential::ApiKey);
        let connector = Principal::Account(
            "a1".into(),
            AccountCredential::Connector { client: "c".into() },
        );
        let mut own_mb = crate::store::StoredIdentity::test_fixture("/k/mine");
        own_mb.account_id = Some("a1".into());
        let mb = Principal::Identity(own_mb);
        let mine = Resource::Mailbox {
            address: "/k/mine",
            account: Some("a1"),
        };
        let theirs = Resource::Mailbox {
            address: "/k/theirs",
            account: Some("a2"),
        };
        for p in [&key, &mb, &connector] {
            assert!(authorize(
                &Actor::Principal(p),
                Action::ManageOwnMailbox,
                &mine,
                &facts()
            )
            .is_ok());
            assert_eq!(
                authorize(
                    &Actor::Principal(p),
                    Action::ManageOwnMailbox,
                    &theirs,
                    &facts()
                ),
                Err(DenyReason::NotPermitted)
            );
        }
        // A mailbox token holds only its own mailbox, even inside its account.
        let sibling = Resource::Mailbox {
            address: "/k/sibling",
            account: Some("a1"),
        };
        assert_eq!(
            authorize(
                &Actor::Principal(&mb),
                Action::ManageOwnMailbox,
                &sibling,
                &facts()
            ),
            Err(DenyReason::NotPermitted)
        );
        // Raising trust is not for a connector.
        assert_eq!(
            authorize(
                &Actor::Principal(&connector),
                Action::RaiseTrust,
                &mine,
                &facts()
            ),
            Err(DenyReason::CredentialNotAccepted)
        );
        assert!(authorize(&Actor::Principal(&key), Action::RaiseTrust, &mine, &facts()).is_ok());
        // Services and the anonymous never reach member actions.
        assert_eq!(
            authorize(&Actor::Anonymous, Action::ManageOwnMailbox, &mine, &facts()),
            Err(DenyReason::CredentialNotAccepted)
        );
        assert_eq!(
            authorize(
                &Actor::Service("calls"),
                Action::ManageOwnMailbox,
                &mine,
                &facts()
            ),
            Err(DenyReason::CredentialNotAccepted)
        );
        assert!(authorize(
            &Actor::Service("calls"),
            Action::CallAdmission,
            &Resource::Platform,
            &facts()
        )
        .is_ok());
        assert_eq!(
            authorize(
                &Actor::Principal(&key),
                Action::CallEvents,
                &Resource::Platform,
                &facts()
            ),
            Err(DenyReason::CredentialNotAccepted)
        );
    }

    #[test]
    fn avatar_visibility() {
        use crate::store::Visibility;
        let owner = Principal::Account("a1".into(), AccountCredential::ApiKey);
        let other = Principal::Account("a2".into(), AccountCredential::ApiKey);
        let avatar = |visibility, viewer_known| Resource::Avatar {
            visibility,
            owner_account: Some("a1"),
            owner_address: "/k/o",
            viewer_known,
        };
        let can =
            |actor: &Actor, r: &Resource| authorize(actor, Action::ViewAvatar, r, &facts()).is_ok();
        // Everyone: even without a credential.
        assert!(can(&Actor::Anonymous, &avatar(Visibility::Everyone, false)));
        // Known: the owner, or a viewer the owner knows; never the anonymous.
        assert!(can(
            &Actor::Principal(&owner),
            &avatar(Visibility::Known, false)
        ));
        assert!(can(
            &Actor::Principal(&other),
            &avatar(Visibility::Known, true)
        ));
        assert!(!can(
            &Actor::Principal(&other),
            &avatar(Visibility::Known, false)
        ));
        assert!(!can(&Actor::Anonymous, &avatar(Visibility::Known, true)));
        // Nobody: only the owner.
        assert!(can(
            &Actor::Principal(&owner),
            &avatar(Visibility::Nobody, false)
        ));
        assert!(!can(
            &Actor::Principal(&other),
            &avatar(Visibility::Nobody, true)
        ));
    }

    /// SDS §14D: phone, discovery and invites are for a person only; display names for the
    /// mailbox's own credentials except a connector; a name is seen like a picture, never by the
    /// anonymous.
    #[test]
    fn contacts_phone_and_names_matrix() {
        use crate::store::Visibility;
        let me = human("a1", None);
        let key = Principal::Account("a1".into(), AccountCredential::ApiKey);
        let connector = Principal::Account(
            "a1".into(),
            AccountCredential::Connector { client: "c".into() },
        );
        let mb = mailbox(Some("a1"));
        let service_account = member_principal(
            "a1",
            MemberSession {
                service_account: true,
                ..MemberSession::default()
            },
        );
        let own = Resource::Account("a1");
        let own_mailbox = Resource::Mailbox {
            address: "/k/agent",
            account: Some("a1"),
        };
        for action in [
            Action::ManagePhone,
            Action::DiscoverContacts,
            Action::SendSmsInvite,
        ] {
            assert!(authorize(&Actor::Principal(&me), action, &own, &facts()).is_ok());
            assert!(authorize(&Actor::Principal(&me), action, &own_mailbox, &facts()).is_ok());
            for no in [&key, &connector, &mb, &service_account] {
                assert!(
                    authorize(&Actor::Principal(no), action, &own, &facts()).is_err(),
                    "{action:?} must refuse automation"
                );
            }
            assert!(authorize(&Actor::Anonymous, action, &own, &facts()).is_err());
            assert!(authorize(&Actor::Service("ops"), action, &own, &facts()).is_err());
            // Someone else's account or mailbox: no grant.
            assert_eq!(
                authorize(
                    &Actor::Principal(&me),
                    action,
                    &Resource::Account("a2"),
                    &facts()
                ),
                Err(DenyReason::NotPermitted)
            );
            // A block or suspension still wins.
            let mut f = facts();
            f.account_suspended = true;
            assert!(authorize(&Actor::Principal(&me), action, &own, &f).is_err());
        }
        // Display names: the mailbox's own credentials, not a connector, not someone else's.
        for ok in [&me, &key, &mb] {
            assert!(authorize(
                &Actor::Principal(ok),
                Action::SetDisplayName,
                &own_mailbox,
                &facts()
            )
            .is_ok());
        }
        assert_eq!(
            authorize(
                &Actor::Principal(&connector),
                Action::SetDisplayName,
                &own_mailbox,
                &facts()
            ),
            Err(DenyReason::CredentialNotAccepted)
        );
        assert_eq!(
            authorize(
                &Actor::Principal(&me),
                Action::SetDisplayName,
                &Resource::Mailbox {
                    address: "/k/else",
                    account: Some("a2")
                },
                &facts()
            ),
            Err(DenyReason::NotPermitted)
        );
        // Seeing a name: visibility like a picture, and never anonymous even when public.
        let other = Principal::Account("a2".into(), AccountCredential::ApiKey);
        let name = |visibility, viewer_known| Resource::Profile {
            visibility,
            owner_account: Some("a1"),
            owner_address: "/k/agent",
            viewer_known,
        };
        let can = |actor: &Actor, r: &Resource| {
            authorize(actor, Action::ViewProfile, r, &facts()).is_ok()
        };
        assert!(!can(&Actor::Anonymous, &name(Visibility::Everyone, true)));
        assert!(can(
            &Actor::Principal(&other),
            &name(Visibility::Everyone, false)
        ));
        assert!(can(
            &Actor::Principal(&other),
            &name(Visibility::Known, true)
        ));
        assert!(!can(
            &Actor::Principal(&other),
            &name(Visibility::Known, false)
        ));
        assert!(!can(
            &Actor::Principal(&other),
            &name(Visibility::Nobody, true)
        ));
        assert!(can(
            &Actor::Principal(&key),
            &name(Visibility::Nobody, false)
        ));
        // Opting a number out needs the texted code (checked by the caller), nothing else.
        let mut proof = facts();
        proof.device_proof = true;
        assert!(authorize(
            &Actor::Anonymous,
            Action::SmsOptOut,
            &Resource::Platform,
            &proof
        )
        .is_ok());
        assert!(authorize(
            &Actor::Anonymous,
            Action::SmsOptOut,
            &Resource::Platform,
            &facts()
        )
        .is_err());
        assert!(authorize(
            &Actor::Principal(&me),
            Action::ManagePhone,
            &Resource::Platform,
            &proof
        )
        .is_err());
        // The two visibility resources do not stand in for each other's actions.
        assert!(authorize(
            &Actor::Principal(&other),
            Action::ViewAvatar,
            &name(Visibility::Everyone, true),
            &facts()
        )
        .is_err());
    }

    #[test]
    fn sensitive_actions_need_step_up_and_never_accept_automation() {
        let own = Resource::Account("a1");
        let strong = human("a1", Some("gold"));
        let weak = human("a1", Some("1"));
        let key = Principal::Account("a1".into(), AccountCredential::ApiKey);
        assert!(authorize(
            &Actor::Principal(&strong),
            Action::SensitiveAccountChange,
            &own,
            &facts()
        )
        .is_ok());
        assert_eq!(
            authorize(
                &Actor::Principal(&weak),
                Action::SensitiveAccountChange,
                &own,
                &facts()
            )
            .unwrap_err()
            .code(),
            "step_up_required"
        );
        assert_eq!(
            authorize(
                &Actor::Principal(&key),
                Action::SensitiveAccountChange,
                &own,
                &facts()
            )
            .unwrap_err()
            .code(),
            "human_required"
        );
        // Unconfigured step-up refuses even a strong session.
        let unconfigured = Facts {
            step_up: StepUpPolicy::default(),
            ..facts()
        };
        assert_eq!(
            authorize(
                &Actor::Principal(&strong),
                Action::SensitiveAccountChange,
                &own,
                &unconfigured
            )
            .unwrap_err()
            .code(),
            "step_up_unavailable"
        );
        // And step-up does not bypass ownership.
        assert_eq!(
            authorize(
                &Actor::Principal(&strong),
                Action::SensitiveAccountChange,
                &Resource::Account("a2"),
                &facts()
            ),
            Err(DenyReason::NotPermitted)
        );
    }

    /// An org API key (SDS §18.2) reaches only `OrgApi`, only on its own org, and only when both
    /// its scope and its creator's live authority allow — never a human or step-up action.
    #[test]
    fn org_keys_are_bounded_by_scope_creator_and_org() {
        let key = Actor::OrgKey {
            key_id: "okey_1",
            org_id: "org_a",
        };
        let both = Facts {
            org_permitted: true,
            scope_granted: true,
            ..facts()
        };
        assert!(authorize(&key, Action::OrgApi, &Resource::Org("org_a"), &both).is_ok());
        // Another org, even with every grant fact set.
        assert_eq!(
            authorize(&key, Action::OrgApi, &Resource::Org("org_b"), &both),
            Err(DenyReason::NotPermitted)
        );
        // Scope without the creator's authority, and authority without the scope.
        for f in [
            Facts {
                org_permitted: true,
                ..facts()
            },
            Facts {
                scope_granted: true,
                ..facts()
            },
        ] {
            assert_eq!(
                authorize(&key, Action::OrgApi, &Resource::Org("org_a"), &f),
                Err(DenyReason::NotPermitted)
            );
        }
        // Mandatory denies still win.
        for f in [
            Facts {
                credential_revoked: true,
                ..both.clone()
            },
            Facts {
                org_inactive: true,
                ..both.clone()
            },
        ] {
            assert!(matches!(
                authorize(&key, Action::OrgApi, &Resource::Org("org_a"), &f),
                Err(DenyReason::Mandatory(_))
            ));
        }
        // Human and step-up actions: `human_required`, however many facts are set.
        for action in [
            Action::OrgAdminister,
            Action::OrgSensitive,
            Action::GroupManage,
            Action::SensitiveAccountChange,
        ] {
            assert_eq!(
                authorize(&key, action, &Resource::Org("org_a"), &both),
                Err(DenyReason::Auth(AuthDenial::HumanRequired))
            );
        }
        // Everything else (mailbox, service, events, device enrollment) is not for a key.
        for action in [
            Action::ReadCapabilities,
            Action::StreamAccountEvents,
            Action::ManageOwnMailbox,
            Action::GroupParticipate,
            Action::DeviceEnroll,
            Action::ViewAvatar,
            Action::WriteFeatureFlags,
        ] {
            assert_eq!(
                authorize(&key, action, &Resource::Org("org_a"), &both),
                Err(DenyReason::CredentialNotAccepted)
            );
        }
        // And no other credential reaches `OrgApi`: not a person, an account key or a service.
        let me = human("a1", Some("gold"));
        let account_key = Principal::Account("a1".into(), AccountCredential::ApiKey);
        for actor in [
            Actor::Principal(&me),
            Actor::Principal(&account_key),
            Actor::Service("ops"),
            Actor::Anonymous,
        ] {
            assert_eq!(
                authorize(&actor, Action::OrgApi, &Resource::Org("org_a"), &both),
                Err(DenyReason::CredentialNotAccepted)
            );
        }
    }

    #[test]
    fn stream_actions_need_the_mailbox_the_group_rules_and_the_right_credential() {
        let me = human("a1", None);
        let key = Principal::Account("a1".into(), AccountCredential::ApiKey);
        let connector = Principal::Account(
            "a1".into(),
            AccountCredential::Connector { client: "c".into() },
        );
        let other = human("a2", None);
        let own = Resource::GroupStream {
            mailbox: "/k/me",
            account: Some("a1"),
        };
        let granted = Facts {
            stream_permitted: true,
            ..facts()
        };
        // The group rules alone are not enough, and neither is owning the mailbox.
        for action in [
            Action::StreamWatch,
            Action::StreamPublish,
            Action::StreamModerate,
        ] {
            assert!(authorize(&Actor::Principal(&me), action, &own, &granted).is_ok());
            assert_eq!(
                authorize(&Actor::Principal(&me), action, &own, &facts()),
                Err(DenyReason::NotPermitted)
            );
            assert_eq!(
                authorize(&Actor::Principal(&other), action, &own, &granted),
                Err(DenyReason::NotPermitted)
            );
        }
        // Agents (API keys) may watch, never go live or moderate; a connector may do neither.
        assert!(authorize(&Actor::Principal(&key), Action::StreamWatch, &own, &granted).is_ok());
        for action in [Action::StreamPublish, Action::StreamModerate] {
            assert!(matches!(
                authorize(&Actor::Principal(&key), action, &own, &granted),
                Err(DenyReason::Auth(_))
            ));
        }
        assert_eq!(
            authorize(
                &Actor::Principal(&connector),
                Action::StreamWatch,
                &own,
                &granted
            ),
            Err(DenyReason::CredentialNotAccepted)
        );
        // A block still wins over every grant.
        let blocked = Facts {
            blocked: true,
            ..granted.clone()
        };
        assert_eq!(
            authorize(&Actor::Principal(&me), Action::StreamWatch, &own, &blocked),
            Err(DenyReason::Mandatory("blocked"))
        );
        // Only services reach the calls service's stream endpoints; never against a mailbox.
        assert!(authorize(
            &Actor::Service("calls"),
            Action::StreamAdmission,
            &Resource::Platform,
            &facts()
        )
        .is_ok());
        assert_eq!(
            authorize(
                &Actor::Principal(&me),
                Action::StreamAdmission,
                &own,
                &granted
            ),
            Err(DenyReason::CredentialNotAccepted)
        );
        // A grant on the resource is for stream actions only.
        assert_eq!(
            authorize(
                &Actor::Principal(&me),
                Action::GroupParticipate,
                &own,
                &granted
            ),
            Err(DenyReason::NotPermitted)
        );
    }

    /// SDS §11.5: a shell is for a person with step-up and a resolved grant; the agent's own side
    /// is its mailbox credential only; nothing else reaches either.
    #[test]
    fn shell_actions_need_a_strong_person_or_the_agents_own_mailbox() {
        let strong = human("a1", Some("gold"));
        let weak = human("a1", None);
        let key = Principal::Account("a1".into(), AccountCredential::ApiKey);
        let connector = Principal::Account(
            "a1".into(),
            AccountCredential::Connector { client: "c".into() },
        );
        let mut own = crate::store::StoredIdentity::test_fixture("/k/agent");
        own.account_id = Some("a1".into());
        let agent_mb = Principal::Identity(own);
        let mut other = crate::store::StoredIdentity::test_fixture("/k/other");
        other.account_id = Some("a1".into());
        let other_mb = Principal::Identity(other);
        let shell = Resource::AgentShell { agent: "/k/agent" };
        let granted = Facts {
            shell_permitted: true,
            ..facts()
        };
        assert!(authorize(
            &Actor::Principal(&strong),
            Action::ShellOpen,
            &shell,
            &granted
        )
        .is_ok());
        // No grant, no shell, however strong the sign-in.
        assert_eq!(
            authorize(
                &Actor::Principal(&strong),
                Action::ShellOpen,
                &shell,
                &facts()
            ),
            Err(DenyReason::NotPermitted)
        );
        assert_eq!(
            authorize(
                &Actor::Principal(&weak),
                Action::ShellOpen,
                &shell,
                &granted
            )
            .unwrap_err()
            .code(),
            "step_up_required"
        );
        for p in [&key, &connector, &agent_mb, &other_mb] {
            for action in [Action::ShellOpen, Action::ShellApprove, Action::ShellView] {
                assert_eq!(
                    authorize(&Actor::Principal(p), action, &shell, &granted)
                        .unwrap_err()
                        .code(),
                    "human_required"
                );
            }
        }
        let org_key = Actor::OrgKey {
            key_id: "k",
            org_id: "o",
        };
        assert_eq!(
            authorize(&org_key, Action::ShellOpen, &shell, &granted)
                .unwrap_err()
                .code(),
            "human_required"
        );
        // A mandatory deny still wins.
        let revoked = Facts {
            credential_revoked: true,
            ..granted.clone()
        };
        assert!(authorize(
            &Actor::Principal(&strong),
            Action::ShellOpen,
            &shell,
            &revoked
        )
        .is_err());
        // The agent's side: exactly its own mailbox credential.
        assert!(authorize(
            &Actor::Principal(&agent_mb),
            Action::ShellAgent,
            &shell,
            &facts()
        )
        .is_ok());
        assert_eq!(
            authorize(
                &Actor::Principal(&other_mb),
                Action::ShellAgent,
                &shell,
                &facts()
            ),
            Err(DenyReason::NotPermitted)
        );
        for p in [&strong, &key, &connector] {
            assert_eq!(
                authorize(&Actor::Principal(p), Action::ShellAgent, &shell, &granted),
                Err(DenyReason::CredentialNotAccepted)
            );
        }
        // The broker is a service on the platform, and only that.
        assert!(authorize(
            &Actor::Service("tunnel"),
            Action::ShellBroker,
            &Resource::Platform,
            &facts()
        )
        .is_ok());
        assert_eq!(
            authorize(
                &Actor::Principal(&strong),
                Action::ShellBroker,
                &Resource::Platform,
                &granted
            ),
            Err(DenyReason::CredentialNotAccepted)
        );
        assert_eq!(
            authorize(
                &Actor::Service("tunnel"),
                Action::ShellOpen,
                &shell,
                &granted
            ),
            Err(DenyReason::CredentialNotAccepted)
        );
    }

    // ---- marketplace foundations (build plan §4, M0) -------------------------------------------

    /// The staff actions that need the write step-up window.
    const STAFF_ACTIONS: [Action; 7] = [
        Action::NestReview,
        Action::NestAttest,
        Action::NestQuarantine,
        Action::PayoutPrepare,
        Action::PayoutApprove,
        Action::StaffFinanceWrite,
        Action::MarketplaceOps,
    ];
    /// The back office reads of personal data (an hour-old step-up suffices).
    const STAFF_READ_ACTIONS: [Action; 4] = [
        Action::StaffPeopleRead,
        Action::StaffPlansRead,
        Action::StaffFinanceRead,
        Action::StaffAuditRead,
    ];

    fn connector(account: &str) -> Principal {
        Principal::Account(
            account.into(),
            AccountCredential::Connector { client: "c".into() },
        )
    }

    fn api_key(account: &str) -> Principal {
        Principal::Account(account.into(), AccountCredential::ApiKey)
    }

    fn staff(roles: &[StaffRole]) -> Facts {
        Facts {
            staff_roles: roles.to_vec(),
            ..facts()
        }
    }

    fn role_for(action: Action) -> StaffRole {
        *action.staff_roles().first().expect("a staff action")
    }

    /// Every credential that is not a person, for a staff action: refused before any role is read.
    #[test]
    fn staff_actions_refuse_every_kind_of_automation() {
        let subject = Resource::StaffSubject("rel_1");
        let all_roles = staff(&StaffRole::ALL);
        let mb = mailbox(Some("a1"));
        let service_account = member_principal(
            "a1",
            MemberSession {
                service_account: true,
                acr: Some("gold".into()),
                auth_time: Some(NOW - 10),
                ..MemberSession::default()
            },
        );
        for action in STAFF_ACTIONS
            .into_iter()
            .chain(STAFF_READ_ACTIONS)
            .chain([Action::StaffOverview])
        {
            for p in [&api_key("a1"), &connector("a1"), &mb, &service_account] {
                assert_eq!(
                    authorize(&Actor::Principal(p), action, &subject, &all_roles),
                    Err(DenyReason::Auth(AuthDenial::HumanRequired)),
                    "{action:?}"
                );
            }
            assert_eq!(
                authorize(
                    &Actor::OrgKey {
                        key_id: "k",
                        org_id: "o"
                    },
                    action,
                    &subject,
                    &all_roles
                ),
                Err(DenyReason::Auth(AuthDenial::HumanRequired))
            );
            for actor in [Actor::Anonymous, Actor::Service("ops")] {
                assert_eq!(
                    authorize(&actor, action, &subject, &all_roles),
                    Err(DenyReason::CredentialNotAccepted)
                );
            }
        }
    }

    #[test]
    fn staff_actions_need_the_role_then_step_up_and_no_conflict() {
        let subject = Resource::StaffSubject("rel_1");
        let strong = human("a1", Some("gold"));
        let weak = human("a1", Some("1"));
        for action in STAFF_ACTIONS {
            let holder = staff(&[role_for(action)]);
            assert!(
                authorize(&Actor::Principal(&strong), action, &subject, &holder).is_ok(),
                "{action:?}"
            );
            // Not staff: told so, however strong the sign-in, and step-up is not offered.
            for p in [&strong, &weak] {
                assert_eq!(
                    authorize(&Actor::Principal(p), action, &subject, &facts()),
                    Err(DenyReason::StaffRequired),
                    "{action:?}"
                );
            }
            // Staff, without a recent strong sign-in.
            assert_eq!(
                authorize(&Actor::Principal(&weak), action, &subject, &holder)
                    .unwrap_err()
                    .code(),
                "step_up_required"
            );
            let stale = human_at("a1", "gold", NOW - 601);
            assert_eq!(
                authorize(&Actor::Principal(&stale), action, &subject, &holder)
                    .unwrap_err()
                    .code(),
                "step_up_required",
                "a strong sign-in older than the limit does not count"
            );
            // Step-up not configured on the server refuses even a strong session.
            let unconfigured = Facts {
                step_up: StepUpPolicy::default(),
                ..holder.clone()
            };
            assert_eq!(
                authorize(&Actor::Principal(&strong), action, &subject, &unconfigured)
                    .unwrap_err()
                    .code(),
                "step_up_unavailable"
            );
            // A conflict and the other mandatory denies beat the role.
            assert_eq!(
                authorize(
                    &Actor::Principal(&strong),
                    action,
                    &subject,
                    &Facts {
                        staff_conflict: true,
                        ..holder.clone()
                    }
                ),
                Err(DenyReason::Mandatory("staff_conflict"))
            );
            assert_eq!(
                authorize(
                    &Actor::Principal(&strong),
                    action,
                    &subject,
                    &Facts {
                        credential_revoked: true,
                        ..holder.clone()
                    }
                ),
                Err(DenyReason::Mandatory("credential_revoked"))
            );
        }
    }

    #[test]
    fn a_staff_role_covers_only_its_own_actions() {
        let strong = human("a1", Some("gold"));
        let subject = Resource::StaffSubject("rel_1");
        let reviewer = staff(&[StaffRole::Reviewer]);
        let finance = staff(&[StaffRole::Finance]);
        for action in [
            Action::NestReview,
            Action::NestAttest,
            Action::NestQuarantine,
        ] {
            assert!(authorize(&Actor::Principal(&strong), action, &subject, &reviewer).is_ok());
            assert_eq!(
                authorize(&Actor::Principal(&strong), action, &subject, &finance),
                Err(DenyReason::StaffRequired)
            );
        }
        for action in [Action::PayoutPrepare, Action::PayoutApprove] {
            assert!(authorize(&Actor::Principal(&strong), action, &subject, &finance).is_ok());
            assert_eq!(
                authorize(&Actor::Principal(&strong), action, &subject, &reviewer),
                Err(DenyReason::StaffRequired)
            );
        }
        // A staff action reaches only a staff subject; holding a role opens nothing else.
        let both = staff(&[StaffRole::Reviewer, StaffRole::Finance]);
        for resource in [
            Resource::Account("a1"),
            Resource::Org("o1"),
            Resource::Platform,
            Resource::Publisher("pub_1"),
            Resource::NestListing("lst_1"),
            Resource::StaffRoles,
        ] {
            assert_eq!(
                authorize(
                    &Actor::Principal(&strong),
                    Action::NestReview,
                    &resource,
                    &both
                ),
                Err(DenyReason::NotPermitted),
                "{resource:?}"
            );
        }
        // A non-staff action does not reach a staff subject, even with every grant fact set.
        let everything = Facts {
            platform_owner: true,
            publisher_permitted: true,
            nest_permitted: true,
            listing_visible: true,
            job_admitted: true,
            ..both.clone()
        };
        for action in [
            Action::PublisherManage,
            Action::NestPublish,
            Action::NestManage,
            Action::StaffRolesManage,
        ] {
            assert_eq!(
                authorize(&Actor::Principal(&strong), action, &subject, &everything),
                Err(DenyReason::NotPermitted),
                "{action:?}"
            );
        }
    }

    /// The back office role matrix: each staff action against each single role.
    #[test]
    fn back_office_actions_follow_the_role_matrix() {
        use StaffRole::*;
        let strong = human("a1", Some("gold"));
        let subject = Resource::StaffSubject("accounts");
        let matrix: &[(Action, &[StaffRole])] = &[
            (
                Action::StaffOverview,
                &[Owner, Support, Finance, Reviewer, Ops],
            ),
            (Action::StaffPeopleRead, &[Owner, Support]),
            (Action::StaffPlansRead, &[Support, Finance]),
            (Action::StaffFinanceRead, &[Finance]),
            (Action::StaffFinanceWrite, &[Finance]),
            (Action::StaffAuditRead, &[Owner]),
            (Action::MarketplaceOps, &[Reviewer, Ops]),
            (Action::NestReview, &[Reviewer]),
            (Action::NestQuarantine, &[Reviewer]),
        ];
        for (action, allowed) in matrix {
            for role in StaffRole::ALL {
                let got = authorize(
                    &Actor::Principal(&strong),
                    *action,
                    &subject,
                    &staff(&[role]),
                );
                if allowed.contains(&role) {
                    assert!(got.is_ok(), "{action:?} {role:?}: {got:?}");
                } else {
                    assert_eq!(got, Err(DenyReason::StaffRequired), "{action:?} {role:?}");
                }
            }
        }
    }

    /// Personal-data reads accept a strong sign-in up to an hour old; writes keep the configured
    /// window; the overview needs no step-up at all, but still a person with a role.
    #[test]
    fn back_office_step_up_windows() {
        let subject = Resource::StaffSubject("accounts");
        let all = staff(&StaffRole::ALL);
        let half_hour = human_at("a1", "gold", NOW - 1800);
        let over_an_hour = human_at("a1", "gold", NOW - STAFF_READ_STEP_UP_SECS - 1);
        let weak = human("a1", Some("1"));
        for action in STAFF_READ_ACTIONS {
            assert!(authorize(&Actor::Principal(&half_hour), action, &subject, &all).is_ok());
            for p in [&over_an_hour, &weak] {
                let e = authorize(&Actor::Principal(p), action, &subject, &all).unwrap_err();
                assert_eq!(e.code(), "step_up_required", "{action:?}");
                // The challenge names the read window, so the browser asks for exactly that.
                match e {
                    DenyReason::Auth(AuthDenial::StepUpRequired { max_age, .. }) => {
                        assert_eq!(max_age, STAFF_READ_STEP_UP_SECS)
                    }
                    other => panic!("{other:?}"),
                }
            }
        }
        for action in [Action::StaffFinanceWrite, Action::MarketplaceOps] {
            let e = authorize(&Actor::Principal(&half_hour), action, &subject, &all).unwrap_err();
            assert_eq!(
                e.code(),
                "step_up_required",
                "{action:?}: writes keep 10 minutes"
            );
        }
        for p in [&weak, &over_an_hour] {
            assert!(authorize(&Actor::Principal(p), Action::StaffOverview, &subject, &all).is_ok());
            assert_eq!(
                authorize(
                    &Actor::Principal(p),
                    Action::StaffOverview,
                    &subject,
                    &facts()
                ),
                Err(DenyReason::StaffRequired)
            );
        }
        // A back office action reaches only a staff subject.
        for resource in [
            Resource::Account("a1"),
            Resource::Platform,
            Resource::StaffRoles,
        ] {
            assert_eq!(
                authorize(
                    &Actor::Principal(&half_hour),
                    Action::StaffPeopleRead,
                    &resource,
                    &all
                ),
                Err(DenyReason::NotPermitted)
            );
        }
    }

    #[test]
    fn every_staff_role_round_trips_and_nothing_else_parses() {
        for role in StaffRole::ALL {
            assert_eq!(StaffRole::parse(role.as_str()), Some(role));
        }
        for bad in ["", "Owner", "admin", "staff", "finance "] {
            assert_eq!(StaffRole::parse(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn only_the_platform_owner_with_step_up_manages_staff_roles() {
        let strong = human("owner", Some("gold"));
        let weak = human("owner", None);
        let roles = Resource::StaffRoles;
        let owner = Facts {
            platform_owner: true,
            ..facts()
        };
        assert!(authorize(
            &Actor::Principal(&strong),
            Action::StaffRolesManage,
            &roles,
            &owner
        )
        .is_ok());
        assert_eq!(
            authorize(
                &Actor::Principal(&weak),
                Action::StaffRolesManage,
                &roles,
                &owner
            )
            .unwrap_err()
            .code(),
            "step_up_required"
        );
        // A stranger, and a staff member who is not the owner.
        for f in [facts(), staff(&[StaffRole::Reviewer, StaffRole::Finance])] {
            assert_eq!(
                authorize(
                    &Actor::Principal(&strong),
                    Action::StaffRolesManage,
                    &roles,
                    &f
                ),
                Err(DenyReason::NotPermitted)
            );
        }
        for p in [
            &api_key("owner"),
            &connector("owner"),
            &mailbox(Some("owner")),
        ] {
            assert_eq!(
                authorize(
                    &Actor::Principal(p),
                    Action::StaffRolesManage,
                    &roles,
                    &owner
                )
                .unwrap_err()
                .code(),
                "human_required"
            );
        }
        assert_eq!(
            authorize(
                &Actor::Service("ops"),
                Action::StaffRolesManage,
                &roles,
                &owner
            ),
            Err(DenyReason::CredentialNotAccepted)
        );
    }

    /// Who may discover: anyone for a visible listing, nobody for a hidden one, no automation
    /// identity that is not a member credential.
    #[test]
    fn discovery_follows_the_listing_and_grants_nothing_else() {
        let listing = Resource::NestListing("lst_1");
        let visible = Facts {
            listing_visible: true,
            ..facts()
        };
        let me = human("a1", None);
        let (key, conn, mb) = (api_key("a1"), connector("a1"), mailbox(Some("a1")));
        for actor in [
            Actor::Anonymous,
            Actor::Principal(&me),
            Actor::Principal(&key),
            Actor::Principal(&conn),
            Actor::Principal(&mb),
        ] {
            assert!(authorize(&actor, Action::NestDiscover, &listing, &visible).is_ok());
            assert_eq!(
                authorize(&actor, Action::NestDiscover, &listing, &facts()),
                Err(DenyReason::NotPermitted)
            );
        }
        for actor in [
            Actor::Service("ops"),
            Actor::OrgKey {
                key_id: "k",
                org_id: "o",
            },
        ] {
            assert_eq!(
                authorize(&actor, Action::NestDiscover, &listing, &visible),
                Err(DenyReason::CredentialNotAccepted)
            );
        }
        // A visible listing buys nothing: buying and installing are other decisions.
        for action in [Action::NestInstall, Action::NestMarketplacePurchase] {
            assert_eq!(
                authorize(&Actor::Principal(&me), action, &listing, &visible),
                Err(DenyReason::NotPermitted)
            );
        }
    }

    #[test]
    fn buyer_actions_are_for_a_person_on_their_own_account_or_org() {
        let me = human("a1", None);
        let own = Resource::Account("a1");
        let (key, conn, mb) = (api_key("a1"), connector("a1"), mailbox(Some("a1")));
        // Paying is per purchase and a person's: there is no mandate for automation to consume.
        for action in [
            Action::NestMarketplacePurchase,
            Action::NestInstall,
            Action::PublisherApply,
        ] {
            assert!(authorize(&Actor::Principal(&me), action, &own, &facts()).is_ok());
            for no in [&key, &conn, &mb] {
                assert_eq!(
                    authorize(&Actor::Principal(no), action, &own, &facts())
                        .unwrap_err()
                        .code(),
                    "human_required",
                    "{action:?}"
                );
            }
            // A stranger's account, an org without the role, and the org with it.
            assert_eq!(
                authorize(
                    &Actor::Principal(&me),
                    action,
                    &Resource::Account("a2"),
                    &facts()
                ),
                Err(DenyReason::NotPermitted)
            );
            assert_eq!(
                authorize(
                    &Actor::Principal(&me),
                    action,
                    &Resource::Org("o1"),
                    &facts()
                ),
                Err(DenyReason::NotPermitted)
            );
            let role = Facts {
                org_permitted: true,
                ..facts()
            };
            assert!(authorize(&Actor::Principal(&me), action, &Resource::Org("o1"), &role).is_ok());
        }
        // Configure and manage: the installed Nest's own grant, a person only.
        let nest = Resource::InstalledNest("nst_1");
        let held = Facts {
            nest_permitted: true,
            ..facts()
        };
        for action in [Action::NestConfigure, Action::NestManage] {
            assert!(authorize(&Actor::Principal(&me), action, &nest, &held).is_ok());
            assert_eq!(
                authorize(&Actor::Principal(&me), action, &nest, &facts()),
                Err(DenyReason::NotPermitted)
            );
            for no in [&key, &conn, &mb] {
                assert!(matches!(
                    authorize(&Actor::Principal(no), action, &nest, &held),
                    Err(DenyReason::Auth(AuthDenial::HumanRequired))
                ));
            }
        }
    }

    #[test]
    fn operating_and_invoking_exclude_connectors_and_need_their_grant() {
        let me = human("a1", None);
        let (key, conn, mb) = (api_key("a1"), connector("a1"), mailbox(Some("a1")));
        let nest = Resource::InstalledNest("nst_1");
        let op = Resource::NestOperation("lst_1");
        let held = Facts {
            nest_permitted: true,
            ..facts()
        };
        let admitted = Facts {
            job_admitted: true,
            ..facts()
        };
        for ok in [&me, &key, &mb] {
            assert!(authorize(&Actor::Principal(ok), Action::NestOperate, &nest, &held).is_ok());
            assert!(authorize(&Actor::Principal(ok), Action::NestInvoke, &op, &admitted).is_ok());
            assert_eq!(
                authorize(&Actor::Principal(ok), Action::NestOperate, &nest, &facts()),
                Err(DenyReason::NotPermitted)
            );
            // Admission is what grants a job; holding a Nest does not stand in for it.
            assert_eq!(
                authorize(&Actor::Principal(ok), Action::NestInvoke, &op, &held),
                Err(DenyReason::NotPermitted)
            );
        }
        assert_eq!(
            authorize(&Actor::Principal(&conn), Action::NestOperate, &nest, &held),
            Err(DenyReason::CredentialNotAccepted)
        );
        assert_eq!(
            authorize(&Actor::Principal(&conn), Action::NestInvoke, &op, &admitted),
            Err(DenyReason::CredentialNotAccepted)
        );
        for actor in [Actor::Anonymous, Actor::Service("ops")] {
            assert_eq!(
                authorize(&actor, Action::NestInvoke, &op, &admitted),
                Err(DenyReason::CredentialNotAccepted)
            );
        }
    }

    /// M1: a job is read by the account that submitted it, through any credential but a
    /// connector; nothing else grants it, and reading grants nothing else.
    #[test]
    fn a_job_is_read_by_its_submitter_only() {
        let me = human("a1", None);
        let (key, conn, mb) = (api_key("a1"), connector("a1"), mailbox(Some("a1")));
        let job = Resource::NestJob("job_1");
        let mine = Facts {
            job_visible: true,
            ..facts()
        };
        let everything_else = Facts {
            job_admitted: true,
            nest_permitted: true,
            publisher_permitted: true,
            listing_visible: true,
            platform_owner: true,
            staff_roles: vec![StaffRole::Reviewer, StaffRole::Finance],
            ..facts()
        };
        for ok in [&me, &key, &mb] {
            assert!(authorize(&Actor::Principal(ok), Action::NestJobRead, &job, &mine).is_ok());
            assert_eq!(
                authorize(
                    &Actor::Principal(ok),
                    Action::NestJobRead,
                    &job,
                    &everything_else
                ),
                Err(DenyReason::NotPermitted)
            );
        }
        assert_eq!(
            authorize(&Actor::Principal(&conn), Action::NestJobRead, &job, &mine),
            Err(DenyReason::CredentialNotAccepted)
        );
        for actor in [
            Actor::Anonymous,
            Actor::Service("ops"),
            Actor::OrgKey {
                key_id: "k",
                org_id: "o",
            },
        ] {
            assert_eq!(
                authorize(&actor, Action::NestJobRead, &job, &mine),
                Err(DenyReason::CredentialNotAccepted)
            );
        }
        // Being able to read a job does not admit one, nor reach anything else.
        for (action, resource) in [
            (Action::NestInvoke, Resource::NestOperation("lst_1")),
            (Action::NestInvoke, Resource::NestJob("job_1")),
            (Action::NestManage, Resource::NestJob("job_1")),
        ] {
            assert_eq!(
                authorize(&Actor::Principal(&me), action, &resource, &mine),
                Err(DenyReason::NotPermitted)
            );
        }
    }

    #[test]
    fn publisher_actions_need_the_profile_grant_and_publishing_needs_step_up() {
        let strong = human("a1", Some("gold"));
        let weak = human("a1", None);
        let stranger = human("a2", Some("gold"));
        let (key, conn, mb) = (api_key("a1"), connector("a1"), mailbox(Some("a1")));
        let publisher = Resource::Publisher("pub_1");
        let granted = Facts {
            publisher_permitted: true,
            ..facts()
        };
        assert!(authorize(
            &Actor::Principal(&weak),
            Action::PublisherManage,
            &publisher,
            &granted
        )
        .is_ok());
        assert!(authorize(
            &Actor::Principal(&strong),
            Action::NestPublish,
            &publisher,
            &granted
        )
        .is_ok());
        assert_eq!(
            authorize(
                &Actor::Principal(&weak),
                Action::NestPublish,
                &publisher,
                &granted
            )
            .unwrap_err()
            .code(),
            "step_up_required"
        );
        for action in [Action::PublisherManage, Action::NestPublish] {
            assert_eq!(
                authorize(&Actor::Principal(&stranger), action, &publisher, &facts()),
                Err(DenyReason::NotPermitted)
            );
            for no in [&key, &conn, &mb] {
                assert_eq!(
                    authorize(&Actor::Principal(no), action, &publisher, &granted)
                        .unwrap_err()
                        .code(),
                    "human_required"
                );
            }
        }
    }

    #[test]
    fn only_a_person_registers_a_notes_key() {
        // Audit 2026-11-03 P2-2: the account key may read its notes, never add a key to them.
        let me = human("a1", None);
        let account_key = Principal::Account("a1".into(), AccountCredential::ApiKey);
        let res = Resource::Account("a1");
        assert!(authorize(
            &Actor::Principal(&me),
            Action::MeetingNotesKey,
            &res,
            &facts()
        )
        .is_ok());
        assert_eq!(
            authorize(
                &Actor::Principal(&account_key),
                Action::MeetingNotesKey,
                &res,
                &facts()
            ),
            Err(DenyReason::Auth(AuthDenial::HumanRequired))
        );
        assert!(authorize(
            &Actor::Principal(&account_key),
            Action::MeetingNotes,
            &res,
            &facts()
        )
        .is_ok());
        let mb = mailbox(Some("a1"));
        assert!(authorize(
            &Actor::Principal(&mb),
            Action::MeetingNotesKey,
            &res,
            &facts()
        )
        .is_err());
    }
}
