// End-to-end tests for `p2p.mjs verify` and the room record (partner-pack/CHAIN.md) against the real board server.
// Run from the repository root after `cargo build`:  node --test client-test/
// p2p.test.mjs imports this file, so both run in one process; it also runs on its own. It starts its own board,
// with a state file, on 127.0.0.1 at the port after $P2P_TEST_PORT (8822 when that is unset), beside the board of
// p2p.test.mjs. The tests stop that board, change
// its state file by hand as someone with access to the machine could, and start it again. Every key is made at
// run time by the client under test, except the fixed TEST key of the old-format fixture. Organisations, rooms
// and findings are invented.
import { after, before, test } from 'node:test';
import assert from 'node:assert/strict';
import { execFileSync, spawn } from 'node:child_process';
import { createHash, createPrivateKey, createPublicKey } from 'node:crypto';
import { chmodSync, existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, statSync, writeFileSync } from 'node:fs';
import http from 'node:http';
import { tmpdir } from 'node:os';
import { dirname, join, relative, resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const CLIENT = join(ROOT, 'partner-pack', 'p2p.mjs');
const { chainEntry, chainPayload } = await import(pathToFileURL(CLIENT).href);
const SERVER = join(ROOT, 'target', 'debug', 'p2paecn-board');
const PORT = Number(process.env.P2P_TEST_PORT || 8821) + 1;
const HOST = `127.0.0.1:${PORT}`;
const BOARD = `http://${HOST}`;
const PAIR = 'a-x';
const SHARED = 'hub';
const HASH = /^[0-9a-f]{64}$/;
const CHANGED = /The record was changed after it was last checked\./;

const work = mkdtempSync(join(tmpdir(), 'p2p-verify-test-'));
const home = name => join(work, `home-${name}`);
const sha256Hex = bytes => createHash('sha256').update(bytes).digest('hex');
let server;
let serverLog = '';
let dir; // the directory of the running board: its state file and operator socket

// ---------------------------------------------------------------- helpers

/** One call on the operator channel. The socket path is given relative to the working directory, which keeps it
 * under the 104-byte limit for Unix socket paths however deep os.tmpdir() is. */
function op(method, path, body) {
  return new Promise((done, fail) => {
    const req = http.request({ socketPath: relative(process.cwd(), join(dir, 'op.sock')), method, path, headers: { 'content-type': 'application/json' } }, res => {
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
    const child = spawn(process.execPath, [CLIENT, ...args], { env, stdio: ['ignore', 'pipe', 'pipe'] });
    let stdout = '';
    let stderr = '';
    child.stdout.on('data', c => (stdout += c));
    child.stderr.on('data', c => (stderr += c));
    child.on('error', fail);
    child.on('close', code => done({ code, stdout, stderr, all: stdout + stderr }));
  });
}

async function ok(who, args) {
  const r = await client(who, args);
  assert.equal(r.code, 0, `${args.join(' ')}: ${r.all}`);
  return r;
}

function file(name, text) {
  const path = join(work, name);
  writeFileSync(path, text, { mode: 0o600 });
  return path;
}

function stopBoard() {
  if (!server || server.exitCode !== null || server.signalCode !== null) return Promise.resolve();
  return new Promise(done => {
    server.once('exit', done);
    server.kill('SIGTERM');
  });
}

/** Starts the board on the state file in `in_`, and waits until a request signed now is no longer "before start". */
async function startBoard(in_) {
  dir = in_;
  serverLog = '';
  const env = { ...process.env, P2PAECN_HOST: HOST, P2PAECN_BIND: HOST, P2PAECN_ADMIN_SOCKET: 'op.sock', P2PAECN_STATE: 'state.json' };
  delete env.P2PAECN_CONFIG;
  server = spawn(SERVER, [], { cwd: dir, env, stdio: ['ignore', 'ignore', 'pipe'] });
  server.stderr.on('data', c => (serverLog += c));
  for (let i = 0; ; i++) {
    if (server.exitCode !== null) throw new Error(`the board exited: ${serverLog}`);
    const up = await new Promise(done => {
      http.get(`${BOARD}/`, res => done(res.resume() && res.statusCode === 200 && existsSync(join(dir, 'op.sock')))).on('error', () => done(false));
    });
    if (up) break;
    if (i > 100) throw new Error(`the board did not start: ${serverLog}`);
    await new Promise(r => setTimeout(r, 100));
  }
  const started = Math.floor(Date.now() / 1000);
  while (Math.floor(Date.now() / 1000) <= started) await new Promise(r => setTimeout(r, 50));
}

const statePath = () => join(dir, 'state.json');
const readState = () => JSON.parse(readFileSync(statePath(), 'utf8'));

/** Stops the board, lets `change` edit its state (or gives it other bytes), and starts it again. */
async function restartWith(change) {
  await stopBoard();
  const state = readState();
  const next = change(state) ?? state;
  writeFileSync(statePath(), typeof next === 'string' ? next : `${JSON.stringify(next)}\n`, { mode: 0o600 });
  chmodSync(statePath(), 0o600);
  await startBoard(dir);
}

/** What someone who can write the state file would do to hide a change: every hash of a room's chain made again. */
function rebuilt(room, entries) {
  let prev = '0'.repeat(64);
  return entries.map((e, i) => {
    const entry = { ...e, seq: i + 1, prev };
    entry.hash = sha256Hex(chainEntry({ ...entry, room }));
    prev = entry.hash;
    return entry;
  });
}

const messagePayload = m => sha256Hex(chainPayload('message', { id: m.id, kind: m.kind, replaces: m.replaces, text: m.text }));
const heads = who => JSON.parse(readFileSync(join(home(who), 'chain-heads.json'), 'utf8'));

async function enrollAndActivate(who, name, org, cls) {
  const issued = await op('POST', '/enroll-codes', { org, class: cls });
  assert.equal(issued.status, 201);
  const r = await ok(who, ['enroll', '--url', BOARD, '--name', name, '--code-file', file(`code-${who}`, `${issued.json.code}\n`)]);
  const fingerprint = r.stdout.match(/^fingerprint (\S+)$/m)[1];
  assert.equal((await op('POST', '/keys/activate', { keyId: `${org}/${name}`, fingerprint })).status, 200);
}

const posted = r => r.stdout.match(/^posted (\S+)$/m)[1];
const cards = {};
let good; // the state file after the board was filled and verified once

// ---------------------------------------------------------------- the board

before(async () => {
  assert.ok(existsSync(SERVER), `build the server first: ${SERVER}`);
  mkdirSync(join(work, 'live'), { mode: 0o700 });
  await startBoard(join(work, 'live'));
  for (const id of ['supplier-a', 'reseller-x', 'reseller-y']) assert.equal((await op('POST', '/orgs', { id })).status, 201);
  assert.equal((await op('POST', '/rooms', { id: PAIR, kind: 'pair', members: [{ org: 'supplier-a' }, { org: 'reseller-x' }] })).status, 201);
  const members = [{ org: 'supplier-a', handle: 'member-1' }, { org: 'reseller-x', handle: 'member-2' }, { org: 'reseller-y', handle: 'member-3' }];
  assert.equal((await op('POST', '/rooms', { id: SHARED, kind: 'shared', members })).status, 201);
  await enrollAndActivate('a', 'agent-1', 'supplier-a', 'text');
  await enrollAndActivate('x', 'agent-1', 'reseller-x', 'text');
  await enrollAndActivate('y', 'loop-1', 'reseller-y', 'fields');

  // A finding in the pair room taken to verified, a notice with its ack, and a finding in the shared room.
  const text = file('first.txt', 'Page two of the item list starts at item 22.\nItem 21 is never returned. Ünïcode ok \u{1F600}');
  cards.first = posted(await ok('x', ['post', PAIR, '--kind', 'finding', '--severity', 'wrong', '--title', 'List endpoint skips one item when paging', '--text-file', text]));
  await ok('a', ['reply', cards.first, '--kind', 'reply', '--text-file', file('reply.txt', 'Reproduced. The second page starts one item late.')]);
  await ok('a', ['stage', cards.first, 'shipped', '--text-file', file('shipped.txt', 'Fixed in the next build.')]);
  await ok('x', ['stage', cards.first, 'closed', '--outcome', 'verified', '--text-file', file('works.txt', 'Works.')]);
  cards.notice = posted(await ok('a', ['post', PAIR, '--kind', 'notice', '--title', 'A response field is renamed in the next kit release', '--text-file', file('notice.txt', 'kind:7:finding\ntext:3:abc')]));
  await ok('x', ['reply', cards.notice, '--kind', 'ack', '--text-file', file('ack.txt', 'Noted.')]);
  cards.shared = posted(await ok('x', ['post', SHARED, '--kind', 'finding', '--severity', 'question', '--title', 'Is a sandbox mode planned for testing orders?', '--text-file', file('q.txt', 'Asking for the next release.')]));
  await ok('a', ['reply', cards.shared, '--kind', 'reply', '--text-file', file('answer.txt', 'Not planned yet.')]);
});

after(async () => {
  await stopBoard();
  rmSync(work, { recursive: true, force: true });
});

// ---------------------------------------------------------------- tests

test('the vectors are what the generator makes now, in both copies, and the client hashes them the same way', () => {
  const made = execFileSync(process.execPath, [join(ROOT, 'tools', 'gen-chain-vectors.mjs')], { encoding: 'utf8' });
  assert.equal(readFileSync(join(ROOT, 'tests', 'vectors', 'chain-1.json'), 'utf8'), made);
  assert.equal(readFileSync(join(ROOT, 'partner-pack', 'vectors', 'chain-1.json'), 'utf8'), made);
  const vectors = JSON.parse(made);
  let entries = 0;
  for (const room of vectors.rooms) {
    let prev = vectors.genesis;
    for (const { values, payloadRecord, entryRecord, entry } of room.entries) {
      // The records in the file are text. Hashing their UTF-8 bytes, without the client's own code, gives the hashes.
      assert.equal(sha256Hex(Buffer.from(payloadRecord, 'utf8')), entry.payload);
      assert.equal(sha256Hex(Buffer.from(entryRecord, 'utf8')), entry.hash);
      assert.equal(entry.prev, prev);
      assert.equal(chainPayload(entry.kind, values).toString('utf8'), payloadRecord);
      assert.equal(chainEntry({ ...entry, room: room.room }).toString('utf8'), entryRecord);
      prev = entry.hash;
      entries++;
    }
    assert.deepEqual(room.head, { seq: room.entries.length, hash: prev });
  }
  assert.equal(entries, 30);
  assert.equal(chainPayload('release', {}).toString('utf8'), 'p2paecn-chain-1 payload release\n');
  // A length is a number of bytes: two for U+00FC, four for U+1F600.
  assert.equal(chainPayload('member-added', { name: 'ü\u{1F600}' }).toString('utf8'), 'p2paecn-chain-1 payload member-added\nname:6:ü\u{1F600}\n');
});

test('verify passes on a pair room and a shared room, and keeps each head in a 0600 file', async () => {
  const first = await ok('x', ['verify']);
  assert.match(first.stdout, new RegExp(`^room ${PAIR}: verified$`, 'm'));
  assert.match(first.stdout, new RegExp(`^room ${SHARED}: verified$`, 'm'));
  assert.match(first.stdout, /^ {2}head {6}entry 10 {2}[0-9a-f]{64}$/m, 'two members, a card, a reply, two moves of two entries each, a notice, an ack');
  assert.match(first.stdout, /compared {2}2 cards, 6 messages and 2 members with 10 entries/);
  assert.match(first.stdout, /compared {2}1 cards, 2 messages and 3 members with 5 entries/);
  assert.equal(first.stdout.match(/no head was saved for this room: this is its first check/g).length, 2);
  // It prints no text of a card, so there is nothing to fence, and no organisation id of the shared room.
  for (const leak of ['Page two', 'sandbox', 'Not planned', 'BEGIN DATA', 'reseller-y']) assert.ok(!first.all.includes(leak), leak);

  const path = join(home('x'), 'chain-heads.json');
  assert.equal(statSync(path).mode & 0o777, 0o600);
  const saved = heads('x');
  assert.deepEqual([saved.format, saved.board, Object.keys(saved.rooms)], ['p2paecn-chain-1', BOARD, [PAIR, SHARED]]);
  assert.ok(saved.rooms[PAIR].seq === 10 && HASH.test(saved.rooms[PAIR].hash) && saved.rooms[SHARED].seq === 5);
  // The saved head is the one the board lists for the room.
  const listed = JSON.parse((await ok('x', ['rooms', '--json'])).stdout).rooms;
  assert.deepEqual(listed.map(r => r.chain), [PAIR, SHARED].map(r => ({ seq: saved.rooms[r].seq, hash: saved.rooms[r].hash })));

  // Again: the saved head is found in the record. One room on its own. JSON.
  const again = await ok('x', ['verify']);
  assert.equal(again.stdout.match(/entry \d+ was the head at the last check \(\S+\): it is still in the record/g).length, 2);
  const one = await ok('x', ['verify', SHARED]);
  assert.ok(one.stdout.includes(`room ${SHARED}: verified`) && !one.stdout.includes(`room ${PAIR}`));
  const json = JSON.parse((await ok('x', ['verify', '--json'])).stdout);
  assert.equal(json.ok, true);
  assert.deepEqual(json.rooms.map(r => [r.room, r.ok, r.entries, r.cards, r.messages, r.members, r.withheld, r.imported, r.problems.length]),
    [[PAIR, true, 10, 2, 6, 2, 0, 0, 0], [SHARED, true, 5, 1, 2, 3, 0, 0, 0]]);
  assert.deepEqual(json.rooms[0].head, { seq: 10, hash: saved.rooms[PAIR].hash });
  assert.equal(json.rooms[0].saved.hash, saved.rooms[PAIR].hash);

  // After more writes the old head is still in the record, and the saved head moves on.
  await ok('a', ['reply', cards.shared, '--kind', 'proof', '--text-file', file('proof.txt', 'We run it against a copy.')]);
  const later = await ok('x', ['verify', SHARED]);
  assert.match(later.stdout, /head {6}entry 6 /);
  assert.match(later.stdout, /entry 5 was the head at the last check/);
  assert.equal(heads('x').rooms[SHARED].seq, 6);
  assert.equal(heads('x').rooms[PAIR].seq, 10, 'a room that was not checked keeps its head');

  // Another member of the same rooms arrives at the same heads: what two members compare out of band.
  await ok('a', ['verify']);
  assert.deepEqual(heads('a').rooms[PAIR].hash, heads('x').rooms[PAIR].hash);
  assert.deepEqual(heads('a').rooms[SHARED].hash, heads('x').rooms[SHARED].hash);

  // Usage and refusals.
  assert.equal((await client('x', ['verify', 'Not A Room'])).code, 1);
  const missing = await client('x', ['verify', 'no-such-room']);
  assert.equal(missing.code, 2);
  assert.match(missing.stderr, /not_found/);
  assert.match((await ok('x', ['help'])).stdout, /verify \[room\]/);
  good = readFileSync(statePath(), 'utf8');
});

test('a fields key verifies the links, authors, times and stages, and says which texts it could not compare', async () => {
  const r = await ok('y', ['verify']);
  assert.match(r.stdout, new RegExp(`^room ${SHARED}: verified$`, 'm'));
  // The card (its title and report) and the two messages after it were written by other members.
  assert.match(r.stdout, /compared {2}1 cards, 3 messages and 3 members with 6 entries/);
  assert.match(r.stdout, /withheld {2}3 cards or messages were compared without their title or text: this key is class fields/);
  for (const leak of ['sandbox', 'Asking for', 'Not planned', 'copy', 'reseller-x', 'supplier-a']) assert.ok(!r.all.includes(leak), leak);
  assert.equal(heads('y').rooms[SHARED].hash, heads('x').rooms[SHARED].hash, 'the head does not depend on the key class');
  assert.equal(JSON.parse((await ok('y', ['verify', '--json'])).stdout).rooms[0].withheld, 3);
});

test('a text changed in the state file: verify exits 5, names the message, and keeps the saved head', async () => {
  const before = readFileSync(join(home('x'), 'chain-heads.json'), 'utf8');
  let changed;
  await restartWith(state => {
    changed = state.messages.find(m => m.text === 'Reproduced. The second page starts one item late.');
    changed.text = 'Could not reproduce.';
  });
  const r = await client('x', ['verify']);
  assert.equal(r.code, 5, r.all);
  assert.match(r.stdout, new RegExp(`^room ${PAIR}: DOES NOT VERIFY$`, 'm'));
  assert.match(r.stdout, new RegExp(`differs {3}message ${changed.id} on card ${cards.first}: its text, kind, author or time is not in the record`));
  assert.match(r.stdout, new RegExp(`differs {3}entry 4 records a message on card ${cards.first} that the board does not show`));
  assert.match(r.stdout, new RegExp(`^room ${SHARED}: verified$`, 'm'), 'the other room is still checked, and verifies');
  assert.match(r.stderr, new RegExp(`^verify: ${PAIR}: the record does not verify\\. `));
  assert.match(r.stderr, CHANGED);
  assert.match(r.stderr, /Tell a person\. Do not post, reply or move a card because of this\./);
  assert.ok(!r.all.includes('Could not reproduce'), 'the changed text itself is not printed');
  assert.equal(readFileSync(join(home('x'), 'chain-heads.json'), 'utf8').replace(/"checked": \d+/g, ''), before.replace(/"checked": \d+/g, ''), 'no head moved');
  assert.equal(heads('x').rooms[PAIR].hash, JSON.parse(before).rooms[PAIR].hash);

  const json = await client('x', ['verify', PAIR, '--json']);
  assert.equal(json.code, 5);
  const out = JSON.parse(json.stdout);
  assert.deepEqual([out.ok, out.rooms[0].ok, out.rooms[0].problems.length], [false, false, 2]);

  // The same change with the record made to match: the chain links, the text agrees, and the saved head gives it away.
  await restartWith(state => {
    const entries = state.chains[PAIR];
    entries[3].payload = messagePayload(state.messages.find(m => m.id === changed.id));
    state.chains[PAIR] = rebuilt(PAIR, entries);
  });
  const hidden = await client('x', ['verify', PAIR]);
  assert.equal(hidden.code, 5, hidden.all);
  assert.match(hidden.stdout, /differs {3}entry 10 is not the entry that was the head when it was last checked \(\S+\): the record was rewritten/);
  assert.equal(hidden.stdout.match(/differs/g).length, 1, 'nothing else differs: only the saved head shows it');
  assert.match(hidden.stderr, CHANGED);

  // The limit, stated in CHAIN.md section 1: a key that never saved a head for this room has nothing to compare
  // with, so the same rewritten record passes its first check.
  await enrollAndActivate('n', 'agent-2', 'supplier-a', 'text');
  const fresh = await ok('n', ['verify', PAIR]);
  assert.match(fresh.stdout, /no head was saved for this room: this is its first check/);
  await restartWith(() => good);
  await ok('x', ['verify']);
});

test('an entry removed from the record: verify fails whether or not the hashes were made again', async () => {
  // The reply and its entry taken out, and nothing else touched: the board does not start on such a file.
  await stopBoard();
  const state = JSON.parse(good);
  const reply = state.messages.find(m => m.text === 'Reproduced. The second page starts one item late.');
  state.messages = state.messages.filter(m => m.id !== reply.id);
  const without = state.chains[PAIR].filter(e => e.seq !== 4);
  state.chains[PAIR] = without;
  writeFileSync(statePath(), JSON.stringify(state), { mode: 0o600 });
  const refused = spawn(SERVER, [], { cwd: dir, env: { ...process.env, P2PAECN_HOST: HOST, P2PAECN_BIND: HOST, P2PAECN_STATE: 'state.json' }, stdio: ['ignore', 'ignore', 'pipe'] });
  let log = '';
  refused.stderr.on('data', c => (log += c));
  assert.notEqual(await new Promise(done => refused.on('exit', done)), 0);
  assert.match(log, /the chain of room a-x does not link at entry 4/);

  // With every hash made again the board starts, and the client finds the saved head gone.
  state.chains[PAIR] = rebuilt(PAIR, without);
  writeFileSync(statePath(), JSON.stringify(state), { mode: 0o600 });
  await startBoard(dir);
  const r = await client('x', ['verify', PAIR]);
  assert.equal(r.code, 5, r.all);
  assert.match(r.stdout, /differs {3}the record now ends at entry 9, but entry 10 was its head when it was last checked \(\S+\): entries were removed/);
  assert.match(r.stderr, CHANGED);

  // The message taken out and the record left alone: an entry is left over.
  await restartWith(() => {
    const s = JSON.parse(good);
    s.messages = s.messages.filter(m => m.id !== reply.id);
    return s;
  });
  const left = await client('x', ['verify', PAIR]);
  assert.equal(left.code, 5, left.all);
  assert.match(left.stdout, new RegExp(`differs {3}entry 4 records a message on card ${cards.first} that the board does not show: it was removed or changed`));

  // A whole card taken out, its messages with it.
  await restartWith(() => {
    const s = JSON.parse(good);
    s.cards = s.cards.filter(c => c.id !== cards.notice);
    s.messages = s.messages.filter(m => m.card !== cards.notice);
    return s;
  });
  const gone = await client('x', ['verify', PAIR]);
  assert.equal(gone.code, 5, gone.all);
  assert.match(gone.stdout, new RegExp(`differs {3}entry 9 and 1 more record a card \\(${cards.notice}\\) that the board does not show: it was removed`));

  // A stage set back in the state file.
  await restartWith(() => {
    const s = JSON.parse(good);
    Object.assign(s.cards.find(c => c.id === cards.first), { stage: 'shipped', outcome: null });
    return s;
  });
  const stage = await client('x', ['verify', PAIR]);
  assert.equal(stage.code, 5, stage.all);
  assert.match(stage.stdout, new RegExp(`differs {3}card ${cards.first}: its stage \\(shipped\\) is not the stage the record holds last for it`));
  await restartWith(() => good);
});

test('a head rolled back to an older state file: verify fails against the saved head', async () => {
  // A backup is taken, the room moves on and is verified, and then the backup is put back.
  const backup = readFileSync(statePath(), 'utf8');
  cards.late = posted(await ok('a', ['post', PAIR, '--kind', 'notice', '--title', 'Maintenance window moved to the weekend', '--text-file', file('late.txt', 'Plan for it.')]));
  const moved = await ok('x', ['verify', PAIR]);
  assert.match(moved.stdout, /head {6}entry 11 /);
  await restartWith(() => backup);
  const r = await client('x', ['verify']);
  assert.equal(r.code, 5, r.all);
  assert.match(r.stdout, /differs {3}the record now ends at entry 10, but entry 11 was its head when it was last checked \(\S+\): entries were removed/);
  assert.match(r.stderr, CHANGED);
  assert.equal(heads('x').rooms[PAIR].seq, 11, 'the saved head stays: it is the evidence');
  // The board then moves on from the old state: the record is as long as before, with another entry 11.
  await ok('a', ['post', PAIR, '--kind', 'notice', '--title', 'Maintenance window stays where it was', '--text-file', file('late2.txt', 'No change.')]);
  const forked = await client('x', ['verify', PAIR]);
  assert.equal(forked.code, 5, forked.all);
  assert.match(forked.stdout, /differs {3}entry 11 is not the entry that was the head when it was last checked \(\S+\): the record was rewritten/);
});

test('claims are in the record: a live claim verifies, and one changed or made up in the state file does not', async () => {
  // The test before left a saved head that the board no longer holds, on purpose. This one starts afresh.
  await restartWith(() => good);
  rmSync(join(home('x'), 'chain-heads.json'));
  await ok('x', ['verify']);
  // An open finding in the pair room, claimed by the other side with a note.
  const open = posted(await ok('x', ['post', PAIR, '--kind', 'finding', '--severity', 'friction', '--title', 'Retry after a timeout sends the order twice', '--text-file', file('twice.txt', 'Seen once in a test run.')]));
  await ok('a', ['claim', open, '--lease', '2h', '--note', 'Tracing it.']);
  const verified = await ok('x', ['verify', PAIR]);
  assert.match(verified.stdout, /compared {2}3 cards, 7 messages and 2 members with 12 entries/);
  const withClaim = readFileSync(statePath(), 'utf8');

  // The note changed, the time moved, the claim given to the other side, a claim made up on another card: each
  // is a claim the board shows that the record does not hold.
  const changes = [
    s => (s.cards.find(c => c.id === open).claim.note = 'Will not look at it.'),
    s => (s.cards.find(c => c.id === open).claim.until += 86400),
    s => (s.cards.find(c => c.id === open).claim.org = 'reseller-x'),
  ];
  for (const change of changes) {
    await restartWith(() => {
      const s = JSON.parse(withClaim);
      change(s);
      return s;
    });
    const r = await client('x', ['verify', PAIR]);
    assert.equal(r.code, 5, r.all);
    assert.match(r.stdout, new RegExp(`differs {3}card ${open}: the claim the board shows \\(by (supplier-a|reseller-x) until \\S+Z\\) is not the last claim the record holds for it`));
    assert.equal(r.stdout.match(/differs/g).length, 1);
    assert.match(r.stderr, CHANGED);
  }
  // What the record cannot show, as CHAIN.md says: a live claim taken off the card in the state file. The
  // board then shows none, as it would for a claim whose time has passed, and the check passes.
  await restartWith(() => {
    const s = JSON.parse(withClaim);
    delete s.cards.find(c => c.id === open).claim;
    return s;
  });
  assert.match((await ok('x', ['verify', PAIR])).stdout, /^room a-x: verified$/m);

  // Released by its holder and claimed by us: the last claim entry is ours, and it verifies for both sides.
  await restartWith(() => withClaim);
  await ok('a', ['release', open]);
  await ok('x', ['claim', open]);
  assert.match((await ok('x', ['verify', PAIR])).stdout, /with 14 entries/);
  assert.match((await ok('a', ['verify', PAIR])).stdout, /^room a-x: verified$/m);

  // A fields key cannot compare the note of another organisation's claim, and says so: one more withheld.
  await ok('a', ['claim', cards.shared, '--note', 'Ours to answer.']);
  const fields = await ok('y', ['verify', SHARED]);
  assert.match(fields.stdout, /with 7 entries/);
  assert.match(fields.stdout, /withheld {2}4 cards or messages were compared without their title or text: this key is class fields/);
  assert.ok(!fields.all.includes('Ours to answer'));
  assert.match((await ok('x', ['verify', SHARED])).stdout, /^room hub: verified$/m);
});

test('a saved-heads file that others can read or that is damaged is refused, not replaced', async () => {
  const path = join(home('a'), 'chain-heads.json');
  const kept = readFileSync(path, 'utf8');
  chmodSync(path, 0o644);
  const open = await client('a', ['verify', SHARED]);
  assert.equal(open.code, 1);
  assert.match(open.stderr, /chain-heads\.json is open to other users/);
  chmodSync(path, 0o600);
  for (const bad of ['not json', '{"format":"p2paecn-chain-1","board":"https://board.example","rooms":{}}', `{"format":"p2paecn-chain-1","board":"${BOARD}","rooms":{"hub":{"seq":1,"hash":"zz","checked":1}}}`]) {
    writeFileSync(path, bad, { mode: 0o600 });
    const r = await client('a', ['verify', SHARED]);
    assert.equal(r.code, 1, r.all);
    assert.equal(readFileSync(path, 'utf8'), bad, 'left as it was');
  }
  writeFileSync(path, kept, { mode: 0o600 });
  await ok('a', ['verify', SHARED]);
});

test('a board upgraded from an old state file: verify passes and says what was imported; a change to the old file shows', async () => {
  await stopBoard();
  const fixture = readFileSync(join(ROOT, 'tests', 'fixtures', 'board-state-v1.json'), 'utf8');
  assert.equal(JSON.parse(fixture).version, 1);
  const old = join(work, 'old');
  mkdirSync(old, { mode: 0o700 });
  writeFileSync(join(old, 'state.json'), fixture, { mode: 0o600 });
  // The profile of the fixture's key supplier-a/agent: a TEST key, 32 bytes of the value 41 as its seed.
  const privateKey = createPrivateKey({ key: Buffer.concat([Buffer.from('302e020100300506032b657004220420', 'hex'), Buffer.alloc(32, 41)]), format: 'der', type: 'pkcs8' });
  const publicKey = createPublicKey(privateKey).export({ format: 'jwk' }).x;
  mkdirSync(home('old'), { mode: 0o700 });
  const profile = { privateKey: privateKey.export({ type: 'pkcs8', format: 'pem' }), publicKey, url: BOARD, name: 'agent', keyId: 'supplier-a/agent', org: 'supplier-a', class: 'text' };
  writeFileSync(join(home('old'), 'profile.json'), JSON.stringify(profile), { mode: 0o600 });

  await startBoard(old);
  assert.match(serverLog, /loaded from a version 1 state file, room records built from it/);
  const first = await ok('old', ['verify']);
  assert.match(first.stdout, /^room a-x: verified$/m);
  assert.match(first.stdout, /^room hub: verified$/m);
  assert.match(first.stdout, /compared {2}3 cards, 11 messages and 2 members with 16 entries/);
  assert.match(first.stdout, /imported {2}entries 1 to 16 were built when the board was upgraded: they prove nothing about the time before/);
  assert.match(first.stdout, /imported {2}entries 1 to 13 were built/);
  assert.equal(readFileSync(join(old, 'state.json'), 'utf8'), fixture, 'reading wrote nothing');

  // A second start on the same file gives the same heads.
  await restartWith(() => fixture);
  const second = await ok('old', ['verify']);
  assert.equal(second.stdout.match(/it is still in the record/g).length, 2);

  // The old file changed by hand before the board ever saved a record: the import comes out differently.
  await restartWith(() => fixture.replace('Looking at it.', 'Will not fix.'));
  const r = await client('old', ['verify']);
  assert.equal(r.code, 5, r.all);
  assert.match(r.stdout, /^room a-x: DOES NOT VERIFY$/m);
  assert.match(r.stdout, /differs {3}entry 16 is not the entry that was the head when it was last checked \(\S+\): the record was rewritten/);
  assert.match(r.stdout, /^room hub: verified$/m);
  assert.match(r.stderr, CHANGED);
});

test('a board that gives a record with a gap, or names a head it does not hold: verify fails on the links', async () => {
  const vectors = JSON.parse(readFileSync(join(ROOT, 'partner-pack', 'vectors', 'chain-1.json'), 'utf8')).rooms[0];
  const all = vectors.entries.map(v => v.entry);
  let entries;
  let head;
  let listedYou = 'supplier-a';
  let listedOthers = ['reseller-x'];
  const stub = http.createServer((req, res) => {
    const url = new URL(req.url, 'http://stub');
    const send = body => res.writeHead(200, { 'content-type': 'application/json' }).end(JSON.stringify(body));
    if (url.pathname === '/v1/rooms') return send({ rooms: [{ id: 'a-x', kind: 'pair', you: listedYou, others: listedOthers }] });
    if (url.pathname === '/v1/rooms/a-x/cards') return send({ cards: [] });
    if (url.pathname === '/v1/rooms/a-x/chain') return send({ room: 'a-x', format: 'p2paecn-chain-1', entries: entries.slice(Number(url.searchParams.get('after'))), head });
    res.writeHead(401).end('unauthorized\n');
  });
  await new Promise(done => stub.listen(0, '127.0.0.1', done));
  try {
    // A profile for the stub: the key signs, the stub does not look.
    mkdirSync(home('stub'), { mode: 0o700 });
    const profile = { ...JSON.parse(readFileSync(join(home('old'), 'profile.json'), 'utf8')), url: `http://127.0.0.1:${stub.address().port}` };
    writeFileSync(join(home('stub'), 'profile.json'), JSON.stringify(profile), { mode: 0o600 });
    const run = async (given, named = given[given.length - 1]) => {
      entries = given;
      head = { seq: named.seq, hash: named.hash };
      return client('stub', ['verify']);
    };
    const gap = await run(all.filter(e => e.seq !== 5).map((e, i) => ({ ...e, seq: i + 1 })));
    assert.equal(gap.code, 5, gap.all);
    assert.match(gap.stdout, /differs {3}entry 5 does not name the hash of entry 4: an entry was removed, added or changed there/);
    const edited = await run(all.map(e => (e.seq === 7 ? { ...e, actor: 'reseller-x' } : e)));
    assert.match(edited.stdout, /differs {3}entry 7 does not have the hash of its own content: it was changed/);
    const unnumbered = await run(all.filter(e => e.seq !== 5));
    assert.match(unnumbered.stdout, /differs {3}entry 5 is missing, out of order or not in the shape of an entry/);
    const other = await run(all.slice(0, 2), all[2]);
    assert.equal(other.code, 5, other.all);
    assert.match(other.stdout, /differs {3}the head the board names \(entry 3\) is not the last entry it gave \(entry 2\)/);
    // The vectors' record with no cards behind it: the links hold, and every card entry is left over.
    const empty = await run(all);
    assert.equal(empty.code, 5);
    assert.match(empty.stdout, /differs {3}entry 3 and 8 more record a card \(0b6f2c1e-0000-4000-8000-000000000001\) that the board does not show: it was removed/);
    // A member the room list names whose last entry in the record is its removal, and one the record never added.
    const hub = JSON.parse(readFileSync(join(ROOT, 'partner-pack', 'vectors', 'chain-1.json'), 'utf8')).rooms[1].entries.map(v => v.entry);
    const membersOnly = hub.filter(e => e.card === null && e.kind !== 'import');
    let prev = '0'.repeat(64);
    const relinked = membersOnly.map((e, i) => {
      const entry = { ...e, seq: i + 1, prev };
      entry.hash = sha256Hex(chainEntry({ ...entry, room: 'a-x' }));
      prev = entry.hash;
      return entry;
    });
    listedOthers = ['member-2', 'member-3', 'member-9'];
    listedYou = 'member-1';
    const removed = await run(relinked);
    assert.equal(removed.code, 5, removed.all);
    assert.match(removed.stdout, /differs {3}member member-3 is in the room, but the last entry of the record about that member is not one that adds it/);
    assert.match(removed.stdout, /differs {3}member member-9 is in the room, but the last entry/);
    assert.equal(removed.stdout.match(/differs/g).length, 2, 'member-1 and member-2 were added and not removed');
    assert.ok(!existsSync(join(home('stub'), 'chain-heads.json')), 'nothing verified, nothing saved');
  } finally {
    await new Promise(r => stub.close(r));
  }
});

// ---------------------------------------------------------------- a board that does not play fair

/**
 * A fake board for `verify`: one room, served from `state`, which a test changes between runs. It checks no
 * signature. By default it is an honest board holding room a-x of the test vectors: its record, and the two cards
 * and seven messages that the record was made from, as supplier-a sees them.
 */
async function fakeBoard(name, { profileClass = 'text' } = {}) {
  const vectors = JSON.parse(readFileSync(join(ROOT, 'partner-pack', 'vectors', 'chain-1.json'), 'utf8')).rooms[0];
  const you = 'supplier-a';
  const mark = actor => ({ mine: actor === you, untrusted: actor !== you });
  const details = new Map();
  for (const { values, entry } of vectors.entries) {
    if (entry.kind === 'card') {
      details.set(entry.card, {
        card: { id: entry.card, room: 'a-x', kind: values.kind, severity: values.severity, title: values.title, reporter: entry.actor, ...mark(entry.actor),
          stage: values.kind === 'finding' ? 'reported' : 'posted', outcome: null, created: entry.time, updated: entry.time, messages: 1, claim: null },
        messages: [{ id: values.message, card: entry.card, author: entry.actor, ...mark(entry.actor), kind: 'report', text: values.text, replaces: null, created: entry.time }],
      });
    } else if (entry.kind === 'message') {
      details.get(entry.card).messages.push({ id: values.id, card: entry.card, author: entry.actor, ...mark(entry.actor), kind: values.kind, text: values.text, replaces: values.replaces, created: entry.time });
    } else if (entry.kind === 'stage') {
      Object.assign(details.get(entry.card).card, { stage: values.stage, outcome: values.outcome });
    }
  }
  const state = {
    room: 'a-x',
    you,
    others: ['reseller-x'],
    entries: vectors.entries.map(v => v.entry),
    details,
    asked: [],
    // What a test may replace: each gets the parsed URL and the honest answer, and returns the answer to give.
    chain: (url, honest) => honest,
    cards: (url, honest) => honest,
    card: (id, honest) => honest,
  };
  const server = http.createServer((req, res) => {
    const url = new URL(req.url, 'http://fake');
    state.asked.push(url.pathname + url.search);
    const send = body => res.writeHead(200, { 'content-type': 'application/json' }).end(JSON.stringify(body));
    const last = state.entries[state.entries.length - 1];
    if (url.pathname === '/v1/rooms') return send({ rooms: [{ id: state.room, kind: 'pair', you: state.you, others: state.others }] });
    if (url.pathname === `/v1/rooms/${state.room}/chain`) {
      const after = Number(url.searchParams.get('after'));
      const page = state.entries.slice(after, after + Number(url.searchParams.get('limit') || 200));
      return send(state.chain(url, { room: state.room, format: 'p2paecn-chain-1', entries: page, head: { seq: last.seq, hash: last.hash } }));
    }
    if (url.pathname === `/v1/rooms/${state.room}/cards`) return send(state.cards(url, { cards: [...state.details.values()].map(d => d.card).reverse() }));
    const id = url.pathname.match(/^\/v1\/cards\/([0-9a-f-]{36})$/)?.[1];
    if (id && state.details.has(id)) return send(state.card(id, structuredClone(state.details.get(id))));
    res.writeHead(404, { 'content-type': 'application/json' }).end('{"error":"not_found"}');
  });
  await new Promise(done => server.listen(0, '127.0.0.1', done));
  // A profile for the fake board: a TEST key made from a fixed seed. The fake does not look at signatures.
  const privateKey = createPrivateKey({ key: Buffer.concat([Buffer.from('302e020100300506032b657004220420', 'hex'), Buffer.alloc(32, 51)]), format: 'der', type: 'pkcs8' });
  mkdirSync(home(name), { mode: 0o700 });
  const profile = { privateKey: privateKey.export({ type: 'pkcs8', format: 'pem' }), publicKey: createPublicKey(privateKey).export({ format: 'jwk' }).x,
    url: `http://127.0.0.1:${server.address().port}`, name: 'agent', keyId: 'supplier-a/agent', org: 'supplier-a', class: profileClass };
  writeFileSync(join(home(name), 'profile.json'), JSON.stringify(profile), { mode: 0o600 });
  const chainRequests = () => state.asked.filter(path => path.includes('/chain')).length;
  return { state, chainRequests, run: args => client(name, args), close: () => new Promise(r => server.close(r)) };
}

test('a title or text the board holds back from a text key: the room does not verify', async () => {
  const fake = await fakeBoard('held-back');
  const fields = await fakeBoard('held-back-fields', { profileClass: 'fields' });
  try {
    const { state } = fake;
    const [first] = [...state.details.keys()];
    // The honest fake verifies: the test below is not passing for another reason.
    const honest = await fake.run(['verify']);
    assert.equal(honest.code, 0, honest.all);
    assert.match(honest.stdout, /compared {2}2 cards, 7 messages and 2 members with 12 entries/);
    assert.ok(!honest.stdout.includes('withheld'));

    // Each way of leaving a text out. A key of class text is to receive every one of them.
    const leaveOut = [
      ['a reply by the other side', d => (d.messages[4].text = null), /differs {3}message 7d3a9f40-0000-4000-8000-000000000005 on card \S+: the board gave no text, which this key is to receive/],
      ['our own reply', d => (d.messages[1].text = null), /differs {3}message 7d3a9f40-0000-4000-8000-000000000002 on card \S+: the board gave no text/],
      ['a text that is not there at all', d => delete d.messages[2].text, /differs {3}message 7d3a9f40-0000-4000-8000-000000000003 on card \S+: the board gave no text/],
      ['the title', d => (d.card.title = null), /differs {3}card \S+: the board gave no title or no report text, which this key is to receive/],
      ['the text of the report', d => (d.messages[0].text = null), /differs {3}card \S+: the board gave no title or no report text/],
    ];
    for (const [what, change, expected] of leaveOut) {
      state.card = (id, honest) => {
        if (id === first) change(honest);
        return honest;
      };
      const r = await fake.run(['verify']);
      assert.equal(r.code, 5, `${what}: ${r.all}`);
      assert.match(r.stdout, /^room a-x: DOES NOT VERIFY$/m, what);
      assert.match(r.stdout, expected, what);
      assert.ok(!r.stdout.includes('withheld'), `${what}: a text key has nothing withheld`);
      assert.match(r.stderr, CHANGED, what);
    }
    assert.equal(heads('held-back').rooms['a-x'].seq, 12, 'the head saved by the honest run is kept');

    // A key of class fields: the other side's texts may be missing and are counted, its own may not.
    const f = fields.state;
    f.card = (id, honest) => {
      honest.messages.filter(m => !m.mine).forEach(m => (m.text = null));
      if (!honest.card.mine) honest.card.title = null;
      return honest;
    };
    const allowed = await fields.run(['verify']);
    assert.equal(allowed.code, 0, allowed.all);
    assert.match(allowed.stdout, /withheld {2}2 cards or messages were compared without their title or text: this key is class fields/);
    f.card = (id, honest) => {
      honest.messages.forEach(m => (m.text = null));
      return honest;
    };
    const own = await fields.run(['verify']);
    assert.equal(own.code, 5, own.all);
    assert.equal(own.stdout.match(/the board gave no text, which this key is to receive/g).length, 4, 'our four messages on the first card');
    assert.match(own.stdout, /differs {3}card 0b6f2c1e-0000-4000-8000-000000000002: the board gave no title or no report text/, 'and the report of our own notice');
  } finally {
    await fake.close();
    await fields.close();
  }
});

test('a room called constructor is a room like any other, and a local error is never a pass', async () => {
  const fake = await fakeBoard('odd-name');
  try {
    const { state } = fake;
    // The members' entries alone, linked again for a room of that name.
    state.room = 'constructor';
    state.entries = rebuilt('constructor', state.entries.filter(e => e.card === null));
    state.details = new Map();
    for (const run of [1, 2]) {
      const r = await fake.run(['verify']);
      assert.equal(r.code, 0, r.all);
      assert.match(r.stdout, /^room constructor: verified$/m);
      assert.match(r.stdout, run === 1 ? /no head was saved for this room: this is its first check/ : /entry 2 was the head at the last check \(\S+\): it is still in the record/);
    }
    const saved = JSON.parse(readFileSync(join(home('odd-name'), 'chain-heads.json'), 'utf8'));
    assert.deepEqual(Object.keys(saved.rooms), ['constructor']);
    assert.deepEqual([saved.rooms.constructor.seq, saved.rooms.constructor.hash], [2, state.entries[1].hash]);
    // The saved head is used: the same room with another record does not verify.
    state.entries = rebuilt('constructor', state.entries.map(e => ({ ...e, time: 5 })));
    const changed = await fake.run(['verify', 'constructor']);
    assert.equal(changed.code, 5, changed.all);
    assert.match(changed.stdout, /differs {3}entry 2 is not the entry that was the head when it was last checked/);
    // By name too, and --json.
    assert.equal(JSON.parse((await fake.run(['verify', 'constructor', '--json'])).stdout).rooms[0].room, 'constructor');

    // Something this program did not foresee must end as a failure of the check, never as a pass. Here the room
    // verifies, and then the saved heads cannot be written, because the profile directory is read-only.
    const fresh = await fakeBoard('odd-error');
    try {
      assert.equal((await fresh.run(['verify'])).code, 0, 'the room itself verifies');
      rmSync(join(home('odd-error'), 'chain-heads.json'));
      chmodSync(home('odd-error'), 0o500);
      const stopped = await fresh.run(['verify']);
      assert.equal(stopped.code, 1, stopped.all);
      assert.match(stopped.stderr, /^verify: a local error stopped the check \(.*\)\. Nothing was verified and no head was saved by this run\.$/m);
      assert.ok(!stopped.stdout.includes('verified'), stopped.stdout);
      const json = await fresh.run(['verify', '--json']);
      assert.deepEqual([json.code, json.stdout], [1, ''], 'no JSON that says ok');
    } finally {
      chmodSync(home('odd-error'), 0o700);
      await fresh.close();
    }
  } finally {
    await fake.close();
  }
});

test('verify reads no further than the head it was first given, and a board whose pages or card list do not end is not followed', async () => {
  const fake = await fakeBoard('endless');
  try {
    const { state } = fake;
    const honest = { chain: state.chain, cards: state.cards };
    const reset = () => {
      Object.assign(state, honest);
      state.asked.length = 0;
    };
    // A record that grows as fast as it is read: every answer names a head 2000 entries further on and gives a
    // full page. The client reads to the first head it was named, sees that the room has moved, tries twice more
    // and stops as uncertain. It does not follow the board.
    const entryLike = seq => ({ ...state.entries[0], seq });
    state.chain = url => {
      const after = Number(url.searchParams.get('after'));
      const limit = Number(url.searchParams.get('limit'));
      return { room: 'a-x', format: 'p2paecn-chain-1', entries: Array.from({ length: limit }, (_, i) => entryLike(after + i + 1)), head: { seq: after + 2000, hash: 'ab'.repeat(32) } };
    };
    const growing = await fake.run(['verify']);
    assert.equal(growing.code, 3, growing.all);
    assert.match(growing.stderr, /uncertain: room a-x kept changing while it was being checked/);
    assert.equal(fake.chainRequests(), 9, 'three tries of two pages and one look after the head, and no more');

    // A head beyond what this client reads at all.
    reset();
    state.chain = (url, real) => ({ ...real, head: { seq: 2000001, hash: real.head.hash } });
    const huge = await fake.run(['verify']);
    assert.equal(huge.code, 5, huge.all);
    assert.match(huge.stdout, /differs {3}the board names entry 2000001 as the head, more than the 2000000 entries this client reads/);
    assert.equal(fake.chainRequests(), 1);

    // A page that makes no progress: the board names entry 12 and gives seven entries, then nothing, for ever.
    reset();
    state.chain = (url, real) => ({ ...real, entries: real.entries.filter(e => e.seq <= 7) });
    const stalled = await fake.run(['verify']);
    assert.equal(stalled.code, 5, stalled.all);
    assert.match(stalled.stdout, /differs {3}the head the board names \(entry 12\) is not the last entry it gave \(entry 7\)/);
    assert.equal(fake.chainRequests(), 2, 'the page that came short ends the fetch; one look after the head');

    // A board that ignores the limit and sends more than the head it named: what is beyond is left alone.
    reset();
    const five = state.entries[4];
    state.chain = (url, real) => ({ ...real, entries: state.entries.slice(Number(url.searchParams.get('after'))).filter(e => Number(url.searchParams.get('after')) < 5), head: { seq: 5, hash: five.hash } });
    state.cards = (url, real) => ({ cards: real.cards.slice(-1) });
    const beyond = JSON.parse((await fake.run(['verify', '--json'])).stdout).rooms[0];
    assert.deepEqual([beyond.entries, beyond.head.seq], [5, 5], 'twelve were sent, five were taken');

    // A card list whose cursor comes round again.
    reset();
    const ids = [...state.details.keys()];
    state.cards = () => ({ cards: [{ id: ids[0] }], nextCursor: ids[0] });
    const round = await fake.run(['verify']);
    assert.equal(round.code, 5, round.all);
    assert.match(round.stdout, /differs {3}the board listed the same card twice: its card list cannot be read to an end/);
    assert.ok(state.asked.filter(path => path.includes('/cards')).length <= 2, state.asked.join(' '));
    // The same cursor twice with different cards, and a page with a cursor and no cards.
    reset();
    let page = 0;
    state.cards = () => ({ cards: [{ id: ids[page++ % 2] }], nextCursor: ids[0] });
    assert.match((await fake.run(['verify'])).stdout, /differs {3}the board gave the same cursor for its card list twice, or a page without cards: the list does not end/);
    reset();
    state.cards = () => ({ cards: [], nextCursor: ids[0] });
    assert.match((await fake.run(['verify'])).stdout, /the same cursor for its card list twice, or a page without cards/);
    // A card list that never repeats: new cards and new cursors for ever. The record holds two cards.
    reset();
    let n = 0;
    const made = () => `0b6f2c1e-0000-4000-9000-${String(++n).padStart(12, '0')}`;
    state.cards = () => {
      const id = made();
      return { cards: [{ id }], nextCursor: id };
    };
    const endless = await fake.run(['verify']);
    assert.equal(endless.code, 5, endless.all);
    assert.match(endless.stdout, /differs {3}the board lists more cards than the 2 its record holds/);
    assert.equal(state.asked.filter(path => path.includes('/cards')).length, 3);
    assert.ok(!existsSync(join(home('endless'), 'chain-heads.json')), 'nothing verified, nothing saved');

    // And the honest board still verifies.
    reset();
    assert.equal((await fake.run(['verify'])).code, 0);
  } finally {
    await fake.close();
  }
});
