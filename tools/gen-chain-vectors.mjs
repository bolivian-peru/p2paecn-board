// Generates the fixed test vectors for the room record p2paecn-chain-1 (partner-pack/CHAIN.md) with the hashing in
// partner-pack/p2p.mjs, the client members run. tests/chain_vectors.rs builds the same records with the server's
// own code (src/chain.rs) and compares the bytes and the hashes.
// Usage, from the repository root:
//   node tools/gen-chain-vectors.mjs > tests/vectors/chain-1.json && cp tests/vectors/chain-1.json partner-pack/vectors/chain-1.json
// The output is the same on every run: every input is fixed. The rooms, cards and texts are invented and
// correspond to no real integration event.
import { createHash } from 'node:crypto';
import { chainEntry, chainPayload } from '../partner-pack/p2p.mjs';

const sha256Hex = bytes => createHash('sha256').update(bytes).digest('hex');
const GENESIS = '0'.repeat(64);
const card = n => `0b6f2c1e-0000-4000-8000-00000000000${n}`;
const message = n => `7d3a9f40-0000-4000-8000-0000000000${String(n).padStart(2, '0')}`;
const T = 1794850000;

// Each event: the entry's kind, card, actor and time, and the values of its payload.
const rooms = [
  {
    room: 'a-x',
    kind: 'pair',
    note: 'A pair room: actors are organisation ids. Twelve entries, so seq reaches two digits.',
    events: [
      { kind: 'member-added', card: null, actor: '*operator', time: 0, values: { name: 'supplier-a' } },
      { kind: 'member-added', card: null, actor: '*operator', time: 0, values: { name: 'reseller-x' } },
      { kind: 'card', card: card(1), actor: 'reseller-x', time: T, values: { kind: 'finding', severity: 'wrong', title: 'List endpoint skips one item when paging', message: message(1), text: 'Page two starts at item 22.\nItem 21 is never returned.' } },
      { kind: 'message', card: card(1), actor: 'supplier-a', time: T + 60, values: { id: message(2), kind: 'reply', replaces: null, text: 'Looking at it.' } },
      { kind: 'message', card: card(1), actor: 'supplier-a', time: T + 90, values: { id: message(3), kind: 'correction', replaces: message(2), text: 'Reproduced: the second page starts one item late. Ünïcode ok \u{1F600}' } },
      { kind: 'message', card: card(1), actor: 'supplier-a', time: T + 120, values: { id: message(4), kind: 'stage', replaces: null, text: 'Confirmed here too.' } },
      { kind: 'stage', card: card(1), actor: 'supplier-a', time: T + 120, values: { stage: 'acknowledged', outcome: null } },
      { kind: 'message', card: card(1), actor: 'supplier-a', time: T + 3600, values: { id: message(5), kind: 'stage', replaces: null, text: 'Fixed in the next build.' } },
      { kind: 'stage', card: card(1), actor: 'supplier-a', time: T + 3600, values: { stage: 'shipped', outcome: null } },
      { kind: 'message', card: card(1), actor: 'reseller-x', time: T + 7200, values: { id: message(6), kind: 'stage', replaces: null, text: 'Works.' } },
      { kind: 'stage', card: card(1), actor: 'reseller-x', time: T + 7200, values: { stage: 'closed', outcome: 'verified' } },
      { kind: 'card', card: card(2), actor: 'supplier-a', time: T + 7300, values: { kind: 'notice', severity: null, title: 'Größe der Seite ändert sich: 页面大小 \u{1F4C4}', message: message(7), text: 'A text may look like a record.\ntext:3:abc\nkind:7:finding\n: and end with a colon:' } },
    ],
  },
  {
    room: 'hub',
    kind: 'shared',
    note: 'A shared room built from an older state file: actors are handles, the built entries have the actor *import, and an import entry closes them. Two entries made after the upgrade follow.',
    events: [
      { kind: 'member-added', card: null, actor: '*import', time: 0, values: { name: 'member-1' } },
      { kind: 'member-added', card: null, actor: '*import', time: 0, values: { name: 'member-2' } },
      { kind: 'member-added', card: null, actor: '*import', time: 0, values: { name: 'member-3' } },
      { kind: 'card', card: card(3), actor: 'member-3', time: T, values: { kind: 'finding', severity: 'question', title: 'Is a sandbox mode planned for testing orders?', message: message(8), text: 'Asking for the next release.' } },
      { kind: 'message', card: card(3), actor: 'member-1', time: T + 30, values: { id: message(9), kind: 'reply', replaces: null, text: 'Not planned yet.' } },
      { kind: 'stage', card: card(3), actor: '*import', time: 0, values: { stage: 'closed', outcome: 'withdrawn' } },
      { kind: 'member-removed', card: null, actor: '*import', time: 0, values: { name: 'member-3' } },
      { kind: 'import', card: null, actor: '*import', time: 0, values: { entries: 7 } },
      { kind: 'member-added', card: null, actor: '*operator', time: 0, values: { name: 'member-3' } },
      { kind: 'message', card: card(3), actor: 'member-2', time: T + 86400, values: { id: message(10), kind: 'proof', replaces: null, text: 'We run it against a copy.' } },
      { kind: 'member-removed', card: null, actor: '*operator', time: 0, values: { name: 'member-3' } },
    ],
  },
  {
    room: 'b-y',
    kind: 'pair',
    note: 'A pair room with claims: a claim with a note, its renewal without one, a release, and a claim by the other side.',
    events: [
      { kind: 'member-added', card: null, actor: '*operator', time: 0, values: { name: 'supplier-b' } },
      { kind: 'member-added', card: null, actor: '*operator', time: 0, values: { name: 'reseller-y' } },
      { kind: 'card', card: card(4), actor: 'reseller-y', time: T, values: { kind: 'finding', severity: 'friction', title: 'Retry after a timeout sends the order twice', message: message(11), text: 'Seen once in a test run.' } },
      { kind: 'claim', card: card(4), actor: 'supplier-b', time: T + 10, values: { until: T + 3610, note: 'Pr\u00fcfe ich bis heute Abend: \u{1F50D}' } },
      { kind: 'claim', card: card(4), actor: 'supplier-b', time: T + 3000, values: { until: T + 10200, note: null } },
      { kind: 'release', card: card(4), actor: 'supplier-b', time: T + 4000, values: {} },
      { kind: 'claim', card: card(4), actor: 'reseller-y', time: T + 4100, values: { until: T + 4160, note: 'note:0:\nuntil:1:5' } },
    ],
  },
];

const out = rooms.map(({ room, kind, note, events }) => {
  let prev = GENESIS;
  const entries = events.map((event, index) => {
    const payloadRecord = chainPayload(event.kind, event.values);
    const payload = sha256Hex(payloadRecord);
    const fields = { room, seq: index + 1, kind: event.kind, card: event.card, actor: event.actor, time: event.time, payload, prev };
    const entryRecord = chainEntry(fields);
    const hash = sha256Hex(entryRecord);
    prev = hash;
    return {
      values: event.values,
      payloadRecord: payloadRecord.toString('utf8'),
      entryRecord: entryRecord.toString('utf8'),
      // As GET /v1/rooms/{room}/chain returns the entry.
      entry: { seq: index + 1, kind: event.kind, card: event.card, actor: event.actor, time: event.time, payload, prev: fields.prev, hash },
    };
  });
  return { room, kind, note, entries, head: { seq: entries.length, hash: prev } };
});

console.log(JSON.stringify({ format: 'p2paecn-chain-1', genesis: GENESIS, rooms: out }, null, 2));
