// SPEC-004 §8.1 verification — bundle artifacts + the agent→client wire boundary.
//
// Usage (build the SPA first so the binary embeds the current frontend):
//   cd src/web && npm run build
//   cd ../server && cargo build --release
//   node scripts/verify-spec-004.mjs            # release binary
//   node scripts/verify-spec-004.mjs --debug    # debug binary
//
// Node has no DOM, so this script deliberately does NOT claim to verify rendering.
// It checks the two things that ARE binary on real artifacts:
//
//   1. The bundle (B17, B28): mermaid and katex are separate chunks reached by
//      dynamic `import()`, and the entry chunk contains no mermaid/d3 payload.
//   2. The wire: markdown, math, a diagram and four XSS payloads travel from a real
//      spawned agent through the real WS route to the client BYTE-FOR-BYTE. This is
//      the boundary that matters — the server must not "helpfully" escape anything,
//      because that would mask whether the frontend's own defences work.
//
// The rendering half of §6 (B1–B16) is covered by 397 vitest tests, and the four
// checks that need a real browser stay a named checklist in §8.2 / docs/STATUS.md.
// Nothing here pretends otherwise.
//
// `SPEC_ADE_DATA_DIR` points at a tempdir, so the developer's real settings.json is
// never touched.

import { spawn } from 'node:child_process';
import { mkdtempSync, writeFileSync, readFileSync, readdirSync, existsSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const REPO = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const PROFILE = process.argv.includes('--debug') ? 'debug' : 'release';
const TARGET = join(REPO, 'src/server/target', PROFILE);
const BIN = join(TARGET, 'spec-ade-server');
const MOCK = join(TARGET, 'mock_acp_agent');
const DIST = join(REPO, 'src/web/dist');
const ASSETS = join(DIST, 'assets');
// Distinct from verify-spec-003's 7392: the two scripts must be runnable together.
const PORT = 7394;
const BASE = `http://127.0.0.1:${PORT}`;
const DATA_DIR = mkdtempSync(join(tmpdir(), 'spec-ade-verify4-data-'));

let pass = 0;
const failures = [];

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
 * The `rich_markdown` chunks, copied from `mock_acp_agent.rs`'s const.
 *
 * Copied rather than derived: the assertion is that these exact bytes survive the
 * round trip, so re-deriving them from the same source would let a shared mistake
 * pass. If the mock changes, this must be updated deliberately.
 */
const EXPECTED_CHUNKS = [
  '# Heading\n\nSome **bold** and a table:\n\n',
  '| lang | ok |\n| --- | --- |\n| rust | yes |\n\n',
  '```rust\nfn main() { println!("hi"); }\n```\n\n',
  'Inline $x^2 + y^2 = z^2$ and echo $PATH in prose.\n\n$$\\int_0^1 x\\,dx = \\frac{1}{2}$$\n\n',
  '```mermaid\ngraph TD\n  A[Start] --> B[End]\n```\n\n',
  "<script>alert('xss')</script>\n\n",
  "<img src=x onerror=alert('xss')>\n\n",
  "[click me](javascript:alert('xss'))\n\n",
  '<iframe src="data:text/html,<script>alert(1)</script>"></iframe>\n',
];

const EXPECTED_THOUGHT = "Reasoning with a payload: <img src=x onerror=alert('thought')>";

// ---------------------------------------------------------------------------
// 1. Bundle (B17, B28)
// ---------------------------------------------------------------------------

function checkBundle() {
  console.log('\n-- §8.1 #1 bundle (B17, B28) --');

  if (!existsSync(ASSETS)) {
    check('dist/assets exists', false, `${ASSETS} missing — run \`npm run build\` in src/web`);
    return;
  }

  const files = readdirSync(ASSETS);
  const entryName = files.find((f) => /^index-[\w-]+\.js$/.test(f));
  check('an entry chunk exists', entryName !== undefined, files.join(', '));
  if (!entryName) return;

  const entry = readFileSync(join(ASSETS, entryName), 'utf8');

  // B28: mermaid's own payload must not be in the entry chunk. Markers are strings
  // that survive minification and belong to mermaid/d3 internals rather than to my
  // wrapper — `"mermaid"` itself appears in the entry as the fence-language check
  // and as the dynamic import path, so it is NOT a usable marker.
  // NB `flowchart` is NOT a usable marker: it is a key in this app's own
  // `mermaid.initialize({flowchart: {htmlLabels: false}})` config, which is in the
  // entry chunk on purpose. Same reason `"mermaid"` is excluded — it appears as the
  // fence-language comparison and as the dynamic import path.
  const forbidden = [
    'sequenceDiagram',
    'd3-selection',
    'cytoscape',
    'ganttDiagram',
    'stateDiagram',
    'katex.min.css',
  ];
  const leaked = forbidden.filter((m) => entry.includes(m));
  check('B28 entry chunk carries no mermaid/d3 payload', leaked.length === 0,
    `leaked markers: ${JSON.stringify(leaked)}`);

  // B17: reached by dynamic import, not statically linked.
  const mermaidImport = entry.match(/import\("\.\/(mermaid[\w.-]*\.js)"\)/);
  check('B17 mermaid is a dynamic import from the entry chunk', mermaidImport !== null);
  const katexImport = entry.match(/import\("\.\/(katex[\w.-]*\.js)"\)/);
  check('B17 katex is a dynamic import from the entry chunk', katexImport !== null);

  // And the chunks those imports name must actually exist on disk, otherwise the
  // import would 404 at runtime and the feature would fail only in production.
  if (mermaidImport) {
    check('the mermaid chunk exists', files.includes(mermaidImport[1]), mermaidImport[1]);
  }
  if (katexImport) {
    const name = katexImport[1];
    check('the katex chunk exists', files.includes(name), name);
    if (files.includes(name)) {
      const katex = readFileSync(join(ASSETS, name), 'utf8');
      check('the katex chunk really contains KaTeX', katex.includes('KaTeX parse error'));
    }
  }

  // KaTeX's font CSS must be lazy too: it is ~30 kB and a conversation with no
  // math should not pay for it.
  const katexCss = files.find((f) => /^katex-[\w-]+\.css$/.test(f));
  check('katex CSS is its own chunk', katexCss !== undefined, files.filter((f) => f.endsWith('.css')).join(', '));
  const entryCss = files.find((f) => /^index-[\w-]+\.css$/.test(f));
  if (entryCss) {
    const css = readFileSync(join(ASSETS, entryCss), 'utf8');
    check('entry CSS does not inline the KaTeX fonts', !css.includes('KaTeX_Main'));
  }

  // highlight.js must be the 16-language manual build, not all 192. `lib/core`
  // plus registrations leaves no trace of the languages we did not register.
  const unregistered = ['fortran', 'erlang', 'perl'].filter((l) => entry.includes(`"${l}"`));
  check('highlight.js is the trimmed build, not all 192 languages', unregistered.length === 0,
    `found: ${JSON.stringify(unregistered)}`);

  // Report sizes rather than asserting a ceiling: a hard limit here would fail on
  // an unrelated dependency bump and teach the reader to ignore it.
  const totalJs = files.filter((f) => f.endsWith('.js'))
    .reduce((n, f) => n + readFileSync(join(ASSETS, f)).length, 0);
  const entryBytes = readFileSync(join(ASSETS, entryName)).length;
  console.log(`  info entry chunk ${entryBytes.toLocaleString()} B, all JS ${totalJs.toLocaleString()} B`);
  console.log(`  info lazy chunks: ${files.filter((f) => /mermaid|katex|Diagram|diagram/.test(f)).length}`);
}

// ---------------------------------------------------------------------------
// 2. Toolchain (§8.1 #2)
// ---------------------------------------------------------------------------

function checkToolchain() {
  console.log('\n-- §8.1 #2 toolchain --');
  const pkg = JSON.parse(readFileSync(join(REPO, 'src/web/package.json'), 'utf8'));
  const dev = pkg.devDependencies ?? {};
  const deps = pkg.dependencies ?? {};

  check('@types/markdown-it is installed', '@types/markdown-it' in dev, JSON.stringify(Object.keys(dev)));
  // The spec's §2 note: this was recorded as TODO(phase-4) and must NOT have been
  // installed — it is unmaintained (last release 2017, markdown-it 8 era).
  check('markdown-it-katex is absent, as decided in §5.4',
    !('markdown-it-katex' in deps) && !('markdown-it-katex' in dev));

  for (const name of ['markdown-it', 'dompurify', 'katex', 'mermaid', 'highlight.js']) {
    check(`${name} is a runtime dependency`, name in deps, JSON.stringify(deps[name]));
  }

  // `npm run build` must gate on types. A build that skips vue-tsc would let a
  // type error ship, so the script asserts the gate exists rather than trusting it.
  check('npm run build runs vue-tsc --noEmit',
    (pkg.scripts?.build ?? '').includes('vue-tsc'), pkg.scripts?.build ?? '(none)');
}

// ---------------------------------------------------------------------------
// 3. Wire boundary (§8.1 #3)
// ---------------------------------------------------------------------------

function writeSettings() {
  const settings = {
    acp_agents: [
      {
        id: 'mock-rich_markdown',
        name: 'Mock (rich_markdown)',
        command: MOCK,
        args: [],
        env: { MOCK_ACP_SCRIPT: 'rich_markdown' },
      },
    ],
  };
  writeFileSync(join(DATA_DIR, 'settings.json'), JSON.stringify(settings, null, 2));
}

async function startServer() {
  const proc = spawn(BIN, ['--port', String(PORT)], {
    env: { ...process.env, SPEC_ADE_DATA_DIR: DATA_DIR },
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
      if ((await fetch(`${BASE}/api/health`)).ok) break;
    } catch {}
    await sleep(100);
  }
  return { proc, token, log: () => log };
}

function api(token) {
  const headers = { 'x-spec-ade-token': token, 'content-type': 'application/json' };
  return {
    async get(path) {
      const r = await fetch(`${BASE}${path}`, { headers });
      return { status: r.status, body: await safeJson(r) };
    },
    async post(path, body) {
      const r = await fetch(`${BASE}${path}`, { method: 'POST', headers, body: JSON.stringify(body) });
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

function attach(token, connectionId, sessionId) {
  const params = new URLSearchParams({ sessionId, token });
  const ws = new WebSocket(`ws://127.0.0.1:${PORT}/api/acp/${connectionId}/ws?${params}`);
  const frames = [];
  let closed = null;
  ws.addEventListener('message', (e) => frames.push(JSON.parse(e.data)));
  ws.addEventListener('close', (e) => {
    closed = { code: e.code, reason: e.reason };
  });

  return {
    frames,
    send: (payload) => ws.send(JSON.stringify(payload)),
    async waitFor(pred, label, timeoutMs = 15_000) {
      const deadline = Date.now() + timeoutMs;
      while (Date.now() < deadline) {
        const hit = frames.find(pred);
        if (hit) return hit;
        if (closed) {
          throw new Error(`socket closed (${closed.code}) waiting for ${label}`);
        }
        await sleep(50);
      }
      throw new Error(`timed out waiting for ${label}; frames: ${JSON.stringify(frames)}`);
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
    close: () => ws.close(),
  };
}

async function checkWire(a, projectId) {
  console.log('\n-- §8.1 #3 wire boundary --');

  const spawned = await a.post('/api/acp/spawn', { agentId: 'mock-rich_markdown', projectId });
  if (spawned.status !== 201) {
    throw new Error(`spawn failed: ${spawned.status} ${JSON.stringify(spawned.body)}`);
  }
  const created = await a.post(`/api/projects/${projectId}/sessions`, {
    connectionId: spawned.body.id,
  });
  if (created.status !== 201) {
    throw new Error(`session failed: ${created.status} ${JSON.stringify(created.body)}`);
  }

  const ws = attach(a.token, spawned.body.id, created.body.id);
  await ws.open();
  ws.send({ type: 'prompt', text: 'show me everything' });
  await ws.waitFor((f) => f.type === 'turn_complete', 'turn_complete');

  const chunks = ws.frames.filter((f) => f.type === 'message_chunk').map((f) => f.text);
  const joined = chunks.join('');

  // Byte-for-byte, per chunk: a concatenated-only assertion would pass even if the
  // server had split or re-joined the stream differently.
  check('every chunk arrives byte-for-byte',
    chunks.length === EXPECTED_CHUNKS.length &&
      EXPECTED_CHUNKS.every((expected, i) => chunks[i] === expected),
    `got ${chunks.length} chunks, expected ${EXPECTED_CHUNKS.length}`);

  // Each payload named individually, so a failure says WHICH one the server ate.
  const payloads = {
    'B3 <script> tag': "<script>alert('xss')</script>",
    'B4 img onerror': "<img src=x onerror=alert('xss')>",
    'B5 javascript: link': "[click me](javascript:alert('xss'))",
    'B6 data: iframe': '<iframe src="data:text/html,<script>alert(1)</script>"></iframe>',
  };
  for (const [label, payload] of Object.entries(payloads)) {
    check(`${label} reaches the client unmodified`, joined.includes(payload));
  }

  // The server must not HTML-escape on the way through. If it did, the frontend's
  // sanitizer would never see a real payload and its tests would prove nothing.
  check('the server does not HTML-escape agent text',
    !joined.includes('&lt;script&gt;') && !joined.includes('&amp;lt;'));

  const markdown = {
    'code fence with language': '```rust\nfn main() { println!("hi"); }\n```',
    'GFM table': '| lang | ok |\n| --- | --- |\n| rust | yes |',
    'inline math': '$x^2 + y^2 = z^2$',
    'block math': '$$\\int_0^1 x\\,dx = \\frac{1}{2}$$',
    'mermaid fence': '```mermaid\ngraph TD\n  A[Start] --> B[End]\n```',
    'a bare $PATH in prose': 'echo $PATH in prose',
  };
  for (const [label, text] of Object.entries(markdown)) {
    check(`${label} survives the round trip`, joined.includes(text));
  }

  // Backslashes in TeX are the classic casualty of an over-eager JSON layer.
  check('TeX backslashes are not doubled or dropped',
    joined.includes('\\int_0^1 x\\,dx') && !joined.includes('\\\\int'));

  // Thoughts go through the same renderer, so they are the same XSS surface.
  const thoughts = ws.frames.filter((f) => f.type === 'thought_chunk').map((f) => f.text).join('');
  check('a thought payload also arrives unmodified', thoughts === EXPECTED_THOUGHT,
    JSON.stringify(thoughts));

  // Every frame must be addressed to the session that asked, or a payload could
  // surface in the wrong transcript.
  const strayIds = [...new Set(ws.frames.map((f) => f.sessionId).filter((id) => id !== undefined))]
    .filter((id) => id !== created.body.id);
  check('no frame is addressed to another session', strayIds.length === 0, JSON.stringify(strayIds));

  // Log events must be strictly increasing: the frontend uses `seq` as a resume
  // cursor, and a repeat would replay a payload into the transcript twice.
  //
  // `ready` is excluded because its `seq` is the replay CURSOR, not a log position
  // (routes/acp.rs sends `"seq": cursor`) — it deliberately equals the last replayed
  // event's seq, so including it here would flag correct behaviour as a bug.
  const ready = ws.frames.find((f) => f.type === 'ready');
  const logSeqs = ws.frames
    .filter((f) => f.type !== 'ready' && typeof f.seq === 'number')
    .map((f) => f.seq);
  check('log event seq is strictly increasing',
    logSeqs.every((n, i) => i === 0 || n > logSeqs[i - 1]), JSON.stringify(logSeqs));
  check('no log event seq is reused', new Set(logSeqs).size === logSeqs.length);
  // The real invariant, independent of how many events happened to be replayed:
  // `ready.seq` is the high-water mark of everything sent before it, and every
  // event after it is strictly newer. That is what makes the cursor safe to resume
  // from without duplicating or skipping a frame.
  const readyIndex = ws.frames.findIndex((f) => f.type === 'ready');
  const before = ws.frames.slice(0, readyIndex).map((f) => f.seq).filter((n) => typeof n === 'number');
  const after = ws.frames.slice(readyIndex + 1).map((f) => f.seq).filter((n) => typeof n === 'number');
  check('ready.seq is the high-water mark of the replayed prefix',
    ready !== undefined && ready.seq === (before.length ? Math.max(...before) : 0),
    JSON.stringify({ ready: ready?.seq, before }));
  check('every event after ready is newer than the cursor',
    after.every((n) => n > ready.seq), JSON.stringify({ ready: ready?.seq, after }));

  ws.close();

  // Replay: reattaching must hand back the same bytes, since a reloaded browser
  // rebuilds the whole transcript from the log.
  const replay = attach(a.token, spawned.body.id, created.body.id);
  await replay.open();
  await replay.waitFor((f) => f.type === 'turn_complete', 'replayed turn_complete');
  const replayed = replay.frames
    .filter((f) => f.type === 'message_chunk')
    .map((f) => f.text)
    .join('');
  check('a replayed transcript is byte-identical', replayed === joined,
    `${replayed.length} B vs ${joined.length} B`);
  replay.close();
}

// ---------------------------------------------------------------------------
// 4. Embedded bundle is this phase's build
// ---------------------------------------------------------------------------

async function checkEmbedded() {
  console.log('\n-- embedded bundle --');
  const html = await (await fetch(`${BASE}/`)).text();
  const asset = html.match(/\/assets\/index-[\w-]+\.js/);
  check('index.html references a JS bundle', asset !== null);
  if (!asset) return;

  const js = await (await fetch(`${BASE}${asset[0]}`)).text();
  // Literals that survive minification and exist only in the SPEC-004 layer.
  const markers = ['md__mermaid', 'Đang suy nghĩ…', 'Kế hoạch', 'chat: KaTeX failed to load'];
  const missing = markers.filter((m) => !js.includes(m));
  check('the embedded bundle is the SPEC-004 build', missing.length === 0,
    `missing: ${JSON.stringify(missing)}`);

  // The dynamic chunks must be served, not just present on disk.
  const mermaidChunk = js.match(/import\("\.\/(mermaid[\w.-]*\.js)"\)/);
  if (mermaidChunk) {
    const r = await fetch(`${BASE}/assets/${mermaidChunk[1]}`);
    check('the server serves the lazy mermaid chunk', r.ok, `status ${r.status}`);
  }
}

// ---------------------------------------------------------------------------

async function main() {
  for (const [label, path] of [
    ['server binary', BIN],
    ['mock agent binary', MOCK],
  ]) {
    if (!existsSync(path)) {
      throw new Error(`${label} not found: ${path}\nbuild it first (see the header of this file)`);
    }
  }

  checkBundle();
  checkToolchain();

  const fixture = mkdtempSync(join(tmpdir(), 'spec-ade-verify4-fixture-'));
  writeSettings();
  const server = await startServer();
  const a = api(server.token);
  a.token = server.token;

  try {
    const project = await a.post('/api/projects', { path: fixture });
    if (project.status !== 201) {
      throw new Error(`project failed: ${project.status} ${JSON.stringify(project.body)}`);
    }
    await checkWire(a, project.body.id);
    await checkEmbedded();
  } finally {
    server.proc.kill('SIGTERM');
    await new Promise((r) => server.proc.once('exit', r));
    rmSync(fixture, { recursive: true, force: true });
    rmSync(DATA_DIR, { recursive: true, force: true });
  }

  console.log(`\n${pass} passed, ${failures.length} failed`);
  if (failures.length) {
    for (const f of failures) console.log(`  - ${f}`);
    process.exit(1);
  }
  console.log('\nSPEC-004 §8.1 verified. §8.2 (real browser) remains a named debt in docs/STATUS.md.');
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
