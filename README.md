<img src="assets/icon.svg" width="64" height="64" alt="">

# tdrive-rs

An alternative desktop client for [TDrive](https://github.com/jeetrex17/TDrive),
written in Rust and [GPUI](https://gpui.rs) (Zed's UI framework).

Like TDrive, it stores your files in a private Telegram channel: file contents
are ordinary channel documents, and the folder structure lives in `TDX1`
control messages. tdrive-rs speaks the same TDX1 wire format, so drives created
with either client are readable by the other.

## Status

MVP: Telegram sign-in (phone + code + 2FA), drive (channel) selection and
creation, full-history scan of TDX1 ops, folder-aware file browsing,
resumable chunked uploads/downloads, rename/move/delete/mkdir.

## Wire format

Every operation is a `TDX1|t=<op>|key=value|...` header in the first line of a
message text or document caption (see `src/wire.rs`, a port of TDrive's
`backend/projection/wire.go`).

| Op | Meaning |
| --- | --- |
| `f` | file upload — the document message itself is the file body |
| `part` | hidden part of a multipart upload (`u=` uuid, `pix=` index) |
| `manifest` | visibility boundary for a multipart file |
| `mkdir` / `rmdir` | folder create/delete |
| `rename` / `move` / `meta` / `tomb` | namespace mutations |
| `fcommit` / `freplace` / `dcommit` / `relocate` / `trash` / `harddel` / `untrash` | versioned writable ops (parsed, projected) |
| `encfg` | encryption config (parsed; decryption not implemented) |

Files larger than ~1.9 GiB are split into parts plus a manifest, matching
TDrive's limits (1900 MiB/part, 40 GiB/file, 32 parts max).

## Architecture

- `src/tg.rs` — [grammers](https://github.com/Lonami/grammers) client on a
  dedicated tokio thread; the UI sends commands and receives events.
- `src/project.rs` — folds ordered ops into an in-memory namespace.
- `src/journal.rs` — persists part receipts / confirmed download blocks so
  transfers survive restarts.
- `src/ui.rs`, `src/input.rs` — GPUI views.

## Build & run

```sh
cargo build
cargo run
```

On first launch you will be asked for an `api_id` / `api_hash` — create an app
at <https://my.telegram.org>. Config, session and transfer journal live under
`~/.local/share/tdrive-rs/`.

Requires the usual GPUI system deps on Linux (vulkan, wayland/xcb, libxkbcommon,
fontconfig, alsa).
