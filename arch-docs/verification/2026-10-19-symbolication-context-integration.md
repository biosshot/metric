# Интеграция source context после PR #13–#20

Дата: 19 октября 2026 года.

Основа — `main` на `89b3df2`, содержащий все семь принятых PR #13, #14,
#16–#20. Дополнения из локальных `3f77c19` и `176d4e1` перенесены в
`codex/symbolication-context-integration`. Исходная ветка
`codex/symbolication-source-context` сохранена. Issue #15 и автоматическая
retention Artifact Bundles в эту работу не входят.

## Итоговое поведение

- Сохранены авторские исправления HTTPS proxy, getsentry/cli bundles, chunk
  permissions, CLI 2.x releases и Cocoa message object.
- JS использует обработку пропущенных кадров и группировку из #13. Ответы
  принимаются только со статусом `completed`; в derived попадают только кадры
  с `data.symbolicated=true`. Статус отражает реальные преобразования, а не
  количество отправленных кадров. Контекст сохраняет исходные индексы даже
  при разбиении стека на несколько запросных stacktraces.
- Artifact lookup оставлен из #17: каждый кандидат получает собственный
  `resolved_with`; callback/download URL строится из конфигурации.
- Сохранены native wire contract, порядок и inline-кадры, project/revision cache
  scope и чтение `dr`/`ar` при processing в MongoDB.
- Native `sourcebundle` поддерживается обоими backend; миграция PostgreSQL 0022
  разрешает тип 6 без изменения существующих debug-файлов.
- `pre_context`, `context_line`, `post_context` сохраняются после symbolication
  и финализации. Контекст ограничен по объёму; превышение его лимитов не должно
  уничтожать пригодные координаты и символы.
- Vue показывает восстановленные вызовы, координаты, исходный код и подсветку
  текущей строки. Переключение на исходные кадры восстанавливает исходный стек.

## Выполненные проверки

| Проверка | Результат |
| --- | --- |
| `cargo test --workspace --all-targets --all-features --locked -j1` | 395 passed, 0 failed, 275 ignored; 48 наборов |
| `cargo clippy --workspace --all-targets --all-features --locked -j1 -- -D warnings` | Пройдено |
| `cargo fmt --all -- --check` и `git diff --check` | Пройдено |
| PostgreSQL migration 21 → 22 | Существующий PDB сохранён; повторный запуск идемпотентен; тип 6 разрешён, тип 7 отклонён |
| MongoDB processing project revisions | Реальная проверка `dr` и `ar` пройдена |
| Настоящий Symbolicator, PostgreSQL, `--sdk --ui --faults` | 37 сценариев пройдено |
| Настоящий Symbolicator, MongoDB, `--ui --faults` | 26 сценариев пройдено |
| Web lint, unit tests, production build | Пройдено; 120 тестов |
| Web E2E, Chromium и Firefox | 40 passed, 2 штатных screenshot-only сценария skipped |
| Prettier изменённых Vue/TypeScript файлов | Пройдено |
| CLI versions | 3.6.2, 2.58.6, getsentry/cli 0.44.1 |
| Реальные CLI debug-file/source-map uploads и releases | Оба MongoDB E2E пройдены; включают новые getsentry bundles, полный batch chunks, artifact-only token и CLI 2.x releases |
| Composed PostgreSQL runtime | Auth, ingest, scope и restart пройдены с generation 22 |
| Documentation, compatibility matrix, deployment profiles, release-workflow tests, ADR-0034 dependency graph | Пройдено |

Реальные прогоны проверяют Breakpad, ELF/DWARF и скомпилированный Windows PDB,
повторную CLI-загрузку с `--include-sources`, холодный/тёплый/negative cache,
revisions, изоляцию проектов, частичные стеки и private callback download.
Добавлены реальные случаи JS-стека с кадром без source location и стека без
единого отправляемого кадра. Первый сохраняет координаты, контекст и индексы
`0, 2, 3`; второй сохраняет raw stack и получает `missing`.

PostgreSQL-прогон включает настоящие Node/Browser source-map errors и девять
закреплённых SDK: Node, Browser, Python, Java, .NET, Go, Rust, Ruby, PHP.
UI-проверки обоих backend требуют три восстановленных вызова, пять строк до и
после первой ошибки, правильную подсветку и работающий original/resolved toggle.
Скриншоты PostgreSQL также просмотрены визуально.

## Артефакты и воспроизведение

Основные отчёты с реальными HTTP-контрактами и отредактированными credentials:

- PostgreSQL: `target/symbolication-tests/b8c32d55bcf04f46a60c806581645379/report.json`.
- MongoDB: `target/symbolication-tests/d7a5bac2065e4d6990353885b593404c/report.json`.
- В этих же каталогах сохранены `native-event.png` и `javascript-event.png`.
- Общий Rust-прогон: `target/context-integration-rust-tests.log`.
- Clippy: `target/context-integration-clippy.log`.
- Web E2E: `target/context-integration-web-e2e.log`.
- CLI E2E: `target/context-integration-cli-e2e.log`.
- PostgreSQL runtime: `target/context-integration-postgres-runtime.log`.

После подготовки зависимостей и тестовых баз по `sdk-tests/README.md`:

```text
cargo build -p metric-server --bin metric-server --locked -j1
python scripts/test-symbolication.py --backend postgres --sdk --ui --faults
python scripts/test-symbolication.py --backend mongodb --ui --faults
```

Использован Symbolicator
`ghcr.io/getsentry/symbolicator:26.6.0@sha256:b66d1d5c796e056bf19f597bb7d1ac39604a767eac501dee3710b8844b12228f`.
Scraping и `connect_to_reserved_ips` не включались. Контролируемый HTTP 503
отмечен в отчётах отдельно и проверяет raw fallback и последующее восстановление.

Первый общий Rust-прогон с обычным параллелизмом исчерпал Windows commit/pagefile
limit. Повторный полный прогон с `-j1` прошёл; код для обхода ошибки не менялся.
275 ignored — отдельные infrastructure/performance-наборы; они не объявляются
выполненными общим прогоном. Целевые реальные проверки перечислены выше отдельно.
Portable PDB, Mach-O, настоящие крэши/minidump processing и нагрузочные измерения
этой интеграцией не проверялись. Список поддерживаемых SDK не расширялся.
