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
    /// Anyone, with or without a credential.
    Anyone,
}

impl Action {
    fn requirement(self) -> Requirement {
        match self {
            Action::ReadCapabilities => Requirement::AnyPrincipal,
            Action::StreamAccountEvents => Requirement::AccountHolder,
            Action::ServiceHealth
            | Action::ReadFeatureFlags
            | Action::WriteFeatureFlags
            | Action::ManageBilling => Requirement::Service,
            Action::SensitiveAccountChange => Requirement::HumanStepUp,
            Action::ManageOwnMailbox | Action::ReportAbuse => Requirement::AnyPrincipal,
            Action::RaiseTrust => Requirement::NotConnector,
            Action::ViewAvatar => Requirement::Anyone,
            Action::CallAdmission | Action::CallEvents | Action::ModerateAvatars => {
                Requirement::Service
            }
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

/// The hooks every decision runs.
pub(crate) const MANDATORY_DENIES: &[MandatoryDeny] = &[deny_revoked, deny_suspended, deny_blocked];

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
        (Actor::Principal(p), Resource::Mailbox { address, account }) => owns(p, address, *account),
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
}
