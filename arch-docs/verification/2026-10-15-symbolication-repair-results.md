# Результаты исправления символикации

Дата: 15 октября 2026 года. Проверено локальное рабочее дерево Metric 0.1.8.

Native Breakpad, ELF/DWARF и настоящий Windows PDB, JavaScript и Node source maps проходят настоящую обработку Symbolicator 26.6.0 через приватные источники Metric. PDB source bundles и JS maps дают три восстановленных вызова с исходным кодом вокруг каждого кадра. Результаты проверены после финализации через API и на страницах событий в Chromium. Список SDK не расширен. Исходная реализация сохранена коммитом `3f77c19` перед дополнением source context.

## Исправления

- Адаптер отправляет native `stacktraces`, числовой `image_size`, JS-модули `sourcemap` и полный объект отключённого scraping; разбирает `status=completed`.
- JS-кадр считается преобразованным только при `data.symbolicated=true`; native использует явный статус кадра. Исходное имя функции само по себе не означает успешную символикацию.
- Native индексы из внутреннего callee-first порядка Symbolicator 26.6.0 переведены в индексы исходных caller-first кадров. Сохранены inline-кадры с общим исходным индексом и полный путь исходного файла.
- Область кэша включает проект и revisions debug-файлов/artifact bundles. Native missing Debug IDs извлекаются из фактического ответа modules.
- JS artifact lookup возвращает `type=bundle`, ID и URL скачивания, построенный из доверенной конфигурации callback; URL не зависит от Host из запроса.
- MongoDB processing project теперь выбирает `dr` и `ar`: без этих полей processor всегда видел нулевые revisions.
- UI показывает derived-кадры с подходящим origin и исходным индексом, сообщает о полном/частичном результате и позволяет переключиться на исходные кадры. Исходный payload не изменяется; контекст минифицированного файла не показывается как контекст восстановленной строки.
- Сохраняются `pre_context`, `context_line`, `post_context` из native/JS ответов в существующем JSON. Пустая текущая строка сохраняется и выделяется. Стандартное окно Symbolicator — до пяти строк с каждой стороны.
- Добавлен native `sourcebundle` (тип 6), capability `sources`, распознавание заголовка SYSB и поддержка обоих storage adapters. Миграция PostgreSQL 0022 расширяет CHECK; отдельный тест обновления с generation 21 подтверждает сохранность прежних debug-файлов и идемпотентность.
- Контекст ограничен 128 KiB на результат, 20 строками с каждой стороны и 4096 байтами на строку. При превышении лимитов отбрасывается только контекст, координаты остаются. Finalizer также убирает новые snippets перед отклонением события по размеру.

Первоначальная сквозная проверка на неизменённом бинарнике воспроизвела `native/malformed` после успешной загрузки файла. Её артефакты остались в `target/symbolication-tests/9d139163fc2d40799776210385079cb6`.

## Проверки

| Набор | Результат |
| --- | --- |
| PostgreSQL, полный локальный прогон с SDK, UI и управляемым HTTP 503 | 35 проверок пройдено |
| MongoDB, реальные контракты с native/JS UI и HTTP 503 | 24 проверки пройдены |
| Настоящий Windows PDB, source bundle и несколько вызовов | Проверены CLI upload, повторный upload с `--include-sources`, точные snippets, warm cache и изоляция проектов |
| Девять существующих SDK | Приём и чтение событий пройдены; входят в полный PostgreSQL-прогон |
| Настоящие Node/Browser SDK из минифицированных сборок | Восстановлены три функции, исходный код и ошибка на `source-map.js:10:9` |
| `metric-symbolication` | 8 unit/transport тестов пройдено |
| `metric-application --lib` | 143 теста пройдено, 11 существующих специальных тестов ignored |
| `metric-domain --lib` | 58 тестов пройдено, 5 существующих специальных тестов ignored |
| PostgreSQL migration 21 → 22 и composed runtime | Оба отдельных интеграционных теста пройдены |
| MongoDB processing project revisions | Отдельный реальный регрессионный тест пройден |
| Существующий `debug_files_e2e` с реальными CLI uploads | Пройден |
| Web UI | 120 тестов пройдено, production build и lint пройдены |
| Clippy изменённых Rust crates, все targets/features | Пройден с `-D warnings` |
| Rust fmt, release dependency checks, validation документации и SDK-матрицы | Пройдены |

Полный Web `format:check` ранее сообщил о 13 неизменённых файлах; SDK Browser/Node `format:check` сообщает об их неизменённых `package-lock.json`. Форматирование всех изменённых Web и SDK файлов проверено отдельно и проходит. Эти посторонние файлы не переформатировались.

Главный отчёт полного прогона: `target/symbolication-tests/751a2c01debd4b3e8ccab50422d22b67/report.json`.
Итоговый MongoDB отчёт с UI и отказами: `target/symbolication-tests/cb1dc6b084b546e4bc8357b4be2d2701/report.json`.
PostgreSQL отчёт с UI: `target/symbolication-tests/a82b621d34b04bf2a5316bf41205be43/report.json`.

В отчётах сохранены результаты случаев, event IDs, настоящие HTTP-запросы/ответы и наблюдения приватных callback. DSN и service tokens отредактированы. Все успешные symbolication-ответы получены от настоящего контейнера; управляемый HTTP 503 отдельно помечен `fault_injected`.

Symbolicator закреплён как `ghcr.io/getsentry/symbolicator:26.6.0@sha256:b66d1d5c796e056bf19f597bb7d1ac39604a767eac501dee3710b8844b12228f`. Проверки проходят с отключёнными scraping и `connect_to_reserved_ips`; production-настройки доступа к IP не расширялись.

У Node SDK результат `partial`: тестовая функция восстановлена, а внутренние/runtime кадры без соответствующих maps остаются исходными. Обычные JS smoke events без загруженных maps получают `missing`; это не объявляется успешным преобразованием. Готовые стеки остальных проверенных SDK сохраняются с `not_required`, когда преобразование не требуется.

## CI и релиз

В Checks добавлен обязательный job `symbolication`: настоящий закреплённый контейнер, PostgreSQL, CLI uploads, Breakpad, ELF и реальный cross-compiled Windows PDB с source bundle, точные native/JS функции, порядок кадров и snippets. Job устанавливает LLVM (`clang`, `lld`, `llvm`). Нет `continue-on-error` или успешного пропуска при отсутствии инфраструктуры. Отчёты сохраняются как CI artifacts.

Проверка release workflow подтверждает зависимость всех публикаций от reusable Checks, включающего новый job. Конфигурация добавлена локально; запуск GitHub Actions и настройка branch rules в этой работе не выполнялись.

## Локальная демонстрация

После реализации оставлен работающий синтетический стенд на `http://localhost:4001`.
Учётная запись и инструкция остановки сохранены в `target/symbolication-tests/0ad8971f97e04c37843acd449a04a8d1/DEMO-RU.txt`. На оставленном стенде пройдены 24 проверки с UI и отказами.

- Native: `http://localhost:4001/events/68bcd1b2954341b78725aaa9bd1270f7` — `metric_pdb_fail(int)`, `metric_pdb_parse(int)`, `metric_pdb_handle(int)`; `native-pdb.c:10`, `:19`, `:28` с исходниками.
- JavaScript: `http://localhost:4001/events/230994f1f3b644f4857816b4a3a55f20` — `metricSourceMapFailure`, `metricSourceMapParse`, `metricSourceMapHandle`; исходный `source-map.js:10:9` и контекст всех трёх кадров.
- ELF/DWARF: `http://localhost:4001/events/1b9940d991b24f04b277068325e36053` — `metric_native_contract`, `metric-symbolication.c:42`.

Снимки проверенных страниц: `target/symbolication-tests/0ad8971f97e04c37843acd449a04a8d1/native-event.png` и `javascript-event.png`.

Команда воспроизведения при подготовленных зависимостях:

```text
python scripts/test-symbolication.py --sdk --ui --faults --keep --port 4001
```

Управление инфраструктурой и подготовка зависимостей описаны в [SDK tests README](../../sdk-tests/README.md#real-symbolication-contracts-and-local-demo).
Настоящие крэши/minidump processing, Portable PDB и Mach-O не входят в новые native-сценарии. PDB создаётся настоящим компилятором и линкером LLVM; тестовые native адреса выбираются из его line records, сам бинарник с делением на ноль не запускается. JavaScript-адреса получаются из реально выполненного минифицированного стека. Исторические события удалённого стенда не переобрабатывались.
