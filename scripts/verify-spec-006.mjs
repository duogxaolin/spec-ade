// SPEC-006 §8.1 verification — real file tree, real release binary, real SSE.
//
// Usage (the SPA must be built before Rust embeds it):
//   npm --prefix src/web run build
//   cargo build --release --manifest-path src/server/Cargo.toml
//   node scripts/verify-spec-006.mjs
//
// This script owns disposable data and project directories, and the only process
// it kills is a child it spawned itself. It never registers or mutates the source
// tree. Reads cross the same HTTP/SSE boundary the browser uses.

import { execFileSync, spawn } from 'node:child_process';
import {
  existsSync, mkdirSync, mkdtempSync, readdirSync, readFileSync, rmSync, symlinkSync, writeFileSync,
} from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const REPO = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const PROFILE = process.argv.includes('--debug') ? 'debug' : 'release';
const BIN = join(REPO, 'src/server/target', PROFILE, 'spec-ade-server');
// Distinct from verify-002..005 so all verifiers may run concurrently.
const PORT = 7396;
const BASE = `http://127.0.0.1:${PORT}`;
const DATA_DIR = mkdtempSync(join(tmpdir(), 'spec-ade-verify6-data-'));
const FIXTURE = mkdtempSync(join(tmpdir(), 'spec-ade-verify6-tree-'));

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

/**
 * A tree with every case the walker has to survive: an ignored directory, a
 * dotted directory, a binary file, a 4k+ line, and a non-ASCII line whose match
 * starts past a multi-byte character.
 */
function buildTree() {
  const write = (rel, text) => {
    const full = join(FIXTURE, rel);
    mkdirSync(dirname(full), { recursive: true });
    writeFileSync(full, text);
  };

  write('.gitignore', 'ignored/\n*.log\n');
  // `ignore` silently drops every gitignore rule without a repository.
  execFileSync('git', ['init', '-q', '-b', 'main'], { cwd: FIXTURE });

  write('src/main.rs', 'fn main() {\n    let needle = 1;\n    println!("needle {needle}");\n}\n');
  write('src/lib.rs', '// no match here\npub fn other() {}\n');
  write('src/deep/nested.rs', 'const NEEDLE_UPPER: u8 = 0; // needle\n');
  write('notes/unicode.txt', 'café needle sau dấu\n');
  write('notes/word.txt', 'needles are not needle\n');
  write('ignored/hidden.rs', 'let needle = "must not appear";\n');
  write('debug.log', 'needle in an ignored extension\n');
  write('.git/config-ish.rs', 'needle inside dot-git\n');
  // A minified line far over MAX_LINE_BYTES; the frame must not carry it whole.
  write('assets/min.js', `var a="${'x'.repeat(9000)}needle";\n`);
  writeFileSync(join(FIXTURE, 'assets/blob.bin'), Buffer.from([0, 1, 2, 3, 0, 0x6e, 0x65, 0x65, 0x64, 0x6c, 0x65]));
  // A name inside the root that resolves outside it — the 403 case, distinct from
  // a literal `..` component, which never reaches the filesystem.
  symlinkSync(tmpdir(), join(FIXTURE, 'escape-link'));
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
  await new Promise((r) => server.proc.once('exit', r));
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
  };
}

/**
 * Read a real SSE stream to completion (or until `stopWhen` says so).
 *
 * The browser's `EventSource` puts the token in the query string because it
 * cannot set headers. The verifier does the same, so the auth compromise itself
 * is exercised.
 */
async function readStream(url, { stopWhen, timeoutMs = 20000 } = {}) {
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), timeoutMs);
  const response = await fetch(url, { signal: controller.signal });
  if (!response.ok || !response.body) {
    clearTimeout(timer);
    return { status: response.status, frames: [], body: await response.text().catch(() => '') };
  }

  const frames = [];
  const reader = response.body.getReader();
  const decoder = new TextDecoder();
  let pending = '';

  try {
    outer: while (true) {
      const { value, done } = await reader.read();
      if (done) break;
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
        if (data.length === 0) continue;
        const text = data.join('\n');
        let parsed = text;
        try {
          parsed = JSON.parse(text);
        } catch {}
        frames.push({ event, body: parsed });
        if (stopWhen ? stopWhen(frames) : event === 'done') break outer;
      }
    }
  } catch (error) {
    if (error?.name !== 'AbortError') throw error;
  } finally {
    clearTimeout(timer);
    await reader.cancel().catch(() => {});
  }
  return { status: response.status, frames };
}

function searchUrl(token, projectId, params = {}) {
  const url = new URL(`${BASE}/api/projects/${projectId}/search`);
  url.searchParams.set('token', token);
  for (const [key, value] of Object.entries(params)) {
    if (Array.isArray(value)) for (const v of value) url.searchParams.append(key, v);
    else url.searchParams.set(key, String(value));
  }
  return url;
}

async function search(token, projectId, params) {
  const result = await readStream(searchUrl(token, projectId, params));
  return {
    status: result.status,
    matches: result.frames.filter((f) => f.event === 'match').map((f) => f.body),
    progress: result.frames.filter((f) => f.event === 'progress').map((f) => f.body),
    errors: result.frames.filter((f) => f.event === 'error').map((f) => f.body),
    done: result.frames.find((f) => f.event === 'done')?.body ?? null,
  };
}

async function verifySearch(a, token, projectId) {
  console.log('\nsearch');

  const literal = await search(token, projectId, { query: 'needle' });
  check('search emits a done frame', literal.done !== null);
  const paths = literal.matches.map((m) => m.path);

  check('finds the literal in a nested file', paths.includes('src/deep/nested.rs'), JSON.stringify(paths));
  check('finds it in src/main.rs', paths.includes('src/main.rs'));
  // The gitignore rules only exist because the fixture is a real repository.
  check('respects .gitignore directories', !paths.includes('ignored/hidden.rs'), JSON.stringify(paths));
  check('respects .gitignore extensions', !paths.includes('debug.log'));
  check('skips .git', !paths.some((p) => p.startsWith('.git/')));
  check('skips binary files', !paths.includes('assets/blob.bin'));

  const main = literal.matches.filter((m) => m.path === 'src/main.rs');
  check('one event per line, not per match', main.length === 2, `${main.length} events`);
  const twoOnALine = main.find((m) => m.ranges.length === 2);
  check('a line with two matches carries two ranges', Boolean(twoOnALine), JSON.stringify(main));

  const unicode = literal.matches.find((m) => m.path === 'notes/unicode.txt');
  if (unicode) {
    const [start, end] = unicode.ranges[0];
    const bytes = Buffer.from(unicode.text, 'utf8');
    // The whole point of the byte-offset contract: slicing in UTF-16 would land
    // one position early because of the é.
    check('ranges are byte offsets into the UTF-8 text',
      bytes.subarray(start, end).toString('utf8') === 'needle',
      `[${start}, ${end}) → ${JSON.stringify(bytes.subarray(start, end).toString('utf8'))}`);
  } else {
    check('ranges are byte offsets into the UTF-8 text', false, 'notes/unicode.txt not matched');
  }

  const long = literal.matches.find((m) => m.path === 'assets/min.js');
  check('a 9kB line is truncated in the frame',
    Boolean(long) && Buffer.byteLength(long.text, 'utf8') <= 4200,
    long ? `${Buffer.byteLength(long.text, 'utf8')} B` : 'no match');

  check('done counts files, not just matches',
    literal.done.files > 0 && literal.done.matches === literal.matches.length,
    JSON.stringify(literal.done));
  check('done reports elapsedMs and filesScanned',
    typeof literal.done.elapsedMs === 'number' && literal.done.filesScanned > 0,
    JSON.stringify(literal.done));
  check('a small tree is not truncated', literal.done.truncated === false);

  const cased = await search(token, projectId, { query: 'NEEDLE', case: 'true' });
  const casedPaths = cased.matches.map((m) => m.path);
  check('case=true excludes lowercase hits',
    casedPaths.includes('src/deep/nested.rs') && !casedPaths.includes('src/main.rs'),
    JSON.stringify(casedPaths));

  const insensitive = await search(token, projectId, { query: 'NEEDLE' });
  check('case-insensitive is the default',
    insensitive.matches.map((m) => m.path).includes('src/main.rs'));

  // `notes/word.txt` is "needles are not needle": the line matches either way, so
  // the assertion has to be on the *ranges*, not on whether the file appears.
  const word = await search(token, projectId, { query: 'needle', word: 'true' });
  const wordLine = word.matches.find((m) => m.path === 'notes/word.txt');
  check('word=true highlights only the standalone occurrence',
    Boolean(wordLine) && wordLine.ranges.length === 1 && wordLine.ranges[0][0] === 16,
    JSON.stringify(wordLine?.ranges));

  const loose = await search(token, projectId, { query: 'needle' });
  const looseLine = loose.matches.find((m) => m.path === 'notes/word.txt');
  check('word=false highlights both occurrences',
    Boolean(looseLine) && looseLine.ranges.length === 2, JSON.stringify(looseLine?.ranges));

  const regex = await search(token, projectId, { query: 'need(le|lex)', regex: 'true' });
  check('regex=true is honoured', regex.matches.length > 0);

  const literalParen = await search(token, projectId, { query: 'need(le|lex)' });
  check('regex=false treats the pattern literally', literalParen.matches.length === 0);

  const globbed = await search(token, projectId, { query: 'needle', glob: ['*.rs'] });
  check('glob includes only matching files',
    globbed.matches.length > 0 && globbed.matches.every((m) => m.path.endsWith('.rs')),
    JSON.stringify(globbed.matches.map((m) => m.path)));

  const excluded = await search(token, projectId, { query: 'needle', glob: ['!*.rs'] });
  check('a leading ! excludes',
    excluded.matches.length > 0 && excluded.matches.every((m) => !m.path.endsWith('.rs')),
    JSON.stringify(excluded.matches.map((m) => m.path)));

  const scoped = await search(token, projectId, { query: 'needle', path: 'notes' });
  check('path scopes the walk',
    scoped.matches.length > 0 && scoped.matches.every((m) => m.path.startsWith('notes/')),
    JSON.stringify(scoped.matches.map((m) => m.path)));

  const capped = await search(token, projectId, { query: 'needle', maxResults: 1 });
  check('maxResults caps and flags truncation',
    capped.matches.length <= 1 && capped.done?.truncated === true,
    JSON.stringify(capped.done));

  // Errors before the stream opens are plain HTTP, not SSE frames.
  const empty = await readStream(searchUrl(token, projectId, { query: '   ' }));
  check('an empty query is 400', empty.status === 400, String(empty.status));
  const badRegex = await readStream(searchUrl(token, projectId, { query: '(', regex: 'true' }));
  check('an invalid regex is 400', badRegex.status === 400, String(badRegex.status));
  // `..` is rejected as a malformed component before the filesystem is touched,
  // which is a 400 — the same split the file API uses (SPEC-002 §12). Only a path
  // that *resolves* outside the root, i.e. through a symlink, is the 403 case.
  const traversal = await readStream(searchUrl(token, projectId, { query: 'needle', path: '../..' }));
  check('a path with .. is refused', traversal.status === 400 || traversal.status === 403,
    String(traversal.status));
  const escape = await readStream(searchUrl(token, projectId, { query: 'needle', path: 'escape-link' }));
  check('a symlink out of the root is 403', escape.status === 403, String(escape.status));
  const missing = await readStream(searchUrl(token, 'no-such-project', { query: 'needle' }));
  check('an unknown project is 404', missing.status === 404, String(missing.status));

  const unauthed = await fetch(new URL(`${BASE}/api/projects/${projectId}/search?query=needle`));
  check('search without a token is 401', unauthed.status === 401, String(unauthed.status));
}

async function verifyMonitor(a, token) {
  console.log('\nmonitor');

  const metrics = await a.get('/api/system/metrics');
  check('GET /metrics is 200', metrics.status === 200, String(metrics.status));
  const m = metrics.body;
  check('cpu has a usage and a core count',
    typeof m?.cpu?.usage === 'number' && m.cpu.coreCount > 0, JSON.stringify(m?.cpu));
  check('memory total is plausible', m?.memory?.total > 0);
  check('host names the OS', typeof m?.host?.os === 'string' && m.host.os.length > 0);
  check('gpu is null or an object, never an error', m?.gpu === null || typeof m?.gpu === 'object');
  check('processes are present', Array.isArray(m?.processes) && m.processes.length > 0);
  check('processCount is the real total, not the page size',
    m.processCount >= m.processes.length, `${m.processCount} vs ${m.processes.length}`);
  check('default topN is 30', m.processes.length <= 30, String(m.processes.length));

  const cpuSorted = m.processes.every((p, i) => i === 0 || m.processes[i - 1].cpu >= p.cpu);
  check('default sort is descending cpu', cpuSorted);

  const byMemory = await a.get('/api/system/metrics?sort=memory&topN=5');
  const mem = byMemory.body.processes;
  check('sort=memory re-orders', mem.every((p, i) => i === 0 || mem[i - 1].memory >= p.memory));
  check('topN caps the list', mem.length <= 5, String(mem.length));

  const overCap = await a.get('/api/system/metrics?topN=999');
  check('topN over the ceiling is clamped, not rejected',
    overCap.status === 200 && overCap.body.processes.length <= 200,
    `${overCap.status} / ${overCap.body?.processes?.length}`);

  // Two samples 3s apart is the whole point of the stream (§3.3).
  const watchUrl = new URL(`${BASE}/api/system/watch`);
  watchUrl.searchParams.set('token', token);
  watchUrl.searchParams.set('topN', '5');
  const started = Date.now();
  const watch = await readStream(watchUrl, {
    stopWhen: (frames) => frames.filter((f) => f.event === 'metrics').length >= 2,
    timeoutMs: 15000,
  });
  const samples = watch.frames.filter((f) => f.event === 'metrics');
  check('watch delivers at least two samples', samples.length >= 2, `${samples.length} in ${Date.now() - started}ms`);
  check('watch samples carry the same shape as /metrics',
    samples.every((s) => typeof s.body?.cpu?.usage === 'number' && Array.isArray(s.body.processes)));
  check('watch honours topN', samples.every((s) => s.body.processes.length <= 5));
  if (samples.length >= 2) {
    const delta = samples[1].body.timestampMs - samples[0].body.timestampMs;
    check('samples are ~3s apart', delta >= 1500 && delta <= 6000, `${delta}ms`);
  }

  const unauthed = await fetch(new URL(`${BASE}/api/system/metrics`));
  check('metrics without a token is 401', unauthed.status === 401, String(unauthed.status));
}

async function verifyKill(a) {
  console.log('\nkill');

  // The only process this script signals is one it started itself.
  const child = spawn(process.execPath, ['-e', 'setInterval(() => {}, 1000)'], { stdio: 'ignore' });
  await sleep(400);

  const killed = await a.post(`/api/system/kill/${child.pid}`, { signal: 'term' });
  check('kill returns 200 for a live pid', killed.status === 200, JSON.stringify(killed.body));
  check('kill echoes the pid and signal',
    killed.body?.pid === child.pid && killed.body?.signal === 'term', JSON.stringify(killed.body));

  const exited = await Promise.race([
    new Promise((r) => child.once('exit', () => r(true))),
    sleep(3000).then(() => false),
  ]);
  check('the child actually died', exited === true);
  if (!exited) child.kill('SIGKILL');

  const gone = await a.post(`/api/system/kill/${child.pid}`, {});
  check('an already-dead pid is 404', gone.status === 404, JSON.stringify(gone.body));
  check('404 uses the process error group', gone.body?.error === 'process', JSON.stringify(gone.body));

  const init = await a.post('/api/system/kill/1', {});
  check('pid 1 is refused with 400', init.status === 400, JSON.stringify(init.body));
  const zero = await a.post('/api/system/kill/0', {});
  check('pid 0 is refused with 400', zero.status === 400, JSON.stringify(zero.body));

  const self = await a.post(`/api/system/kill/${await serverPid()}`, {});
  check('the server refuses to kill itself', self.status === 400, JSON.stringify(self.body));

  const badSignal = await a.post(`/api/system/kill/${process.pid}`, { signal: 'nope' });
  check('an unknown signal name is 400 in the signal group',
    badSignal.status === 400 && badSignal.body?.error === 'signal', JSON.stringify(badSignal.body));
}

/** The server's own pid, found through the port it is listening on. */
let cachedPid = null;
async function serverPid() {
  if (cachedPid !== null) return cachedPid;
  const out = execFileSync('lsof', ['-nP', `-iTCP:${PORT}`, '-sTCP:LISTEN', '-t'], { encoding: 'utf8' });
  cachedPid = Number(out.trim().split('\n')[0]);
  return cachedPid;
}

function verifyBundle() {
  console.log('\nbundle');
  const assets = join(REPO, 'src/web/dist/assets');
  if (!existsSync(assets)) {
    check('dist/assets exists', false, 'build the SPA first');
    return;
  }
  const files = readdirSync(assets).filter((f) => f.endsWith('.js'));
  const entryName = files.find((f) => /^index-/.test(f));
  const entry = entryName ? readFileSync(join(assets, entryName), 'utf8') : '';
  check('an entry chunk exists', entry.length > 0);

  // §5.9: no chart library. 60 points is one <path d="…">.
  const charts = ['chart.js', 'apexcharts', 'echarts', 'd3-scale', 'uplot', 'recharts'];
  const found = charts.filter((name) => entry.includes(name));
  check('no chart library in the entry chunk', found.length === 0, JSON.stringify(found));

  const strings = ['Tìm trong dự án', 'Lọc tiến trình', 'Kết thúc'];
  const missing = strings.filter((s) => !entry.includes(s));
  check('the search/monitor UI strings shipped', missing.length === 0, JSON.stringify(missing));

  console.log(`  info entry ${entry.length.toLocaleString()} B`);
}

function verifyBinaryStrings() {
  const binary = readFileSync(BIN);
  // The routes must be in the binary that was actually built, not only in source.
  const needles = ['/api/system/metrics', '/api/system/watch', '/search'];
  const missing = needles.filter((n) => !binary.includes(n));
  check('the release binary carries the SPEC-006 routes', missing.length === 0, JSON.stringify(missing));
}

async function main() {
  if (!existsSync(BIN)) {
    throw new Error(`binary not found: ${BIN}\nbuild it first (see the header of this file)`);
  }
  buildTree();
  console.log(`binary:   ${BIN}`);
  console.log(`data dir: ${DATA_DIR}`);
  console.log(`fixture:  ${FIXTURE}`);

  const server = await startServer();
  const a = api(server.token);
  try {
    const registered = await a.post('/api/projects', { path: FIXTURE });
    if (registered.status !== 200 && registered.status !== 201) {
      throw new Error(`could not register the fixture: ${registered.status} ${JSON.stringify(registered.body)}`);
    }
    const projectId = registered.body.id;

    await verifySearch(a, server.token, projectId);
    await verifyMonitor(a, server.token);
    await verifyKill(a);
    verifyBundle();
    verifyBinaryStrings();
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
  console.log('\nSPEC-006 §8.1 verified. The four checks in §8.2 remain manual.');
}

main().catch((error) => {
  console.error(error);
  rmSync(FIXTURE, { recursive: true, force: true });
  rmSync(DATA_DIR, { recursive: true, force: true });
  process.exit(1);
});
