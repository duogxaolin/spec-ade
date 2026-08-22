// SPEC-007 §8 verification — real claws, real cron fire, real keepAlive, real release binary.
//
// Usage (the SPA must be built before Rust embeds it):
//   npm --prefix src/web run build
//   cargo build --release --manifest-path src/server/Cargo.toml
//   node scripts/verify-spec-007.mjs
//
// This script owns disposable data and project directories, and the only process
// it kills is a child it spawned itself. It never registers or mutates the source
// tree. Reads cross the same HTTP boundary the browser uses.
//
// The agent catalogue cannot be created over HTTP (the settings API exposes only
// the editor branch), so the script seeds `settings.json` — snake_case top-level
// keys — into its data dir before boot, exactly what a user editing the file
// does. Seeding the token too means the verifier knows it before the server
// speaks; no log scraping.

import { spawn } from 'node:child_process';
import {
  existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync,
} from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { randomUUID } from 'node:crypto';

const REPO = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const PROFILE = process.argv.includes('--debug') ? 'debug' : 'release';
const TARGET = join(REPO, 'src/server/target', PROFILE);
const BIN = join(TARGET, 'spec-ade-server');
const MOCK = join(TARGET, 'mock_acp_agent');
// Distinct from verify-002..006 so all verifiers may run concurrently.
const PORT = 7397;
const BASE = `http://127.0.0.1:${PORT}`;
const TOKEN = randomUUID().replaceAll('-', '');
const DATA_DIR = mkdtempSync(join(tmpdir(), 'spec-ade-verify7-data-'));
const FIXTURE = mkdtempSync(join(tmpdir(), 'spec-ade-verify7-tree-'));

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

/** Poll `fn` until it returns truthy, or null once `timeoutMs` has elapsed. */
async function waitFor(fn, timeoutMs = 10000, stepMs = 250) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    try {
      const value = await fn();
      if (value) return value;
    } catch {}
    await sleep(stepMs);
  }
  return null;
}

/**
 * The fixture project: nothing special is needed for claws themselves, but the
 * skills endpoint walks the workspace, so plant one skill with frontmatter.
 */
function buildFixture() {
  const dir = join(FIXTURE, '.claude/skills/verify-skill-007');
  mkdirSync(dir, { recursive: true });
  writeFileSync(
    join(dir, 'SKILL.md'),
    '---\ndescription: Planted by verify-spec-007\n---\nSay verified.\n',
  );
}

/** Seed the catalogue the settings API will not let us create over HTTP. */
function seedSettings() {
  const agent = (id, script) => ({
    id,
    name: `Mock (${script ?? 'chunks'})`,
    command: MOCK,
    args: [],
    env: { MOCK_ACP_SCRIPT: script ?? 'chunks' },
  });
  // Top-level Settings keys are snake_case — only nested types carry
  // `rename_all = "camelCase"` (see acp-orchestration.rs's comment on the same trap).
  writeFileSync(
    join(DATA_DIR, 'settings.json'),
    JSON.stringify({
      auth_token: TOKEN,
      acp_agents: [agent('mock-chunks'), agent('mock-dier', 'die_after_handshake')],
    }),
  );
}

async function startServer() {
  const proc = spawn(BIN, ['-p', String(PORT), '-H', '127.0.0.1', '--no-open'], {
    env: { ...process.env, SPEC_ADE_DATA_DIR: DATA_DIR },
    stdio: ['ignore', 'pipe', 'pipe'],
  });
  let log = '';
  proc.stdout.on('data', (c) => { log += c.toString(); });
  proc.stderr.on('data', (c) => { log += c.toString(); });

  let ready = false;
  for (let i = 0; i < 150 && !ready; i += 1) {
    try {
      ready = (await fetch(`${BASE}/api/health`)).ok;
    } catch {}
    if (!ready) await sleep(100);
  }
  if (!ready) {
    proc.kill('SIGKILL');
    throw new Error(`server did not become ready; log:\n${log}`);
  }
  return { proc, log: () => log };
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
    put: (path, body) => request('PUT', path, body),
    del: (path) => request('DELETE', path),
  };
}

function clawInput(projectId, over = {}) {
  return {
    name: 'review-bot',
    agentId: 'mock-chunks',
    projectId,
    schedules: [],
    ...over,
  };
}

const EVERY_SECOND = '* * * * * *';

async function verifyTokenGate(a) {
  console.log('\ntoken gate');

  const health = await fetch(`${BASE}/api/health`);
  check('health is open', health.ok, String(health.status));

  const denied = await fetch(`${BASE}/api/claws`);
  check('claws without a token is 401', denied.status === 401, String(denied.status));
  const wrong = await fetch(`${BASE}/api/claws`, { headers: { 'x-spec-ade-token': 'nope' } });
  check('a wrong token is 401 too', wrong.status === 401, String(wrong.status));

  const ok = await a.get('/api/claws');
  check('the right token passes', ok.status === 200 && Array.isArray(ok.body), JSON.stringify(ok.body));
}

async function verifyCrud(a, projectId) {
  console.log('\ncrud');

  const created = await a.post('/api/claws', clawInput(projectId, {
    schedules: [{ cron: '0 9 * * *', prompts: ['morning review'] }],
  }));
  check('create is 201', created.status === 201, JSON.stringify(created.body));
  const claw = created.body;
  check('the id is server-generated', typeof claw?.id === 'string' && claw.id.length > 0);
  check('a new claw reports stopped', claw?.status?.state === 'stopped', JSON.stringify(claw?.status));
  check('scheduleCount counts enabled schedules', claw?.status?.scheduleCount === 1, JSON.stringify(claw?.status));
  check('nextRunAt is computed on create', typeof claw?.status?.nextRunAt === 'string', JSON.stringify(claw?.status));
  check('the cron is echoed as a human description',
    Array.isArray(claw?.status?.scheduleDescriptions)
      && claw.status.scheduleDescriptions[0].includes('09:00'),
    JSON.stringify(claw?.status?.scheduleDescriptions));

  const listed = await a.get('/api/claws');
  check('list contains the new claw', listed.body.some((c) => c.id === claw.id));
  const elsewhere = await a.get(`/api/claws?projectId=${randomUUID()}`);
  check('?projectId filters', !elsewhere.body.some((c) => c.id === claw.id));
  const scoped = await a.get(`/api/claws?projectId=${projectId}`);
  check('?projectId=own keeps it', scoped.body.some((c) => c.id === claw.id));

  const got = await a.get(`/api/claws/${claw.id}`);
  check('get by id returns the definition', got.status === 200 && got.body?.id === claw.id);

  const renamed = await a.put(`/api/claws/${claw.id}`, clawInput(projectId, {
    name: 'renamed-bot',
    schedules: [{ cron: '0 9 * * *', prompts: ['morning review'] }],
  }));
  check('put renames in place', renamed.status === 200 && renamed.body?.name === 'renamed-bot'
    && renamed.body?.id === claw.id, JSON.stringify(renamed.body));

  const removed = await a.del(`/api/claws/${claw.id}`);
  check('delete is 204', removed.status === 204, String(removed.status));
  const gone = await a.get(`/api/claws/${claw.id}`);
  check('get after delete is 404 group claw',
    gone.status === 404 && gone.body?.error === 'claw', JSON.stringify(gone.body));
  const twice = await a.del(`/api/claws/${claw.id}`);
  check('a second delete is 404 too',
    twice.status === 404 && twice.body?.error === 'claw', JSON.stringify(twice.body));

  return claw.id;
}

async function verifyValidation(a, projectId) {
  console.log('\nvalidation');

  const badAgent = await a.post('/api/claws', clawInput(projectId, { agentId: 'ghost' }));
  check('an unknown agent is 404 group agent',
    badAgent.status === 404 && badAgent.body?.error === 'agent', JSON.stringify(badAgent.body));

  const badProject = await a.post('/api/claws', clawInput('ghost-project'));
  check('an unknown project is 404 group project',
    badProject.status === 404 && badProject.body?.error === 'project', JSON.stringify(badProject.body));

  const badCron = await a.post('/api/claws', clawInput(projectId, {
    schedules: [
      { cron: '0 9 * * *', prompts: ['fine'] },
      { cron: '0 9 * *', prompts: ['broken'] },
    ],
  }));
  check('a bad cron is 400 group cron',
    badCron.status === 400 && badCron.body?.error === 'cron', JSON.stringify(badCron.body));
  check('the cron error names the offending index',
    badCron.body?.schedule === 1 && String(badCron.body?.detail).startsWith('schedule 1'),
    JSON.stringify(badCron.body));

  const telegram = await a.post('/api/claws', clawInput(projectId, {
    permissionMode: 'ask_via_telegram',
  }));
  check('ask_via_telegram is refused by name',
    telegram.status === 400 && telegram.body?.error === 'claw'
      && String(telegram.body?.detail).includes('ask_via_telegram'),
    JSON.stringify(telegram.body));

  const emptyName = await a.post('/api/claws', clawInput(projectId, { name: '   ' }));
  check('a blank name is 400',
    emptyName.status === 400 && emptyName.body?.error === 'claw', JSON.stringify(emptyName.body));

  const promptless = await a.post('/api/claws', clawInput(projectId, {
    schedules: [{ cron: EVERY_SECOND, prompts: [] }],
  }));
  check('an enabled schedule needs prompts', promptless.status === 400, JSON.stringify(promptless.body));
}

async function verifyLifecycle(a, projectId) {
  console.log('\nstart/stop lifecycle');

  // A prompts-only claw with no schedules: starting brings the connection up and
  // then idles — no trigger can interfere with the assertions below.
  const made = await a.post('/api/claws', clawInput(projectId, { name: 'lifecycle-bot' }));
  const id = made.body.id;

  const started = await a.post(`/api/claws/${id}/start`);
  check('start is 200', started.status === 200, JSON.stringify(started.body));

  const idle = await waitFor(async () => {
    const r = await a.get(`/api/claws/${id}`);
    return r.body?.status?.state === 'idle' ? r.body.status : null;
  }, 15000);
  check('the claw reaches idle', Boolean(idle), JSON.stringify(idle));
  check('the connection id names the claw',
    typeof idle?.connectionId === 'string' && idle.connectionId.startsWith('claw:'),
    JSON.stringify(idle));

  const again = await a.post(`/api/claws/${id}/start`);
  check('double start is 409 group claw',
    again.status === 409 && again.body?.error === 'claw', JSON.stringify(again.body));

  const stopped = await a.post(`/api/claws/${id}/stop`);
  check('stop is 200', stopped.status === 200, JSON.stringify(stopped.body));
  const afterStop = await waitFor(async () => {
    const r = await a.get(`/api/claws/${id}`);
    return r.body?.status?.state === 'stopped' ? r.body.status : null;
  }, 5000);
  check('the claw reports stopped', Boolean(afterStop), JSON.stringify(afterStop));

  const reStop = await a.post(`/api/claws/${id}/stop`);
  check('stop is idempotent', reStop.status === 200 && reStop.body?.status?.state === 'stopped',
    JSON.stringify(reStop.body));

  await a.del(`/api/claws/${id}`);
}

async function verifySkills(a, projectId) {
  console.log('\nskills');

  const list = await a.get(`/api/projects/${projectId}/skills`);
  check('skills is 200', list.status === 200 && Array.isArray(list.body), String(list.status));
  // Discovery also walks the real $HOME, so ambient user skills legitimately
  // appear — assert on the planted fixture by name, never on total length.
  const planted = list.body.find((s) => s.name === 'verify-skill-007');
  check('the planted workspace skill is discovered', Boolean(planted), JSON.stringify(planted));
  check('its source is workspace', planted?.source === 'workspace');
  check('frontmatter survives',
    planted?.description === 'Planted by verify-spec-007' && String(planted?.prompt).includes('Say verified.'),
    JSON.stringify(planted));

  const missing = await a.get(`/api/projects/ghost-project/skills`);
  check('skills of an unknown project is 404 group project',
    missing.status === 404 && missing.body?.error === 'project', JSON.stringify(missing.body));
}

async function verifyScheduleFire(a, projectId) {
  console.log('\nreal schedule fire');

  const made = await a.post('/api/claws', clawInput(projectId, {
    name: 'heartbeat',
    schedules: [{ label: 'tick', cron: EVERY_SECOND, prompts: ['go'] }],
  }));
  const id = made.body.id;
  const started = await a.post(`/api/claws/${id}/start`);
  check('start is 200', started.status === 200, JSON.stringify(started.body));

  const fired = await waitFor(async () => {
    const r = await a.get(`/api/claws/${id}`);
    return r.body?.status?.lastRunAt ? r.body.status : null;
  }, 15000);
  check('the every-second schedule actually fired (lastRunAt set)', Boolean(fired), JSON.stringify(fired));
  check('a completed turn resets the restart streak', fired?.restarts === 0, JSON.stringify(fired));
  check('the claw is live between triggers',
    fired?.state === 'running' || fired?.state === 'idle', JSON.stringify(fired));

  await a.post(`/api/claws/${id}/stop`);
  await a.del(`/api/claws/${id}`);
}

async function verifyKeepAliveCap(a, projectId) {
  console.log('\nkeepAlive cap of 3');

  // `die_after_handshake` survives spawn and handshake, then exits mid-turn —
  // so the death only happens once a trigger sends the first prompt.
  const made = await a.post('/api/claws', clawInput(projectId, {
    name: 'doomed',
    agentId: 'mock-dier',
    keepAlive: true,
    schedules: [{ cron: EVERY_SECOND, prompts: ['go'] }],
  }));
  const id = made.body.id;
  const started = await a.post(`/api/claws/${id}/start`);
  check('start is 200 (the agent dies later, mid-turn)',
    started.status === 200, JSON.stringify(started.body));

  // Backoff is 1s/2s/4s, so reaching the cap takes ≥7s after the first death.
  const gaveUp = await waitFor(async () => {
    const r = await a.get(`/api/claws/${id}`);
    return r.body?.status?.state === 'error' ? r.body.status : null;
  }, 45000);
  check('the claw ends in error', Boolean(gaveUp), JSON.stringify(gaveUp));
  check('restarts climbed to the cap', gaveUp?.restarts === 3, JSON.stringify(gaveUp));
  check('lastError names the give-up',
    String(gaveUp?.lastError).includes('giving up after 3 restarts'), JSON.stringify(gaveUp));

  // An `error` slot is a finished placeholder, not a live loop — the next start
  // must replace it rather than collide with it (409 here would mean the runtime
  // treats error as running).
  const recovered = await a.post(`/api/claws/${id}/start`);
  check('a manual start recovers an errored claw', recovered.status === 200,
    JSON.stringify(recovered.body));

  await a.post(`/api/claws/${id}/stop`);
  await a.del(`/api/claws/${id}`);
}

function verifyBinaryStrings() {
  const binary = readFileSync(BIN);
  // The routes must be in the binary that was actually built, not only in source.
  const needles = ['/api/claws', '/skills'];
  const missing = needles.filter((n) => !binary.includes(n));
  check('the release binary carries the SPEC-007 routes', missing.length === 0, JSON.stringify(missing));
}

async function main() {
  if (!existsSync(BIN)) {
    throw new Error(`binary not found: ${BIN}\nbuild it first (see the header of this file)`);
  }
  if (!existsSync(MOCK)) {
    throw new Error(`mock agent not found: ${MOCK}\nit builds alongside the server (see the header of this file)`);
  }
  buildFixture();
  seedSettings();
  console.log(`binary:   ${BIN}`);
  console.log(`mock:     ${MOCK}`);
  console.log(`data dir: ${DATA_DIR}`);
  console.log(`fixture:  ${FIXTURE}`);

  const server = await startServer();
  const a = api(TOKEN);
  try {
    const registered = await a.post('/api/projects', { path: FIXTURE });
    if (registered.status !== 200 && registered.status !== 201) {
      throw new Error(`could not register the fixture: ${registered.status} ${JSON.stringify(registered.body)}`);
    }
    const projectId = registered.body.id;

    await verifyTokenGate(a);
    await verifyCrud(a, projectId);
    await verifyValidation(a, projectId);
    await verifyLifecycle(a, projectId);
    await verifySkills(a, projectId);
    await verifyScheduleFire(a, projectId);
    await verifyKeepAliveCap(a, projectId);
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
  console.log('\nSPEC-007 §8 verified.');
}

main().catch((error) => {
  console.error(error);
  rmSync(FIXTURE, { recursive: true, force: true });
  rmSync(DATA_DIR, { recursive: true, force: true });
  process.exit(1);
});
