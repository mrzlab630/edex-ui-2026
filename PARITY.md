# PARITY

## Mission

Фиксировать, что переносится из legacy `eDEX-UI`, что осознанно отбрасывается, и что переосмысляется в `rust-core`.

## Carry Forward

- workspace-first model
- terminal-first developer workflow
- cyberpunk / Hackers product thesis как будущая UX-направленность, но не как ядровая архитектура
- быстрый доступ к локальной машине
- локальные и удалённые рабочие сценарии
- файловая навигация как first-class capability
- сессионность как базовая часть продукта

## Reinterpret

- terminal backend:
  от legacy `node-pty + Electron renderer` к daemon-owned session model
- tabs/windows:
  от UI-first представления к session/workspace domain contracts
- filesystem panel:
  от renderer widget к `nav-engine + file-index + context-engine`
- update/runtime logic:
  от Electron main/renderer glue к long-lived Rust daemon services
- AI:
  от отсутствующего subsystem к core-tier `agent subsystem + context-engine + claw-bridge + policy`
- persistence:
  от ad-hoc app config/files к encrypted canonical store + recovery/export model
- remote workflows:
  от вспомогательных возможностей к core SSH/tmux substrate

## Do Not Carry Forward

- Electron-specific process model
- renderer-owned state
- inline UI event architecture
- browser/webview constraints as source of architecture
- legacy system monitor widgets as core requirement
- archived dependency ecosystem as compatibility target

## V1 Parity Gates

`rust-core v1` считается достигшим функционального паритета не тогда, когда он копирует весь legacy UI, а когда он доказуемо умеет:

1. держать единый canonical daemon state
2. создавать и восстанавливать локальные сессии
3. управлять SSH host profiles и tunnels
4. attach/detach tmux-backed local/remote sessions
5. хранить encrypted settings/history
6. выполнять базовую file navigation/query модель
7. отдавать AI-fast context через daemon-owned retrieval plane
8. импортировать и экспортировать encrypted state bundle

## Deliberate Non-Parity

Следующие области не являются целью parity для `rust-core`:

- точное повторение старого Electron UI
- повторение старой сетки виджетов
- повторение legacy visual implementation
- сохранение старых runtime hacks ради совместимости
