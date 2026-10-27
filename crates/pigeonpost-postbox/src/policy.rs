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
    /// A group's live stream, acted on as `mailbox` (with its stored account binding). The group
    /// rules (membership, role, the group's `stream_policy`, the stage) are resolved from the rows
    /// the request changes into [`Facts::stream_permitted`].
    GroupStream {
        mailbox: &'a str,
        account: Option<&'a str>,
    },
    /// A mailbox's picture, with the facts its visibility is decided on.
    Avatar {
        visibility: crate::store::Visibility,
        owner_account: Option<&'a str>,
        owner_address: &'a str,
        /// The owner counts at least one of the viewer's mailboxes as known (SDS §13.1).
        viewer_known: bool,
    },
}

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
            Action::ViewAvatar => Requirement::Anyone,
            Action::CallAdmission
            | Action::CallEvents
            | Action::ModerateAvatars
            | Action::GroupCallAdmission => Requirement::Service,
            Action::GroupParticipate | Action::CalendarUse => Requirement::NotConnector,
            Action::MeetingAccess => Requirement::Service,
            Action::MeetingNotes => Requirement::AccountHolder,
            Action::MeetingsCalls | Action::MeetingsIntel => Requirement::Service,
            Action::GroupManage | Action::OrgAdminister => Requirement::Human,
            Action::OrgSensitive => Requirement::HumanStepUp,
            Action::DeviceEnroll => Requirement::Anyone,
            Action::OrgApi => Requirement::OrgKey,
            Action::StreamWatch => Requirement::NotConnector,
            Action::StreamPublish | Action::StreamModerate => Requirement::Human,
            Action::StreamAdmission => Requirement::Service,
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
    /// The actor holds no grant on this resource.
    NotPermitted,
}

impl DenyReason {
    pub(crate) fn code(&self) -> &'static str {
        match self {
            DenyReason::CredentialNotAccepted => "credential_not_accepted",
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

/// The hooks every decision runs.
pub(crate) const MANDATORY_DENIES: &[MandatoryDeny] = &[
    deny_revoked,
    deny_suspended,
    deny_blocked,
    deny_org_inactive,
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
        (Requirement::Human | Requirement::HumanStepUp, Actor::OrgKey { .. }) => {
            return Err(DenyReason::Auth(AuthDenial::HumanRequired))
        }
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
    }

    // 2. Mandatory denies.
    for hook in hooks {
        if let Some(code) = hook(actor, action, resource, facts) {
            return Err(DenyReason::Mandatory(code));
        }
    }

    // 3. An action-specific grant on this resource; 4. otherwise deny.
    let granted = match (actor, resource) {
        (Actor::Service(_), Resource::Platform) => true,
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
            let own = match actor {
                Actor::Principal(p) => owns(p, owner_address, *owner_account),
                _ => false,
            };
            match visibility {
                crate::store::Visibility::Everyone => true,
                // Being known needs someone to know: never the anonymous.
                crate::store::Visibility::Known => {
                    own || (*viewer_known && matches!(actor, Actor::Principal(_)))
                }
                crate::store::Visibility::Nobody => own,
            }
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
}
