# OpenPaint

An open-source digital drawing & painting app — a free alternative to Clip
Studio Paint, focused on **web comics** (both infinite-scroll webtoon and
print-style pages) and on being a **digital sketchbook with genuinely good page
management**.

Quality first. Design/UX inspired by CSP and Procreate. Not a fork of Krita.

## Motivation
Open source drawing software already exists, like Krita, and premium versions exist like CSP or photoshop. What is the purpose of this? The issue with Krita, and other free apps is that they just feel clunky and poorly made (in my opnion but since people pay for software I assume some of you agree). The issue with the paid apps, is the pricing is ridiculous in my opinion. They always tempt you with subscription and take money, or with these shop things. I just wanna draw on high quality software.

More specifically, I only want some necessary features to be high quality. Art only needs a few tools to get most of the work done, and I hope this software can serve this purpose. Honestly procreate is great and I would've been quite happy with this (for the most part) though it does not work on windows and this targets windows.

Further, *page management* is a severely lacking feature. Only csp ex seems to have any form of page management. It would be so nice to have a digital sketchbook for practicing or to have pages to easily create comics. Even worse, infinite scrolling comics are a *pain* to create in csp even. You need to manaully extend the canvas every single time you reach the limit, and then stich things back together. How nice it would be to have a vertical scroll mode that just automatically extended.

## Status

**Usable, and not finished.** It is a real painting application: you can open a scan, draw on it,
work in layers across many pages, and get a webtoon strip out the other end. It is not yet a
replacement for everything CSP does, and the list below says which parts.

What is there:

- A **tiled GPU canvas** in linear light, with tiles spilling to the CPU, so document size is not
  limited by graphics memory. Pressure and tilt through Windows Ink.
- A **brush engine** with presets, a loadable bitmap tip, and six response curves.
- **Layers**: blend modes, opacity, clipping, alpha lock, a plain lock, reorder, merge, duplicate.
- **Pages**, which is the point of the project — a document is many pages, and a webtoon is one
  very tall one.
- **Selection** by lasso, rectangle and wand, a bucket, an eyedropper, and a transform with a live
  preview on the GPU.
- **Panels that follow what you are doing** — one shows the settings of the tool in your hand, one
  shows what the active layer is made of, and neither is a tab you have to go and find.
- **Text layers** where the text stays editable, with font substitution reported rather than
  silently swapped.
- **Import** of PNG and JPEG, as a document or as a layer; **export** of a page, of every page, or
  of every page stacked into one strip; copy and paste through the system clipboard.
- **Autosave and crash recovery**, and a `.openpaint` file that is an ordinary SQLite database.

What is *not* there yet, and is on purpose rather than forgotten: layer groups, speech bubbles,
on-canvas text editing with a caret, PSD import or export, CMYK and print colour management, and
a marching-ants selection outline that actually marches. See [`docs/TODO.md`](docs/TODO.md) §4.

## Building and running

```
cargo run --release
```

Windows first, because that is where the stylus work is; the engine crates are portable and the
platform-specific part is one input backend.

## Testing

Two kinds, and the second exists because the first was not enough:

```
cargo test --workspace     # 975 tests
./tools/sweep.ps1          # 30 scenarios, 1266 assertions, driving the real application
```

The scenarios operate the running application through the real UI -- moving the pointer, pressing
named controls read out of an atlas the app writes every frame -- and assert against the state it
reports about itself. They exist because 926 unit tests were once green while the brush painted
nothing. See [`docs/DRIVING.md`](docs/DRIVING.md).

## Docs

- [`docs/DECISIONS.md`](docs/DECISIONS.md) — agreed direction, architecture, and every decision
  with the reason it was made.
- [`docs/RELEASE_PLAN.md`](docs/RELEASE_PLAN.md) — what a first release needed, and what it did not.
- [`docs/TODO.md`](docs/TODO.md) — decided but not built.
- [`docs/CONTEXTUAL_PANELS.md`](docs/CONTEXTUAL_PANELS.md) — where a tool's settings belong, and
  the handoff for picking that work up.
- [`docs/OPEN_QUESTIONS.md`](docs/OPEN_QUESTIONS.md) — decisions still open.
- [`docs/DRIVING.md`](docs/DRIVING.md) — how the application is driven and tested through its UI.

## Tech (working plan)

Rust core engine (portable), GPU rendering via **wgpu**, tiled canvas + linear-
space compositing. Windows-first for stylus (Windows Ink: Surface / Veikk /
Wacom). Engine is isolated from the UI so the UI framework can evolve.

## License

[GPLv3](LICENSE).
