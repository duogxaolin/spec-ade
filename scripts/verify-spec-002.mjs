// SPEC-002 §8 runtime verification — drives the REAL binary over HTTP.
//
// Usage (build the SPA first so the binary embeds the current frontend):
//   cd src/web && npm run build
//   cd ../server && cargo build --release
//   node scripts/verify-spec-002.mjs            # release binary
//   node scripts/verify-spec-002.mjs --debug    # debug binary
//
// Two projects are registered: the spec-ade repo itself (read-only checks, so
// gitignore behaviour is exercised against a real tree) and a throwaway temp
// fixture (all mutating checks, so the repo is never touched). `SPEC_ADE_DATA_DIR`
// points at a tempdir, so the developer's real settings.json is never touched.

import { spawn } from 'node:child_process';
import { mkdtempSync, writeFileSync, mkdirSync, readFileSync, existsSync, appendFileSync, realpathSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

// Derived from this file's own location so the script survives a move to
// another machine (STATUS.md exists precisely for that handoff).
const REPO = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const PROFILE = process.argv.includes('--debug') ? 'debug' : 'release';
const BIN = join(REPO, 'src/server/target', PROFILE, 'spec-ade-server');
const PORT = 7391;
const BASE = `http://127.0.0.1:${PORT}`;
const DATA_DIR = mkdtempSync(join(tmpdir(), 'spec-ade-verify-data-'));

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

/** Start the server, resolving once it prints its token. */
async function startServer() {
  const proc = spawn(BIN, ['-p', String(PORT), '-H', '127.0.0.1', '--no-open'], {
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
  // Wait for the listener to actually accept connections.
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
      const r = await fetch(`${BASE}${path}`, { method: 'POST', headers, body: JSON.stringify(body) });
      return { status: r.status, body: await safeJson(r) };
    },
    async put(path, body) {
      const r = await fetch(`${BASE}${path}`, { method: 'PUT', headers, body: JSON.stringify(body) });
      return { status: r.status, body: await safeJson(r) };
    },
    async patch(path, body) {
      const r = await fetch(`${BASE}${path}`, { method: 'PATCH', headers, body: JSON.stringify(body) });
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

async function main() {
  if (!existsSync(BIN)) {
    throw new Error(`binary not found: ${BIN}\nbuild it first (see the header of this file)`);
  }
  console.log(`binary:   ${BIN}`);
  console.log(`data dir: ${DATA_DIR}`);
  const server = await startServer();
  const a = api(server.token);
  const fixture = mkdtempSync(join(tmpdir(), 'spec-ade-verify-fixture-'));

  try {
    await checkRepoProject(a);
    await checkFixtureProject(a, fixture);
    await checkSettings(a);
    await checkTraversal(a);
    await checkProjectPersistence(a, server, fixture);
  } finally {
    await stopServer(server);
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

// #1-4, #14: register the real repo, check the tree + language-relevant reads.
async function checkRepoProject(a) {
  console.log('\n-- repo project (§8 #1-4, #14) --');
  const created = await a.post('/api/projects', { path: REPO });
  check('add project = repo → 201', created.status === 201, JSON.stringify(created.body));
  const id = created.body?.id;

  const tree = await a.get(`/api/projects/${id}/tree`);
  const rootNames = (tree.body?.entries ?? []).map((e) => e.name);
  check('root tree shows src/', rootNames.includes('src'));
  check('.gitignore visible at root (hidden files shown)', rootNames.includes('.gitignore'));

  const webTree = await a.get(`/api/projects/${id}/tree?path=src/web`);
  const webNames = (webTree.body?.entries ?? []).map((e) => e.name);
  check(
    'src/web/node_modules absent (root .gitignore applies to nested dir)',
    !webNames.includes('node_modules'),
    `entries: ${JSON.stringify(webNames)}`,
  );

  const serverTree = await a.get(`/api/projects/${id}/tree?path=src/server`);
  const serverNames = (serverTree.body?.entries ?? []).map((e) => e.name);
  check('src/server/target absent (gitignored)', !serverNames.includes('target'));

  const mainRs = await a.get(`/api/projects/${id}/file?path=src/server/src/main.rs`);
  check('main.rs reads as text', mainRs.body?.kind === 'text');
  check('main.rs content looks like Rust', /fn main/.test(mainRs.body?.content ?? ''));

  const appVue = await a.get(`/api/projects/${id}/file?path=src/web/src/App.vue`);
  check('App.vue reads as text', appVue.body?.kind === 'text');
  check('App.vue content looks like a Vue SFC', /<script setup/.test(appVue.body?.content ?? ''));

  const readme = await a.get(`/api/projects/${id}/file?path=README.md`);
  check('README.md reads as text', readme.body?.kind === 'text', JSON.stringify(readme.body));

  const lockPath = 'src/server/Cargo.lock';
  const lock = await a.get(`/api/projects/${id}/file?path=${lockPath}`);
  check(
    '#14 Cargo.lock opens as text (no size-based refusal for a large real file)',
    lock.body?.kind === 'text',
    JSON.stringify({ kind: lock.body?.kind, size: lock.body?.size }),
  );

  await a.del(`/api/projects/${id}`);
}

// #5, #7, #8, #9, #10: mutations against a disposable fixture, never the repo.
async function checkFixtureProject(a, fixture) {
  console.log('\n-- fixture project (§8 #5, #7-10) --');
  writeFileSync(join(fixture, 'note.txt'), 'hello');
  writeFileSync(join(fixture, 'logo.png'), Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x00, 0x01, 0x02]));
  mkdirSync(join(fixture, 'sub'));
  writeFileSync(join(fixture, 'sub', 'inner.txt'), 'x');

  const created = await a.post('/api/projects', { path: fixture });
  const id = created.body?.id;
  check('add fixture project → 201', created.status === 201, JSON.stringify(created.body));

  // #8: binary file reported, not opened.
  const png = await a.get(`/api/projects/${id}/file?path=logo.png`);
  check('#8 .png reports kind:binary', png.body?.kind === 'binary', JSON.stringify(png.body));
  check('#8 binary response has no content field', png.body?.content === undefined);

  // #5: edit one character, rev-guarded write, verify on disk.
  const read1 = await a.get(`/api/projects/${id}/file?path=note.txt`);
  const write1 = await a.put(`/api/projects/${id}/file?path=note.txt`, {
    content: 'hallo',
    rev: read1.body.rev,
  });
  check('#5 rev-guarded write succeeds', write1.status === 200, JSON.stringify(write1.body));
  check(
    '#5 exactly one character changed on disk',
    readFileSync(join(fixture, 'note.txt'), 'utf8') === 'hallo',
  );

  // #7: an external write (simulating the SPEC-001 terminal) races the editor's
  // stale rev, then "Ghi đè" (force overwrite) recovers.
  appendFileSync(join(fixture, 'note.txt'), '!');
  const staleWrite = await a.put(`/api/projects/${id}/file?path=note.txt`, {
    content: 'edited-in-editor',
    rev: write1.body.rev,
  });
  check('#7 stale rev after external write → 409', staleWrite.status === 409, JSON.stringify(staleWrite.body));
  check('#7 409 body carries currentRev for "Ghi đè"', typeof staleWrite.body?.currentRev === 'string');
  const forced = await a.put(`/api/projects/${id}/file?path=note.txt`, { content: 'edited-in-editor' });
  check('#7 force overwrite (no rev) succeeds', forced.status === 200, JSON.stringify(forced.body));
  check(
    '#7 force overwrite actually replaced the external edit on disk',
    readFileSync(join(fixture, 'note.txt'), 'utf8') === 'edited-in-editor',
  );

  // #9: create / rename / delete, each confirmed with a real fs read.
  const createRes = await a.post(`/api/projects/${id}/entries`, { path: 'created.txt', kind: 'file' });
  check('#9 create → 201', createRes.status === 201, JSON.stringify(createRes.body));
  check('#9 create → file exists on disk', existsSync(join(fixture, 'created.txt')));

  const renameRes = await a.patch(`/api/projects/${id}/entries?path=created.txt`, { newPath: 'renamed.txt' });
  check('#9 rename → 200', renameRes.status === 200, JSON.stringify(renameRes.body));
  check(
    '#9 rename reflected on disk',
    !existsSync(join(fixture, 'created.txt')) && existsSync(join(fixture, 'renamed.txt')),
  );

  const deleteRes = await a.del(`/api/projects/${id}/entries?path=renamed.txt`);
  check('#9 delete → 204', deleteRes.status === 204);
  check('#9 delete reflected on disk', !existsSync(join(fixture, 'renamed.txt')));

  // #10: non-empty directory needs recursive=true.
  const noRecursive = await a.del(`/api/projects/${id}/entries?path=sub`);
  check('#10 delete non-empty dir without recursive → error', noRecursive.status >= 400, String(noRecursive.status));
  check('#10 dir untouched after refused delete', existsSync(join(fixture, 'sub', 'inner.txt')));
  const recursive = await a.del(`/api/projects/${id}/entries?path=sub&recursive=true`);
  check('#10 delete non-empty dir with recursive=true → 204', recursive.status === 204);
  check('#10 dir actually gone from disk', !existsSync(join(fixture, 'sub')));

  await a.del(`/api/projects/${id}`);
}

// #11: settings PUT lands, is readable, and hits disk.
async function checkSettings(a) {
  console.log('\n-- settings (§8 #11) --');
  const put = await a.put('/api/settings', { editor: { tabSize: 8 } });
  check('#11 PUT tabSize=8 → 200', put.status === 200, JSON.stringify(put.body));
  check('#11 response reflects tabSize 8', put.body?.editor?.tabSize === 8);

  const get = await a.get('/api/settings');
  check('#11 GET returns tabSize 8', get.body?.editor?.tabSize === 8);
  check('#11 authToken never exposed', get.body?.editor?.authToken === undefined && get.body?.authToken === undefined);

  const onDisk = JSON.parse(readFileSync(join(DATA_DIR, 'settings.json'), 'utf8'));
  check('#11 tabSize 8 persisted to settings.json', onDisk.editor?.tab_size === 8 || onDisk.editor?.tabSize === 8,
    JSON.stringify(onDisk.editor));

  // Out-of-range must be refused rather than silently clamped.
  const bad = await a.put('/api/settings', { editor: { tabSize: 99 } });
  check('out-of-range tabSize → 400 (no silent clamp)', bad.status === 400, String(bad.status));
  const after = await a.get('/api/settings');
  check('refused patch left the stored value intact', after.body?.editor?.tabSize === 8);

  // null = back to default ([INVENTED-3]).
  const reset = await a.put('/api/settings', { editor: { tabSize: null } });
  check('null resets tabSize to default 2', reset.body?.editor?.tabSize === 2, JSON.stringify(reset.body?.editor));

  // authToken is neither readable nor writable ([INVENTED-1]).
  const forbidden = await a.put('/api/settings', { authToken: 'hijack' });
  check('PUT authToken → 403', forbidden.status === 403, String(forbidden.status));

  // Leave it at 8: §8 #11 requires it to still be 8 after the restart below.
  await a.put('/api/settings', { editor: { tabSize: 8 } });
}

// #12: path traversal refused.
async function checkTraversal(a) {
  console.log('\n-- path guard (§8 #12) --');
  const created = await a.post('/api/projects', { path: REPO });
  const id = created.body?.id;

  const escape = await a.get(`/api/projects/${id}/file?path=../../../etc/passwd`);
  check('#12 ../../../etc/passwd → 400/403', escape.status === 400 || escape.status === 403, String(escape.status));
  check('#12 no file content leaked', escape.body?.content === undefined);

  const absolute = await a.get(`/api/projects/${id}/file?path=/etc/passwd`);
  check('absolute path → 400/403', absolute.status === 400 || absolute.status === 403, String(absolute.status));

  const noToken = await fetch(`${BASE}/api/projects`);
  check('unauthenticated request → 401', noToken.status === 401, String(noToken.status));

  await a.del(`/api/projects/${id}`);
}

// #13: the project list survives a restart, and the SPA is served by the binary.
async function checkProjectPersistence(a, server, fixture) {
  console.log('\n-- persistence + SPA (§8 #13) --');
  const created = await a.post('/api/projects', { path: fixture, name: 'Fixture' });
  check('register project for restart test → 201', created.status === 201);

  const spa = await fetch(`${BASE}/`);
  const html = await spa.text();
  check('binary serves the embedded SPA', spa.ok && /<div id="app">/.test(html), String(spa.status));
  const assetMatch = html.match(/src="(\/assets\/[^"]+\.js)"/);
  check('SPA references a built JS bundle', assetMatch !== null);
  if (assetMatch) {
    const asset = await fetch(`${BASE}${assetMatch[1]}`);
    const js = await asset.text();
    // Markers must be literals that survive minification. Runtime-assembled
    // URLs (`fileUrl(id, 'tree', path)`) never appear as `/tree?path=` in the
    // bundle, so these are string constants unique to the SPEC-002 layer.
    const markers = ['tooLarge', 'currentRev', 'spec_ade_active_project', 'Ghi đè'];
    const missing = markers.filter((m) => !js.includes(m));
    check(
      'embedded bundle is the SPEC-002 build',
      asset.ok && missing.length === 0,
      `status ${asset.status}, missing: ${JSON.stringify(missing)}`,
    );
  }

  await stopServer(server);
  const restarted = await startServer();
  try {
    const a2 = api(restarted.token);
    const list = await a2.get('/api/projects');
    // The server stores the CANONICAL path, and on macOS /var is a symlink to
    // /private/var — so the raw mkdtemp path never matches. Resolve first.
    const canonical = realpathSync(fixture);
    const found = (list.body ?? []).find((p) => p.path === canonical);
    check('#13 project list survives a restart', found !== undefined, JSON.stringify(list.body));
    check('#13 project name persisted', found?.name === 'Fixture');

    const settings = await a2.get('/api/settings');
    check('#11 tabSize is still 8 after restart', settings.body?.editor?.tabSize === 8,
      JSON.stringify(settings.body?.editor));
    check('#13 token is stable across restart', restarted.token === server.token);
  } finally {
    // Hand the caller a live handle so its own stopServer call is a no-op-safe
    // kill of an already-dead process.
    server.proc = restarted.proc;
  }
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
