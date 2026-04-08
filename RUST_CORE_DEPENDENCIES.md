# Rust Core Dependencies

Актуально на `2026-04-08`.

Этот файл фиксирует dependency bill of materials для ветки `rust-core`.
Фокус только на `core v1` для Linux.

## Decision Summary

Ключевое решение:

- строить `daemon-first` ядро на узком dependency surface
- опираться на сильный Linux substrate вместо попытки переписать всё внутри Rust
- держать `encrypted embedded DB` как canonical store
- строить `AI-fast retrieval` через derived local indexes, а не через отдельную vector DB

Практически это означает:

- `tokio + local UDS protocol`, а не `gRPC`/`tarpc`
- `rusqlite + SQLCipher path`, а не ORM-first stack
- `OpenSSH + tmux` как внешние системные опоры
- `SQLite FTS5 + blake3` как retrieval/index plane

## Minimization Rules

- Не добавлять crate в workspace baseline, если он нужен только одной поздней подсистеме.
- Для маленького glue-кода лучше писать локальный адаптер, чем тянуть utility-crate ради одной функции.
- Никогда не копировать в проект криптографию, DB bindings, D-Bus Secret Service или filesystem watcher internals.
- Для Linux-only low-level paths сначала смотреть на `rustix` и системные инструменты, а не на broad cross-platform convenience crates.

## Baseline Now

Эти зависимости разумно принять в bootstrap нового ядра уже сейчас:

- `tokio`
- `serde`
- `serde_json`
- `toml`
- `thiserror`
- `anyhow` only at binary/adapter edges
- `tracing`
- `tracing-subscriber` only in binaries with minimal features
- `rusqlite`
- `secrecy`
- `zeroize`
- `blake3`
- `uuid` with minimal feature set

Скорее не baseline, а scoped-by-subsystem:

- `tempfile`
- `age`
- `notify`
- `ignore`
- `rustix`
- `procfs`
- `fs4`
- `zstd`
- `secret-service`
- `portable-pty`
- `tokio-util`
- `time`

## Version Policy

- Для `tokio` использовать стабильную minor-линию и не гнаться за каждым новым minor.
- Для всех security/storage crates фиксировать точные версии при bootstrap workspace.
- Для `rusqlite` держать минимально необходимый feature set, а не включать широкие convenience bundles без нужды.

## Per-Dependency Verdict

### Keep

- `tokio`
  Основной async runtime для daemon, processes и Unix sockets.
- `serde`
  Базовая сериализация доменных типов и wire payloads.
- `serde_json`
  Нужен для control plane payloads, audit/event envelopes, manifests и agent/task data.
- `toml`
  Нужен для human-editable config fragments.
- `thiserror`
  Правильный error layer для library crates.
- `anyhow`
  Только для binaries и adapter edges.
- `tracing`
  Structured instrumentation across daemon, session, ssh and context flows.
- `tracing-subscriber`
  Подключать только в binary crates и с минимальными feature flags.
- `rusqlite`
  Canonical embedded DB layer.
  Не копировать и не заменять самописным FFI.
- `secrecy`
  Wrapper types for secrets in memory.
- `zeroize`
  Secure zeroing for key/passphrase-bearing data.
- `blake3`
  Fast content hashing for file index, dedup, transcript chunk anchoring and bundle integrity helpers.
- `uuid`
  Stable IDs for workspaces, sessions, tunnels, tasks and history streams.
  Включать минимальные feature flags, а не широкий default surface.

### Keep But Scoped

- `age`
  Нужен для encrypted import/export/recovery bundles.
  Не baseline до появления recovery/import-export code path.
- `tempfile`
  Нужен только в recovery/import-export staging.
- `secret-service`
  Focused Linux fallback path for user-session secret storage through Secret Service API.
  Не нужен до появления реальной `secrets-store` реализации.
  Важно: lookup attributes по спецификации не считаются secret material и могут храниться незашифрованно, значит чувствительные host/workspace identifiers туда класть нельзя.
- `notify`
  Нужен только вместе с `file-index`.
  Сразу проектировать с `PollWatcher` fallback для тех случаев, где native events ненадёжны.
- `ignore`
  Нужен тогда, когда `.gitignore` semantics становятся частью parity.
  Для самого раннего controlled cold scan можно обойтись локальным walker-ом.
- `procfs`
  Нужен только в `system-observer`.
- `fs4`
  Нужен только если реально потребуется advisory locking или filesystem coordination.
- `zstd`
  Нужен только если bundle/export size станет реальной проблемой.
- `time`
  Добавлять только если появляются calendrical operations, formatting rules или richer retention math.

### Prefer Local Thin Layer Instead Of Extra Crate

- `tokio-util`
  Не тащить в baseline автоматически.
  Если нужен только простой length-prefixed UDS protocol, сначала допустим маленький локальный framing layer.
  Если wire protocol начнёт усложняться, тогда подключать `tokio-util` c feature `codec`.
- `tokio-rusqlite`
  Не брать “на всякий случай”.
  Для v1 лучше явно выбрать policy:
  или локальный single DB worker вокруг `rusqlite`,
  или осознанное принятие `tokio-rusqlite`.
- `procfs`
  Для самых ранних метрик допустимы точечные локальные `/proc` readers.
  Если surface растёт, тогда уже подключать crate.

### Smaller Linux-First Alternative Candidates

- `rustix`
  Главный low-level escape hatch для Linux-specific IO, FDs, termios и PTY-related code.
  Лучше держать его как контролируемый низкоуровневый инструмент, чем тащить broad cross-platform abstraction раньше времени.
- `rustix-openpty`
  Более узкий кандидат для Linux-first/local PTY bring-up, чем `portable-pty`.

### Defer Or Avoid For Now

- `portable-pty`
  Быстрый путь к PTY abstraction, но не baseline для Linux-only core.
  Для минимального Linux-first старта сначала лучше оценить более узкий local adapter.
- `oo7`
  Полезен для sandbox/file-backed secret storage, но для текущего headless daemon-first Linux core heavier than needed.
  Для простого Secret Service fallback более узкий `secret-service` crate сейчас лучше подходит по scope.

## External System Dependencies

### Baseline

- `tmux`
  Persistent session substrate and control mode integration.
- `ssh`
  Primary remote transport.
- `ssh-agent`
  First-class auth substrate for SSH usage.
- `sftp`
  Базовый remote file path.

### Optional Integrations

- `systemd`
  Рекомендуемый supervision/lifecycle layer для daemon deployment, но не обязательный для core API.
- `systemd-creds`
  Сильная Linux integration для deployment-specific credential wrapping, но не универсальный runtime key store для всего ядра.
- `scp`
  Compatibility-only path.
  Начиная с OpenSSH 9.0 `scp` по умолчанию использует SFTP protocol, поэтому базовая линия всё равно должна мыслить через `ssh + sftp`.
- `rg`
  Optional accelerator/debug tool.
  Не должен становиться canonical retrieval path ядра.

## Recommended Configuration Choices

### Database path

- Canonical choice: `rusqlite`
- Encryption path:
  - preferred for reproducibility: `bundled-sqlcipher`
  - acceptable Linux ops fallback: system `sqlcipher`
- Retrieval path:
  - use SQLite `FTS5` for derived full-text indexes
  - verify FTS5 support in the chosen SQLCipher build path before locking bootstrap scripts

### Database concurrency path

Выбрать один из двух путей и не держать оба в baseline:

- preferred now: local single DB worker around `rusqlite`
- later only if needed: `tokio-rusqlite`

### Local API path

- Use `tokio::net::UnixListener` / `UnixStream`
- Start with a small local length-prefixed protocol implementation
- Add `tokio-util::codec::LengthDelimitedCodec` only if protocol machinery starts to justify it
- Serialize typed command/query/event payloads with `serde`
- Keep the wire contract local and daemon-owned

### Remote path

- Phase 1-3 should rely on system `OpenSSH`
- Use `tmux -C` / control mode for persistent sessions
- Do not treat a Rust SSH library as the foundation of v1 transport

## Deferred Or Optional Candidates

- `ssh-key`
  Good companion crate for SSH metadata parsing and validation.
- `openssh`
  Acceptable wrapper over system OpenSSH later, but not required for the first vertical slice.
- `openssh-sftp-client`
  Optional structured remote FS layer later if `sftp` process orchestration becomes too coarse.
- `notify-debouncer-full`
  Add only if raw watcher event noise becomes a real maintenance cost.
- `jwalk`
  Consider only if profiling proves cold scans are a bottleneck.
- `sysinfo`
  Optional convenience layer, but not a replacement for Linux-first `/proc` observation.
- `postcard`
  Compact binary wire format option later if JSON or serde-backed framed payloads become too heavy.
- `tantivy`
  Only if SQLite FTS5 eventually proves insufficient for retrieval quality or scale.

## Defer Or Reject For Core V1

- `sqlx` as primary operational store
  Rejected for v1. The core needs tight SQLite/SQLCipher control, not ORM-style abstraction.
- `tarpc`
  Deferred. Too much framework weight for the first local UDS control plane.
- `tonic` / `gRPC`
  Deferred. Too heavy for daemon-local RPC in the first core branch.
- pure-Rust SSH stack as primary transport (`russh`, `ssh-rs`, `ssh2`)
  Deferred. For v1, system OpenSSH remains the stronger substrate.
- vector DB as canonical retrieval store
  Rejected for v1. Retrieval indexes must stay derived from canonical encrypted state.
- plugin runtimes (`wasmtime`, `wasmer`, `extism`)
  Deferred. They drag the plan away from the core line.
- UI/runtime crates (`ratatui`, `tauri`, `wry`, `egui`, `slint`)
  Out of scope for `rust-core`.

## Not Pinned Yet

- dedicated migration framework
  Пока не фиксируется. Для раннего ядра достаточно SQL migration files, исполняемых контролируемо из `recovery-manager`.

## Crate Mapping

### `core-domain`

- `serde`
- `uuid`
- `thiserror`

### `core-events`

- `serde`
- `uuid`

### `core-state`

- `serde`
- `uuid`
- `rusqlite`
- `secrecy`

### `core-policy`

- `serde`
- `thiserror`

### `core-observability`

- `tracing`
- `tracing-subscriber`

### `runtime-daemon`

- `tokio`
- `serde`
- `serde_json`
- `toml`
- `anyhow`
- `tracing`
- `tracing-subscriber`

Optional later:

- `tokio-util`
- `time`

### `runtime-api`

- `tokio`
- `serde`
- `serde_json`
- `uuid`

Optional later:

- `tokio-util`

### `session-broker`

- `tokio`
- `uuid`

Optional later:

- `portable-pty`
- `rustix`
- `rustix-openpty`

### `tmux-bridge`

- `tokio`
- `serde`
- `uuid`

External dependency:

- `tmux`

### `ssh-bridge`

- `tokio`
- `serde`
- `uuid`

External dependencies:

- `ssh`
- `ssh-agent`
- `sftp`

Optional later:

- `scp`
- `ssh-key`
- `openssh`

### `nav-engine`

- `serde`
- `uuid`

Optional later:

- `ignore`

### `file-index`

- `blake3`
- `rusqlite`
- `uuid`

Optional later:

- `notify`
- `ignore`
- `jwalk`
- `tantivy`
- `time`

### `history-store`

- `rusqlite`
- `blake3`
- `secrecy`
- `zeroize`
- `uuid`

Optional later:

- `zstd`
- `fs4`
- `time`

### `context-engine`

- `rusqlite`
- `serde`
- `serde_json`
- `blake3`
- `uuid`

Optional later:

- `time`

### `system-observer`

- `serde`

Optional later:

- `tokio`
- `procfs`
- `rustix`
- `sysinfo`

### `claw-bridge`

- `tokio`
- `serde`
- `serde_json`
- `uuid`

Optional later:

- `time`

Rule:

- integrate `claw-code` through process/provider boundary first
- do not depend on unstable internal crates from `claw-code` in v1

### `config-model`

- `serde`
- `toml`
- `uuid`

Optional later:

- `time`

### `secrets-store`

- `secrecy`
- `zeroize`

Optional later:

- `secret-service`
- `age`
- `systemd-creds`
- `oo7`

### `recovery-manager`

- `rusqlite`
- `age`
- `tempfile`
- `serde`
- `serde_json`
- `uuid`

Optional later:

- `zstd`
- `fs4`
- `time`

## Explicit Non-Goals For The Dependency Graph

- no web stack in the core branch
- no UI rendering crates
- no external search engine as canonical store
- no homegrown crypto format
- no premature plugin runtime
- no large RPC framework until a simple UDS protocol is proven insufficient

## Source References

- Tokio: https://docs.rs/crate/tokio/latest
- Tokio UDS: https://docs.rs/tokio/latest/tokio/net/struct.UnixListener.html
- tokio-util codec: https://docs.rs/tokio-util/latest/tokio_util/codec/index.html
- LengthDelimitedCodec: https://docs.rs/tokio-util/latest/tokio_util/codec/length_delimited/struct.LengthDelimitedCodec.html
- Serde: https://docs.rs/serde/latest/serde/
- serde_json source metadata: https://docs.rs/crate/serde_json/latest/source/Cargo.toml.orig
- TOML: https://docs.rs/toml/latest/toml/
- Tracing: https://docs.rs/tracing/latest/tracing/
- tracing-subscriber: https://docs.rs/crate/tracing-subscriber/latest
- rusqlite features: https://docs.rs/crate/rusqlite/latest/features
- tokio-rusqlite: https://docs.rs/tokio-rusqlite/latest/tokio_rusqlite/
- SQLite WAL: https://sqlite.org/wal.html
- SQLite FTS5: https://www.sqlite.org/fts5.html
- SQLCipher: https://github.com/sqlcipher/sqlcipher
- systemd-creds: https://www.freedesktop.org/software/systemd/man/latest/systemd-creds.html
- Secret Service spec: https://specifications.freedesktop.org/secret-service/latest-single/
- secret-service crate: https://docs.rs/secret-service/latest/secret_service/
- oo7: https://docs.rs/oo7/latest/oo7/
- age: https://docs.rs/age/latest/age/
- zeroize: https://docs.rs/zeroize/latest/zeroize/
- secrecy: https://docs.rs/secrecy/latest/secrecy/
- portable-pty: https://docs.rs/crate/portable-pty/latest
- rustix: https://docs.rs/rustix/latest/rustix/
- rustix termios example surface: https://docs.rs/rustix/latest/rustix/termios/fn.tcsetpgrp.html
- notify: https://docs.rs/notify/latest/notify/
- ignore: https://docs.rs/crate/ignore/0.4.25/source/README.md
- procfs: https://docs.rs/procfs/latest/procfs/
- blake3: https://docs.rs/blake3/latest/blake3/
- fs4: https://docs.rs/crate/fs4/latest/source/Cargo.toml.orig
- OpenSSH manuals: https://www.openssh.org/manual.html
- OpenSSH release notes: https://www.openssh.com/releasenotes.html
- tmux control mode wiki: https://github.com/tmux/tmux/wiki/Control-Mode
- tmux man page: https://man7.org/linux/man-pages/man1/tmux.1.html
- claw-code: https://github.com/ultraworkers/claw-code
