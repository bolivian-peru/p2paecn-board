// End-to-end tests of the client against a board with rate limits, and against a stub that always says 429.
// Run from the repository root after `cargo build`:  node --test client-test/
// p2p.test.mjs imports this file. It starts its own board on 127.0.0.1 at the second port after $P2P_TEST_PORT
// (8823 when that is unset), with 2 writes per key and 8 requests per key in a window of 6 seconds.
import { after, before, test } from 'node:test';
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { existsSync, mkdirSync, mkdtempSync, readdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import http from 'node:http';
import { tmpdir } from 'node:os';
import { dirname, join, relative, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const CLIENT = join(ROOT, 'partner-pack', 'p2p.mjs');
const SERVER = join(ROOT, 'target', 'debug', 'p2paecn-board');
const PORT = Number(process.env.P2P_TEST_PORT || 8821) + 2;
const HOST = `127.0.0.1:${PORT}`;
const BOARD = `http://${HOST}`;
const WAITING = /The board's rate limit is reached for this key or room\. Waiting (\d+) s, then sending the same request again \(try (\d) of 4\)\./g;

const work = mkdtempSync(join(tmpdir(), 'p2p-limits-test-'));
const home = name => join(work, `home-${name}`);
let server;
let serverLog = '';

function op(method, path, body) {
  return new Promise((done, fail) => {
    const req = http.request({ socketPath: relative(process.cwd(), join(work, 'op.sock')), method, path, headers: { 'content-type': 'application/json' } }, res => {
      let text = '';
      res.on('data', c => (text += c));
      res.on('end', () => done({ status: res.statusCode, json: text ? JSON.parse(text) : null }));
    });
    req.on('error', fail);
    req.end(body ? JSON.stringify(body) : '');
  });
}

function client(who, args) {
  return new Promise((done, fail) => {
    const env = { PATH: process.env.PATH, HOME: work, P2P_HOME: home(who) };
    const started = Date.now();
    const child = spawn(process.execPath, [CLIENT, ...args], { env, stdio: ['ignore', 'pipe', 'pipe'] });
    let stdout = '';
    let stderr = '';
    child.stdout.on('data', c => (stdout += c));
    child.stderr.on('data', c => (stderr += c));
    child.on('error', fail);
    child.on('close', code => done({ code, stdout, stderr, all: stdout + stderr, took: Date.now() - started }));
  });
}

function file(name, text) {
  const path = join(work, name);
  writeFileSync(path, text, { mode: 0o600 });
  return path;
}

const pendingFiles = who => (existsSync(join(home(who), 'pending')) ? readdirSync(join(home(who), 'pending')) : []);
const post = (who, n) => client(who, ['post', 'a-x', '--kind', 'notice', '--title', `Maintenance window number ${n} is planned`, '--text-file', file('n.txt', 'Plan for it.')]);
const sleep = ms => new Promise(r => setTimeout(r, ms));

before(async () => {
  assert.ok(existsSync(SERVER), `build the server first: ${SERVER}`);
  const env = { ...process.env, P2PAECN_HOST: HOST, P2PAECN_BIND: HOST, P2PAECN_ADMIN_SOCKET: 'op.sock',
    P2PAECN_LIMIT_KEY_WRITES: '2', P2PAECN_LIMIT_KEY_REQUESTS: '8', P2PAECN_LIMIT_WINDOW_SECONDS: '6' };
  for (const name of ['P2PAECN_CONFIG', 'P2PAECN_STATE', 'P2PAECN_LIMIT_ROOM_WRITES', 'P2PAECN_LIMIT_UNSIGNED', 'P2PAECN_SOURCE_HEADER']) delete env[name];
  server = spawn(SERVER, [], { cwd: work, env, stdio: ['ignore', 'ignore', 'pipe'] });
  server.stderr.on('data', c => (serverLog += c));
  for (let i = 0; ; i++) {
    if (server.exitCode !== null) throw new Error(`the board exited: ${serverLog}`);
    const up = await new Promise(done => {
      http.get(`${BOARD}/`, res => done(res.resume() && res.statusCode === 200 && existsSync(join(work, 'op.sock')))).on('error', () => done(false));
    });
    if (up) break;
    if (i > 100) throw new Error(`the board did not start: ${serverLog}`);
    await sleep(100);
  }
  for (const id of ['supplier-a', 'reseller-x']) assert.equal((await op('POST', '/orgs', { id })).status, 201);
  assert.equal((await op('POST', '/rooms', { id: 'a-x', kind: 'pair', members: [{ org: 'supplier-a' }, { org: 'reseller-x' }] })).status, 201);
  const issued = await op('POST', '/enroll-codes', { org: 'reseller-x', class: 'text' });
  const enrolled = await client('x', ['enroll', '--url', BOARD, '--name', 'agent-1', '--code-file', file('code-x', `${issued.json.code}\n`)]);
  assert.equal(enrolled.code, 0, enrolled.all);
  const fingerprint = enrolled.stdout.match(/^fingerprint (\S+)$/m)[1];
  assert.equal((await op('POST', '/keys/activate', { keyId: 'reseller-x/agent-1', fingerprint })).status, 200);
});

after(async () => {
  if (server && server.exitCode === null) {
    await new Promise(done => {
      server.once('exit', done);
      server.kill('SIGTERM');
    });
  }
  rmSync(work, { recursive: true, force: true });
});

test('a write over the key\'s limit: the client says so, waits as long as the board asks, and the write is done once', async () => {
  assert.match(serverLog, /limits per 6 s: 8 requests and 2 writes per key/);
  await sleep(6200); // a fresh window
  const first = await post('x', 1);
  const second = await post('x', 2);
  assert.deepEqual([first.code, second.code, first.stderr, second.stderr], [0, 0, '', ''], first.all + second.all);
  const third = await post('x', 3);
  assert.equal(third.code, 0, third.all);
  const said = [...third.stderr.matchAll(WAITING)];
  assert.ok(said.length >= 1 && said.length <= 3, third.stderr);
  assert.ok(Number(said[0][1]) >= 1 && Number(said[0][1]) <= 6 && said[0][2] === '2', third.stderr);
  assert.ok(third.took >= 1000, `it waited: ${third.took} ms`);
  assert.match(third.stdout, /^posted [0-9a-f-]{36}$/m);
  assert.deepEqual(pendingFiles('x'), []);
  await sleep(6200);
  const listed = JSON.parse((await client('x', ['cards', 'a-x', '--json'])).stdout).cards;
  assert.equal(listed.length, 3, 'three cards, the third once');
});

test('reads over the key\'s limit are retried too, and every one succeeds', async () => {
  await sleep(6200);
  const runs = await Promise.all(Array.from({ length: 12 }, () => client('x', ['inbox'])));
  for (const r of runs) assert.equal(r.code, 0, r.all);
  const waited = runs.filter(r => r.stderr.includes('rate limit is reached'));
  assert.ok(waited.length >= 4, `twelve reads against a limit of 8: ${waited.length} waited`);
  for (const r of runs) assert.match(r.stdout, /^waiting on us \(0\)$/m);
});

test('a board that keeps saying 429: four tries, then exit 3, and a write keeps its pending file', async () => {
  let asked = [];
  let retryAfter = '1';
  const stub = http.createServer((req, res) => {
    asked.push({ method: req.method, url: req.url, idem: req.headers['idempotency-key'], nonce: req.headers['p2p-nonce'] });
    req.resume();
    const headers = { 'content-type': 'application/json' };
    if (retryAfter !== null) headers['retry-after'] = retryAfter;
    res.writeHead(429, headers).end('{"error":"rate_limited","retryAfter":1}');
  });
  await new Promise(done => stub.listen(0, '127.0.0.1', done));
  try {
    mkdirSync(home('stub'), { mode: 0o700 });
    const profile = { ...JSON.parse(readFileSync(join(home('x'), 'profile.json'), 'utf8')), url: `http://127.0.0.1:${stub.address().port}` };
    writeFileSync(join(home('stub'), 'profile.json'), JSON.stringify(profile), { mode: 0o600 });

    const read = await client('stub', ['inbox']);
    assert.equal(read.code, 3, read.all);
    assert.equal(asked.length, 4);
    assert.equal(new Set(asked.map(a => a.nonce)).size, 4, 'each try is signed afresh');
    assert.deepEqual([...read.stderr.matchAll(WAITING)].map(m => m[2]), ['2', '3', '4']);
    assert.match(read.stderr, /uncertain: the board's rate limit is still reached, so this was not done\. Re-run the same command later\.$/m);

    asked = [];
    const write = await post('stub', 9);
    assert.equal(write.code, 3, write.all);
    assert.equal(asked.length, 4);
    assert.equal(new Set(asked.map(a => a.idem)).size, 1, 'every try carries the same idempotency key');
    assert.equal(pendingFiles('stub').length, 1, 'the write is not done: its pending file stays');
    // Run again, it sends the same idempotency key.
    const key = asked[0].idem;
    asked = [];
    retryAfter = null;
    const again = await post('stub', 9);
    assert.equal(again.code, 3);
    assert.deepEqual([asked.length, asked[0].idem], [1, key], 'no Retry-After: no retry, and the same key');

    // A wait the client does not take: longer than two minutes, or not a number of seconds.
    for (const value of ['121', '99999', 'soon', '-1', '0']) {
      asked = [];
      retryAfter = value;
      const r = await client('stub', ['rooms']);
      assert.deepEqual([r.code, asked.length], [3, 1], `Retry-After ${value}: ${r.all}`);
      assert.ok(r.took < 1500 && !r.stderr.includes('Waiting'));
    }
  } finally {
    await new Promise(r => stub.close(r));
  }
});
