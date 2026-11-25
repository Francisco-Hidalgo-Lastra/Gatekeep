# Gatekeep — notes for Claude

A desktop mail client (Rust, egui/eframe 0.37) for Gmail (IMAP + App Password) and
Outlook/Hotmail (IMAP + OAuth device code). See `README.md` for user-facing behavior.

## Commands

- Run: `cargo run --release`
- Check: `cargo check` (8 pre-existing dead-code warnings in `src/rules.rs`; not ours)
- Test: `cargo test`. `cargo test -- --ignored ollama` also hits a local Ollama.
- The repo has no commits yet (branch `master`). Don't commit unless asked.

## Architecture

- `src/main.rs` starts the app. `src/app.rs` is the whole UI (`GatekeepApp`).
- `src/worker.rs` runs all network and credential I/O on one background thread.
  The UI sends `Command`s and reads `Event`s over channels; it never blocks on I/O.
- The worker handles commands strictly in order and emits `Event::Done` after each one.
  This is the command queue: a new command waits until the previous one finishes.
- `App.in_flight` counts commands sent but not yet `Done` (running + waiting).
  `App.status` is the label of the most recently sent command.
  `status_label()` in `app.rs` shows "… · N in queue" in the toolbar when N > 1.
- `busy()` is `in_flight > 0`. It only drives the spinner/status label and skips auto-refresh.
  Buttons are never disabled while busy: clicking one just sends a command that waits in
  the queue. Don't reintroduce `add_enabled(!busy)` on buttons.
- Exception: "🧹 Delete blocked mail" (`Command::TrashBlocked`) locks the whole UI behind a
  progress modal (`App.sweep`, `sweep_window()`) until its own `Done` arrives. The worker sends
  `Event::Progress` for it only.
- `src/ollama.rs` is the exception to "all I/O in `worker.rs`": it runs Ollama requests
  (summaries, inbox report) on its own thread with its own queue (`AiCommand`/`AiEvent`),
  so a slow generation never holds up mail commands. It doesn't touch `in_flight`.
  UI state for it is `App.ai`. Results stream in as `AiEvent::Chunk`; a `JobId` ties them
  to a summary or the report, and `AiHandle::stop` cancels one.
- Other modules: `auth.rs` (credentials, keychain fallback), `oauth.rs` (Microsoft
  device code), `mail.rs` (IMAP client), `filter.rs` (delete/blocked-sender search
  and validation), `rules.rs` (display rules), `config.rs`, `model.rs`, `theme.rs`.

## Safety rules (product invariants)

- Never send mail and never mark mail as read.
- Reading uses IMAP `EXAMINE` and `BODY.PEEK`. Keep it that way.
- The only mutation is moving mail to Gmail Trash or Outlook Deleted Items, and only on
  user action (or a blocked sender). Don't add permanent deletes.
- An empty delete filter must never be allowed.
- Credentials go to the OS keychain. The `account.json` fallback is `0600`.
  Microsoft access tokens stay in memory only.

## Conventions

- Rust edition 2024 (let-chains are used). Keep `cargo check` clean of new warnings.
- Keep UI state changes on the UI thread; do I/O only in `worker.rs`.
- Comments explain why, briefly. The existing style is short doc comments.
