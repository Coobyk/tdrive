<img src="assets/icon.svg" width="64" height="64" alt="">

# tdrive-rs

An alternative desktop client for [TDrive](https://github.com/jeetrex17/TDrive),
written in Rust and [GPUI](https://gpui.rs) (Zed's UI framework).

Like TDrive, it stores your files in a private Telegram channel: file contents
are ordinary channel documents, and the folder structure lives in `TDX1`
control messages. tdrive-rs speaks the same TDX1 wire format, so drives created
with either client are readable by the other.

## Status

Working client: Telegram sign-in (phone + code + 2FA), personal and shared
drives (broadcast channels / megagroups + invite links), folder browsing,
resumable chunked and encrypted uploads/downloads, mkdir/rename/move,
trash/restore/purge, search, incremental and live sync.

## TDX1 compatibility

`src/wire.rs` is a port of TDrive's `backend/projection/wire.go`. All 17 op
types parse and format; emitted strings are verified **byte-for-byte** against
the Go `projection.Format` output in the `golden_format_parity` test
(regenerate the table with `go run /tmp/wiregen/main.go` inside a TDrive
checkout). `deterministic_op_id` matches `projection.DeterministicOpID`.

`src/project.rs` ports `apply.go` semantics: versioned-op `oid` dedup,
expected-revision compare-and-swap, trash entries (original parent/name for
restore landing), subtree tombstone/restore, move-cycle rejection,
first-write-wins mkdir/parts, live-only rename/move, and `meta` upsert merging.
Deleting emits the same `trash` op TDrive writes (deterministic id, `del`,
30-day `purge`); restore emits `untrash`; purge emits `harddel` and deletes
the Telegram body messages.

Encryption (`src/crypto.rs`) implements TDE1 exactly: 50-byte header,
64 KiB chunks, XChaCha20-Poly1305, HKDF-SHA256 file keys
(`tdrive/file/v1`), and the Argon2id password vault (`encfg`). Unlocked
drives encrypt uploads and decrypt downloads; encrypted empty files are
allowed like TDrive.

Not yet covered: thumbnail/rendition display (`rend=` is parsed and
preserved), media streaming, photo backup, mounts, and file adoption —
the mount-write ops (`fcommit`/`freplace`/`dcommit`/`relocate`) are fully
parsed, projected and formatted.

## Architecture

- `src/tg.rs` — [grammers](https://github.com/Lonami/grammers) client on a
  dedicated tokio thread; the UI sends commands and receives events.
- `src/project.rs` — folds ordered ops into a namespace; persisted per
  channel under `~/.local/share/tdrive-rs/namespaces/` for incremental sync.
- `src/journal.rs` — persists part receipts / confirmed download blocks /
  sync watermarks so transfers and scans survive restarts.
- `src/crypto.rs`, `src/wire.rs` — format ports (see above).
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
