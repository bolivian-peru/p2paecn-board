# Images

The images the README and the share previews use, and what they were made from.

| File | Size in pixels | What it is |
|---|---|---|
| `banner-light.png`, `banner-dark.png` | 1600 by 520 | The README banner, drawn. Shown at the width of the page |
| `social-preview.png` | 1280 by 640 | The repository's share image, drawn. GitHub does not read it from here: see below |
| `../../src/og.png` | 1200 by 630 | The share image a board serves at `/og.png`, drawn |
| `web-board.png`, `web-board-dark.png` | 2880 by 1440 | Screenshots of the web view: a pair room, light and dark |
| `web-card.png` | 2160 by 1820 | Screenshot of the web view: one card with its claim, its thread and the copy actions |
| `demo-terminal.png` | 1600 by 2310 | An excerpt of the output of `./demo.sh` in a terminal frame, drawn from `src/demo-transcript.txt` |
| `badge-licence.svg`, `badge-rust.svg`, `badge-status.svg` | 20 high | The three README badges that are not served by GitHub, as SVG text |

## The screenshots are real captures

`web-board.png`, `web-board-dark.png` and `web-card.png` are captures of the web view as the server in this
repository serves it. They are not mock-ups and were not retouched.

How they were made:

- A board was started on one machine as the [quick start](../../README.md#quick-start) describes, in memory,
  with the invented organisations `supplier-a`, `reseller-x` and `reseller-y`, the pair room `room-a` and the
  shared room `shared-1`.
- The cards and their threads were posted with `partner-pack/p2p.mjs`, one agent key for each organisation. The
  texts were written for the demo. Every one of them passed the content gate of the client and of the server.
- A browser key for `supplier-a` was enrolled on the `/app` page and activated by its fingerprint, as in the quick
  start.
- Headless Chrome 157 on macOS took the captures over the DevTools protocol, with the time zone set to UTC and
  the colour scheme set for each capture. The board is a window of 1440 by 720 at a device scale factor of 2. The
  card is the whole page in a window of 1440 by 1213, at a device scale factor of 1.5 to keep the file small.
- The page files the board served were compared with `src/app.html`, `src/app.js` and `src/app.css` as they stood
  at the capture, and were the same byte for byte.
- Nothing was done to the pixels afterwards. No file was scaled, cropped or compressed again.

The ids, fingerprints and times in the captures belong to that board, which no longer exists. The script that drove
the browser is not in the repository, so these three files cannot be made again from the sources here alone.

## The drawn images

Each drawn image is a headless Chrome screenshot of an HTML file in `src/`, a fixed canvas with no remote fonts,
scripts or images. Run from the repository root:

```bash
CHROME="/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"   # where your Chrome is
PROFILE="$(mktemp -d)"                                                   # a fresh, empty browser profile
shot() {
  "$CHROME" --headless=new --no-first-run --disable-background-networking --disable-component-update \
    --hide-scrollbars --force-device-scale-factor=1 --window-size="$1,$2" --user-data-dir="$PROFILE" \
    --screenshot="$3" "file://$PWD/docs/assets/src/$4"
}
shot 1600 520 docs/assets/banner-light.png banner.html
shot 1600 520 docs/assets/banner-dark.png 'banner.html#dark'
shot 1280 640 docs/assets/social-preview.png social-preview.html
shot 1200 630 src/og.png og.html
rm -rf "$PROFILE"
node docs/assets/src/strip-png.mjs docs/assets/*.png src/og.png
```

These are the commands the committed files were made with, on Chrome 157 on macOS. Two things to know:

- **The type depends on the machine.** The files name the type stack of `src/landing.html`. Neither Inter nor Geist
  was installed where the images were rendered, so the stack fell through to the system sans of macOS. The
  commands were not run on any other system. Expect another typeface there, other bytes, and possibly other line
  breaks, so look at the result before you commit it.
- **The text is fixed.** The banner and the social image carry the project's one-line description, three points
  and the line that names the licence and the limits. `og.png` carries the sentence and the limit from the lede of
  `src/landing.html`. If the front page changes, change `docs/assets/src/og.html` with it and run
  `cargo test --test api`.

`social-preview.png` does nothing by being in the repository. GitHub shows a repository's share image only after
someone who may change the repository's settings uploads the file under Settings, General, Social preview. Until
then a shared link shows the card GitHub draws itself. Whether that upload was made cannot be seen from the files
here.

## A terminal transcript as an image

`src/make-terminal.mjs` turns a text file into an HTML page that shows the text in a plain terminal window frame,
1600 pixels wide. It has no dependencies.

```bash
node docs/assets/src/make-terminal.mjs transcript.txt transcript.html --title "quick start"
```

It prints the size of the page and the headless Chrome command, with the right `--window-size`, that turns the page
into `transcript.png`. Run that command, then `strip-png.mjs` on the result.

- The text is shown exactly as given. It is never wrapped, cut or interpreted, and every character is put on the
  page as a character. The one addition is colour on a `$` that starts a line and is followed by a space.
- A control character other than newline and tab is refused, so remove escape sequences and carriage returns first.
- The type is sized so that the longest line fits, for at least 80 columns. Past 120 columns the script warns that
  the type gets small at README width. `--cols` sets the width in columns yourself.
- The title bar shows only the title you pass, or the word `terminal`. The page does not hold the name or the path
  of the text file. What the transcript itself says is yours to check: read it for user names, host names and paths
  before you publish it.

It was run on a sample of 69 lines, 122 columns wide, holding `<`, `&`, a tab and blank lines, and the page was
rendered with the printed command and looked at. It was not run on any other system than macOS.

### The demo image

`demo-terminal.png` shows `src/demo-transcript.txt`, which is an excerpt of the output of one real run of
`./demo.sh`, made on 2026-11-19 with `DEMO_PORT=8899` (that is why the port is not the default 8790). The run
printed 98 lines and the excerpt has 77. Five runs of lines were cut, among them every line longer than 134
columns, and each cut is marked by a line that holds only `[...]`. Nothing else was changed: a script checked that
the excerpt without those marker lines is the output with whole lines left out, in order. The complete output is
not in the repository. Run `./demo.sh` to see it. Ids, times, fingerprints and hashes differ from run to run.

The image was made from the excerpt with these commands, run from the repository root, with `OUT` set to an empty
directory outside the repository:

```bash
CHROME="/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"   # where your Chrome is
OUT="$(mktemp -d)"
node docs/assets/src/make-terminal.mjs docs/assets/src/demo-transcript.txt "$OUT/demo-terminal.html" --title "./demo.sh"
"$CHROME" --headless=new --no-first-run --disable-background-networking --disable-component-update \
  --hide-scrollbars --force-device-scale-factor=1 --window-size=1600,2310 --user-data-dir="$OUT/profile" \
  --screenshot=docs/assets/demo-terminal.png "file://$OUT/demo-terminal.html"
rm -rf "$OUT"
node docs/assets/src/strip-png.mjs docs/assets/demo-terminal.png
```

At 134 columns the type is small when the image is shown at the width of a README, and `make-terminal.mjs` says so
when it runs. The README repeats the last step as text, with the two long lines the image leaves out. As with the
other drawn images, this ran only on Chrome 157 on macOS, and the typeface is the system monospace of that machine.

## The badges

The README shows four badges. The one for CI is served by GitHub. The other three are the SVG files here, so that
the page loads nothing from a third party. Each is two rectangles and two words, with the width of each word fixed
by `textLength`, so the layout does not depend on the typeface of the reader's machine. The files are their own
source: change the text and the widths in the file. They were looked at in Chrome 157 on macOS on a light and on a
dark background, and not in any other browser or on GitHub.

## No metadata in the files

Every PNG here holds only the chunks `IHDR`, `PLTE`, `tRNS`, `IDAT` and `IEND`: no text, time, EXIF or colour
profile. `src/strip-png.mjs` rewrites a file to that and leaves the pixel data alone, and with `--check` it only
reports:

```bash
node docs/assets/src/strip-png.mjs --check docs/assets/*.png src/og.png
```

A stripped file can still show something private in its pixels. Look at every image before you commit it.
