# Rust Core Implementation Plan

## 0. Target Outcome

Получить Linux-first secure Rust core, который поддерживает:

- encrypted settings and state
- encrypted terminal/chat history
- локальные и удалённые сессии
- tmux-backed persistence
- SSH profiles and tunnels
- file navigation and previews
- AI agent workflows through `claw-code`
- fast AI-facing context retrieval
- import/export/recovery

## 1. Scope Boundary

### In scope

- Linux only
- Rust headless core
- core only in current branch
- Unix socket local control plane
- SSH/tmux/session model
- navigation model
- AI agent integration
- encrypted storage and key lifecycle
- import/export/recovery

### Out of scope for phase 1

- macOS
- Windows
- any concrete UI implementation
- multi-window / multi-monitor UX
- plugin host / widget platform
- browser/Electron gateway
- team collaboration cloud

## 2. Repository Strategy

Ветка `rust-core` используется как architectural incubation branch.

Implementation bootstrap starts with a minimal phase-1 subset.
Полный crate tree остаётся целевой структурой, но не должен scaffold-иться раньше, чем понадобится реальной работой.

Целевая структура репозитория:

```text
rust/
  Cargo.toml
  crates/
    core-domain/
    core-events/
    core-state/
    core-policy/
    core-observability/
    runtime-daemon/
    runtime-api/
    session-broker/
    tmux-bridge/
    ssh-bridge/
    nav-engine/
    file-index/
    history-store/
    context-engine/
    system-observer/
    claw-bridge/
    config-model/
    secrets-store/
    recovery-manager/
    test-harness/
```

Current implementation bootstrap intentionally starts smaller:

```text
rust/
  crates/
    core-domain/
    core-events/
    core-policy/
    core-state/
    runtime-api/
    runtime-daemon/
    session-broker/
    tmux-bridge/
    ssh-bridge/
```

Остальные crates добавляются только по мере реальной работы, а не заранее.

### Bootstrap acceptance slice

До расширения phase-1 объёма минимально принимаемый вертикальный срез должен оставаться таким:

- один local daemon
- один UDS transport
- `Ping`
- `Health`
- `RegisterWorkspace`
- in-memory canonical state only

Для этого bootstrap slice не являются acceptance-обязательствами:

- agent task execution
- session attach protocol
- event subscriptions
- encrypted persistence
- secrets integration
- history/context retrieval

## 3. Architectural Style

### 3.1 Headless daemon

Центральный процесс:

- хранит state
- управляет сессиями
- управляет туннелями
- исполняет tasks/jobs
- эмитит typed events
- принимает команды от локальных клиентов
- owns encrypted persistence and key access

### 3.2 Client adapters

В текущем плане клиенты не проектируются и не реализуются.
Нужен только ядровой control/query boundary.

### 3.3 Control plane

Рекомендуемая схема:

- internal typed API in Rust
- local RPC over Unix Domain Socket

Нормативное решение для ранних фаз:

- внутренний контракт: strongly typed Rust service layer
- primary local transport: UDS
- no browser/Electron transport in active core phases

Storage requirements:

- control plane metadata is not the persistence layer
- canonical persistence is encrypted at rest
- no plaintext fallback store for production mode

## 4. Domain Model

### 4.1 Workspace

```text
Workspace
  id
  name
  roots[]
  bookmarks[]
  session_refs[]
  host_refs[]
  agent_profiles[]
  retention_policy
```

### 4.2 Session

```text
Session
  id
  workspace_id
  session_kind(local|ssh|tmux|task|agent)
  backing(local_pty|tmux_session|remote_tmux|remote_shell)
  status
  cwd
  env_profile
  history_ref
```

### 4.3 Host

```text
HostProfile
  id
  display_name
  ssh_target
  jump_hosts[]
  auth_strategy
  default_workspace
  tunnel_profiles[]
  policies[]
```

### 4.4 Tunnel

```text
TunnelProfile
  id
  host_id
  direction(local|remote|dynamic)
  bind_address
  bind_port
  target_host
  target_port
  lifecycle(manual|auto|workspace_bound)
  transport_kind(ssh)
```

### 4.5 Navigation

```text
NavState
  workspace_id
  location
  columns[]
  preview
  selection
  history
  bookmarks
```

### 4.6 Agent

```text
AgentTask
  id
  workspace_id
  session_context[]
  target_files[]
  tool_permissions
  prompt
  status
  outputs
  transcript_ref
```

Rule:

- `agent subsystem` belongs to the core platform
- concrete agent runtimes do not own the core domain model
- `claw-code` is the canonical first provider, not the sole architectural truth

### 4.7 History

```text
HistoryStream
  id
  owner_kind(terminal|agent|chat|task)
  owner_id
  workspace_id
  retention_policy
  encryption_profile
  exportable(true|false)
```

## 5. Crate-by-Crate Plan

### `core-domain`

Содержит основные типы:

- workspace
- session
- host
- tunnel
- agent task
- encryption profile

Без IO и side effects.

### `core-events`

Typed event model:

- `SessionStarted`
- `SessionStopped`
- `HostConnected`
- `TunnelOpened`
- `NavSelectionChanged`
- `AgentTaskStarted`
- `AgentTaskCompleted`
- `HistoryCompacted`
- `RecoveryCompleted`

### `core-state`

Единое state store:

- in-memory canonical state
- persistence snapshots
- recovery on restart
- encrypted state metadata coordination

### `core-policy`

Permission and capability layer:

- filesystem scopes
- command execution scopes
- tunnel privileges
- agent tool permissions
- context sensitivity labels
- retrieval redaction policies

### `core-observability`

- structured logs
- tracing spans
- audit events for agent/ssh/tunnel actions
- crash diagnostics and health endpoints

### `runtime-daemon`

Главный бинарь/служба:

- boot
- config loading
- service wiring
- state persistence
- API startup
- service supervision
- key unlock workflow

### `runtime-api`

Control plane contract:

- commands
- queries
- event subscriptions
- session attach protocol

### `session-broker`

Управление всеми видами сессий:

- local PTY
- tmux local
- remote shell
- remote tmux
- agent tasks as pseudo-sessions

### `tmux-bridge`

Интеграция через `tmux` control mode:

- create/attach/detach/list sessions
- list panes/windows
- subscribe to pane output and metadata
- resize propagation
- command execution

### `ssh-bridge`

Управление:

- host profiles
- command execution
- tunnel lifecycle
- health checks
- reconnect policy
- OpenSSH config parsing/import

Implementation note:

- Phase 1 uses battle-tested system binaries and agent tooling where practical: `ssh`, `ssh-agent`, `scp`/`sftp`, `tmux`
- no attempt to fully replace OpenSSH behavior in early milestones

### `nav-engine`

Навигационная модель по мотивам `ranger`:

- multi-column traversal
- preview model
- bookmarks/jumps
- fuzzy actions
- local/remote abstraction

### `file-index`

- watcher
- metadata cache
- search
- content index hooks
- remote index adapter later
- AI-facing derived views for fast workspace retrieval

### `history-store`

- terminal history
- AI chat history
- command transcript storage
- retention policies
- export/import support
- encrypted search/index hooks as mandatory rule
- structured chunking by workspace/session/host/task
- fast retrieval-oriented read models for agents
- purge coupling with canonical retention/deletion policy

### `context-engine`

- compose agent context from files, histories, sessions and workspace state
- serve low-latency retrieval APIs to agent runtimes
- enforce policy-filtered context windows
- merge canonical state with derived indexes
- avoid repeated full rescans by agents
- apply redaction/classification before context leaves the daemon
- default deny for raw sensitive transcript retrieval unless policy explicitly allows it

### `system-observer`

- CPU/RAM/disk/network/processes
- `/proc` readers
- lightweight telemetry for internal services and policy engines

### `claw-bridge`

Интеграция с `claw-code`:

- запуск agent tasks
- auth/config bridge
- workspace context mapping
- permissions mapping
- artifact/result ingestion
- provider health/version reporting
- explicit separation between `agent-core contracts` and `claw` implementation details

### `config-model`

- user config
- workspace config
- host profiles
- secret references
- import/export manifest model

### `secrets-store`

- references to Secret Service / keyring entries
- ssh-agent integration metadata
- no raw private key persistence in plain config
- wrapped master keys / unlock descriptors
- TPM2-backed sealed credential as primary Linux v1 path when supported
- Secret Service fallback path
- passphrase fallback for portable/headless import-export

### `recovery-manager`

- encrypted export/import orchestration
- schema/version migration checks
- rekey workflows
- restore validation
- credential rebinding after transfer

## 5.1 Deferred Complexity

These areas are intentionally deferred until core contracts stabilize:

- any concrete UI
- plugin host and ABI
- remote content indexing beyond basic traversal/search hooks
- advanced searchable encryption beyond practical v1 needs

Not deferred:

- AI-fast local retrieval over terminal/chat/file history that is required for core agent workflows

## 6. SSH and Tunnel Plan

### Phase 1

- host profiles
- direct ssh launch
- local forwards
- remote forwards
- dynamic socks tunnels
- all remote transport via SSH-backed paths
- define remote data locality rule

### Phase 2

- jump hosts
- workspace-bound tunnels
- reconnect strategies
- host health and diagnostics

### Phase 3

- remote workspace templates
- per-host AI policies

## 7. tmux Plan

### Why tmux first

- solved problem for persistence
- mature session semantics
- easier than building mux semantics from scratch

### Implementation

- use `tmux -C` / control mode
- map tmux IDs to internal session objects
- keep our own domain state above tmux
- detect and recover from tmux restarts / stale sockets

### Rule

tmux is a substrate, not the whole domain model.

## 8. Navigation Plan

### Required features

- multi-column browser
- preview panel
- shell-style bookmarks
- recent/jump stack
- fuzzy quick switch
- remote path browsing

### Rule

Navigation must remain client-agnostic and belong to core domain contracts, not to any future surface implementation.

Rule for current branch:

- only the domain model and retrieval contracts matter
- no UI implementation is part of this plan

## 9. AI / `claw-code` Plan

### Integration goals

- treat `agent subsystem` as core-tier platform capability
- use `claw-code` as the canonical first provider/runtime
- map workspaces into agent contexts
- map permissions into tool policies
- expose sessions/files/history as contextual resources
- expose them through fast daemon-owned retrieval APIs, not UI caches

### Core Rule

`AI is not an optional addon in this product.`

But:

- `agent-core` must remain the owner of task/domain/policy/context contracts
- `claw-code` must remain behind provider boundaries
- no core storage/session/history/file contract may be shaped by unstable internal details of `claw-code`

### Recommended first path

Do not fork `claw-code` into the core immediately.

Instead:

1. define `agent-core` contracts inside the core platform
2. define `AgentProvider` trait
3. implement `ClawProvider` as the first canonical provider
4. start with process-level integration
5. move deeper only after contracts stabilize

And in parallel:

6. define `ContextProvider` / `ContextQuery` contracts inside the core
7. make agent retrieval use indexed daemon state before raw filesystem rescans

Risk note:

- `claw-code` should be version-pinned and isolated behind provider boundaries
- no core contract may depend directly on unstable internal details of `claw-code`

### AgentProvider contract

```text
start_task()
stream_events()
cancel_task()
list_tools()
check_health()
resume_session()
```

### Context requirements for agents

The core must support fast local access to:

- recent terminal history by workspace/session/host
- AI chat transcripts by task/workspace
- recent file changes
- indexed file metadata
- selected content excerpts/chunks
- bookmarks, jumps and active workspace state

Recommended rule:

- canonical store remains authoritative
- retrieval/index layer is rebuilt or incrementally maintained from canonical events/state
- indexes containing sensitive data are encrypted at rest too
- remote context should prefer remote-local retrieval over central mirroring

## 10. Security Model

### Core ideas

- all external clients are less trusted than the daemon
- every agent action is policy-checked
- workspace boundaries are explicit
- persisted state is encrypted by default
- histories are encrypted by default

### Linux assumptions

- UDS permissions
- optional systemd --user service
- filesystem capability scopes
- SSH key usage delegated carefully

### Encryption-at-rest model

Recommended canonical model:

- encrypted embedded database as source of truth
- wrapped master key outside the DB
- history/settings/session metadata stored in encrypted form
- encrypted export bundles for transfer/import

Preferred key hierarchy:

1. master key generated randomly
2. data encryption keys derived or wrapped per store/bundle
3. master key kept via strongest available Linux mechanism

Preferred Linux key storage order:

1. Secret Service / system keyring for standard user-session deployments
2. deployment-specific `systemd-creds` / TPM2-backed integration when environment explicitly supports and opts into it
3. passphrase unlock fallback for portable/headless mode

Explicit rule:

- no plaintext private keys
- no plaintext terminal history
- no plaintext AI chat history
- no plaintext settings store in production mode
- no plaintext derived indexes
- no plaintext bootstrap metadata

## 11. Persistence

Нужно сохранять:

- workspaces
- host profiles
- tunnel profiles
- bookmarks
- encrypted settings
- encrypted terminal history
- encrypted AI chat history
- session recovery metadata
- agent task history metadata

Рекомендуемый формат:

- canonical encrypted embedded DB for operational state
- encrypted derived indexes for fast AI retrieval and search
- encrypted file bundles for import/export/backup
- encrypted bootstrap/recovery descriptors only

## 14.1 Storage Decision

### Decision

Выбрать:

`encrypted embedded database as canonical store`

с экспортом/импортом через:

`encrypted portable files/bundles`

### Why database wins

- transactionality
- schema migrations
- linked state between sessions, hosts, tunnels, history, workspaces
- efficient querying
- retention and cleanup policies
- single source of truth
- good base for building daemon-owned retrieval/index read models

### Why files still remain

- fast deploy
- transfer between machines
- backup/restore
- explicit export/import workflows

### Recommended practical path

- Rust storage layer built around SQLite
- encrypted at rest via SQLCipher-compatible path
- retrieval/index tables or sidecar encrypted indexes derived from canonical state
- file exports as encrypted archives with manifest + payload

Mandatory rule for derived indexes:

- encrypted at rest
- rebuildable from canonical state
- scoped by retention policy
- deletable together with canonical purge

## 12. Key Lifecycle And Data Deletion

The core must explicitly support:

- initial key generation
- key rotation / rewrap
- rekey after import/export
- retention executor
- selective deletion of histories
- rebuild of derived indexes after key changes or purges
- bootstrap/recovery descriptor re-encryption

Rule:

- deletion semantics must be defined before history growth begins
- derived indexes must never outlive canonical deletion policy

## 12.1 Context Classification And Redaction

Before data is exposed to agent retrieval:

- transcripts and file excerpts must be classifiable
- sensitive spans must be markable/redactable
- raw secret-bearing terminal history must default to deny
- policy may grant elevated retrieval only explicitly

The context plane must distinguish at least:

- safe metadata
- redactable content
- sensitive raw content

## 13. Testing Strategy

### Required layers

1. unit tests for domain/state/policy
2. integration tests for daemon services
3. mock parity harness for session/agent/tunnel flows
4. golden scenario tests for navigation/workspace behavior

### Reference discipline

По примеру `claw-code` нужен:

- `PARITY.md`
- honest checkpointing
- scenario harness
- migration readiness criteria

## 14. Phased Roadmap

### Phase 0 — Definition

- finalize product contract
- define domain types
- define API contract
- write `PARITY.md`
- define secret/key handling model
- define retention model for chat and terminal history
- define export/import bundle format
- define AI context query model and retrieval SLAs
- define remote data locality model
- define key rotation and deletion semantics

### Phase 1a — Bootstrap control plane

- `runtime-daemon`
- `core-domain`
- `core-state`
- `runtime-api`

Deliverable:

- daemon boots
- commands/queries work over UDS
- `Ping` works
- `Health` works
- `RegisterWorkspace` works
- state mutations go through one canonical in-memory path

### Phase 1b — Secure persistence foundation

- basic persistence
- `core-observability`
- `secrets-store`
- `history-store`
- `context-engine`
- `recovery-manager`

Deliverable:

- workspace config loads
- audit logs and health status exist
- encrypted state store works
- encrypted history store works
- core can answer at least basic low-latency context queries over recent history/state
- import/export/recovery path exists

### Phase 2 — Sessions

- `session-broker`
- local PTY integration
- `tmux-bridge`

Deliverable:

- local persistent sessions
- attach/detach
- event stream

### Phase 3 — Remote

- `ssh-bridge`
- tunnel management
- remote tmux sessions

Deliverable:

- open remote host
- open tunnel
- attach to remote tmux-backed session
- import at least one OpenSSH-style host profile
- prove that all remote operational traffic uses SSH-backed transport
- prove remote context retrieval policy without unsafe central mirroring

### Phase 4 — Navigation

- `nav-engine`
- `file-index`

Deliverable:

- ranger-like navigation model over local files
- preview-capable API
- file retrieval layer usable by agents without full rescans

### Phase 5 — AI

- `claw-bridge`
- agent task model
- permission mapping
- `context-engine` retrieval integration

Deliverable:

- run agent task against workspace
- persist encrypted chat/task transcript history
- agent receives low-latency context from daemon-owned indexes and history stores

## 14.2 Execution Campaign To V1

Implementation order is mandatory:

1. lock the control plane
   - keep `runtime-daemon`, `runtime-api`, `core-domain`, `core-state` small and typed
   - do not widen transport or add alternate clients
2. finish session orchestration
   - `session-broker`
   - register/list/remove session contracts
   - workspace-bound session lifecycle
3. land tmux substrate
   - `tmux-bridge`
   - attach/detach/list
   - map tmux objects into internal session contracts
4. land SSH substrate
   - `ssh-bridge`
   - host profiles
   - tunnels
   - remote tmux/session attach
5. land secure persistence
   - `config-model`
   - `secrets-store`
   - `recovery-manager`
   - encrypted canonical store
6. land history/context plane
   - `history-store`
   - `file-index`
   - `context-engine`
   - AI-fast retrieval APIs
7. land agent integration
   - `claw-bridge`
   - provider boundary
   - policy-aware tool/context mapping
8. stabilize v1
   - parity review
   - recovery drills
   - performance passes
   - host-level integration tests outside sandbox

Gate rule:

- no next phase is considered complete until its commands, queries, state transitions and tests are in place

Current checkpoint on `2026-04-08`:

- Phase 1a complete
- Phase 1b is materially landed in code: SQLite-backed canonical state persistence is live through `core-state`, `history-store` is live for terminal/AI/system transcripts, `secrets-store` already wraps state/history payloads with `env -> Secret Service` key-source chain, `context-engine` now serves basic daemon-owned `ContextSearch`, and `recovery-manager` already ships encrypted export/import/rekey workflows over state/history/ssh snapshots
- Phase 2 foundation remains in progress with `session-broker`, host-backed `tmux-bridge`, daemon-side local tmux registration flow with `plan -> preflight -> external side effect -> commit -> cleanup`, and daemon-side `ssh-bridge` import/query flow over strict-subset OpenSSH host profiles
- Phase 4 foundation has started in code through `file-index`: daemon-side refresh/list/stat/preview/search for Linux-first local resource access without UI coupling
- WAR ROOM update: `agent subsystem` is now explicitly treated as core-tier architecture, while `claw-code` remains the canonical first provider implementation behind provider boundaries
- `claw-bridge` is now landed in code: daemon-side `AgentProviderStatus` probes `claw version/doctor/status`, `RunAgentTask` uses core-owned `AgentTask` contracts, and successful runs persist `ChatUser` / `ChatAgent` transcript events into durable history
- host acceptance is now landed as code through `runtime-daemon/src/bin/host_acceptance.rs` and has passed on the real host: live UDS daemon transport, file indexing, context retrieval, recovery export/import, fake-claw agent execution, and tmux session cleanup are all proven end-to-end
- Remaining gaps before a stricter core v1 are TPM2-backed key storage, encrypted persisted derived indexes beyond rebuildable in-memory context, live remote tunnel/session execution against a real SSH target, and a successful bounded `claw prompt` execution against the real installed binary on this host

## 15. Non-Negotiable Rules

1. No UI work inside `rust-core` execution plan.
2. No AI integration that bypasses policy.
3. No plaintext persistence of settings, histories or private keys in production mode.
4. No remote transport outside SSH-backed paths.
5. No full mirroring of remote state by default.
6. No plugin platform before core API v1.

## 16. First Three Concrete Deliverables

### Deliverable A

`PARITY.md`

Список:

- что переносим из eDEX
- что не переносим
- что переосмысляем

### Deliverable B

Rust workspace skeleton:

- daemon
- domain
- state
- api
- session broker
- history store
- secrets store
- recovery manager

### Deliverable C

Core vertical slice:

- encrypted workspace state
- create local session
- attach tmux-backed session
- persist encrypted history
- answer basic agent context query
- export and restore state bundle

## 17. Success Criteria

Систему можно считать идущей в правильном направлении, если:

- один daemon держит весь canonical state
- локальная и удалённая tmux-сессия имеют единый UX-контракт
- AI task использует daemon-owned context plane
- AI/runtime layer получает быстрый доступ к истории и файлам без дорогостоящего полного сканирования
- import/export/recovery не нарушают encryption model

## 18. Critical Risks

1. Over-scope risk
Trying to deliver daemon, deep remote substrate, advanced indexing and deep AI at once will stall the project. Phase discipline is mandatory.

2. `claw-code` coupling risk
If `ClawProvider` leaks `claw-code` internals into core contracts, future upgrades will become expensive.

3. tmux dependency risk
tmux gives persistence quickly, but can distort the domain if treated as the product model instead of a substrate.

4. Remote locality risk
If remote context is implemented via naive central copying, the core will become slow, leaky and harder to secure.

5. Crypto portability risk
Strong encryption plus easy portability is easy to get wrong. Export/import format, key wrapping and recovery flow must be designed before implementation spreads.

## 21. External References

- `claw-code`
  https://github.com/ultraworkers/claw-code
- `ranger`
  https://github.com/ranger/ranger
- `tmux` control mode
  https://github.com/tmux/tmux/wiki/Control-Mode
- `OpenSSH` manuals
  https://www.openssh.org/manual.html
- `systemd-creds`
  https://www.freedesktop.org/software/systemd/man/latest/systemd-creds.html
- `SQLite FTS5`
  https://www.sqlite.org/fts5.html
