// Generates the fixed test vectors for p2paecn-sig-1 with the signer in partner-pack/p2p.mjs, the client members run.
// Usage, from the repository root:
//   node tools/gen-vectors.mjs > tests/vectors/sig-1.json && cp tests/vectors/sig-1.json partner-pack/vectors/sig-1.json
// The output is the same on every run: the key comes from a fixed seed and every input is fixed (Ed25519 signatures
// are deterministic). The key is a TEST key: it signs nothing but these vectors. The example finding is invented and
// corresponds to no real integration event.
import { createHash, createPrivateKey, createPublicKey } from 'node:crypto';
import { canonicalString, contentDigest, signedHeaders } from '../partner-pack/p2p.mjs';

// A PKCS8 wrapper for a raw 32-byte Ed25519 seed, then the seed: SHA-256 of a fixed label.
const PKCS8_ED25519_PREFIX = Buffer.from('302e020100300506032b657004220420', 'hex');
const seed = createHash('sha256').update('p2paecn-sig-1 test vector key (TEST ONLY)').digest();
const privateKey = createPrivateKey({ key: Buffer.concat([PKCS8_ED25519_PREFIX, seed]), format: 'der', type: 'pkcs8' });
const privateKeyPem = privateKey.export({ type: 'pkcs8', format: 'pem' });
const publicKeyB64Url = createPublicKey(privateKey).export({ format: 'jwk' }).x;
const keyId = 'reseller-x/agent-1';
const created = '1794850000';

// What a person reads to the operator: first 20 hex characters of SHA-256 over the raw 32-byte key, in groups of four.
const hex = createHash('sha256').update(Buffer.from(publicKeyB64Url, 'base64url')).digest('hex').slice(0, 20);
const fingerprint = hex.match(/.{4}/g).join('-');

const inputs = [
  { name: 'get-no-body', method: 'GET', url: 'https://board.test/v1/inbox', body: '', idempotencyKey: '', nonce: 'AAAAAAAAAAAAAAAAAAAAAAAA' },
  { name: 'get-with-query', method: 'GET', url: 'https://board.test/v1/rooms/room-a/cards?stage=shipped&limit=5', body: '', idempotencyKey: '', nonce: 'BBBBBBBBBBBBBBBBBBBBBBBB' },
  { name: 'post-with-body', method: 'POST', url: 'https://board.test/v1/rooms/room-a/cards', body: '{"kind":"finding","severity":"wrong","title":"List endpoint skips one item when paging","text":"Page two starts one item late. Ünïcode ok."}', idempotencyKey: '5f0c1c0e-7b1f-4c58-9d55-3f6a1f0f9a11', nonce: 'CCCCCCCCCCCCCCCCCCCCCCCC' },
];

const cases = inputs.map(i => {
  const { host, pathname, search } = new URL(i.url);
  const path = pathname + search;
  const payload = Buffer.from(i.body, 'utf8');
  const headers = signedHeaders({ privateKey: privateKeyPem, host, method: i.method, path, payload, keyId, idempotencyKey: i.idempotencyKey, created, nonce: i.nonce });
  return {
    name: i.name, method: i.method, host, pathAndQuery: path, body: i.body, headers,
    canonical: canonicalString({ method: i.method, host, path, created, nonce: i.nonce, keyId, digest: contentDigest(payload), idempotencyKey: i.idempotencyKey }),
  };
});

console.log(JSON.stringify({ scheme: 'p2paecn-sig-1', keyId, publicKeyB64Url, fingerprint, nowUnix: Number(created), cases }, null, 2));
