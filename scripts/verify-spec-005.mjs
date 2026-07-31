// SPEC-005 §8.1 verification — real git repository, real release binary, real SSE.
//
// Usage (the SPA must be built before Rust embeds it):
//   npm --prefix src/web run build
//   cargo build --release --manifest-path src/server/Cargo.toml
//   node scripts/verify-spec-005.mjs
//
// This script owns disposable data and repository directories. It never registers
// or mutates the source tree. Reads and writes cross the same HTTP/SSE boundary the
// browser uses; `git` CLI checks are independent witnesses for mutations.

import { execFileSync, spawn } from 'node:child_process';
import { existsSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const REPO = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const PROFILE = process.argv.includes('--debug') ? 'debug' : 'release';
const BIN = join(REPO, 'src/server/target', PROFILE, 'spec-ade-server');
// Distinct from verify-002/003/004 so all verifiers may run concurrently.
const PORT = 7395;
const BASE = `http://127.0.0.1:${PORT}`;
const DATA_DIR = mkdtempSync(join(tmpdir(), 'spec-ade-verify5-data-'));
const FIXTURE = mkdtempSync(join(tmpdir(), 'spec-ade-verify5-repo-'));

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

const sleep = (ms) => new Promise((resolveSleep) => setTimeout(resolveSleep, ms));

function git(...args) {
  return execFileSync('git', args, {
    cwd: FIXTURE,
    encoding: 'utf8',
    env: { ...process.env, LC_ALL: 'C', GIT_TERMINAL_PROMPT: '0' },
  }).trim();
}

function initRepo() {
  git('init', '-b', 'main');
  git('config', 'user.name', 'Spec ADE Verifier');
  git('config', 'user.email', 'verify@spec-ade.invalid');
  git('config', 'commit.gpgsign', 'false');
  writeFileSync(join(FIXTURE, 'shared.txt'), 'initial\n');
}

async function startServer() {
  const proc = spawn(BIN, ['-p', String(PORT), '-H', '127.0.0.1', '--no-open'], {
    env: { ...process.env, SPEC_ADE_DATA_DIR: DATA_DIR },
    stdio: ['ignore', 'pipe', 'pipe'],
  });
  let log = '';
  let token = null;
  const onData = (chunk) => {
    log += chunk.toString();
    const match = log.match(/token=([0-9a-f]{16,})/);
    if (match) token = match[1];
  };
  proc.stdout.on('data', onData);
  proc.stderr.on('data', onData);

  for (let i = 0; i < 100 && !token; i += 1) await sleep(100);
  if (!token) {
    proc.kill('SIGKILL');
    throw new Error(`server did not report a token; log:\n${log}`);
  }

  let ready = false;
  for (let i = 0; i < 100 && !ready; i += 1) {
    try {
      ready = (await fetch(`${BASE}/api/health`)).ok;
    } catch {}
    if (!ready) await sleep(100);
  }
  if (!ready) {
    proc.kill('SIGKILL');
    throw new Error(`server did not become ready; log:\n${log}`);
  }
  return { proc, token, log: () => log };
}

async function stopServer(server) {
  if (server.proc.exitCode !== null) return;
  server.proc.kill('SIGTERM');
  await new Promise((resolveExit) => server.proc.once('exit', resolveExit));
}

function api(token) {
  const headers = { 'x-spec-ade-token': token, 'content-type': 'application/json' };
  async function request(method, path, body) {
    const response = await fetch(`${BASE}${path}`, {
      method,
      headers,
      body: body === undefined ? undefined : JSON.stringify(body),
    });
    let parsed = null;
    if (response.status !== 204) {
      try {
        parsed = await response.json();
      } catch {}
    }
    return { status: response.status, body: parsed };
  }
  return {
    get: (path) => request('GET', path),
    post: (path, body) => request('POST', path, body),
    del: (path) => request('DELETE', path),
  };
}

/**
 * Collect named SSE frames from a real streaming `fetch` response.
 *
 * The browser's `EventSource` puts the token in the query string because it cannot
 * set headers. The verifier does the same, so the auth compromise itself is tested.
 */
async function openWatch(token, projectId) {
  const controller = new AbortController();
  const url = new URL(`${BASE}/api/projects/${projectId}/git/watch`);
  url.searchParams.set('token', token);
  const response = await fetch(url, { signal: controller.signal });
  if (!response.ok || !response.body) {
    throw new Error(`SSE watch failed: ${response.status} ${await response.text()}`);
  }

  const frames = [];
  const reader = response.body.getReader();
  const decoder = new TextDecoder();
  let pending = '';
  let stopped = false;

  const done = (async () => {
    try {
      while (!stopped) {
        const { value, done: streamDone } = await reader.read();
        if (streamDone) break;
        pending += decoder.decode(value, { stream: true }).replaceAll('\r\n', '\n');
        let boundary;
        while ((boundary = pending.indexOf('\n\n')) >= 0) {
          const block = pending.slice(0, boundary);
          pending = pending.slice(boundary + 2);
          let event = 'message';
          const data = [];
          for (const line of block.split('\n')) {
            if (line.startsWith('event:')) event = line.slice(6).trimStart();
            else if (line.startsWith('data:')) data.push(line.slice(5).trimStart());
          }
          if (data.length > 0) {
            const text = data.join('\n');
            let body = text;
            try { body = JSON.parse(text); } catch {}
            frames.push({ event, body });
          }
        }
      }
    } catch (error) {
      if (!stopped && error?.name !== 'AbortError') throw error;
    }
  })();

  return {
    frames,
    async waitFor(predicate, label, timeoutMs = 8000) {
      const deadline = Date.now() + timeoutMs;
      while (Date.now() < deadline) {
        const frame = frames.find((candidate) => predicate(candidate));
        if (frame) return frame;
        await sleep(50);
      }
      throw new Error(`timed out waiting for SSE ${label}; frames: ${JSON.stringify(frames)}`);
    },
    async close() {
      stopped = true;
      controller.abort();
      try { await done; } catch {}
    },
  };
}

function gitRoute(projectId, route) {
  return `/api/projects/${projectId}/git/${route}`;
}

async function verifyFlow(a, token) {
  console.log('\n-- real repository + API + SSE (§8.1) --');

  const created = await a.post('/api/projects', { path: FIXTURE, name: 'Git verifier' });
  check('register real git repository → 201', created.status === 201, JSON.stringify(created.body));
  if (created.status !== 201) throw new Error('cannot continue without a project');
  const projectId = created.body.id;

  const initial = await a.get(gitRoute(projectId, 'status'));
  check('status sees an unborn repository', initial.status === 200 && initial.body?.isRepo === true,
    JSON.stringify(initial.body));
  check('status sees shared.txt as untracked', initial.body?.counts?.untracked === 1,
    JSON.stringify(initial.body?.counts));

  const watch = await openWatch(token, projectId);
  try {
    await watch.waitFor(
      (frame) => frame.event === 'status' && frame.body?.counts?.untracked === 1,
      'initial untracked status',
    );
    check('SSE subscriber receives the initial repository state', true);

    const staged = await a.post(gitRoute(projectId, 'stage'), {
      paths: ['shared.txt'],
      unstage: false,
    });
    check('stage → fresh status with one staged path', staged.status === 200 && staged.body?.counts?.staged === 1,
      JSON.stringify(staged.body));
    check('git CLI independently sees shared.txt in the index', git('diff', '--cached', '--name-only') === 'shared.txt');
    await watch.waitFor(
      (frame) => frame.event === 'status' && frame.body?.counts?.staged === 1,
      'staged status',
    );

    const committed = await a.post(gitRoute(projectId, 'commit'), {
      message: 'initial commit',
      amend: false,
    });
    check('commit → clean status', committed.status === 200 && committed.body?.counts?.staged === 0,
      JSON.stringify(committed.body));
    check('git CLI independently sees the commit', git('log', '-1', '--format=%s') === 'initial commit');
    await watch.waitFor(
      (frame) => frame.event === 'status' && frame.body?.head?.oid && frame.body?.counts?.staged === 0,
      'first commit status',
    );

    const branch = await a.post(gitRoute(projectId, 'branch'), {
      name: 'feature',
      checkout: true,
    });
    check('create + checkout branch → feature', branch.status === 200 && branch.body?.head?.branch === 'feature',
      JSON.stringify(branch.body));
    check('git CLI independently sees feature checked out', git('branch', '--show-current') === 'feature');
    await watch.waitFor(
      (frame) => frame.event === 'status' && frame.body?.head?.branch === 'feature',
      'feature branch',
    );

    writeFileSync(join(FIXTURE, 'shared.txt'), 'feature side\n');
    await watch.waitFor(
      (frame) => frame.event === 'status' && frame.body?.head?.branch === 'feature' && frame.body?.counts?.changed === 1,
      'feature edit',
    );
    await a.post(gitRoute(projectId, 'stage'), { paths: ['shared.txt'], unstage: false });
    const featureCommit = await a.post(gitRoute(projectId, 'commit'), {
      message: 'feature change',
      amend: false,
    });
    check('feature commit succeeds', featureCommit.status === 200, JSON.stringify(featureCommit.body));

    const checkoutMain = await a.post(gitRoute(projectId, 'checkout'), { target: 'main', force: false });
    check('checkout main succeeds without force', checkoutMain.status === 200 && checkoutMain.body?.head?.branch === 'main',
      JSON.stringify(checkoutMain.body));
    writeFileSync(join(FIXTURE, 'shared.txt'), 'main side\n');
    await a.post(gitRoute(projectId, 'stage'), { paths: ['shared.txt'], unstage: false });
    const mainCommit = await a.post(gitRoute(projectId, 'commit'), {
      message: 'main change',
      amend: false,
    });
    check('divergent main commit succeeds', mainCommit.status === 200, JSON.stringify(mainCommit.body));

    const merge = await a.post(gitRoute(projectId, 'merge'), { from: 'feature', noFf: false });
    // A conflict is a successful transition into a useful repository state, not a
    // failed request: the returned status lets the panel open its conflict editor
    // immediately, without a second GET racing another writer.
    check('conflicting merge → fresh conflicted status',
      merge.status === 200 && merge.body?.state === 'merge' &&
        merge.body?.counts?.conflicted === 1 &&
        merge.body?.entries?.some((entry) => entry.path === 'shared.txt' && entry.conflicted),
      JSON.stringify(merge.body));
    await watch.waitFor(
      (frame) => frame.event === 'status' && frame.body?.state === 'merge' && frame.body?.counts?.conflicted === 1,
      'merge conflict',
    );

    const conflict = await a.get(`${gitRoute(projectId, 'conflict')}?path=shared.txt`);
    check('conflict endpoint returns all three text sides',
      conflict.status === 200 && conflict.body?.base === 'initial\n' &&
        conflict.body?.ours === 'main side\n' && conflict.body?.theirs === 'feature side\n',
      JSON.stringify(conflict.body));

    const resolved = await a.post(gitRoute(projectId, 'resolve'), {
      path: 'shared.txt',
      content: 'resolved result\n',
    });
    check('resolve writes + stages content and clears conflict',
      resolved.status === 200 && resolved.body?.state === 'merge' &&
        resolved.body?.counts?.conflicted === 0 && resolved.body?.counts?.staged === 1,
      JSON.stringify(resolved.body));
    check('resolved content is really on disk', readFileSync(join(FIXTURE, 'shared.txt'), 'utf8') === 'resolved result\n');
    check('git CLI sees no unmerged paths', git('diff', '--name-only', '--diff-filter=U') === '');
    await watch.waitFor(
      (frame) => frame.event === 'status' && frame.body?.state === 'merge' &&
        frame.body?.counts?.conflicted === 0 && frame.body?.counts?.staged === 1,
      'resolved merge status',
    );

    const mergeCommit = await a.post(gitRoute(projectId, 'commit'), {
      message: 'merge feature',
      amend: false,
    });
    check('commit completes the merge',
      mergeCommit.status === 200 && mergeCommit.body?.state === 'clean' &&
        mergeCommit.body?.counts?.staged === 0,
      JSON.stringify(mergeCommit.body));
    check('git CLI sees a two-parent merge commit', git('show', '-s', '--format=%P', 'HEAD').split(/\s+/).length === 2);
    await watch.waitFor(
      (frame) => frame.event === 'status' && frame.body?.state === 'clean' &&
        frame.body?.head?.branch === 'main' && frame.body?.counts?.staged === 0,
      'completed merge',
    );

    const statusFrames = watch.frames.filter((frame) => frame.event === 'status');
    check('one SSE subscriber stayed live across the full mutation flow', statusFrames.length >= 7,
      `${statusFrames.length} status frames`);
    check('the watcher never reported stopped', watch.frames.every((frame) => frame.event !== 'stopped'),
      JSON.stringify(watch.frames.filter((frame) => frame.event === 'stopped')));
  } finally {
    await watch.close();
    await a.del(`/api/projects/${projectId}`);
  }
}

async function verifyBundle() {
  console.log('\n-- embedded Git frontend + lazy merge chunk --');

  const htmlResponse = await fetch(`${BASE}/`);
  const html = await htmlResponse.text();
  const entryPath = html.match(/\/assets\/(index-[\w-]+\.js)/)?.[1];
  check('embedded SPA names an entry chunk', htmlResponse.ok && Boolean(entryPath));
  if (!entryPath) return;

  const entryResponse = await fetch(`${BASE}/assets/${entryPath}`);
  const entry = await entryResponse.text();
  // Export names occur once in the tiny lazy-import wrappers and therefore are
  // not payload witnesses. These literals come from merge's implementation/theme
  // and are absent from our wrappers as well as CodeMirror core.
  const payloadMarkers = [
    'cm-mergeViewEditors',
    'cm-deletedChunk',
    'originalDocChangeEffect',
    'collapsed-unchanged-code',
    'Revert this chunk',
  ];
  const leaked = payloadMarkers.filter((marker) => entry.includes(marker));
  check('@codemirror/merge payload is absent from the entry chunk', leaked.length === 0,
    `leaked payload markers: ${JSON.stringify(leaked)}`);

  const imports = [...entry.matchAll(/import\("\.\/(index-[\w-]+\.js)"\)/g)].map((match) => match[1]);
  let mergeChunk = null;
  for (const name of new Set(imports)) {
    const response = await fetch(`${BASE}/assets/${name}`);
    if (!response.ok) continue;
    const source = await response.text();
    if (payloadMarkers.every((marker) => source.includes(marker))) {
      mergeChunk = { name, source };
      break;
    }
  }
  check('entry reaches @codemirror/merge only through a dynamic chunk', mergeChunk !== null,
    `dynamic index chunks: ${JSON.stringify([...new Set(imports)])}`);

  const markers = ['đang poll', 'không phải git repository', 'Chuyển và bỏ thay đổi'];
  const missing = markers.filter((marker) => !entry.includes(marker));
  check('release binary embeds the SPEC-005 panel build', missing.length === 0,
    `missing: ${JSON.stringify(missing)}`);

  console.log(`  info entry ${entry.length.toLocaleString()} B${mergeChunk ? `, merge chunk ${mergeChunk.name} ${mergeChunk.source.length.toLocaleString()} B` : ''}`);
}

async function main() {
  if (!existsSync(BIN)) {
    throw new Error(`binary not found: ${BIN}\nbuild it first (see the header of this file)`);
  }
  initRepo();
  console.log(`binary:   ${BIN}`);
  console.log(`data dir: ${DATA_DIR}`);
  console.log(`fixture:  ${FIXTURE}`);

  const server = await startServer();
  const a = api(server.token);
  try {
    await verifyFlow(a, server.token);
    await verifyBundle();
  } finally {
    await stopServer(server);
    rmSync(FIXTURE, { recursive: true, force: true });
    rmSync(DATA_DIR, { recursive: true, force: true });
  }

  console.log(`\n${pass} passed, ${failures.length} failed`);
  if (failures.length > 0) {
    console.log('\nFailures:');
    for (const failure of failures) console.log(`  - ${failure}`);
    process.exit(1);
  }
  console.log('\nSPEC-005 §8.1 verified. The three real-browser checks remain in §8.2.');
}

main().catch((error) => {
  console.error(error);
  rmSync(FIXTURE, { recursive: true, force: true });
  rmSync(DATA_DIR, { recursive: true, force: true });
  process.exit(1);
});
