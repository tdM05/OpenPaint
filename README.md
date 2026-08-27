# OpenPaint

An open-source digital drawing & painting app — a free alternative to Clip
Studio Paint, focused on **web comics** (both infinite-scroll webtoon and
print-style pages) and on being a **digital sketchbook with genuinely good page
management**.

Quality first. Design/UX inspired by CSP and Procreate. Not a fork of Krita.

## Motivation
Open source drawing software already exists, like Krita, and premium versions exist like CSP or photoshop. What is the purpose of this? The issue with Krita, and other free apps is that they just feel clunky and poorly made (in my opnion but since people pay for software I assume some of you agree). The issue with the paid apps, is the pricing is ridiculous in my opinion. They always tempt you with subscription and take money, or with these shop things. I just wanna draw on high quality software.


## Status

Very early — planning + Phase 0 (engine vertical slice) in progress.

## Docs

- [`docs/DECISIONS.md`](docs/DECISIONS.md) — agreed direction, architecture, roadmap.
- [`docs/OPEN_QUESTIONS.md`](docs/OPEN_QUESTIONS.md) — decisions still open.

## Tech (working plan)

Rust core engine (portable), GPU rendering via **wgpu**, tiled canvas + linear-
space compositing. Windows-first for stylus (Windows Ink: Surface / Veikk /
Wacom). Engine is isolated from the UI so the UI framework can evolve.

## License

[GPLv3](LICENSE).
