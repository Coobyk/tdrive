# Refactor decision log

Each fix is listed above the problems it resolved. Process: problem list
(hidden links / needless links / surplus / special cases) → first-principles
redesign → fix → audit until clean. Three audit rounds ran after the main
pass; the last reported only items fixed in the final commit.

## Structure

- **`project.rs` (`Namespace`) owns every derivation of the namespace.**
  `find_child`, `resolve_path`, `body_parts`, `body_msg_ids`,
  `logical_size`, `subtree_*`, `delete_body_msg_ids`, `dirent`,
  `trash_entries`, `search`, `has_rendition`, `MAX_DEPTH`.
  - Body-part resolution (`upload_uuid` parts → `content_msg_id` →
    `msg_id`) was hand-rolled in fusefs, the stream surface, and media
    with two different fallback policies.
  - `if encrypted { psz } else { size }` appeared ~7 times; callers
    (dav, surfaces, UI row meta) now call `FileEntry::logical_size()`.
  - Path→object resolution was reimplemented ~7 times (dav 3×, fusefs,
    UI crumbs) with divergent decode/order rules.
  - Trash/search/listing filters were duplicated; `unadopted` docs were
    lost on restart (snapshot now persists them).

- **`ops.rs` owns op construction; `VfsOp` is the writable-mount
  vocabulary.** `dav` and `fusefs` both route writes through it.
  - ~20 inline `Op{}` literals drifted on id schemes, `rev`/`drev`,
    timestamps, and `rend=` handling (now emitted only on `FilePart`,
    matching the parser).
  - `DavOp` was a second write vocabulary duplicating `VfsOp` intent.

- **`remote.rs` (`Remote`) owns the Telegram plumbing surfaces share**:
  client, peer cell, master-key cell, `send_op`, `send_doc_op`
  (document send + caption-scan msg-id recovery), `read_range`
  (multipart + TDE1-aware), `delete_bodies`, `BODY_CHUNK`.
  - `send_op`/`recover_msg_id` existed in two copies; a third send path
    (mounts) committed msg id `0` unchecked.
  - dav/stream re-assembled the same client+peer+master+ns tuple per
    request; stream held a stale peer after drive switches.
  - `DOWNLOAD_CHUNK` (tg) and `CHUNK` (remote) were the same 512 KiB.

- **`http.rs` is the one HTTP/1.1 core** (request read, header map,
  Range parse, response writer incl. declared-length `respond_len`).
  - dav and stream had divergent request readers, range parsers
    (both botched suffix ranges), and response writers; DAV `HEAD`
    reported `Content-Length: 0` via an `X-Content-Length-Note` hack.

- **`tg/` domain split**: `auth`, `drives`, `sync`, `vfs`, `surfaces`,
  `upload`, `download`, `media`, `vault`, `backup`, `surfaces`. The root
  keeps the type vocabulary, the worker loop, and shared spawn/save
  helpers.
  - 16 concerns lived in one 3k-line module; Command mixed public API
    and internal mailbox traffic.

- **`ui/` domain split**: `controls`, `drive`, `modals`, `screens`,
  `transfers`, `viewer` + `input.rs` (`TextField`).
  - 650-line `render_drive`, 350-line `render_modal`, six near-identical
    confirm paths where Enter and click could diverge.

## Shared policies (each was 2+ copies)

- `spawn_cancellable` — token mint + `cancels` registration + ctx +
  spawn. Fixed a real bug: downloads registered token A and checked
  token B, so pause/cancel was a no-op.
- `upload_transfer_id`/`download_transfer_id`/`parse_transfer_id` —
  the `up:`/`dl:` convention lived at ~13 sites; single uploads now key
  by uuid (name-keyed ids collided on same-name files).
- `staging_path` — one `.tdrive-part` formula (download + cancel).
- `strip_tdr1` — one rendition-envelope strip (thumb + preview fetches).
- `atomic_json_write` — one tmp+rename save (config/journal/namespace).
- `journal.resumable_uploads` — one resume predicate.
- `decoded_path` — one dav path decode; `chats_from` — one
  updates-envelope→channel walk; `spawn_rendition_fetch` + `FetchKind` —
  one fetch pipeline for thumb and preview.
- `DriveKind` enum replaces `"broadcast"`/`"megagroup"` stringly-typing;
  `wire::RENDITION_THUMB`/`RENDITION_PREVIEW` (`"thumbnail"`/`"preview"`
  per Go TDrive, docs previously said `"thumb"`).

## UI state

- `paused_downloads`: `HashMap<id,(fid,dir)>` → `HashSet` (journal owns
  destinations); `cancelled: HashSet` suppresses unwind double-reports.
- Pause on a non-journaled (single or encrypted-multipart) upload
  degrades to cancel — it can't resume, so it doesn't offer a dead
  resume button.
- `open_modal(prefill, placeholder)` resets the shared `f_name` per
  dialog; breadcrumbs route through `set_cwd` (nav history consistent);
  the download dir persists the *picked* folder; `render_auth` matches
  `AuthPassword` explicitly; move modal filters the object's subtree.
- `thumb_requested` stays marked on failure (no infinite refetch);
  `ThumbnailFailed` emits when no source exists.

## Removed surplus

`channel_title`, `pending_uploads`, `ProgressReader.total`,
`upload_single_task`'s duplicate `path`/`pt_source` params, dead
`JournalChanged` event, dead `encrypted?psz:psz` conditional, dead
`ev`/`hint` wire branches, redundant mkdir test, restating comments,
detached/stale doc comments (read-only mounts, 1 MiB chunks, stray
`recover_msg_id` doc on `is_video_name`).

## Bugs fixed along the way

- Crash-resume re-uploaded finished files (manifest_msg_id unset).
- `sign_out` left peer/channel/vault mounted on the old drive.
- `poll_pending_join` blocked the command loop ~120 s.
- `ffmpeg -ss 10%` invalid flag for video posters.
- `backup_tick` emitted a user-facing error every 60 s on an
  empty/fully-backed-up watch dir.
- Upload-dir walks awaited mkdir RTTs on the command loop
  (stall→burst pattern); walk is off-loop now.
- Flood waits capped at 8 tries / 600 s → `FloodSleep` sleeps the
  requested duration with no try cap.
