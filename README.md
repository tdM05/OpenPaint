# OpenPaint

An open-source digital drawing & painting app — a free alternative to Clip
Studio Paint, focused on **web comics** (both infinite-scroll webtoon and
print-style pages) and on being a **digital sketchbook with genuinely good page
management**.

Quality first. Design/UX inspired by CSP and Procreate. Not a fork of Krita.

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
