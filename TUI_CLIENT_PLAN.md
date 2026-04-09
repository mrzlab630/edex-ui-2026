# TUI Client Plan

## Mission

Построить `tui-client` как тонкий Linux-first terminal adapter поверх существующего `rust-core` daemon API.

Эта работа идёт в отдельной ветке `tui-client` и не меняет правило `No UI work inside rust-core execution plan`. `rust-core` остаётся каноническим headless ядром, а TUI здесь рассматривается как отдельный client adapter.

TUI не владеет state, policy или session logic. Он только:

- подключается к daemon по UDS
- запрашивает typed data
- отображает состояние
- отправляет команды пользователя обратно в daemon

## Hard Boundaries

- никаких прямых доступов TUI к SQLite/state/history files
- никаких прямых вызовов `tmux`, `ssh`, `claw` из TUI
- никаких новых domain моделей поверх `runtime-api`
- никакого дублирования policy logic из `core-policy`
- никакой логики, которая должна жить в daemon, не переносится в UI

## First Slice

Первая полноценная версия TUI должна уметь:

- health/status daemon
- workspace overview
- workspace bootstrap через daemon command
- session list
- local tmux session create/remove
- local file navigation в пределах выбранного workspace
- file search в пределах текущего workspace path
- jump to workspace roots/bookmarks and open top file-search hit
- recent history с `workspace/session` scope
- context search с `workspace/session` scope
- agent provider status
- submit agent task with `workspace/session` scope carried through daemon boundary

## Execution Order

### Phase 1: Transport

- crate `tui-client`
- blocking UDS client через `std::os::unix::net::UnixStream`
- reuse `runtime-api::{encode_json_frame, decode_json_frame}`
- typed request helpers для:
  - `Health`
  - `Workspaces`
  - `Sessions`
  - `RecentHistory`
  - `ContextSearch`
  - `AgentProviderStatus`
  - `FileList`
  - `FilePreview`
  - `RunAgentTask`

### Phase 2: TUI App Model

- `AppState`
- `FocusPane`
- `SelectedWorkspace`
- `SelectedSession`
- `FileCursor`
- `HistoryCursor`
- `StatusLine`
- error surface без panic-oriented flow

### Phase 3: Layout

Начальный layout:

- top bar: daemon status / socket / provider
- left pane: workspaces + sessions
- center pane: files
- right pane: history/context
- bottom bar: hints / errors / current action

### Phase 4: Interaction

- `q` quit
- `Tab` cycle panes
- arrows / `j`,`k` move
- `Enter` open directory or preview file
- `w` bootstrap workspace from current root
- `n` create local tmux session
- `x` remove selected session
- `h` toggle history/context scope between workspace and selected session
- `g` jump to workspace root
- `b` cycle roots/bookmarks
- `o` open top file-search hit
- `f` file search from current directory/root
- `c` clear transient search results
- `/` context search
- `a` open agent prompt mode
- `r` refresh current pane

### Phase 5: Agent Flow

- prompt input overlay
- send `RunAgentTask`
- render output in dedicated result area
- do not implement streaming v1
- transcript remains daemon-owned via existing history writes

## Dependencies

Minimal TUI stack:

- `crossterm`
- `anyhow`
- `runtime-api`

Deliberately excluded in v1:

- async runtime in the TUI client
- background task frameworks
- state management libraries
- editor widgets
- plugin systems
- `ratatui` и иные layout/rendering frameworks

## Acceptance For First TUI Milestone

TUI milestone is accepted when:

- `cargo check --workspace --all-targets` passes
- `cargo test --workspace` passes
- `cargo clippy --workspace --all-targets -- -D warnings` passes
- TUI starts on host
- TUI renders health + sessions + files from a real daemon
- TUI can seed a workspace through daemon API
- TUI can create and remove a local tmux session through daemon API
- file navigation stays inside selected workspace
- agent task submit roundtrip works against daemon boundary
- проверкой по коду подтверждено, что TUI не обращается напрямую к SQLite/history files
- проверкой по коду подтверждено, что TUI не вызывает `tmux`, `ssh`, `claw`
- все file operations проходят только через workspace-scoped daemon API

## Censor Targets

Цензор должен атаковать здесь:

- thin-client boundary violations
- duplicated core logic in TUI
- dependency bloat
- UI state that re-implements canonical state
- accidental bypass around workspace-scoped file API
- accidental direct subprocess execution from TUI
