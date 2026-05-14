# Cairn Desktop Electron Alpha

This package is the first Electron GUI alpha for issue #115. It is intentionally
fixture-backed and talks to the Rust `cairn-desktop` backend over local
HTTP/JSON.

## Development

Start the Rust backend:

```bash
cargo run -p cairn-desktop --bin cairn-desktop-server
```

Start the renderer:

```bash
npm run dev
```

Launch Electron against the dev renderer:

```bash
NODE_ENV=development npm run electron
```

Run checks:

```bash
npm test
npm run build
```

## Tauri Alternative

The design brief keeps Tauri as a slim shell alternative for users who prefer a
smaller runtime. This alpha implements Electron first because §13.2 names it as
the default desktop shell and because Chromium rendering parity matters for the
graph and editor surfaces. A future Tauri package should reuse the same Rust
backend API and renderer model rather than adding a second data path.
