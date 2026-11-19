#!/usr/bin/env node
// Rewrites PNG files so that they hold only the chunks IHDR, PLTE, tRNS, IDAT and IEND. Text, time, EXIF,
// colour profile and every other chunk is dropped, so an image carries nothing of the machine that made it
// beyond its pixels. The pixel data is not touched. It has no dependencies.
//
//   node docs/assets/src/strip-png.mjs <file.png> [more.png ...]     rewrite each file in place
//   node docs/assets/src/strip-png.mjs --check <file.png> [...]      change nothing, exit 1 if a file holds more
//
// For each file it prints the size in pixels, the size in bytes and the chunks the file now holds.

import { readFileSync, writeFileSync } from 'node:fs';

const KEEP = new Set(['IHDR', 'PLTE', 'tRNS', 'IDAT', 'IEND']);
const SIGNATURE = Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]);

const args = process.argv.slice(2);
const check = args[0] === '--check';
const files = check ? args.slice(1) : args;
if (files.length === 0) {
  process.stderr.write('usage: node strip-png.mjs [--check] <file.png> [more.png ...]\n');
  process.exit(1);
}

let extra = false;
for (const file of files) {
  const data = readFileSync(file);
  if (data.length < 8 || !data.subarray(0, 8).equals(SIGNATURE)) {
    process.stderr.write(`strip-png: ${file} is not a PNG\n`);
    process.exit(1);
  }
  const kept = [SIGNATURE];
  const names = [];
  const dropped = [];
  let at = 8;
  let width = 0;
  let height = 0;
  let ended = false;
  while (at + 12 <= data.length && !ended) {
    const length = data.readUInt32BE(at);
    const type = data.toString('latin1', at + 4, at + 8);
    const end = at + 12 + length;
    if (end > data.length) break;
    if (type === 'IHDR') {
      width = data.readUInt32BE(at + 8);
      height = data.readUInt32BE(at + 12);
    }
    if (KEEP.has(type)) {
      kept.push(data.subarray(at, end));
      if (names[names.length - 1]?.type === type) names[names.length - 1].n += 1;
      else names.push({ type, n: 1 });
    } else dropped.push(type);
    ended = type === 'IEND';
    at = end;
  }
  if (!ended || width === 0) {
    process.stderr.write(`strip-png: ${file} is cut short or has no IHDR and IEND\n`);
    process.exit(1);
  }
  // Bytes after IEND are not part of the image and are dropped as well.
  if (at < data.length) dropped.push(`${data.length - at} bytes after IEND`);
  const out = Buffer.concat(kept);
  if (dropped.length > 0) extra = true;
  if (!check && dropped.length > 0) writeFileSync(file, out);
  const list = names.map(({ type, n }) => (n > 1 ? `${type} x${n}` : type)).join(' ');
  const size = check ? data.length : out.length;
  const note = dropped.length === 0 ? '' : `  ${check ? 'also holds' : 'dropped'}: ${dropped.join(', ')}`;
  process.stdout.write(`${file}  ${width} by ${height}  ${size} bytes  ${list}${note}\n`);
}
process.exit(check && extra ? 1 : 0);
