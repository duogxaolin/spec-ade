//! How a connection answers `session/request_permission` (SPEC-007 §5.2).
//!
//! SPEC-003 had exactly one policy — [INVENTED-5], *always ask* — because a chat
//! session always has a human attached. A Claw does not: it fires at 09:00 while
//! nobody is watching, so a parked prompt would wedge the turn until the 300s
//! timeout cancelled it. `claws.mdx:54-59` therefore gives a Claw three modes,
//! and this module is the decision function for all three.
//!
//! Two rules that are **not** negotiable, both from §5.2 and §9.1:
//!
//! 1. **Automatic does not mean invisible.** Whatever this module decides, the
//!    caller still emits `PermissionRequest` *and* `PermissionResolved` into the
//!    event log. `auto_approve` is allowed to skip the user; it is not allowed to
//!    skip the audit trail.
//! 2. **The fallbacks fail safe, per mode.** If the agent offers no allow-shaped
//!    option, `AutoApprove` **parks** rather than picking something arbitrary. If
//!    it offers no reject-shaped option, `DenyAll` **errors** rather than parking —
//!    the mode promised the user read-only, and asking would break that promise
//!    just as surely as approving would.
//!
//! `PermissionOptionKind` is `#[non_exhaustive]`, so the wildcard arm in
//! [`classify`] is required by the compiler *and* reachable in practice: a future
//! ACP version can add a kind, and an unknown kind must count as neither allow nor
//! reject.

use agent_client_protocol::schema::v1::{PermissionOption, PermissionOptionKind};

/// The permission behaviour of one ACP connection.
///
/// Injected at spawn alongside `AcpLimits`, for the same reason: it is a property
/// of *this* connection, and reading it from a global would make two connections
/// with different modes impossible.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PermissionPolicy {
    /// Park and wait for a human — the SPEC-003 behaviour, and the default so an
    /// ordinary chat connection keeps it without every caller opting in (E27).
    #[default]
    AskViaUi,
    /// Answer with the first allow-shaped option.
    AutoApprove,
    /// Answer with the first reject-shaped option.
    DenyAll,
}

/// What the connection should do with one request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// Store the responder and wait for a human.
    Ask,
    /// Answer `Selected(option_id)` immediately.
    Select(String),
    /// Answer with a JSON-RPC error: no option matched, and this mode must not ask.
    Refuse(String),
}

/// Which side of the allow/reject split a kind falls on. `None` = neither, which
/// is what an unmodelled future variant must be treated as.
fn classify(kind: PermissionOptionKind) -> Option<bool> {
    match kind {
        PermissionOptionKind::AllowOnce | PermissionOptionKind::AllowAlways => Some(true),
        PermissionOptionKind::RejectOnce | PermissionOptionKind::RejectAlways => Some(false),
        // `#[non_exhaustive]`: a kind we do not know is not an approval.
        _ => None,
    }
}

impl PermissionPolicy {
    /// The wire value, for logs and the `permission_resolved` reason.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::AskViaUi => "ask_via_ui",
            Self::AutoApprove => "auto_approve",
            Self::DenyAll => "deny_all",
        }
    }

    /// Decide what to do with the options the agent offered.
    ///
    /// "First" means first in the agent's own order — the agent lists its
    /// preferred option first, and reordering would substitute our judgement for
    /// the agent's on a choice we have no basis to second-guess.
    pub fn decide(self, options: &[PermissionOption]) -> Decision {
        match self {
            Self::AskViaUi => Decision::Ask,
            Self::AutoApprove => match first_of(options, true) {
                Some(id) => Decision::Select(id),
                // Fail *safe*, not open: with nothing allow-shaped on offer, the
                // only honest move is to find a human.
                None => Decision::Ask,
            },
            Self::DenyAll => match first_of(options, false) {
                Some(id) => Decision::Select(id),
                // Not `Ask`: `deny_all` is a promise that this connection never
                // interrupts anyone, and parking here would break it.
                None => {
                    Decision::Refuse("deny_all: the agent offered no reject option".to_string())
                }
            },
        }
    }
}

/// First option whose kind classifies as `allow`.
fn first_of(options: &[PermissionOption], allow: bool) -> Option<String> {
    options
        .iter()
        .find(|o| classify(o.kind) == Some(allow))
        .map(|o| o.option_id.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    /// `PermissionOption` is `#[non_exhaustive]`, so it is built through its
    /// constructor rather than a struct literal.
    fn opt(id: &str, kind: PermissionOptionKind) -> PermissionOption {
        PermissionOption::new(id.to_string(), id.to_string(), kind)
    }

    /// What `mock_acp_agent`'s `permission` script offers.
    fn both() -> Vec<PermissionOption> {
        vec![
            opt("allow", PermissionOptionKind::AllowOnce),
            opt("reject", PermissionOptionKind::RejectOnce),
        ]
    }

    #[test]
    fn auto_approve_picks_the_allow_option() {
        // E23's pure half.
        assert_eq!(
            PermissionPolicy::AutoApprove.decide(&both()),
            Decision::Select("allow".to_string())
        );
        // AllowAlways counts too, and order is the agent's: reject is listed
        // first here, so a naive "options[0]" would pass the line above and fail
        // this one.
        let reordered = vec![
            opt("reject", PermissionOptionKind::RejectOnce),
            opt("always", PermissionOptionKind::AllowAlways),
        ];
        assert_eq!(
            PermissionPolicy::AutoApprove.decide(&reordered),
            Decision::Select("always".to_string())
        );
    }

    #[test]
    fn deny_all_picks_the_reject_option() {
        // E24's pure half.
        assert_eq!(
            PermissionPolicy::DenyAll.decide(&both()),
            Decision::Select("reject".to_string())
        );
        let always = vec![
            opt("allow", PermissionOptionKind::AllowOnce),
            opt("never", PermissionOptionKind::RejectAlways),
        ];
        assert_eq!(
            PermissionPolicy::DenyAll.decide(&always),
            Decision::Select("never".to_string())
        );
    }

    #[test]
    fn ask_via_ui_never_answers_by_itself() {
        // E25 — this is the assertion that goes red if the policy branch is
        // wired in the wrong order and starts swallowing chat prompts.
        assert_eq!(PermissionPolicy::AskViaUi.decide(&both()), Decision::Ask);
        assert_eq!(PermissionPolicy::default(), PermissionPolicy::AskViaUi);
    }

    #[test]
    fn auto_approve_falls_back_to_asking_not_to_guessing() {
        // E26. Reject-only options: approving is impossible, so a human is the
        // only correct answer. `Select` of anything here would be a fail-open bug.
        let reject_only = vec![opt("reject", PermissionOptionKind::RejectOnce)];
        assert_eq!(
            PermissionPolicy::AutoApprove.decide(&reject_only),
            Decision::Ask
        );
        assert_eq!(PermissionPolicy::AutoApprove.decide(&[]), Decision::Ask);
    }

    #[test]
    fn deny_all_refuses_rather_than_asking() {
        // The other half of E26: `deny_all` must never park.
        let allow_only = vec![opt("allow", PermissionOptionKind::AllowOnce)];
        assert!(matches!(
            PermissionPolicy::DenyAll.decide(&allow_only),
            Decision::Refuse(_)
        ));
        assert!(matches!(
            PermissionPolicy::DenyAll.decide(&[]),
            Decision::Refuse(_)
        ));
    }

    #[test]
    fn modes_round_trip_their_wire_names() {
        for (p, s) in [
            (PermissionPolicy::AskViaUi, "ask_via_ui"),
            (PermissionPolicy::AutoApprove, "auto_approve"),
            (PermissionPolicy::DenyAll, "deny_all"),
        ] {
            assert_eq!(p.as_str(), s);
        }
    }
}
