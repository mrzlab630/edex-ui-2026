# Rust Core Idea

## Mission

Построить Linux-first систему для максимально удобной работы на локальной и удаленной машине:

- session orchestration
- SSH и туннели
- tmux-сессионность
- navigation state и file retrieval
- AI и agent workflows
- зашифрованное хранение состояния и истории
- быстрый import/export/recovery

## Current Branch Scope

В ветке `rust-core` сейчас не разрабатывается UI.

Текущий фокус только на ядре:

- модель данных
- хранение состояния
- шифрование
- сессии
- SSH/tunnels
- tmux substrate
- AI/agent runtime integration
- import/export/recovery

Все решения ниже касаются только ядра.
Любые UI, окна, multi-monitor surfaces, browser adapters и plugin marketplace не входят в текущую линию реализации.

## Product Thesis

Ценность системы лежит не в технологии рендеринга окна, а в сочетании:

- `session orchestration`
- `remote operations`
- `filesystem navigation`
- `workspace model`
- `AI agent integration`
- `secure persistent memory`
- `fast access to machine resources`

Ядро должно быть единым.

## Core Principles

1. Headless first
Ядро не знает о конкретном UI.

2. Linux first
Первая версия проектируется только под Linux:
- `tmux`
- `ssh`
- `OpenSSH config`
- Unix sockets
- `/proc`
- `inotify`
- systemd user services where appropriate

3. Daemon first
Control plane по умолчанию локальный и daemon-first:

- Unix domain socket as primary transport
- canonical state lives in the daemon
- no UI cache becomes the source of truth

4. Local-first and remote-native
Локальная машина является first-class target, удалённые машины тоже являются first-class target, а не "дополнительной функцией".

5. AI is a subsystem, not a gimmick
AI не должен быть встроен как чат-сбоку. Он должен иметь доступ к:
- сессиям
- рабочему пространству
- истории команд
- файлам
- tool execution contracts
- policy/permission model

Но доступ должен быть не "через медленный обход всего подряд", а через быстрый локальный context plane:

- daemon-owned retrieval APIs
- indexed history
- indexed file metadata/content views
- session-aware context assembly
- policy-filtered context windows for agents

Быстрый доступ не означает сырой доступ.
Перед попаданием данных в AI-facing context plane ядро должно поддерживать:

- sensitivity labels
- secret-bearing transcript detection
- redaction/classification rules
- default-deny posture for raw sensitive terminal/chat history

6. Data locality first

Контекст должен собираться как можно ближе к месту жизни данных.

- local data indexed locally
- remote data should not be mirrored wholesale by default
- remote agent/context retrieval should prefer remote execution over central copying

7. Parity discipline
Старый eDEX-UI остаётся reference implementation для UX-паттернов и поведения, но не для архитектуры.

8. Secrets are references, not payloads

Ядро не хранит приватные ключи и чувствительные данные "как есть" в обычных конфигах.
Базовая модель должна опираться на:

- `ssh-agent`
- Secret Service / system keyring where appropriate
- secret references instead of raw secret blobs

9. Encrypted by default

Любые persisted данные ядра считаются чувствительными по умолчанию:

- настройки
- история терминалов
- история AI-чатов
- session metadata
- host profiles
- tunnel profiles
- bookmarks and workspace state

Они не должны сохраняться в plaintext на диске.

Это требование распространяется и на:

- derived indexes
- retrieval read models
- bootstrap/recovery metadata

## What The New System Must Do

### 1. AI Agent Integration via `claw-code`

Глубокая интеграция с `claw-code` означает:

- локальный agent runtime как сервисный слой
- tool execution contracts
- session-aware agent context
- workspace-aware prompts
- review/apply workflows
- возможность запускать agent tasks из любого UI
- очень быстрый доступ к релевантной истории, файлам и state без UI-mediated scraping

`claw-code` важен здесь как референс для:

- Rust-first runtime
- parity discipline
- CLI and session workflows
- roadmap-oriented migration

Практическое правило:

- AI агент не должен получать контекст через "прочитай вот эту папку ещё раз"
- ядро должно уметь быстро собирать agent context из уже известных state/history/file indexes
- remote agent context должен собираться без тотального копирования remote state в центр

WAR ROOM rule:

- `AI subsystem` belongs to the core platform of this product
- `claw-code` is the canonical first runtime/provider for that subsystem
- but `claw-code` must not become the owner of core domain contracts

То есть:

- `AgentTask`
- `AgentPolicy`
- `AgentContext`
- `AgentSession`

должны принадлежать ядру системы, а не копировать внутреннюю модель конкретного provider-а.

### 2. SSH Tunnels and Remote Control

Система должна поддерживать:

- хранение профилей хостов
- OpenSSH-compatible configs where practical
- хранение jump-host и tunnel recipes
- lifecycle управления туннелями
- health status
- быстрый reconnect
- durable remote sessions

Это не отдельный модуль "для админов". Это часть базовой модели продукта.

Правило:

- удалённый доступ и удалённый control plane не строятся на отдельном нешифрованном транспорте
- remote execution и transport идут через SSH-based paths
- локальный daemon control plane остаётся UDS-first

Первая практическая реализация должна использовать battle-tested substrate:

- `ssh`
- `ssh-agent`
- `tmux`

а не пытаться сразу переписать весь remote stack нативно внутри ядра.

### 3. Navigation Inspired by `ranger`

Из `ranger` стоит брать не код, а идеи:

- multi-column navigation
- preview-first file browsing
- keyboard-first control model
- shell-friendly movement
- быстрые операции по файлам без mouse-first UX

Новая система должна реализовать навигацию как domain concept:

- tree / columns / preview
- history and jumps
- bookmarks
- fuzzy switch
- remote filesystem navigation

### 4. tmux Session Model

`tmux` не нужно заменять в первой фазе. Его нужно использовать как battle-tested session substrate там, где это даёт выигрыш:

- attach/detach
- persistent remote/local sessions
- pane/window/session state
- reconnection after UI restart

Интеграция должна идти через control mode и явный session broker.

### 5. Fast Access to Local Machine Resources

Система должна иметь прямой доступ к:

- процессы
- сеть
- CPU/RAM/disk
- файловые индексы
- active sessions
- dev tools
- local services

Но через typed services и permissions, а не через хаотичный прямой доступ клиентов в систему.

### 6. Settings and History as First-Class Data

Ядро обязано хранить:

- настройки пользователя
- настройки workspace
- SSH profiles
- tunnel profiles
- terminal history
- AI chat history
- session restore metadata

История должна быть:

- шифруемой
- переносимой
- экспортируемой
- импортируемой
- пригодной для selective retention и selective wipe

И ещё:

- нормализованной
- индексируемой
- пригодной для быстрого retrieval для AI
- привязанной к workspace/session/host context

### 7. Import / Export / Recovery

Ядро должно поддерживать:

- быстрый экспорт состояния
- перенос между Linux-машинами
- импорт с проверкой формата и версии
- восстановление после сбоя
- rekey и credential rebinding после переноса

## Trust Boundary

Primary trust model:

- daemon is trusted
- agent runtimes are semi-trusted and policy-bound

Следствие:

- agent tools не получают host access без явной capability/policy
- SSH/tunnel/secret operations идут только через daemon services

## Recommended Architecture

### The Shape

Новая система должна иметь форму:

`headless Rust daemon + typed APIs + encrypted data plane`

### Core layers

1. `domain`
- workspaces
- sessions
- hosts
- tunnels
- files
- tasks
- agents

2. `runtime`
- async runtime
- job scheduler
- event bus
- state store
- permissions/policy

3. `integrations`
- tmux bridge
- ssh bridge
- claw bridge
- filesystem watch/index
- metrics/system info

4. `data plane`
- encrypted state store
- history store
- retrieval/context indexes
- secret references
- export/import packager

5. `api surfaces`
- local RPC over Unix domain socket
- daemon-owned control/query interfaces

## Storage Thesis

Лучший путь для ядра:

`canonical encrypted embedded database + encrypted derived indexes + encrypted file bundles`

То есть:

- основное рабочее состояние живёт во внутренней зашифрованной БД
- быстрые AI-facing read models и retrieval indexes строятся поверх canonical store и тоже шифруются
- импорт/экспорт/backup/перенос идут через зашифрованные файловые bundles

Почему не "только файлы":

- хуже транзакционность
- хуже миграции
- хуже индексация истории
- хуже целостность при множестве взаимосвязанных сущностей
- выше риск разъезда состояния

Почему не "только БД без файлового слоя":

- хуже переносимость
- хуже резервное копирование
- хуже быстрый import/export между машинами

Итог:

- `DB` как source of truth
- `derived encrypted indexes` как fast context plane for AI
- `files` как exchange format

## Encryption Strategy

Рекомендуемая схема:

1. master key не хранится рядом с данными
2. data encryption keys используются для конкретных stores/bundles
3. master key хранится максимально безопасно доступным на Linux способом

Приоритет хранения master key:

1. TPM2-backed sealed credential as primary Linux v1 path when supported
2. Secret Service / system keyring as fallback for user-session Linux deployments
3. passphrase-based unlock fallback for portable/headless import-export scenarios

Практическое следствие:

- encrypted DB at rest
- encrypted export bundles
- encrypted bootstrap and recovery descriptors
- no raw private keys in plain config
- no plaintext chat/terminal history on disk

## Why Not Keep Expanding Electron

Если продолжать feature growth на старой базе, мы:

- закрепим legacy behavior in the wrong layer
- увеличим объём будущей parity work
- усложним extraction of clean domain contracts
- отложим реальный переход к core-first system

Поэтому текущий Electron-проект должен стать:

- reference UX surface
- parity oracle
- source of useful patterns

Но не главным местом для нового продуктового роста.

## Why Rust

Rust здесь нужен не ради моды, а потому что он подходит для:

- long-lived local daemon
- PTY/SSH/session orchestration
- typed service boundaries
- encrypted storage and key lifecycle
- secure integration with strong Linux substrate
- secure capability model
- AI runtime integration

## What Is Explicitly Not The Goal

- не делать ещё один "просто терминал"
- не делать просто prettier file manager
- не делать AI chat поверх shell
- не строить UI раньше, чем стабилизируется ядро
- не строить plugin platform раньше, чем стабилизируется ядро
- не централизовать remote data ценой тотального копирования и риска утечки

## Initial Strategic Choice

### Chosen direction

Стартовать новый проект как:

`Rust core first`

### Near-term implementation strategy

Сначала строится ядро и один core vertical slice:

- encrypted state
- sessions
- ssh/tmux
- history/context retrieval
- import/export/recovery

## Source References

- `claw-code`: Rust workspace + parity discipline + agent harness
  https://github.com/ultraworkers/claw-code
- `ranger`: keyboard-first multi-column file navigation
  https://github.com/ranger/ranger
- `tmux` control mode
  https://github.com/tmux/tmux/wiki/Control-Mode
- `systemd-creds`: optional Linux deployment integration for credential encryption and TPM2-backed sealing
  https://www.freedesktop.org/software/systemd/man/latest/systemd-creds.html
- `SQLite FTS5`: full-text retrieval surface for derived local indexes
  https://www.sqlite.org/fts5.html
