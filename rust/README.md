# rust-core workspace

Минимальный implementation bootstrap для ветки `rust-core`.

Текущий каркас намеренно ограничен phase-1 поверхностью:

- `core-domain`
- `core-events`
- `core-policy`
- `core-state`
- `file-index`
- `history-store`
- `context-engine`
- `claw-bridge`
- `secrets-store`
- `recovery-manager`
- `runtime-api`
- `runtime-daemon`
- `session-broker`
- `tmux-bridge`
- `ssh-bridge`

Текущий bootstrap acceptance slice ещё уже:

- local daemon only
- Unix Domain Socket transport only
- `Ping`
- `Health`
- `RegisterWorkspace`

Поверх bootstrap уже начат следующий core slice:

- `RegisterSession`
- `RemoveSession`
- `Sessions`
- SQLite-backed canonical state persistence via `EDEX_CORE_STATE_DB`
- SQLite-backed terminal/AI/system history via `history-store` and `EDEX_CORE_HISTORY_DB`
- daemon-side `ContextSearch` over a derived `context-engine`, rebuilt from history on restart
- daemon-side `RefreshFileIndex` / `FileList` / `FileStat` / `FilePreview` / `FileSearch` for Linux-first local resource access
- `secrets-store` with source chain `EDEX_CORE_MASTER_PASSPHRASE -> Secret Service lookup` and encrypted wrapping for state snapshots and history content
- `recovery-manager` with encrypted export/import/rekey recovery bundles
- host-backed `tmux-bridge` for isolated local tmux session create/list with idempotent empty-runtime handling
- daemon-side `RegisterLocalTmuxSession` command wired to real tmux runtime through `plan -> preflight -> external side effect -> commit -> cleanup`
- daemon-side `AppendHistoryEntry` / `RecentHistory` flow for terminal and AI transcript events
- daemon-side `ImportSshConfig` / `SshHosts` flow with API-owned SSH DTOs
- `ssh-bridge` with strict-subset OpenSSH import and typed ssh/tunnel command builders
- `claw-bridge` with real `claw` provider probing through `version/doctor/status`
- daemon-side `RunAgentTask` / `AgentProviderStatus` flow with core-owned `AgentTask`, provider boundary execution, and automatic `ChatUser` / `ChatAgent` transcript writes into `history-store`

Текущий hardened-core milestone дополнительно закрепляет:

- bounded in-memory history retention
- streamed encrypted recovery export/import/rekey, без giant JSON string path
- workspace-scoped file access на уровне самого API contract
- policy enforcement для agent execution, включая deny для `danger-full-access`
- canonicalized workspace roots и canonicalized agent/file path checks

Все остальные команды и будущие core-модули считаются provisional до тех пор, пока этот минимальный local control plane не закреплён как стабильная база.

Что здесь принципиально не делается заранее:

- нет UI crates
- нет plugin runtime
- нет web/gRPC stack
- нет remote/PTY helper crates по умолчанию
- нет раннего `tokio-util`/`tokio-rusqlite`/`portable-pty`

## TUI Adapter Branch

Ветка `tui-client` открывает следующий слой поверх этого ядра: тонкий Linux-first terminal client, который общается только с daemon по UDS и не владеет state/policy/session logic.

Текущий первый TUI slice:

- crate `tui-client`
- только `crossterm`, без `ratatui` и других layout frameworks
- reuse `runtime-api::{encode_json_frame, decode_json_frame}`
- `Workspaces` query как явный daemon-owned overview contract
- workspace bootstrap через `RegisterWorkspace`
- local tmux session create/remove через `RegisterLocalTmuxSession` / `RemoveSession`
- workspaces / sessions / files / preview / history / context / agent status / agent prompt
- smoke mode: `cargo run -p tui-client -- --smoke`

Hard boundary для `tui-client`:

- нет прямых доступов к SQLite/history/state files
- нет прямых вызовов `tmux`, `ssh`, `claw`
- нет собственной policy logic
- все file operations идут только через workspace-scoped daemon API

Правило bootstrap:

1. сначала доменные типы, state и local control plane
2. затем session/ssh/tmux adapters
3. затем durable state/history/context plane и secrets integrations
4. затем file-index и recovery workflows
5. только потом deeper remote integrations and richer agent orchestration

Сборка:

```bash
cargo build --workspace
```

Проверка текущего bootstrap-среза:

```bash
cargo test --workspace --offline
EDEX_CORE_BOOTSTRAP_ONLY=1 cargo run -p runtime-daemon --offline
EDEX_CORE_BOOTSTRAP_ONLY=1 EDEX_CORE_STATE_DB=/tmp/edex-ui-2026-rust-core.sqlite3 cargo run -p runtime-daemon --offline
EDEX_CORE_BOOTSTRAP_ONLY=1 EDEX_CORE_STATE_DB=/tmp/edex-ui-2026-rust-core.sqlite3 EDEX_CORE_HISTORY_DB=/tmp/edex-ui-2026-rust-history.sqlite3 cargo run -p runtime-daemon --offline
EDEX_CORE_BOOTSTRAP_ONLY=1 EDEX_CORE_MASTER_PASSPHRASE=test-passphrase EDEX_CORE_STATE_DB=/tmp/edex-ui-2026-rust-core.sqlite3 EDEX_CORE_HISTORY_DB=/tmp/edex-ui-2026-rust-history.sqlite3 cargo run -p runtime-daemon
cargo build -p runtime-daemon --bin host_acceptance
./target/debug/host_acceptance
cargo run -p tui-client -- --smoke
../scripts/run-tui-dev.sh
../scripts/run-tui-dev.sh --smoke
```

Примечание:

- в sandbox-средах end-to-end UDS bind может быть запрещён политикой окружения; для этого в `runtime-daemon` оставлен host-only ignored test на реальный socket roundtrip
- реальные `tmux` integration tests подтверждены host-side вне sandbox, потому что sandbox режет операции с tmux sockets
- `ssh-bridge` сейчас сознательно не пытается “понимать почти весь OpenSSH”; он импортирует только явный строгий поднабор и возвращает прозрачные `unsupported`-ошибки для global directives, wildcard hosts и неподдержанных host directives
- `core-state` теперь умеет durable snapshot persistence через bundled SQLite; на текущем этапе это canonical state foundation, а не финальная encrypted store
- `history-store` уже хранит терминальные и AI-события как durable transcript plane, но это ещё не финальный encrypted retrieval/context layer
- `context-engine` сейчас intentionally derived и rebuildable: индекс живёт в памяти daemon и поднимается заново из durable history после рестарта
- `file-index` сейчас local-first и metadata/path-search oriented: без watcher layer и без full-text file content indexing
- `recovery-manager` уже делает streamed encrypted export/import/rekey bundles, но ещё не закрывает TPM2-backed key hierarchy
- `secrets-store` уже умеет optional Secret Service lookup через `EDEX_CORE_SECRET_SERVICE_LOOKUP=1`; текущая host-side probatio подтверждена только для env source, а не для живого user keyring
- host-side local `claw doctor` / `claw status` proof подтверждён; реальный bounded `claw prompt` запуск против внешнего provider path на этом хосте пока не дал завершённого ответа и завершился по `timeout 20s`
- host acceptance harness теперь существует как [host_acceptance.rs](/home/mrz/projects/edex-ui-2026/rust/crates/runtime-daemon/src/bin/host_acceptance.rs) и доказан host-side: live UDS transport, file-index, context, recovery import/export, fake-claw agent execution и tmux cleanup прошли end-to-end
- первый TUI smoke against live daemon тоже подтверждён host-side через отдельный socket в `/tmp`: `TUI SMOKE OK`
- populated TUI smoke тоже подтверждён host-side: `TUI SMOKE OK workspaces=1 sessions=1 files=26`
- TPM2-backed key storage остаётся следующим Linux-specific слоем
