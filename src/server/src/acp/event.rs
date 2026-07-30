//! WebSocket event payloads and the `SessionUpdate` → `AcpEvent` translation
//! (SPEC-003 §3.2 / §5.3).
//!
//! Three business rules must not be broken here:
//!
//! - `tool_call_update` is a **patch**: an absent field means *unchanged*, not
//!   *cleared*. So the patch is forwarded as the raw sparse JSON object the agent
//!   sent — filling in defaults would invent state the agent never reported.
//! - `plan` is a **full snapshot** → the client replaces, never appends.
//! - An unrecognized `sessionUpdate` tag is skipped, not fatal. The schema already
//!   carries variants this phase does not model, and future ACP versions will add
//!   more; tearing down a live agent over one unknown notification would be a bug.
//!
//! All five `stopReason`s are normal ends of a turn. `refusal` in particular is
//! the agent declining — surfacing it as an error would misreport what happened.

use serde::{Deserialize, Serialize};

use agent_client_protocol::schema::v1::{ContentBlock, ContentChunk, SessionUpdate};

/// One event in a session's log, serialized straight to a WS text frame.
///
/// `seq` is added by the socket layer at send time (from [`super::log::LoggedEvent`])
/// rather than stored here, so the log owns sequencing and an event can't carry a
/// stale `seq` after a replay.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AcpEvent {
    /// Agent response text, append to the current message.
    MessageChunk { text: String },
    /// Agent reasoning text, rendered separately from the answer.
    ThoughtChunk { text: String },
    /// A tool call started. Keyed by `toolCallId`.
    ToolCall {
        #[serde(rename = "toolCall")]
        tool_call: serde_json::Value,
    },
    /// A sparse patch on an existing tool call — only the fields present change.
    ToolCallUpdate {
        #[serde(rename = "toolCall")]
        tool_call: serde_json::Value,
    },
    /// The agent's full plan. Replaces any previous plan.
    Plan { plan: serde_json::Value },
    /// Context-window / cost update.
    Usage { usage: serde_json::Value },
    /// The session's mode changed.
    Mode { mode: serde_json::Value },
    /// The agent wants permission. The ACP request stays open until answered.
    PermissionRequest {
        #[serde(rename = "requestId")]
        request_id: String,
        #[serde(rename = "toolCall")]
        tool_call: serde_json::Value,
        options: Vec<PermissionOptionView>,
    },
    /// A permission request is no longer answerable (answered, cancelled or
    /// timed out) — lets a reattaching client drop a stale prompt instead of
    /// showing buttons that would only produce an error.
    PermissionResolved {
        #[serde(rename = "requestId")]
        request_id: String,
        outcome: String,
    },
    /// The turn ended. Carries one of the five ACP stop reasons.
    TurnComplete {
        #[serde(rename = "stopReason")]
        stop_reason: String,
    },
    /// Session lifecycle, so a reattaching client knows whether a turn is live.
    SessionState { state: SessionState },
    /// The agent process is gone (exited, killed, or connection torn down).
    ConnectionClosed { reason: String },
    /// A non-fatal problem: the session and the agent both keep running.
    Error { message: String },
    /// Replay could not start where the client asked — it has a gap.
    Truncated {
        #[serde(rename = "fromSeq")]
        from_seq: u64,
    },
}

/// A permission option as shown to the user. Flattened from the ACP schema so the
/// frontend does not need to know ACP's shape, and so `optionId` round-trips
/// verbatim (it is the agent's identifier, not ours).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PermissionOptionView {
    #[serde(rename = "optionId")]
    pub option_id: String,
    pub name: String,
    pub kind: String,
}

/// Lifecycle of one ACP session as Spec ADE sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionState {
    /// No turn running; a `prompt` is accepted.
    Idle,
    /// A turn is in flight; another `prompt` is refused ([INVENTED-4]).
    Prompting,
    /// The connection is gone; nothing more will arrive.
    Closed,
}

/// Concatenate the text of a content chunk, ignoring non-text blocks.
///
/// Images/audio in an agent message are out of scope for this phase (the chat UI
/// that would render them is SPEC-004). Returning `None` for a chunk with no text
/// means it is skipped rather than logged as an empty message.
fn chunk_text(chunk: &ContentChunk) -> Option<String> {
    match &chunk.content {
        ContentBlock::Text(t) => Some(t.text.clone()),
        _ => None,
    }
}

/// Serialize a schema value into the sparse JSON the client receives.
///
/// `skip_serializing_none` on the schema types is what makes this preserve patch
/// sparseness: an unset `Option` field is omitted from the output entirely, so a
/// `tool_call_update` carrying only `status` stays exactly that on the wire.
fn to_json<T: Serialize>(value: &T) -> Option<serde_json::Value> {
    match serde_json::to_value(value) {
        Ok(v) => Some(v),
        Err(e) => {
            tracing::warn!("acp: could not serialize session update payload: {e}");
            None
        }
    }
}

/// Translate one `session/update` payload.
///
/// `None` means "nothing to show the user" — either a variant this phase does not
/// model or a chunk with no renderable text. Callers log at `debug` and move on.
pub fn translate(update: &SessionUpdate) -> Option<AcpEvent> {
    match update {
        SessionUpdate::AgentMessageChunk(c) => {
            chunk_text(c).map(|text| AcpEvent::MessageChunk { text })
        }
        SessionUpdate::AgentThoughtChunk(c) => {
            chunk_text(c).map(|text| AcpEvent::ThoughtChunk { text })
        }
        // The user's own message echoed back. Spec ADE already rendered it when
        // the prompt was sent, so echoing it again would duplicate the bubble.
        SessionUpdate::UserMessageChunk(_) => None,
        SessionUpdate::ToolCall(tc) => {
            to_json(tc).map(|tool_call| AcpEvent::ToolCall { tool_call })
        }
        SessionUpdate::ToolCallUpdate(tc) => {
            to_json(tc).map(|tool_call| AcpEvent::ToolCallUpdate { tool_call })
        }
        SessionUpdate::Plan(p) => to_json(p).map(|plan| AcpEvent::Plan { plan }),
        SessionUpdate::UsageUpdate(u) => to_json(u).map(|usage| AcpEvent::Usage { usage }),
        SessionUpdate::CurrentModeUpdate(m) => to_json(m).map(|mode| AcpEvent::Mode { mode }),
        // Known-but-unmodelled variants (available_commands_update,
        // config_option_update, session_info_update, …) and any variant added by
        // a future schema. Skipping is deliberate: see the module docs.
        other => {
            tracing::debug!("acp: skipping unmodelled session update: {other:?}");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_client_protocol::schema::v1::{
        Plan, PlanEntry, PlanEntryPriority, PlanEntryStatus, TextContent, ToolCall, ToolCallStatus,
        ToolCallUpdate, ToolCallUpdateFields, ToolKind, UsageUpdate,
    };

    fn text(s: &str) -> ContentChunk {
        ContentChunk::new(ContentBlock::Text(TextContent::new(s.to_string())))
    }

    #[test]
    fn message_and_thought_chunks_carry_text() {
        assert_eq!(
            translate(&SessionUpdate::AgentMessageChunk(text("hi"))),
            Some(AcpEvent::MessageChunk { text: "hi".into() })
        );
        assert_eq!(
            translate(&SessionUpdate::AgentThoughtChunk(text("hmm"))),
            Some(AcpEvent::ThoughtChunk { text: "hmm".into() })
        );
    }

    #[test]
    fn user_message_chunk_is_not_echoed_back() {
        // Spec ADE renders the prompt locally when it is sent; replaying the
        // agent's echo would show the same bubble twice.
        assert_eq!(
            translate(&SessionUpdate::UserMessageChunk(text("hi"))),
            None
        );
    }

    #[test]
    fn tool_call_update_stays_sparse() {
        // A9/A6: the patch must contain ONLY what the agent sent. If defaults
        // leaked in, a client merging the patch would reset title/kind.
        let update = SessionUpdate::ToolCallUpdate(ToolCallUpdate::new(
            "call-1",
            ToolCallUpdateFields::new().status(ToolCallStatus::Completed),
        ));
        let Some(AcpEvent::ToolCallUpdate { tool_call }) = translate(&update) else {
            panic!("expected a tool_call_update event");
        };
        let obj = tool_call.as_object().expect("object");
        assert_eq!(
            obj.get("toolCallId").and_then(|v| v.as_str()),
            Some("call-1")
        );
        assert_eq!(
            obj.get("status").and_then(|v| v.as_str()),
            Some("completed")
        );
        assert!(
            !obj.contains_key("title"),
            "absent field must stay absent: {obj:?}"
        );
        assert!(
            !obj.contains_key("kind"),
            "absent field must stay absent: {obj:?}"
        );
        assert!(
            !obj.contains_key("content"),
            "absent field must stay absent: {obj:?}"
        );
    }

    #[test]
    fn tool_call_carries_the_fields_the_agent_set() {
        let update = SessionUpdate::ToolCall(
            ToolCall::new("call-1", "Read mock.txt")
                .kind(ToolKind::Read)
                .status(ToolCallStatus::Pending),
        );
        let Some(AcpEvent::ToolCall { tool_call }) = translate(&update) else {
            panic!("expected a tool_call event");
        };
        assert_eq!(
            tool_call.get("title").and_then(|v| v.as_str()),
            Some("Read mock.txt")
        );
        assert_eq!(tool_call.get("kind").and_then(|v| v.as_str()), Some("read"));
    }

    #[test]
    fn plan_is_a_full_snapshot() {
        let update = SessionUpdate::Plan(Plan::new(vec![
            PlanEntry::new("a", PlanEntryPriority::High, PlanEntryStatus::Pending),
            PlanEntry::new("b", PlanEntryPriority::Low, PlanEntryStatus::Completed),
        ]));
        let Some(AcpEvent::Plan { plan }) = translate(&update) else {
            panic!("expected a plan event");
        };
        let entries = plan
            .get("entries")
            .and_then(|v| v.as_array())
            .expect("entries");
        assert_eq!(
            entries.len(),
            2,
            "every entry must be present: replace, not patch"
        );
    }

    #[test]
    fn usage_update_is_forwarded() {
        let Some(AcpEvent::Usage { usage }) =
            translate(&SessionUpdate::UsageUpdate(UsageUpdate::new(120, 200_000)))
        else {
            panic!("expected a usage event");
        };
        assert_eq!(usage.get("used").and_then(|v| v.as_u64()), Some(120));
        assert_eq!(usage.get("size").and_then(|v| v.as_u64()), Some(200_000));
    }

    #[test]
    fn events_serialize_with_a_stable_tag() {
        // The frontend switches on `type`; renaming one silently would break it.
        let json = serde_json::to_value(AcpEvent::TurnComplete {
            stop_reason: "refusal".into(),
        })
        .unwrap();
        assert_eq!(json["type"], "turn_complete");
        assert_eq!(json["stopReason"], "refusal");

        let json = serde_json::to_value(AcpEvent::Truncated { from_seq: 7 }).unwrap();
        assert_eq!(json["type"], "truncated");
        assert_eq!(json["fromSeq"], 7);

        let json = serde_json::to_value(AcpEvent::SessionState {
            state: SessionState::Prompting,
        })
        .unwrap();
        assert_eq!(json["type"], "session_state");
        assert_eq!(json["state"], "prompting");
    }
}
