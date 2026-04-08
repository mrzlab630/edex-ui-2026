## [2026-04-08] Project Baseline
- **Decision:** Завести `KNOWLEDGE.md` как основной файл долговременной памяти проекта и обновлять его по мере новых технических решений и подтвержденных наблюдений.
- **Reason:** Проект возобновляется после длительного перерыва, и нужна единая точка фиксации архитектурного состояния, рисков, решений и особенностей окружения.
- **Context:** Пользователь поставил задачу возобновить развитие `edex-ui-2026` и привести проект в актуальное состояние на 2026 год.

## [2026-04-08] Current Project State
- **Decision:** Зафиксировать проект как форк архивного `eDEX-UI` 2.2.8 с текущим чистым рабочим деревом и отдельным runtime-пакетом в `src/`.
- **Reason:** Это определяет стартовую точку модернизации и объясняет, почему далее потребуется аудит безопасности, зависимостей и Electron-архитектуры.
- **Context:** `origin` указывает на `git@github.com:mrzlab630/edex-ui-2026.git`; upstream-проект в документации помечен как архивный; рабочее дерево на момент старта пусто по `git status --short`.

## [2026-04-08] Runtime And Architecture Snapshot
- **Decision:** Считать текущую архитектуру legacy Electron-приложением: main process в `src/_boot.js`, renderer в `src/_renderer.js`, UI собирается императивно, terminal backend построен на `node-pty` и `ws`.
- **Reason:** Это ключевой архитектурный факт для планирования модернизации на 2026 год.
- **Context:** Root `package.json` запускает Electron и сборку; `src/package.json` содержит runtime-зависимости; приложение поднимает основной TTY и до четырех дополнительных вкладок.

## [2026-04-08] Environment Risk
- **Decision:** Зафиксировать текущее локальное окружение как потенциально несовместимое с исходным проектом без адаптаций: `Node v22.12.0`, `npm 11.12.0`.
- **Reason:** Проект завязан на старые Electron и native-модули, в частности `electron@^12.1.0` и `node-pty@0.10.1`, поэтому установка и сборка могут потребовать pinning, rebuild или downgrade toolchain.
- **Context:** Перед установкой зависимостей обнаружено значительное расхождение между современным host toolchain и возрастом проекта.

## [2026-04-08] Modernization Strategy Shift
- **Decision:** Базовая стратегия изменена: не тянуть старый системный toolchain под legacy-зависимости, а обновлять зависимости проекта до актуальных версий и адаптировать кодовую базу под новые API и требования безопасности.
- **Reason:** Пользователь явно выбрал путь модернизации проекта на 2026 год вместо поддержки устаревшего окружения ради обратной совместимости.
- **Context:** Попытка установить `src`-зависимости показала несовместимость старых `node-pty` и `node-gyp` с современными `Node` и `Python`; после этого принято решение двигаться через обновление пакетов и переписывание кода.

## [2026-04-08] Dependency Uplift Wave 1
- **Decision:** Обновить root- и `src`-зависимости до актуальных версий из npm registry, включая переход с `electron-rebuild` на `@electron/rebuild` и со старых `xterm*` пакетов на scoped `@xterm/*`.
- **Reason:** Это снимает основную блокировку по legacy native toolchain и переводит проект на текущий набор пакетов, с которым уже можно чинить код по фактическим несовместимостям.
- **Context:** После правки `package.json` и `src/package.json` команды `npm install` в корне и в `src/` успешно завершились на текущем локальном `Node v22.12.0`.

## [2026-04-08] Code Migration Wave 1
- **Decision:** Выполнить первый проход адаптации кода под новый стек: убрать обращения к `electron.remote`, перевести terminal imports на `@xterm/*`, заменить устаревший глобальный `pdfjsLib` на lazy `import()` из `pdfjs-dist/legacy/build/pdf.mjs`, а обработку открытия новых окон перевести на `setWindowOpenHandler`.
- **Reason:** Эти места являлись прямыми точками несовместимости после обновления зависимостей и должны были быть исправлены до следующего runtime smoke test.
- **Context:** Локальные проверки подтвердили корректное разрешение `@xterm/xterm`, core addons, ligatures addon через прямой `.mjs` import и нового PDF.js entry point; полноценный headless smoke run пока не выполнен, потому что в окружении отсутствует `xvfb-run`.

## [2026-04-08] WAR ROOM Findings
- **Decision:** Признать три главных фронта модернизации: `Electron bridge/security`, `terminal lifecycle`, `secondary modules with legacy inline actions`.
- **Reason:** Разведка легионеров и внешняя сверка с актуальными docs показали, что именно эти три зоны дают максимум несовместимостей с паттернами апреля 2026.
- **Context:** Electron требует `preload + contextBridge + main-owned privileged APIs`; terminal stack страдает от custom lifecycle и tight coupling; secondary modules держатся на inline DOM actions, global renderer state и устаревших interaction patterns.

## [2026-04-08] Bridge Migration Wave
- **Decision:** Убрать `@electron/remote` из runtime-кода и заменить его preload bridge на [src/preload.js](/home/mrz/projects/edex-ui-2026/src/preload.js) с IPC-хендлерами в main process.
- **Reason:** Это самый крупный архитектурный шаг к современному Electron-коду без поддержки legacy `remote`-модели.
- **Context:** В `src/_boot.js` добавлены bootstrap/bridge IPC handlers, BrowserWindow переведён на `preload` и `contextIsolation: true`; renderer, terminal, update checker и netstat переведены на `window.edex.*`; затем `@electron/remote` был удалён из `src/package.json`, а `npm install` в `src/` завершился успешно.

## [2026-04-08] Interaction Model Cleanup
- **Decision:** Начать поэтапное удаление inline `onclick` и action-string паттернов, начиная с modal engine, tabs, fuzzy finder и update flow.
- **Reason:** Эти паттерны нехарактерны для кода 2026 и мешают дальнейшему ужесточению security profile renderer-а.
- **Context:** `Modal` теперь поддерживает function actions без inline `onclick`; settings/shortcuts/fuzzy finder/update checker частично переведены на callbacks и bridge actions. Крупный оставшийся legacy-узел — inline actions в `src/classes/filesystem.class.js`.

## [2026-04-08] Filesystem Interaction Refactor
- **Decision:** Перевести файловую сетку в `src/classes/filesystem.class.js` с inline command-string/`onclick` модели на delegated click handling и явные методы действий.
- **Reason:** `filesystem` был последним крупным источником inline action scripting в ключевом runtime-пути приложения.
- **Context:** Добавлены `_activeEntries`, delegated click handling, явный `handleEntryClick`, click-логика для `fs_space_bar`, а `openFile`/`openMedia` расширены для работы с entry objects. После правки grep по ключевым файлам больше не находит inline `onclick`.

## [2026-04-08] War Room Delegation
- **Decision:** Развернуть WAR ROOM с независимыми легионами по фронтам: Electron shell/security, terminal stack и library/build modernization.
- **Reason:** Пользователь потребовал параллельный разбор кодовой базы и сверку с актуальными паттернами, пакетами, библиотеками и SDK на апрель 2026 года через глобальный поиск, GitHub и официальные источники.
- **Context:** `Contex7` как MCP/ресурс в текущей сессии недоступен, поэтому сравнение с актуальными решениями делегировано легионерам через внешние исследовательские сессии с опорой на официальную документацию и GitHub search.

## [2026-04-08] WAR ROOM Research Constraints
- **Decision:** Проводить внешнюю сверку актуальных решений через официальные docs, npm registry и GitHub search; `Contex7` в текущей сессии недоступен.
- **Reason:** Пользователь требует ориентироваться на состояние библиотек и SDK на апрель 2026 года, а встроенного ресурса `Contex7` в этой среде нет.
- **Context:** В сессии отсутствуют MCP resources/templates; внешняя разведка уже ведется через Electron docs, npm package pages и GitHub-поиск.

## [2026-04-08] Confirmed Modernization Patterns
- **Decision:** Принять как целевые паттерны: `preload + contextBridge + ipcMain.handle/ipcRenderer.invoke` вместо прямого `remote`, scoped `@xterm/*` пакеты вместо deprecated `xterm*`, и ESM/legacy entry points `pdfjs-dist` вместо старого глобального script include.
- **Reason:** Эти паттерны подтверждены актуальной официальной документацией и напрямую соотносятся с уязвимыми и устаревшими участками текущей кодовой базы.
- **Context:** Electron docs рекомендуют context isolation и preload bridge; breaking changes явно фиксируют удаление `remote` и `new-window`; npm pages xterm помечают старые пакеты deprecated; PDF.js getting started описывает `pdf.mjs` и `pdf.worker.mjs` как актуальный prebuilt layout.

## [2026-04-08] Build Verification Snapshot
- **Decision:** Зафиксировать, что packaging на текущем host частично подтвержден: Linux artifacts для `x64`, `arm64` и `armv7l` собираются, а `ia32` ломается на системных multilib headers.
- **Reason:** Это отделяет реальные кодовые проблемы проекта от ограничений хоста сборки.
- **Context:** `npm run build-linux` создал `dist/eDEX-UI-Linux-x86_64.AppImage`, `dist/eDEX-UI-Linux-arm64.AppImage`, `dist/eDEX-UI-Linux-armv7l.AppImage` и соответствующие unpacked directories; `ia32` rebuild `node-pty` упал на `bits/libc-header-start.h: No such file or directory`.

## [2026-04-08] Runtime Compatibility Mode
- **Decision:** Временно вернуть BrowserWindow в совместимый режим `contextIsolation: false`, сохранив preload bridge и IPC-контракты.
- **Reason:** Текущий renderer по-прежнему целиком опирается на CommonJS globals (`require`, `__dirname`, script-loaded classes), и прямой переход на `contextIsolation: true` ломает приложение до полноценной модульной миграции.
- **Context:** Реальный smoke test под Electron 41 показал последовательные падения на отсутствии `module`, `global` и `require` в renderer page context; после этого preload получил fallback `window.edex = edexApi` для non-isolated режима.

## [2026-04-08] Smoke Test Success
- **Decision:** Считать source-runtime smoke test на Electron 41 подтвержденным.
- **Reason:** После серии совместимых адаптаций приложение дошло до рабочего состояния main+renderer+TTY без новых критических падений в течение дополнительного наблюдения.
- **Context:** Команда `./node_modules/.bin/electron src --nointro --no-sandbox --enable-logging=stderr` показала `Connected to frontend!`, `Startup Timer run for: 4.84s`, `Resized TTY to 157 034`; исправлены реальные runtime-разрывы по `shell-env`, `module.exports` в script-loaded классах, `global`/`require` bootstrap, а также ESM default export shifts в `color` и `pretty-bytes`.

## [2026-04-08] Remaining Runtime Debt
- **Decision:** Зафиксировать следующие оставшиеся неблокирующие runtime-долги после успешного запуска.
- **Reason:** Они не мешают старту приложения, но важны для следующей волны приведения к состоянию апреля 2026.
- **Context:** Во время успешного smoke test остался warning `Ligatures addon disabled: You must set the allowProposedApi option to true to use proposed API`; кроме того, полная миграция renderer-а на безопасный isolated/preload-only профиль всё ещё впереди.
