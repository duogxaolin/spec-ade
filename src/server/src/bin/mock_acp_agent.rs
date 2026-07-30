//! A real ACP agent used as a test fixture (SPEC-003 §7).
//!
//! Integration tests must not drive `claude`/`codex`: those need network access
//! and credentials, and their output is non-deterministic — you cannot assert a
//! binary outcome against them. So this binary is a genuine ACP agent built with
//! the same crate, whose behaviour is picked by `MOCK_ACP_SCRIPT`:
//!
//! | value             | behaviour |
//! |-------------------|-----------|
//! | `chunks`(default) | two `agent_message_chunk`s, then `end_turn` |
//! | `thought`         | a thought chunk, then a message chunk, then `end_turn` |
//! | `tool_call`       | `tool_call` (pending) → `tool_call_update` (completed) |
//! | `permission`      | asks `session/request_permission`, echoes the choice |
//! | `refusal`         | stops with `refusal` (a normal terminal state) |
//! | `max_tokens`      | stops with `max_tokens` |
//! | `plan`            | two `plan` updates, the second a full replacement |
//! | `unknown_variant` | emits a `sessionUpdate` tag this client doesn't know |
//! | `fs_read`         | calls `fs/read_text_file` and reports what it got |
//! | `fs_write`        | calls `fs/write_text_file` |
//! | `slow`            | streams a chunk every 50ms until cancelled |
//! | `die_on_start`    | exits before `initialize` — the 502 spawn-failure path |
//! | `die_after_handshake` | exits mid-turn — the transport-EOF path |
//!
//! `initialize` logs the `ClientCapabilities` it was handed to stderr. That is the
//! only way a test can check the *client* advertised honestly (A22): capabilities
//! are sent, never echoed back, so the receiving side has to report them.
//!
//! Real agents are exercised by hand in SPEC-003 §8 instead.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use agent_client_protocol::schema::v1::SessionId;
use agent_client_protocol::schema::v1::{
    AgentCapabilities, CancelNotification, ContentBlock, ContentChunk, Implementation,
    InitializeRequest, InitializeResponse, NewSessionRequest, NewSessionResponse, PermissionOption,
    PermissionOptionKind, Plan, PlanEntry, PlanEntryPriority, PlanEntryStatus, PromptRequest,
    PromptResponse, ReadTextFileRequest, RequestPermissionOutcome, RequestPermissionRequest,
    SessionNotification, SessionUpdate, StopReason, TextContent, ToolCall, ToolCallStatus,
    ToolCallUpdate, ToolCallUpdateFields, ToolKind, WriteTextFileRequest,
};
use agent_client_protocol::{Agent, Client, ConnectionTo, Result, Stdio, UntypedMessage};

fn script() -> String {
    std::env::var("MOCK_ACP_SCRIPT").unwrap_or_else(|_| "chunks".to_string())
}

/// Path the `fs_read` / `fs_write` scripts operate on, relative to the session cwd.
fn fs_target() -> String {
    std::env::var("MOCK_ACP_FS_PATH").unwrap_or_else(|_| "mock.txt".to_string())
}

#[tokio::main]
async fn main() -> Result<()> {
    if script() == "die_on_start" {
        // Exercises the "agent dies before initialize" path: stderr is the only
        // evidence the user gets, so make sure something lands there.
        eprintln!("mock agent: refusing to start (MOCK_ACP_SCRIPT=die_on_start)");
        std::process::exit(3);
    }

    let cancelled = Arc::new(AtomicBool::new(false));

    Agent
        .builder()
        .name("mock-acp-agent")
        .on_receive_request(
            async move |req: InitializeRequest, responder, _conn| {
                // Report what the client claimed. Capabilities travel one way, so
                // without this there is no observable record of the client's side
                // of the handshake for a test to assert on (A22).
                let caps = req.client_capabilities;
                eprintln!(
                    "mock agent: clientCapabilities readTextFile={} writeTextFile={} terminal={}",
                    caps.fs.read_text_file, caps.fs.write_text_file, caps.terminal
                );
                responder.respond(
                    InitializeResponse::new(req.protocol_version)
                        .agent_capabilities(AgentCapabilities::new())
                        .agent_info(Implementation::new(
                            "mock-acp-agent".to_string(),
                            env!("CARGO_PKG_VERSION").to_string(),
                        )),
                )
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            async move |_req: NewSessionRequest, responder, _conn| {
                responder.respond(NewSessionResponse::new("mock-session-1"))
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            {
                let cancelled = cancelled.clone();
                async move |req: PromptRequest, responder, conn: ConnectionTo<Client>| {
                    cancelled.store(false, Ordering::SeqCst);
                    // The script must NOT run inline: a handler body occupies the
                    // dispatch loop, so `block_task()` (the `permission`/`fs_*`
                    // scripts) would deadlock, and a `session/cancel` notification
                    // could never be delivered while `slow` is streaming.
                    // `Responder::send_fn` is `Send`, so we park it and answer late.
                    conn.spawn({
                        let conn = conn.clone();
                        let cancelled = cancelled.clone();
                        async move {
                            let stop = run_script(&req, &conn, &cancelled).await?;
                            responder.respond(PromptResponse::new(stop))
                        }
                    })
                }
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_notification(
            {
                let cancelled = cancelled.clone();
                async move |_notif: CancelNotification, _conn| {
                    cancelled.store(true, Ordering::SeqCst);
                    Ok(())
                }
            },
            agent_client_protocol::on_receive_notification!(),
        )
        .connect_to(Stdio::new())
        .await
}

/// Emit one `session/update` notification.
fn notify(conn: &ConnectionTo<Client>, session: &SessionId, update: SessionUpdate) -> Result<()> {
    conn.send_notification(SessionNotification::new(session.clone(), update))
}

/// Chunks for the `rich_markdown` script (SPEC-004 §8.1 #3).
///
/// Kept as a const so `scripts/verify-spec-004.mjs` can assert against the same
/// literals: a test that builds its own expected string tests its own arithmetic.
const RICH_MARKDOWN_CHUNKS: &[&str] = &[
    "# Heading\n\nSome **bold** and a table:\n\n",
    "| lang | ok |\n| --- | --- |\n| rust | yes |\n\n",
    "```rust\nfn main() { println!(\"hi\"); }\n```\n\n",
    // Inline and block math. `$PATH` is here on purpose: it must NOT be treated
    // as math, and it must survive the server untouched either way.
    "Inline $x^2 + y^2 = z^2$ and echo $PATH in prose.\n\n$$\\int_0^1 x\\,dx = \\frac{1}{2}$$\n\n",
    "```mermaid\ngraph TD\n  A[Start] --> B[End]\n```\n\n",
    // The four payloads from §6 B3-B6.
    "<script>alert('xss')</script>\n\n",
    "<img src=x onerror=alert('xss')>\n\n",
    "[click me](javascript:alert('xss'))\n\n",
    "<iframe src=\"data:text/html,<script>alert(1)</script>\"></iframe>\n",
];

fn text_chunk(text: &str) -> ContentChunk {
    ContentChunk::new(ContentBlock::Text(TextContent::new(text.to_string())))
}

/// The prompt's text blocks joined — lets one script cover several cases.
fn prompt_text(req: &PromptRequest) -> String {
    req.prompt
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text(t) => Some(t.text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join(" ")
}

async fn run_script(
    req: &PromptRequest,
    conn: &ConnectionTo<Client>,
    cancelled: &AtomicBool,
) -> Result<StopReason> {
    let s = &req.session_id;
    match script().as_str() {
        "chunks" => {
            notify(
                conn,
                s,
                SessionUpdate::AgentMessageChunk(text_chunk("Hello, ")),
            )?;
            notify(
                conn,
                s,
                SessionUpdate::AgentMessageChunk(text_chunk("world!")),
            )?;
            Ok(StopReason::EndTurn)
        }
        "thought" => {
            notify(
                conn,
                s,
                SessionUpdate::AgentThoughtChunk(text_chunk("thinking...")),
            )?;
            notify(
                conn,
                s,
                SessionUpdate::AgentMessageChunk(text_chunk("answer")),
            )?;
            Ok(StopReason::EndTurn)
        }
        "refusal" => Ok(StopReason::Refusal),
        "max_tokens" => Ok(StopReason::MaxTokens),

        "tool_call" => {
            notify(
                conn,
                s,
                SessionUpdate::ToolCall(
                    ToolCall::new("call-1", "Read mock.txt")
                        .kind(ToolKind::Read)
                        .status(ToolCallStatus::Pending),
                ),
            )?;
            // A sparse patch: only `status` is present, so a correct client must
            // leave `title`/`kind` alone rather than resetting them to defaults.
            notify(
                conn,
                s,
                SessionUpdate::ToolCallUpdate(ToolCallUpdate::new(
                    "call-1",
                    ToolCallUpdateFields::new().status(ToolCallStatus::Completed),
                )),
            )?;
            Ok(StopReason::EndTurn)
        }

        "plan" => {
            notify(
                conn,
                s,
                SessionUpdate::Plan(Plan::new(vec![
                    PlanEntry::new(
                        "step one",
                        PlanEntryPriority::High,
                        PlanEntryStatus::Pending,
                    ),
                    PlanEntry::new("step two", PlanEntryPriority::Low, PlanEntryStatus::Pending),
                ])),
            )?;
            // A plan is a full snapshot, so this replaces both entries above with
            // a single one — a client that appends would end up showing three.
            notify(
                conn,
                s,
                SessionUpdate::Plan(Plan::new(vec![PlanEntry::new(
                    "only step",
                    PlanEntryPriority::Medium,
                    PlanEntryStatus::Completed,
                )])),
            )?;
            Ok(StopReason::EndTurn)
        }

        "permission" => {
            let outcome = conn
                .send_request(RequestPermissionRequest::new(
                    s.clone(),
                    ToolCallUpdate::new("call-1", ToolCallUpdateFields::new()),
                    vec![
                        PermissionOption::new("allow", "Allow", PermissionOptionKind::AllowOnce),
                        PermissionOption::new("reject", "Reject", PermissionOptionKind::RejectOnce),
                    ],
                ))
                .block_task()
                .await?;
            // Echo the decision back as text so a test can assert the agent truly
            // received the client's choice, not just that the RPC completed.
            let decision = match outcome.outcome {
                RequestPermissionOutcome::Selected(sel) => {
                    format!("selected:{}", sel.option_id)
                }
                RequestPermissionOutcome::Cancelled => "cancelled".to_string(),
                _ => "unknown".to_string(),
            };
            notify(
                conn,
                s,
                SessionUpdate::AgentMessageChunk(text_chunk(&decision)),
            )?;
            Ok(StopReason::EndTurn)
        }

        "fs_read" => {
            // Two cases in one script, picked by the prompt text so a test does not
            // need a second agent process: the normal sliced read, and an attempt
            // to escape the project root.
            let escaping = prompt_text(req).contains("escape");
            let request = if escaping {
                // An absolute path well outside any project. The guard must refuse
                // it, and no content may come back.
                ReadTextFileRequest::new(s.clone(), "/etc/passwd")
            } else {
                // 1-based `line`, so this must yield lines 2 and 3 — an off-by-one
                // in the slice shows up as line 1 or line 4 in the reply.
                ReadTextFileRequest::new(s.clone(), fs_target())
                    .line(2)
                    .limit(2)
            };
            let res = conn.send_request(request).block_task().await;
            let reply = match res {
                Ok(r) => format!("read_ok:{}", r.content),
                Err(e) => format!("read_refused:{}", e.message),
            };
            notify(
                conn,
                s,
                SessionUpdate::AgentMessageChunk(text_chunk(&reply)),
            )?;
            Ok(StopReason::EndTurn)
        }

        "fs_write" => {
            let target =
                std::env::var("MOCK_ACP_FS_PATH").unwrap_or_else(|_| "agent-wrote.txt".to_string());
            let res = conn
                .send_request(WriteTextFileRequest::new(
                    s.clone(),
                    target,
                    "written by the agent\n".to_string(),
                ))
                .block_task()
                .await;
            let reply = match res {
                Ok(_) => "write_ok".to_string(),
                Err(e) => format!("write_err:{}", e.message),
            };
            notify(
                conn,
                s,
                SessionUpdate::AgentMessageChunk(text_chunk(&reply)),
            )?;
            Ok(StopReason::EndTurn)
        }

        "die_after_handshake" => {
            // Exits mid-turn, which on the wire is the same transport EOF an
            // externally-killed agent produces (A19). `exit` rather than returning
            // an error: an error would still be a well-formed JSON-RPC reply.
            eprintln!("mock agent: exiting mid-turn (MOCK_ACP_SCRIPT=die_after_handshake)");
            std::process::exit(4);
        }

        "slow" => {
            // Streams until the client cancels. Bounded so a broken test times out
            // with output rather than hanging the suite forever.
            for i in 0..200 {
                if cancelled.load(Ordering::SeqCst) {
                    return Ok(StopReason::Cancelled);
                }
                notify(
                    conn,
                    s,
                    SessionUpdate::AgentMessageChunk(text_chunk(&format!("tick{i} "))),
                )?;
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            Ok(StopReason::EndTurn)
        }

        "unknown_variant" => {
            // Hand-rolled JSON because `SessionUpdate` is a closed Rust enum — there
            // is no way to construct a tag it doesn't know. A future ACP version
            // adding a variant looks exactly like this on the wire, so the client
            // must skip it and keep the connection alive rather than tearing down.
            conn.send_notification(UntypedMessage::new(
                "session/update",
                serde_json::json!({
                    "sessionId": s,
                    "update": {
                        "sessionUpdate": "quantum_entanglement_chunk",
                        "payload": { "spooky": true }
                    }
                }),
            )?)?;
            // A known-good chunk after it: proves the connection survived, which is
            // the actual criterion. Without this, a dead connection would pass too.
            notify(
                conn,
                s,
                SessionUpdate::AgentMessageChunk(text_chunk("still alive")),
            )?;
            Ok(StopReason::EndTurn)
        }

        "rich_markdown" => {
            // SPEC-004 §8.1 #3: the boundary test. Everything the chat UI has to
            // render — fences, tables, math, a diagram — plus four XSS payloads,
            // sent as separate chunks so the assertion also covers reassembly.
            //
            // The point is that the SERVER changes none of it. Escaping here would
            // look like a fix and would in fact hide the frontend's defences, so the
            // verify script asserts these strings arrive byte-for-byte.
            for chunk in RICH_MARKDOWN_CHUNKS {
                notify(conn, s, SessionUpdate::AgentMessageChunk(text_chunk(chunk)))?;
            }
            // A thought carrying a payload too: `ChatThought` renders markdown
            // through the same pipeline, so it is the same XSS surface.
            notify(
                conn,
                s,
                SessionUpdate::AgentThoughtChunk(text_chunk(
                    "Reasoning with a payload: <img src=x onerror=alert('thought')>",
                )),
            )?;
            Ok(StopReason::EndTurn)
        }

        other => {
            eprintln!("mock agent: unknown MOCK_ACP_SCRIPT={other}, defaulting to end_turn");
            Ok(StopReason::EndTurn)
        }
    }
}
