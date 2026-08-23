// SPEC-008 §8.1 verification — real layout persistence over the real release binary.
//
// Usage (the SPA must be built first so Rust embeds it):
//   npm --prefix src/web run build
//   cargo build --release --manifest-path src/server/Cargo.toml
//   node scripts/verify-spec-008.mjs
//
// The pane tree is OPAQUE to the server (SPEC-008 §3.3): it stores and returns the
// JSON verbatim and never parses the grammar. So these checks are structure-blind
// on the tree itself and instead prove (a) round-trip fidelity, (b) top-level field
// merge, (c) the two real guards — the 256 KiB cap and the registered-project-key
// check, (d) disk persistence, (e) cascade on project delete, (f) the token gate on
// both verbs. DOM/drag/xterm behaviour cannot cross HTTP and is verified by hand
// (§8.2, recorded in docs/STATUS.md).
//
// This script owns disposable data and project directories and only ever kills a
// child it spawned itself; it never mutates the source tree. The token is seeded
// into settings.json before boot, so the verifier knows it without scraping logs.

import { spawn } from 'node:child_process';
import { existsSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { randomUUID } from 'node:crypto';

const REPO = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const PROFILE = process.argv.includes('--debug') ? 'debug' : 'release';
const TARGET = join(REPO, 'src/server/target', PROFILE);
const BIN = join(TARGET, 'spec-ade-server');
// Distinct from verify-002..007 so every verifier may run concurrently.
const PORT = 7398;
const BASE = `http://127.0.0.1:${PORT}`;
const TOKEN = randomUUID().replaceAll('-', '');
const DATA_DIR = mkdtempSync(join(tmpdir(), 'spec-ade-verify8-data-'));
const FIXTURE = mkdtempSync(join(tmpdir(), 'spec-ade-verify8-proj-'));

let pass = 0;
const failures = [];

function check(name, ok, detail = '') {
  if (ok) {
    pass += 1;
    console.log(`  ok   ${name}`);
  } else {
    failures.push(`${name}${detail ? ` — ${detail}` : ''}`);
    console.log(`  FAIL ${name}${detail ? ` — ${detail}` : ''}`);
  }
}

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

// Order-independent deep equality. The server re-serialises the opaque tree through
// serde_json, whose default Map sorts object keys — so a JSON.stringify compare would
// false-fail on key order alone. Structural compare is what round-trip actually means.
function deepEqual(a, b) {
  if (a === b) return true;
  if (typeof a !== typeof b || a === null || b === null) return false;
  if (Array.isArray(a) || Array.isArray(b)) {
    if (!Array.isArray(a) || !Array.isArray(b) || a.length !== b.length) return false;
    return a.every((x, i) => deepEqual(x, b[i]));
  }
  if (typeof a === 'object') {
    const ka = Object.keys(a);
    const kb = Object.keys(b);
    if (ka.length !== kb.length) return false;
    return ka.every((k) => Object.prototype.hasOwnProperty.call(b, k) && deepEqual(a[k], b[k]));
  }
  return false;
}

// Seed the token the settings API deliberately will not let us set over HTTP (it is a
// snake_case top-level key on disk). Every other Settings field is #[serde(default)],
// so a one-key file boots a full server.
function seedSettings() {
  writeFileSync(join(DATA_DIR, 'settings.json'), JSON.stringify({ auth_token: TOKEN }));
}

async function startServer() {
  const proc = spawn(BIN, ['-p', String(PORT), '-H', '127.0.0.1', '--no-open'], {
    env: { ...process.env, SPEC_ADE_DATA_DIR: DATA_DIR },
    stdio: ['ignore', 'pipe', 'pipe'],
  });
  let log = '';
  proc.stdout.on('data', (c) => (log += c.toString()));
  proc.stderr.on('data', (c) => (log += c.toString()));

  for (let i = 0; i < 150; i += 1) {
    try {
      if ((await fetch(`${BASE}/api/health`)).ok) return { proc, log: () => log };
    } catch {
      // not up yet
    }
    await sleep(100);
  }
  proc.kill('SIGKILL');
  throw new Error(`server did not become ready; log:\n${log}`);
}

/** JSON fetch with the token; returns {status, body}. */
async function call(method, path, body, opts = {}) {
  const res = await fetch(`${BASE}${path}`, {
    method,
    headers: {
      Authorization: `Bearer ${TOKEN}`,
      ...(body === undefined ? {} : { 'Content-Type': 'application/json' }),
    },
    body: body === undefined ? undefined : JSON.stringify(body),
    ...opts,
  });
  let json = null;
  try {
    json = await res.json();
  } catch {
    // empty body is fine
  }
  return { status: res.status, body: json };
}

/** The split tree used for round-trip: two panes, one file tab + one terminal tab. */
function sampleTree() {
  return {
    id: 'leaf-a',
    kind: 'leaf',
    tabs: [
      { id: 'tab-1', kind: 'file', title: 'main.rs', params: { path: 'src/main.rs' } },
      { id: 'tab-2', kind: 'terminal', title: 'sh', params: {} },
    ],
    activeTabId: 'tab-1',
  };
}

// ---- §8.1 steps -------------------------------------------------------------

async function main() {
  if (!existsSync(BIN)) {
    console.error(`missing ${BIN} — build first (see header comment)`);
    process.exit(1);
  }
  seedSettings();
  const server = await startServer();

  try {
    // (1) F19 — defaults are the all-empty document.
    const first = await call('GET', '/api/layout');
    check(
      'GET /api/layout defaults to an empty document',
      first.status === 200 &&
        deepEqual(first.body, { projectLayouts: {}, lastLayout: null, layoutPresets: [] }),
      `status=${first.status} body=${JSON.stringify(first.body)}`,
    );

    // (2) Register a project so a layout key is legal.
    const proj = await call('POST', '/api/projects', { path: FIXTURE });
    const pid = proj.body?.id;
    check('project registered for layout keys', (proj.status === 200 || proj.status === 201) && typeof pid === 'string',
      `status=${proj.status}`);
    if (!pid) throw new Error('cannot continue without a project');

    // (3) PUT a real tree + template → accepted.
    const tree = sampleTree();
    const put = await call('PUT', '/api/layout', {
      projectLayouts: { [pid]: tree },
      lastLayout: tree,
    });
    check(
      'PUT with tree + lastLayout returns ok',
      put.status === 200 && deepEqual(put.body, {
        projectLayouts: { [pid]: tree },
        lastLayout: tree,
        layoutPresets: [],
      }),
      `status=${put.status} body=${JSON.stringify(put.body)}`,
    );

    // (4) F26/F27/F28 data layer — the GET returns exactly what was stored, verbatim.
    const after = await call('GET', '/api/layout');
    const gotTree = after.body?.projectLayouts?.[pid];
    check(
      'GET round-trips the tree verbatim (tabs, params, activeTabId)',
      after.status === 200 && deepEqual(gotTree, tree),
      `status=${after.status} tree=${JSON.stringify(gotTree)}`,
    );
    check('GET round-trips lastLayout', deepEqual(after.body?.lastLayout ?? null, tree));

    // (5) F20 — a PUT without projectLayouts/lastLayout must not wipe them.
    const presetOnly = await call('PUT', '/api/layout', {
      layoutPresets: [{ name: 'split-v', tree: sampleTree() }],
    });
    const merged = await call('GET', '/api/layout');
    check(
      'PUT of only layoutPresets preserves projectLayouts + lastLayout',
      presetOnly.status === 200 &&
        deepEqual(merged.body?.projectLayouts?.[pid], tree) &&
        deepEqual(merged.body?.lastLayout ?? null, tree) &&
        Array.isArray(merged.body?.layoutPresets) &&
        merged.body.layoutPresets.length === 1,
      `status=${presetOnly.status} body=${JSON.stringify(merged.body)}`,
    );

    // (6) F21 — an unregistered project id is rejected with group "layout".
    const unknown = await call('PUT', '/api/layout', {
      projectLayouts: { 'no-such-project': sampleTree() },
    });
    check(
      'unknown project key → 400 group=layout',
      unknown.status === 400 && unknown.body?.error === 'layout',
      `status=${unknown.status} body=${JSON.stringify(unknown.body)}`,
    );

    // (7) F22 — over the 256 KiB cap is rejected before parsing.
    const fat = await call('PUT', '/api/layout', {
      projectLayouts: {
        [pid]: {
          id: 'leaf-fat',
          kind: 'leaf',
          tabs: [{ id: 't', kind: 'file', title: 'f', params: { path: 'x'.repeat(300 * 1024) } }],
          activeTabId: 't',
        },
      },
    });
    check(
      '>256 KiB body → 400 group=layout',
      fat.status === 400 && fat.body?.error === 'layout',
      `status=${fat.status} body=${JSON.stringify(fat.body)}`,
    );

    // (8) F23 — the wire shape is camelCase, the disk shape snake_case; both real.
    const disk = JSON.parse(readFileSync(join(DATA_DIR, 'settings.json'), 'utf8'));
    check(
      'settings.json on disk holds project_layouts / last_layout / layout_presets',
      typeof disk.project_layouts === 'object' &&
        deepEqual(disk.project_layouts?.[pid], tree) &&
        deepEqual(disk.last_layout ?? null, tree) &&
        Array.isArray(disk.layout_presets),
      JSON.stringify(Object.keys(disk)),
    );
    // And the wire never leaks the disk names.
    const noLeak = await call('GET', '/api/layout');
    check(
      'wire document uses camelCase keys only',
      Object.keys(noLeak.body).every((k) => !k.includes('_')),
      JSON.stringify(Object.keys(noLeak.body)),
    );

    // (9) F24 — deleting the project cascades its layout away.
    const del = await call('DELETE', `/api/projects/${pid}`);
    const cascaded = await call('GET', '/api/layout');
    check(
      'DELETE project drops its layout key',
      (del.status === 200 || del.status === 204) &&
        !(pid in (cascaded.body?.projectLayouts ?? {})) &&
        deepEqual(cascaded.body?.lastLayout ?? null, tree),
      `del=${del.status} layouts=${JSON.stringify(cascaded.body?.projectLayouts ?? {})}`,
    );

    // (10) F25 — both verbs demand the token.
    const noAuthGet = await fetch(`${BASE}/api/layout`);
    const noAuthPut = await fetch(`${BASE}/api/layout`, { method: 'PUT' });
    check(
      'layout without token → 401 on GET and PUT',
      noAuthGet.status === 401 && noAuthPut.status === 401,
      `get=${noAuthGet.status} put=${noAuthPut.status}`,
    );
  } finally {
    server.proc.kill('SIGKILL');
  }

  console.log('');
  if (failures.length > 0) {
    console.error(`SPEC-008 verify: FAIL (${failures.length}/${pass + failures.length} checks failed)`);
    for (const f of failures) console.error(`  - ${f}`);
    process.exit(1);
  }
  console.log(`SPEC-008 verify: PASS (${pass} checks)`);
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});



