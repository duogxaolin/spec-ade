// SPEC-003 §8 runtime verification — drives the REAL binary over HTTP + WebSocket.
//
// Usage (build the SPA first so the binary embeds the current frontend):
//   cd src/web && npm run build
//   cd ../server && cargo build --release
//   node scripts/verify-spec-003.mjs            # release binary
//   node scripts/verify-spec-003.mjs --debug    # debug binary
//
// Why the mock agent and not `claude`: §8's ten steps need network, credentials
// and a human reading a screen. This script covers the part that can be asserted
// binarily against the shipped binary — the ACP wire protocol end to end,
// through the real process spawn, the real WS route and the real event log. The
// remaining human steps stay a checklist in the spec.
//
// `SPEC_ADE_DATA_DIR` points at a tempdir, so the developer's real settings.json
// is never touched. `settings.json` is written before the server starts, because
// the agent catalogue is read-only over HTTP this phase (§3.4).

import { spawn, execFileSync } from 'node:child_process';
import { mkdtempSync, writeFileSync, readFileSync, existsSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

// `WebSocket` is global from Node 21 on; no dependency needed.

const REPO = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const PROFILE = process.argv.includes('--debug') ? 'debug' : 'release';
const TARGET = join(REPO, 'src/server/target', PROFILE);
const BIN = join(TARGET, 'spec-ade-server');
// Built alongside the server by any `cargo build`; a dev artifact, never shipped.
const MOCK = join(TARGET, 'mock_acp_agent');
const PORT = 7392;
const BASE = `http://127.0.0.1:${PORT}`;
const DATA_DIR = mkdtempSync(join(tmpdir(), 'spec-ade-verify3-data-'));

let pass = 0;
const failures = [];
/** The server mints this at startup; set once in `main` and read by `attach`. */
let SERVER_TOKEN = null;

function check(name, ok, detail = '') {
  if (ok) {
    pass++;
    console.log(`  ok   ${name}`);
  } else {
    failures.push(`${name}${detail ? ` — ${detail}` : ''}`);
    console.log(`  FAIL ${name}${detail ? ` — ${detail}` : ''}`);
  }
}

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

/**
 * Seed the catalogue with mock agents, one per script.
 *
 * A separate entry per script rather than one entry reconfigured per spawn:
 * `acp_agents` is read at spawn time from a snapshot, and rewriting the file
 * mid-run would race the server's own in-memory copy.
 */
function writeSettings(scripts) {
  const agents = scripts.map((script) => ({
    id: `mock-${script}`,
    name: `Mock (${script})`,
    command: MOCK,
    args: [],
    // `fs_read` and `fs_write` share one env var, so the path is per script:
    // pointing both at one file would have the write destroy what the read
    // asserts on. Relative, because the fs bridge resolves against the project.
    env: {
      MOCK_ACP_SCRIPT: script,
      MOCK_ACP_FS_PATH: script === 'fs_write' ? 'agent-wrote.txt' : 'read-me.txt',
    },
  }));
  // A2: an entry whose executable does not exist, so the spawn path's failure
  // handling is exercised against the real binary rather than only in tests.
  agents.push({
    id: 'mock-missing-binary',
    name: 'Mock (missing binary)',
    command: join(DATA_DIR, 'no-such-agent-binary'),
    args: [],
    env: {},
  });
  writeFileSync(
    join(DATA_DIR, 'settings.json'),
    // Top-level keys are snake_case (`Settings` has no `rename_all`) while the
    // nested agent entries are camelCase. Getting this backwards makes the
    // server silently fall back to the default catalogue.
    JSON.stringify({ acp_agents: agents }, null, 2),
  );
}

async function startServer() {
  const proc = spawn(BIN, ['-p', String(PORT), '-H', '127.0.0.1', '--no-open'], {
    env: {
      ...process.env,
      SPEC_ADE_DATA_DIR: DATA_DIR,
      // Step 9: 30 minutes is not verifiable in a script. The override exists
      // for exactly this (it is not a test seam — tests inject `AcpLimits`).
      SPEC_ADE_ACP_IDLE_SECS: '4',
      SPEC_ADE_ACP_PERMISSION_SECS: '3',
    },
    stdio: ['ignore', 'pipe', 'pipe'],
  });
  let log = '';
  let token = null;
  const onData = (buf) => {
    log += buf.toString();
    const m = log.match(/token=([0-9a-f]{16,})/);
    if (m) token = m[1];
  };
  proc.stdout.on('data', onData);
  proc.stderr.on('data', onData);

  for (let i = 0; i < 100 && !token; i++) await sleep(100);
  if (!token) {
    proc.kill('SIGKILL');
    throw new Error(`server did not report a token; log:\n${log}`);
  }
  for (let i = 0; i < 100; i++) {
    try {
      const r = await fetch(`${BASE}/api/health`);
      if (r.ok) break;
    } catch {}
    await sleep(100);
  }
  return { proc, token, log: () => log };
}

async function stopServer(server) {
  server.proc.kill('SIGTERM');
  await new Promise((r) => server.proc.once('exit', r));
}

function api(token) {
  const headers = { 'x-spec-ade-token': token, 'content-type': 'application/json' };
  return {
    async get(path) {
      const r = await fetch(`${BASE}${path}`, { headers });
      return { status: r.status, body: await safeJson(r) };
    },
    async post(path, body) {
      const r = await fetch(`${BASE}${path}`, {
        method: 'POST',
        headers,
        body: JSON.stringify(body),
      });
      return { status: r.status, body: await safeJson(r) };
    },
    async del(path) {
      const r = await fetch(`${BASE}${path}`, { method: 'DELETE', headers });
      return { status: r.status, body: await safeJson(r) };
    },
  };
}

async function safeJson(r) {
  if (r.status === 204) return null;
  try {
    return await r.json();
  } catch {
    return null;
  }
}

/**
 * Attach to a session's event stream and collect frames.
 *
 * Everything on this socket is JSON text, so there is no binary path to handle.
 * The token goes in the query string: a browser cannot set headers on a WS
 * upgrade, and the server accepts it there for that reason.
 */
function attach(token, connectionId, sessionId, afterSeq) {
  const params = new URLSearchParams({ sessionId, token });
  if (afterSeq !== undefined) params.set('after_seq', String(afterSeq));
  const ws = new WebSocket(`ws://127.0.0.1:${PORT}/api/acp/${connectionId}/ws?${params}`);

  const frames = [];
  let closed = null;
  ws.addEventListener('message', (e) => frames.push(JSON.parse(e.data)));
  ws.addEventListener('close', (e) => {
    closed = { code: e.code, reason: e.reason };
  });

  return {
    frames,
    get closed() {
      return closed;
    },
    send(payload) {
      ws.send(JSON.stringify(payload));
    },
    /** Wait for a frame matching `pred`, or throw with what did arrive. */
    async waitFor(pred, label, timeoutMs = 15_000) {
      const deadline = Date.now() + timeoutMs;
      while (Date.now() < deadline) {
        const hit = frames.find(pred);
        if (hit) return hit;
        if (closed) {
          throw new Error(
            `socket closed (${closed.code}) while waiting for ${label}; frames: ${JSON.stringify(frames)}`,
          );
        }
        await sleep(50);
      }
      throw new Error(`timed out waiting for ${label}; frames: ${JSON.stringify(frames)}`);
    },
    /** Every `message_chunk` concatenated — what the user would be reading. */
    text() {
      return frames
        .filter((f) => f.type === 'message_chunk')
        .map((f) => f.text)
        .join('');
    },
    close() {
      ws.close();
    },
    async open() {
      const deadline = Date.now() + 10_000;
      while (Date.now() < deadline) {
        if (ws.readyState === 1) return;
        if (closed) throw new Error(`socket closed before opening: ${JSON.stringify(closed)}`);
        await sleep(25);
      }
      throw new Error('socket never opened');
    },
  };
}

/** Spawn a mock agent and open a session on it. */
async function session(a, script, projectId) {
  const spawned = await a.post('/api/acp/spawn', {
    agentId: `mock-${script}`,
    projectId,
  });
  if (spawned.status !== 201) {
    throw new Error(`spawn ${script} failed: ${spawned.status} ${JSON.stringify(spawned.body)}`);
  }
  const created = await a.post(`/api/projects/${projectId}/sessions`, {
    connectionId: spawned.body.id,
  });
  if (created.status !== 201) {
    throw new Error(`session on ${script} failed: ${created.status} ${JSON.stringify(created.body)}`);
  }
  return { connectionId: spawned.body.id, sessionId: created.body.id, info: spawned.body };
}

/** True if a pid is alive. `kill -0` reports permission, not existence, so 0. */
function alive(pid) {
  try {
    process.kill(pid, 0);
    return true;
  } catch {
    return false;
  }
}

/** Pids whose command line mentions `needle`. */
function pidsMatching(needle) {
  try {
    const out = execFileSync('ps', ['-Ao', 'pid=,command='], { encoding: 'utf8' });
    return out
      .split('\n')
      .filter((line) => line.includes(needle))
      .map((line) => Number(line.trim().split(/\s+/)[0]))
      .filter((n) => Number.isFinite(n));
  } catch {
    return [];
  }
}

const SCRIPTS = [
  'chunks',
  'tool_call',
  'plan',
  'permission',
  'refusal',
  'fs_read',
  'fs_write',
  'slow',
  'unknown_variant',
  'die_after_handshake',
];

async function main() {
  for (const [label, path] of [
    ['server binary', BIN],
    ['mock agent binary', MOCK],
  ]) {
    if (!existsSync(path)) {
      throw new Error(`${label} not found: ${path}\nbuild it first (see the header of this file)`);
    }
  }

  const fixture = mkdtempSync(join(tmpdir(), 'spec-ade-verify3-fixture-'));
  // Four lines so the mock's `line(2).limit(2)` read has an unambiguous answer:
  // an off-by-one shows up as line 1 or line 4 in the reply.
  writeFileSync(join(fixture, 'read-me.txt'), 'one\ntwo\nthree\nfour\n');
  writeSettings(SCRIPTS);

  console.log(`binary:   ${BIN}`);
  console.log(`mock:     ${MOCK}`);
  console.log(`data dir: ${DATA_DIR}`);
  console.log(`fixture:  ${fixture}`);

  const server = await startServer();
  SERVER_TOKEN = server.token;
  const a = api(server.token);

  try {
    const project = await a.post('/api/projects', { path: fixture });
    if (project.status !== 201) {
      throw new Error(`could not register fixture project: ${JSON.stringify(project.body)}`);
    }
    const projectId = project.body.id;

    await checkCatalogueAndSpawn(a, projectId);
    await checkStreaming(a, projectId);
    await checkToolCallsAndPlan(a, projectId);
    await checkPermission(a, projectId);
    await checkFsBridge(a, projectId, fixture);
    await checkCancelAndBusy(a, projectId);
    await checkReplay(a, projectId);
    await checkLifecycle(a, projectId);
    // Last: the idle reaper's short clock only makes sense once nothing else
    // needs a long-lived connection.
    await checkIdleReaper(a, projectId);
    await checkEmbeddedBundle();
  } finally {
    await stopServer(server);
    // Anything the server failed to reap would outlive this script.
    for (const pid of pidsMatching('mock_acp_agent')) {
      try {
        process.kill(pid, 'SIGKILL');
      } catch {}
    }
    rmSync(fixture, { recursive: true, force: true });
    rmSync(DATA_DIR, { recursive: true, force: true });
  }

  console.log(`\n${pass} passed, ${failures.length} failed`);
  if (failures.length) {
    console.log('\nFailures:');
    for (const f of failures) console.log(`  - ${f}`);
    process.exit(1);
  }
}

// A1, A2, A3, A22 — §8 #1.
async function checkCatalogueAndSpawn(a, projectId) {
  console.log('\n-- catalogue + spawn (A1, A2, A3, A22; §8 #1) --');

  const agents = await a.get('/api/acp/agents');
  // One entry per script, plus the deliberately unlaunchable one used below.
  check(
    'agent catalogue reports the seeded mocks',
    agents.body?.length === SCRIPTS.length + 1,
    JSON.stringify(agents.body?.map((x) => x.id)),
  );

  const spawned = await a.post('/api/acp/spawn', { agentId: 'mock-chunks', projectId });
  check('A1 spawn → 201', spawned.status === 201, JSON.stringify(spawned.body));
  check(
    'A1 handshake reports the agent name',
    spawned.body?.agentInfo?.name === 'mock-acp-agent',
    JSON.stringify(spawned.body?.agentInfo),
  );
  check(
    'A1 agentCapabilities present',
    spawned.body?.agentCapabilities !== undefined &&
      spawned.body?.agentCapabilities !== null,
    JSON.stringify(spawned.body?.agentCapabilities),
  );

  const listed = await a.get('/api/acp');
  const row = (listed.body ?? []).find((c) => c.id === spawned.body?.id);
  check('A1 connection is listed', row !== undefined, JSON.stringify(listed.body));

  check('A1 agent process is alive', pidsMatching('mock_acp_agent').length > 0);

  // A22 + [INVENTED-11] (§8 #10): capabilities travel one way and are never
  // echoed back, so the only observable record is the agent reporting what it
  // was handed. The mock logs them to stderr at `initialize`, which is also the
  // proof the stderr endpoint captures something real.
  const stderr = await a.get(`/api/acp/${spawned.body?.id}/stderr`);
  const captured = stderr.body?.stderr ?? '';
  check('[INVENTED-11] stderr is captured and readable', stderr.status === 200 &&
    captured.includes('clientCapabilities'), `${stderr.status} ${JSON.stringify(captured)}`);
  check('A22 client advertised fs.* true and terminal false',
    captured.includes('readTextFile=true writeTextFile=true terminal=false'),
    JSON.stringify(captured));

  const created = await a.post(`/api/projects/${projectId}/sessions`, {
    connectionId: spawned.body?.id,
  });
  check('A3 session → 201', created.status === 201, JSON.stringify(created.body));
  // The agent's id, verbatim — the mock answers `session/new` with this exact
  // string, so a server that minted its own id instead would fail here.
  check(
    'A3 agentSessionId comes from the agent',
    created.body?.agentSessionId === 'mock-session-1',
    JSON.stringify(created.body?.agentSessionId),
  );
  check(
    'A3 the id the client is given is Spec ADE own, not the agent one',
    typeof created.body?.id === 'string' && created.body.id !== created.body.agentSessionId,
    JSON.stringify(created.body?.id),
  );

  const unknown = await a.post('/api/acp/spawn', { agentId: 'mock-nope', projectId });
  check('unknown agent id → 404', unknown.status === 404, JSON.stringify(unknown.body));

  // A2: the executable is missing, so the handshake can never happen.
  const before = (await a.get('/api/acp')).body?.length ?? 0;
  const bad = await a.post('/api/acp/spawn', { agentId: 'mock-missing-binary', projectId });
  check('A2 unlaunchable command → 502', bad.status === 502, JSON.stringify(bad.body));
  check(
    'A2 the failure detail says something usable',
    typeof bad.body?.detail === 'string' && bad.body.detail.length > 0,
    JSON.stringify(bad.body),
  );
  const after = (await a.get('/api/acp')).body?.length ?? 0;
  check('A2 a failed spawn leaves no connection behind', before === after, `${before} → ${after}`);
}

// A4, A5 — §8 #2.
async function checkStreaming(a, projectId) {
  console.log('\n-- streaming (A4, A5; §8 #2) --');

  const s = await session(a, 'chunks', projectId);
  const ws = attach(SERVER_TOKEN, s.connectionId, s.sessionId);
  await ws.open();

  const ready = await ws.waitFor((f) => f.type === 'ready', 'ready');
  check('ready reports the session and its state', ready.sessionId === s.sessionId &&
    ready.state === 'idle', JSON.stringify(ready));

  ws.send({ type: 'prompt', text: 'hello' });
  const done = await ws.waitFor((f) => f.type === 'turn_complete', 'turn_complete');
  const chunks = ws.frames.filter((f) => f.type === 'message_chunk');
  check('A4 ≥1 message_chunk arrived', chunks.length >= 1, `${chunks.length}`);
  check('A4 exactly one turn_complete', ws.frames.filter((f) => f.type === 'turn_complete').length === 1);
  check('A4 stopReason is end_turn', done.stopReason === 'end_turn', JSON.stringify(done));
  check(
    'A4 seq numbers are strictly increasing from 1',
    ws.frames
      .filter((f) => typeof f.seq === 'number' && f.type !== 'ready')
      .every((f, i, all) => (i === 0 ? f.seq >= 1 : f.seq > all[i - 1].seq)),
    JSON.stringify(ws.frames.map((f) => [f.type, f.seq])),
  );
  ws.close();

  // A5: a refusal is a normal end of turn, not an error.
  const r = await session(a, 'refusal', projectId);
  const rws = attach(SERVER_TOKEN, r.connectionId, r.sessionId);
  await rws.open();
  await rws.waitFor((f) => f.type === 'ready', 'ready');
  rws.send({ type: 'prompt', text: 'do something disallowed' });
  const refused = await rws.waitFor((f) => f.type === 'turn_complete', 'turn_complete');
  check('A5 refusal passes through as a stopReason', refused.stopReason === 'refusal',
    JSON.stringify(refused));
  check('A5 no error frame accompanies a refusal',
    rws.frames.every((f) => f.type !== 'error'),
    JSON.stringify(rws.frames.filter((f) => f.type === 'error')));
  rws.close();
}

// A6, A7, A8 — §8 #3.
async function checkToolCallsAndPlan(a, projectId) {
  console.log('\n-- tool calls + plan (A6, A7, A8; §8 #3) --');

  const s = await session(a, 'tool_call', projectId);
  const ws = attach(SERVER_TOKEN, s.connectionId, s.sessionId);
  await ws.open();
  await ws.waitFor((f) => f.type === 'ready', 'ready');
  ws.send({ type: 'prompt', text: 'read a file' });
  await ws.waitFor((f) => f.type === 'turn_complete', 'turn_complete');

  const call = ws.frames.find((f) => f.type === 'tool_call');
  const update = ws.frames.find((f) => f.type === 'tool_call_update');
  check('A6 tool_call announced with a title and kind',
    Boolean(call?.toolCall?.toolCallId) && Boolean(call?.toolCall?.title),
    JSON.stringify(call));
  // `pending` is the schema default and is skipped on the wire, so the opening
  // frame carries no `status` at all — absent means pending, per ACP. What must
  // hold is that the announcement does not claim a terminal status and the
  // update does.
  check('A6 status is not terminal on announce and completed on update',
    call?.toolCall?.status === undefined && update?.toolCall?.status === 'completed',
    JSON.stringify([call?.toolCall?.status, update?.toolCall?.status]));
  check('A6 the update is sparse: only what the agent sent',
    update !== undefined &&
      Object.keys(update.toolCall).every((k) => ['toolCallId', 'status', 'content'].includes(k)),
    JSON.stringify(update?.toolCall && Object.keys(update.toolCall)));
  check('A6 both frames name the same call',
    call?.toolCall?.toolCallId === update?.toolCall?.toolCallId);
  ws.close();

  // A7: two plans in one turn; the second must not accumulate onto the first.
  const p = await session(a, 'plan', projectId);
  const pws = attach(SERVER_TOKEN, p.connectionId, p.sessionId);
  await pws.open();
  await pws.waitFor((f) => f.type === 'ready', 'ready');
  pws.send({ type: 'prompt', text: 'plan something' });
  await pws.waitFor((f) => f.type === 'turn_complete', 'turn_complete');
  const plans = pws.frames.filter((f) => f.type === 'plan');
  check('A7 two plan events arrived', plans.length === 2, `${plans.length}`);
  check('A7 the second plan is a full replacement, not a concatenation',
    plans.length === 2 && plans[1].plan.entries.length < plans[0].plan.entries.length,
    JSON.stringify(plans.map((x) => x.plan.entries.length)));
  pws.close();

  // A8: a variant this server does not model must not kill the stream.
  const u = await session(a, 'unknown_variant', projectId);
  const uws = attach(SERVER_TOKEN, u.connectionId, u.sessionId);
  await uws.open();
  await uws.waitFor((f) => f.type === 'ready', 'ready');
  uws.send({ type: 'prompt', text: 'send something odd' });
  const uDone = await uws.waitFor((f) => f.type === 'turn_complete', 'turn_complete');
  check('A8 the turn still completes after an unmodelled update',
    uDone.stopReason === 'end_turn', JSON.stringify(uDone));
  const kinds = new Set(uws.frames.map((f) => f.type));
  check('A8 no junk event type leaked onto the wire',
    [...kinds].every((k) =>
      ['ready', 'message_chunk', 'turn_complete', 'session_state'].includes(k)),
    JSON.stringify([...kinds]));
  uws.close();
}

// A9, A10, A11 — §8 #4.
async function checkPermission(a, projectId) {
  console.log('\n-- permission (A9, A10, A11; §8 #4) --');

  const s = await session(a, 'permission', projectId);
  const ws = attach(SERVER_TOKEN, s.connectionId, s.sessionId);
  await ws.open();
  await ws.waitFor((f) => f.type === 'ready', 'ready');
  ws.send({ type: 'prompt', text: 'write a file' });

  const req = await ws.waitFor((f) => f.type === 'permission_request', 'permission_request');
  check('A9 the request carries options', Array.isArray(req.options) && req.options.length >= 2,
    JSON.stringify(req.options));
  check('A9 each option has an id, name and kind',
    (req.options ?? []).every((o) => o.optionId && o.name && o.kind),
    JSON.stringify(req.options));

  // A10 first: a made-up option must be refused and leave the request parked,
  // because answering the agent with a guess is worse than making the user retry.
  ws.send({ type: 'permission_response', requestId: req.requestId, optionId: 'invented' });
  const err = await ws.waitFor((f) => f.type === 'error', 'error for a bad optionId');
  check('A10 an unknown optionId is refused', /is not offered/i.test(err.message ?? ''),
    JSON.stringify(err));
  check('A10 the error frame carries no seq', err.seq === undefined, JSON.stringify(err));

  // Still parked, so the real answer works.
  const chosen = req.options.find((o) => o.kind?.includes('allow')) ?? req.options[0];
  ws.send({ type: 'permission_response', requestId: req.requestId, optionId: chosen.optionId });
  const resolved = await ws.waitFor((f) => f.type === 'permission_resolved', 'permission_resolved');
  check('A9/A10 the request was still answerable after the bad attempt',
    resolved.requestId === req.requestId, JSON.stringify(resolved));
  const done = await ws.waitFor((f) => f.type === 'turn_complete', 'turn_complete');
  check('A9 the agent continued after being granted permission',
    done.stopReason === 'end_turn' && ws.text().includes(chosen.optionId),
    `${JSON.stringify(done)} text=${ws.text()}`);
  ws.close();

  // A11: nobody answers. SPEC_ADE_ACP_PERMISSION_SECS=3, so the sweep has to.
  const t = await session(a, 'permission', projectId);
  const tws = attach(SERVER_TOKEN, t.connectionId, t.sessionId);
  await tws.open();
  await tws.waitFor((f) => f.type === 'ready', 'ready');
  tws.send({ type: 'prompt', text: 'write a file' });
  await tws.waitFor((f) => f.type === 'permission_request', 'permission_request');
  const timedOut = await tws.waitFor(
    (f) => f.type === 'permission_resolved',
    'permission_resolved by timeout',
    20_000,
  );
  check('A11 an unanswered request resolves as cancelled', timedOut.outcome === 'cancelled',
    JSON.stringify(timedOut));
  const tDone = await tws.waitFor((f) => f.type === 'turn_complete', 'turn_complete');
  check('A11 the agent was told, and finished its turn',
    tDone.stopReason === 'end_turn' && tws.text().includes('cancelled'),
    `${JSON.stringify(tDone)} text=${tws.text()}`);
  tws.close();
}

// A16, A17, A18 — §8 #4, #8.
async function checkFsBridge(a, projectId, fixture) {
  console.log('\n-- fs bridge (A16, A17, A18; §8 #4, #8) --');

  const s = await session(a, 'fs_read', projectId);
  const ws = attach(SERVER_TOKEN, s.connectionId, s.sessionId);
  await ws.open();
  await ws.waitFor((f) => f.type === 'ready', 'ready');
  ws.send({ type: 'prompt', text: 'read the fixture' });
  await ws.waitFor((f) => f.type === 'turn_complete', 'turn_complete');
  // `line(2).limit(2)` over one/two/three/four: 1-based, so exactly two+three.
  check('A16 read honours 1-based line and limit', ws.text() === 'read_ok:two\nthree\n',
    JSON.stringify(ws.text()));
  ws.close();

  // A17 / §8 #8: an absolute path outside the project must be refused, and no
  // content may come back — a guard that logs but still reads is not a guard.
  const e = await session(a, 'fs_read', projectId);
  const ews = attach(SERVER_TOKEN, e.connectionId, e.sessionId);
  await ews.open();
  await ews.waitFor((f) => f.type === 'ready', 'ready');
  ews.send({ type: 'prompt', text: 'escape the root' });
  await ews.waitFor((f) => f.type === 'turn_complete', 'turn_complete');
  check('A17 a read outside the project root is refused',
    ews.text().startsWith('read_refused:'), JSON.stringify(ews.text()));
  check('A17 no file content leaked', !ews.text().includes('root:'), JSON.stringify(ews.text()));
  ews.close();

  // A18: the write must land on disk, not just be acknowledged.
  const w = await session(a, 'fs_write', projectId);
  const wws = attach(SERVER_TOKEN, w.connectionId, w.sessionId);
  await wws.open();
  await wws.waitFor((f) => f.type === 'ready', 'ready');
  wws.send({ type: 'prompt', text: 'write a file' });
  await wws.waitFor((f) => f.type === 'turn_complete', 'turn_complete');
  check('A18 the agent reports the write succeeded', wws.text() === 'write_ok',
    JSON.stringify(wws.text()));
  const written = join(fixture, 'agent-wrote.txt');
  check('A18 the file exists on disk', existsSync(written));
  check('A18 the file has the agent content',
    existsSync(written) && readFileSync(written, 'utf8') === 'written by the agent\n',
    existsSync(written) ? JSON.stringify(readFileSync(written, 'utf8')) : 'missing');
  wws.close();
}

// A14, A15 — §8 #5.
async function checkCancelAndBusy(a, projectId) {
  console.log('\n-- cancel + busy (A14, A15; §8 #5) --');

  const s = await session(a, 'slow', projectId);
  const ws = attach(SERVER_TOKEN, s.connectionId, s.sessionId);
  await ws.open();
  await ws.waitFor((f) => f.type === 'ready', 'ready');
  ws.send({ type: 'prompt', text: 'take your time' });
  // Wait for the turn to be genuinely running before interfering with it.
  await ws.waitFor((f) => f.type === 'message_chunk', 'first chunk');

  // A15: a second prompt must be refused without touching the running turn.
  ws.send({ type: 'prompt', text: 'and another thing' });
  const busy = await ws.waitFor((f) => f.type === 'error', 'busy error');
  check('A15 a second prompt while prompting is refused',
    /already in progress/i.test(busy.message ?? ''), JSON.stringify(busy));
  check('A15 the refusal carries no seq: it answers a frame, not the log',
    busy.seq === undefined, JSON.stringify(busy));

  // A14: cancel mid-turn.
  ws.send({ type: 'cancel' });
  const done = await ws.waitFor((f) => f.type === 'turn_complete', 'turn_complete');
  check('A14 cancel yields stopReason cancelled', done.stopReason === 'cancelled',
    JSON.stringify(done));
  check('A15 the running turn produced exactly one turn_complete',
    ws.frames.filter((f) => f.type === 'turn_complete').length === 1);
  ws.close();
}

// A12, A13, A21 — §8 #6.
async function checkReplay(a, projectId) {
  console.log('\n-- replay (A12, A13, A21; §8 #6) --');

  const s = await session(a, 'chunks', projectId);
  const first = attach(SERVER_TOKEN, s.connectionId, s.sessionId);
  await first.open();
  await first.waitFor((f) => f.type === 'ready', 'ready');
  first.send({ type: 'prompt', text: 'hello' });
  await first.waitFor((f) => f.type === 'turn_complete', 'turn_complete');

  const all = first.frames.filter((f) => f.type !== 'ready' && typeof f.seq === 'number');
  const fullText = first.text();
  const cut = all[Math.floor(all.length / 2)].seq;
  first.close();
  await sleep(200);

  // A21: closing a socket must not kill the agent.
  const listed = await a.get('/api/acp');
  check('A21 the connection survives its socket closing',
    (listed.body ?? []).some((c) => c.id === s.connectionId), JSON.stringify(listed.body));

  // A12: resume from the midpoint and expect exactly the tail.
  const resumed = attach(SERVER_TOKEN, s.connectionId, s.sessionId, cut);
  await resumed.open();
  const ready = await resumed.waitFor((f) => f.type === 'ready', 'ready');
  const replayed = resumed.frames.filter((f) => f.type !== 'ready' && typeof f.seq === 'number');
  check('A12 replay starts right after the cursor',
    replayed.length > 0 && replayed[0].seq === cut + 1,
    JSON.stringify([cut, replayed.map((f) => f.seq)]));
  check('A12 no event before the cursor is resent',
    replayed.every((f) => f.seq > cut), JSON.stringify(replayed.map((f) => f.seq)));
  check('A12 no duplicate seq in the replay',
    new Set(replayed.map((f) => f.seq)).size === replayed.length);
  check('A12 the replay reaches the log head', ready.seq === all.at(-1).seq,
    `${ready.seq} vs ${all.at(-1).seq}`);
  check('A12 nothing was lost: cursor + replay reconstructs the reply',
    all.filter((f) => f.seq <= cut && f.type === 'message_chunk').map((f) => f.text).join('') +
      replayed.filter((f) => f.type === 'message_chunk').map((f) => f.text).join('') === fullText,
    JSON.stringify(fullText));
  check('A13 a current cursor produces no truncated frame',
    replayed.every((f) => f.type !== 'truncated'));

  // A cursor past the head: nothing to replay, and nothing may be invented to
  // fill it. (The other half of A13 — a cursor *older* than the log's start,
  // which yields `truncated` — needs a log pruned by size. That is not
  // reachable from here without megabytes of traffic, so it stays covered by
  // the unit test over `EventLog::replay_from`.)
  const ahead = attach(SERVER_TOKEN, s.connectionId, s.sessionId, all.at(-1).seq + 500);
  await ahead.open();
  await ahead.waitFor((f) => f.type === 'ready', 'ready');
  check('a cursor past the head replays nothing rather than inventing events',
    ahead.frames.every((f) => f.type === 'ready'),
    JSON.stringify(ahead.frames.map((f) => f.type)));
  ahead.close();
  resumed.close();
}

// A19, A20 — §8 #7.
async function checkLifecycle(a, projectId) {
  console.log('\n-- lifecycle (A19, A20; §8 #7) --');

  // A19: the agent dies on its own, which on the wire is the same transport EOF
  // an externally-killed process produces.
  const d = await session(a, 'die_after_handshake', projectId);
  const dws = attach(SERVER_TOKEN, d.connectionId, d.sessionId);
  await dws.open();
  await dws.waitFor((f) => f.type === 'ready', 'ready');
  dws.send({ type: 'prompt', text: 'go' });
  const closedEvent = await dws.waitFor(
    (f) => f.type === 'connection_closed',
    'connection_closed',
  );
  check('A19 an agent exiting emits connection_closed',
    typeof closedEvent.reason === 'string' && closedEvent.reason.length > 0,
    JSON.stringify(closedEvent));

  // A19: it must also leave the list, not linger as a dead row.
  let gone = false;
  for (let i = 0; i < 60 && !gone; i++) {
    const list = await a.get('/api/acp');
    gone = !(list.body ?? []).some((c) => c.id === d.connectionId);
    if (!gone) await sleep(200);
  }
  check('A19 a dead connection leaves GET /api/acp', gone);
  dws.close();

  // A20. Every mock runs the same executable, so `ps` cannot say which pid
  // belongs to which connection: the observable claim is that exactly one agent
  // process disappears and the others are untouched. (The process-*group* half of
  // A20 is not observable through this mock — it spawns no children of its own —
  // so it stays covered by the integration test and the §8 manual run.)
  const k = await session(a, 'chunks', projectId);
  const kws = attach(SERVER_TOKEN, k.connectionId, k.sessionId);
  await kws.open();
  await kws.waitFor((f) => f.type === 'ready', 'ready');
  const pidsBefore = pidsMatching('mock_acp_agent');
  check('A20 the agent process is running before the kill', pidsBefore.length > 0);

  const deleted = await a.del(`/api/acp/${k.connectionId}`);
  check('A20 DELETE → 204', deleted.status === 204, `${deleted.status}`);

  let dropped = [];
  for (let i = 0; i < 50 && dropped.length === 0; i++) {
    dropped = pidsBefore.filter((pid) => !alive(pid));
    if (dropped.length === 0) await sleep(100);
  }
  check('A20 exactly one agent process died', dropped.length === 1,
    JSON.stringify({ before: pidsBefore, dropped }));
  check('A20 the other connections were left alone',
    pidsBefore.filter((pid) => alive(pid)).length === pidsBefore.length - dropped.length);

  let unlisted = false;
  for (let i = 0; i < 30 && !unlisted; i++) {
    const list = await a.get('/api/acp');
    unlisted = !(list.body ?? []).some((c) => c.id === k.connectionId);
    if (!unlisted) await sleep(100);
  }
  check('A20 the killed connection is unlisted', unlisted);

  // The socket must not hang open on a connection that no longer exists.
  let socketClosed = false;
  for (let i = 0; i < 50 && !socketClosed; i++) {
    if (kws.closed) socketClosed = true;
    else await sleep(100);
  }
  check('A20 the attached socket closes', socketClosed);
}

// [INVENTED-10] — §8 #9.
async function checkIdleReaper(a, projectId) {
  console.log('\n-- idle reaper ([INVENTED-10]; §8 #9) --');

  // SPEC_ADE_ACP_IDLE_SECS=4. This connection gets a session but no socket, so
  // nothing holds a watcher guard and the reaper is allowed to collect it.
  const idle = await a.post('/api/acp/spawn', { agentId: 'mock-chunks', projectId });
  const idleId = idle.body?.id;
  check('a fresh connection is listed', typeof idleId === 'string');

  // A watched connection under the same clock, so a reaper that ignores the
  // "in use" predicate fails here instead of silently killing live chat tabs.
  const watched = await session(a, 'chunks', projectId);
  const ws = attach(SERVER_TOKEN, watched.connectionId, watched.sessionId);
  await ws.open();
  await ws.waitFor((f) => f.type === 'ready', 'ready');

  let reaped = false;
  let watchedStillThere = true;
  for (let i = 0; i < 100 && !reaped; i++) {
    const list = await a.get('/api/acp');
    const ids = (list.body ?? []).map((c) => c.id);
    reaped = !ids.includes(idleId);
    watchedStillThere = ids.includes(watched.connectionId);
    if (!reaped) await sleep(200);
  }
  check('§8 #9 an unwatched idle connection is reaped', reaped);
  check('§8 #9 a watched connection survives the same sweep', watchedStillThere);
  check('§8 #9 reaped means gone, not just unlisted',
    (await a.get(`/api/acp/${idleId}/stderr`)).status === 404);

  // And the survivor still works, so "survives" means alive, not just listed.
  ws.send({ type: 'prompt', text: 'still there?' });
  const done = await ws.waitFor((f) => f.type === 'turn_complete', 'turn_complete');
  check('§8 #9 the survivor still answers a prompt', done.stopReason === 'end_turn',
    JSON.stringify(done));
  ws.close();
}

/** The embedded SPA must be this phase's build, not a stale one. */
async function checkEmbeddedBundle() {
  console.log('\n-- embedded bundle --');
  const index = await fetch(`${BASE}/`);
  const html = await index.text();
  const asset = html.match(/\/assets\/index-[\w-]+\.js/);
  check('index.html references a JS bundle', asset !== null);
  if (!asset) return;

  const js = await (await fetch(`${BASE}${asset[0]}`)).text();
  // Literals that survive minification and exist only in the SPEC-003 layer.
  // Runtime-assembled URLs never appear verbatim, so they cannot be markers.
  const markers = ['permission_response', 'tool_call_update', 'connection_closed', '+ Session'];
  const missing = markers.filter((m) => !js.includes(m));
  check('the embedded bundle is the SPEC-003 build', missing.length === 0,
    `missing: ${JSON.stringify(missing)}`);
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
