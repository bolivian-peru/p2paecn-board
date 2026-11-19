#!/usr/bin/env node
// Writes an HTML page that shows a text file, exactly as given, in a plain terminal window frame.
// The page is 1600 pixels wide, for a README image shown at 800. It has no dependencies.
//
//   node docs/assets/src/make-terminal.mjs <text file> <output html> [--title <text>] [--cols <n>]
//
// The text is never wrapped, cut, coloured by meaning or interpreted: every character is put on the page as a
// character. One thing is added for the eye: a "$" that starts a line and is followed by a space is drawn in the
// accent colour. The page does not hold the name or the path of the text file, and the title bar shows only the
// title you pass (the word "terminal" when you pass none).
//
// The script prints the size of the page and the headless Chrome command that turns it into a PNG.

import { readFileSync, writeFileSync } from 'node:fs';
import { isAbsolute } from 'node:path';

const WIDTH = 1600;        // the page, in pixels
const BORDER = 1;          // the frame line
const PAD_X = 48;          // left and right of the text
const PAD_Y = 40;          // above and below the text
const BAR = 72;            // the title bar
const ADVANCE = 0.6021;    // the widest advance, in em, of the monospace fonts in the stack
const MAX_FONT = 28;       // pixels: no larger than this, however short the lines are
const MIN_COLS = 80;
const WIDE_COLS = 120;     // beyond this the type gets small at README width

const fail = message => {
  process.stderr.write(`make-terminal: ${message}\n`);
  process.exit(1);
};

const args = process.argv.slice(2);
const positional = [];
const options = { title: 'terminal', cols: undefined };
for (let i = 0; i < args.length; i += 1) {
  const arg = args[i];
  if (arg === '--title' || arg === '--cols') {
    const value = args[i + 1];
    if (value === undefined) fail(`${arg} needs a value`);
    options[arg.slice(2)] = value;
    i += 1;
  } else if (arg.startsWith('--')) fail(`unknown option ${arg}`);
  else positional.push(arg);
}
if (positional.length !== 2) {
  fail('usage: node make-terminal.mjs <text file> <output html> [--title <text>] [--cols <n>]');
}
const [input, output] = positional;

let text;
try {
  text = new TextDecoder('utf-8', { fatal: true }).decode(readFileSync(input));
} catch (error) {
  fail(error instanceof TypeError ? 'the text file is not valid UTF-8' : `cannot read the text file (${error.code ?? 'error'})`);
}
// The newline that ends the file is not a line of its own. Nothing else is taken away.
if (text.endsWith('\n')) text = text.slice(0, -1);
const lines = text.split('\n');

/** Columns a line takes: a tab runs to the next stop of 8, a character outside ASCII counts as two so that a wide one cannot leave the frame. */
function columns(line) {
  let n = 0;
  for (const ch of line) {
    if (ch === '\t') n += 8 - (n % 8);
    else n += ch.codePointAt(0) > 0x7e ? 2 : 1;
  }
  return n;
}

let longest = 0;
lines.forEach((line, i) => {
  for (const ch of line) {
    const code = ch.codePointAt(0);
    if ((code < 0x20 && ch !== '\t') || (code >= 0x7f && code <= 0x9f)) {
      const name = `U+${code.toString(16).toUpperCase().padStart(4, '0')}`;
      fail(`line ${i + 1} holds the control character ${name}. Remove escape sequences and carriage returns first: the text is shown as given.`);
    }
  }
  longest = Math.max(longest, columns(line));
});

let cols = Math.max(MIN_COLS, longest);
if (options.cols !== undefined) {
  if (!/^[1-9][0-9]{1,2}$/.test(options.cols)) fail('--cols is a number of columns from 10 to 999');
  cols = Number(options.cols);
  if (cols < longest) fail(`the longest line takes ${longest} columns, more than --cols ${cols}. The text is never wrapped or cut.`);
}
if (cols > WIDE_COLS) {
  process.stderr.write(`make-terminal: note: at ${cols} columns the type is small when the image is shown 800 pixels wide. Shorter lines read better.\n`);
}

const inner = WIDTH - 2 * BORDER - 2 * PAD_X;
const font = Math.min(MAX_FONT, Math.floor((inner / cols / ADVANCE) * 100) / 100);
const lineHeight = Math.round(font * 1.5);
const height = 2 * BORDER + BAR + 2 * PAD_Y + lines.length * lineHeight;

const escape = s => s.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;');
const body = lines.map(line => (line.startsWith('$ ') ? `<span class="p">$</span>${escape(line.slice(1))}` : escape(line))).join('\n');

const page = `<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<title>terminal transcript</title>
<!-- Written by docs/assets/src/make-terminal.mjs. ${lines.length} lines, ${cols} columns, ${WIDTH} by ${height} pixels. -->
<style>
:root{
  --sans:"Inter","Geist",ui-sans-serif,system-ui,-apple-system,"Segoe UI Variable Text","Segoe UI",Roboto,"Helvetica Neue",Arial,sans-serif;
  --mono:ui-monospace,"SF Mono","JetBrains Mono","Cascadia Mono","Roboto Mono",Menlo,Consolas,monospace;
  --bg:#0B0D0C; --surface:#121514; --line:#242927; --line-strong:#343A37;
  --text:#EDF0EC; --text-2:#B1B8B2; --text-3:#858D87; --accent:#62D69E;
}
*,*::before,*::after{box-sizing:border-box}
html,body{margin:0;width:${WIDTH}px;height:${height}px;overflow:hidden}
body{
  background:var(--bg);
  color:var(--text);
  border:${BORDER}px solid var(--line-strong);
  -webkit-font-smoothing:antialiased;
}
.bar{
  position:relative;
  display:flex;
  align-items:center;
  gap:14px;
  height:${BAR}px;
  padding:0 28px;
  background:var(--surface);
  border-bottom:1px solid var(--line);
}
.bar i{
  width:18px;height:18px;flex:none;
  border:2px solid var(--line-strong);
  border-radius:50%;
}
.bar p{
  position:absolute;
  left:0;right:0;
  margin:0;
  text-align:center;
  font:500 24px/1 var(--sans);
  color:var(--text-2);
}
pre{
  margin:0;
  padding:${PAD_Y}px ${PAD_X}px;
  font-family:var(--mono);
  font-size:${font}px;
  line-height:${lineHeight}px;
  font-variant-ligatures:none;
  white-space:pre;
  tab-size:8;
}
.p{color:var(--accent)}
</style>
</head>
<body>
<div class="bar"><i></i><i></i><i></i><p>${escape(options.title)}</p></div>
<pre id="t">${body}</pre>
<script>
// A monospace font wider than the stack's usual ones is made smaller until the longest line fits. The line
// height is fixed, so the size of the page does not change.
const t = document.getElementById('t');
const room = ${inner};
const range = document.createRange();
range.selectNodeContents(t);
const wide = range.getBoundingClientRect().width;
if (wide > room) t.style.fontSize = Math.floor(${font} * room / wide * 100) / 100 + 'px';
</script>
</body>
</html>
`;

try {
  writeFileSync(output, page);
} catch (error) {
  fail(`cannot write the page (${error.code ?? 'error'})`);
}

const png = `${output.replace(/\.html?$/i, '')}.png`;
const address = isAbsolute(output) ? `file://${output}` : `file://$PWD/${output}`;
process.stdout.write(`wrote ${output}: ${lines.length} lines, ${cols} columns, ${WIDTH} by ${height} pixels
To make the PNG, with CHROME set to where your Chrome is:

  "$CHROME" --headless=new --no-first-run --hide-scrollbars --force-device-scale-factor=1 \\
    --window-size=${WIDTH},${height} --user-data-dir="$(mktemp -d)" \\
    --screenshot="${png}" "${address}"

Then strip the PNG: node docs/assets/src/strip-png.mjs "${png}"
`);
