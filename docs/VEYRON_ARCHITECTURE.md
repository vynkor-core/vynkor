# Veyron — Архитектура ядра (Rust)

> Минимальное ядро, которое корректно работает с изолированными плагинами через UDS.

---

## Структура папок

```
veyron-kernel/
│
├── Cargo.toml
├── build.rs                    # генерация кода из .proto
├── proto/
│   └── veyron_protocol.proto   # единственный источник истины протокола
│
├── src/
│   ├── main.rs                 # точка входа, инициализация и запуск
│   │
│   ├── kernel/
│   │   ├── mod.rs              # публичный API ядра
│   │   ├── lifecycle.rs        # старт, стоп, graceful shutdown
│   │   └── state.rs            # глобальное состояние ядра (Arc<KernelState>)
│   │
│   ├── transport/
│   │   ├── mod.rs
│   │   ├── server.rs           # UDS сервер: принимает подключения плагинов
│   │   ├── connection.rs       # одно соединение с плагином (read/write loop)
│   │   └── framing.rs          # длина-фрейм поверх UDS (4 байта len + payload)
│   │
│   ├── registry/
│   │   ├── mod.rs
│   │   ├── plugin_registry.rs  # реестр зарегистрированных плагинов
│   │   └── manifest.rs         # парсинг и валидация PluginManifest
│   │
│   ├── permissions/
│   │   ├── mod.rs
│   │   └── checker.rs          # проверка: есть ли у плагина право на действие
│   │
│   ├── dispatcher/
│   │   ├── mod.rs
│   │   ├── action_dispatcher.rs # роутинг ActionRequest к нужному хендлеру
│   │   └── command_sender.rs    # отправка KernelCommand в плагин
│   │
│   ├── event_bus/
│   │   ├── mod.rs
│   │   ├── bus.rs              # pub/sub шина событий
│   │   ├── subscriptions.rs    # таблица подписок плагин → event_types
│   │   └── store.rs            # персистентный EventStore (SQLite) для retry
│   │
│   ├── ai/
│   │   ├── mod.rs
│   │   ├── provider.rs         # trait AiProvider
│   │   ├── openrouter.rs       # реализация для OpenRouter
│   │   ├── ollama.rs           # реализация для Ollama (локальный)
│   │   ├── cache.rs            # кеш ответов AI (LRU)
│   │   └── semaphore.rs        # GPU семафор — ограничение одновременных запросов
│   │
│   ├── scheduler/
│   │   ├── mod.rs
│   │   └── scheduler.rs        # таймеры и аларм (для плагинов alarm и timer)
│   │
│   ├── health/
│   │   ├── mod.rs
│   │   └── monitor.rs          # ping/pong, watchdog, детект зависших плагинов
│   │
│   ├── config/
│   │   ├── mod.rs
│   │   └── settings.rs         # конфиг ядра из файла + env переменных
│   │
│   └── error.rs                # единый тип ошибок ядра
│
├── tests/
│   ├── integration/
│   │   ├── test_plugin_lifecycle.rs
│   │   ├── test_permissions.rs
│   │   └── test_event_delivery.rs
│   └── fixtures/
│       └── mock_plugin.rs      # заглушка плагина для тестов
│
└── configs/
    └── kernel.toml             # дефолтный конфиг
```

---

## Компоненты ядра — подробно

### 1. `transport/` — Транспортный слой

**Что делает:** принимает подключения от плагинов, читает и пишет сообщения.

**Ключевая деталь — framing.** UDS это поток байт, не пакеты. Нужно добавить
границы сообщений. Самый простой способ — length-prefix framing:

```
[ 4 байта big-endian длина ][ N байт protobuf payload ]
[ 4 байта big-endian длина ][ N байт protobuf payload ]
...
```

`server.rs` слушает UDS сокет (`/tmp/veyron.sock`), при подключении спавнит
Tokio task для каждого плагина. `connection.rs` содержит read loop и write half.
Каждое соединение живёт в своём task, ядро не блокируется на одном плагине.

---

### 2. `registry/` — Реестр плагинов

**Что делает:** хранит информацию о всех живых плагинах.

При подключении плагин обязан прислать `PluginRegister` как первое сообщение.
Реестр проверяет: нет ли уже плагина с таким `plugin_id`, корректен ли манифест.
Если всё ок — плагин получает `PluginRegisterAck` с `accepted = true` и списком
`granted_permissions`. Реестр хранит `HashMap<plugin_id, PluginEntry>` где
`PluginEntry` содержит канал для отправки сообщений плагину.

Без успешной регистрации ядро игнорирует все последующие сообщения от плагина.

---

### 3. `permissions/` — Проверка разрешений

**Что делает:** перед каждым `ActionRequest` проверяет, есть ли у плагина право.

Логика простая — при регистрации в реестр записывается `HashSet<PermissionType>`
для каждого плагина. `checker.rs` содержит одну функцию:

```
fn check(plugin_id, required_permission) -> Result<(), PermissionDenied>
```

Если плагин не объявил `PERMISSION_NETWORK` в манифесте, а пытается сделать
`ActionRequest { action: "http_get" }` — ядро возвращает `ACTION_PERMISSION_DENY`
и не выполняет действие. Плагин не может обойти это — он физически не имеет
прямого доступа к сети ядра, только через UDS.

---

### 4. `dispatcher/` — Диспетчер действий

**Что делает:** роутит `ActionRequest` от плагина к нужному хендлеру внутри ядра.

`action_dispatcher.rs` — это match по полю `action`:

```
"http_get"       → network::http_get(params)
"read_file"      → filesystem::read_file(params)
"write_file"     → filesystem::write_file(params)
"get_metrics"    → system::get_metrics(params)
"schedule_timer" → scheduler::create_timer(params)
"send_notify"    → notifications::send(params)
```

Каждый хендлер — async функция, возвращает `ActionResponse`. Диспетчер
автоматически оборачивает результат и отправляет обратно плагину через его
соединение.

`command_sender.rs` — обратная сторона: ядро само инициирует команду плагину
(напр. `reload_config`, `health_check`). Находит плагин в реестре по `plugin_id`,
шлёт через его канал.

---

### 5. `event_bus/` — Шина событий

**Что делает:** pub/sub между ядром и плагинами.

`bus.rs` хранит таблицу подписок: `HashMap<event_type, Vec<plugin_id>>`.
Когда плагин шлёт `Subscribe { event_types: ["stt.result", "alarm.fired"] }` —
его `plugin_id` добавляется в соответствующие списки.

При публикации события ядро находит всех подписчиков и шлёт каждому `Event`.
Плагин должен ответить `EventAck` — иначе событие помечается как недоставленное.

`store.rs` — SQLite таблица для retry. Если плагин не ответил `EventAck` за
отведённое время — событие помечается `pending`, retry worker переотправляет.
Это даёт at-least-once гарантию доставки.

---

### 6. `ai/` — AI оркестрация

**Что делает:** единая точка входа для всех AI запросов от плагинов.

Плагин шлёт `AiRequest`, ядро:
1. Проверяет `PERMISSION_AI` в манифесте
2. Проверяет кеш — если такой промпт уже есть, отдаёт кешированный ответ
3. Берёт слот GPU семафора (ограничение одновременных запросов)
4. Шлёт запрос провайдеру (OpenRouter / Ollama)
5. При `stream = true` — стримит `AiStreamChunk` обратно плагину
6. Освобождает GPU слот

`provider.rs` — trait `AiProvider` с методом `complete(messages, params) -> AiResponse`.
Добавить нового провайдера = реализовать этот trait. Ядро не знает о конкретных
провайдерах, только о трейте.

---

### 7. `scheduler/` — Планировщик

**Что делает:** управляет таймерами и алармами от плагинов `alarm` и `timer`.

Плагин шлёт `ActionRequest { action: "schedule_timer", params: { delay_ms: 5000 } }`.
Ядро создаёт Tokio `sleep` задачу. Когда время выходит — публикует в EventBus:
`Event { event_type: "timer.fired", payload: { timer_id: "..." } }`.

Плагин подписан на `timer.fired` и получает событие. Аларм работает аналогично
но с привязкой к времени суток через `datetime`.

---

### 8. `health/` — Мониторинг здоровья

**Что делает:** следит за живостью плагинов.

`monitor.rs` периодически (каждые N секунд, конфигурируемо) шлёт `Ping` всем
зарегистрированным плагинам. Если плагин не ответил `Pong` за таймаут —
считается умершим. Ядро:
1. Удаляет плагин из реестра
2. Публикует `Event { event_type: "system.plugin_died", payload: { plugin_id } }`
3. Закрывает соединение

Watchdog не пытается перезапустить плагин — это задача внешнего процесс-менеджера
(systemd, launchd). Ядро только фиксирует факт смерти.

---

### 9. `config/` — Конфигурация

**Что делает:** загружает конфиг ядра при старте.

`kernel.toml` содержит:
- путь к UDS сокету
- разрешённые плагины и их лимиты
- настройки AI провайдера и API ключи
- таймауты, лимиты
- путь к SQLite для EventStore

Env переменные имеют приоритет над файлом. Конфиг читается один раз при
старте и кладётся в `Arc<Settings>` для шаринга между компонентами.

---

## Поток данных — полный цикл

```
Плагин "weather"                     Ядро (Rust)
────────────────────────────────────────────────────────

1. connect() ────────────────────────► UDS accept()
                                        │
2. PluginRegister ───────────────────► registry::register()
   {plugin_id: "weather",               │ проверить манифест
    permissions: [NETWORK]}             │ записать permissions
                                        │
   PluginRegisterAck ◄─────────────── granted: [NETWORK]
   {accepted: true}                     │
                                        │
3. Subscribe ────────────────────────► event_bus::subscribe()
   {event_types: ["system.ready"]}      │
                                        │
4. — kernel публикует событие —        event_bus::publish("system.ready")
                                        │
   Event ◄──────────────────────────── найти подписчиков → "weather"
   {event_type: "system.ready"}         │
                                        │
5. EventAck ─────────────────────────► store::mark_delivered()
                                        │
6. ActionRequest ────────────────────► permissions::check(NETWORK) ✓
   {action: "http_get",                 │
    params: {url: "api.weather..."}}    dispatcher::handle("http_get")
                                        │
   ActionResponse ◄──────────────────── {status: OK, data: {...}}
```

---

## Минимально для работы

Это компоненты без которых ядро не запустится корректно:

| Компонент      | Без него                                      |
|----------------|-----------------------------------------------|
| transport      | плагины не могут подключиться                 |
| registry       | нет контроля кто подключён                   |
| permissions    | плагины делают что хотят                     |
| dispatcher     | ActionRequest некуда роутить                 |
| event_bus      | плагины не получают события                  |
| health/monitor | ядро не знает о мёртвых плагинах             |
| config         | ядро не знает где сокет и какие провайдеры   |

Всё остальное (`ai/`, `scheduler/`) — подключается позже, по мере нужды.

---

## Зависимости Cargo (минимум)

```toml
[dependencies]
tokio       = { version = "1", features = ["full"] }
prost       = "0.12"          # protobuf
prost-types = "0.12"
bytes       = "1"
serde       = { version = "1", features = ["derive"] }
serde_json  = "1"
rusqlite    = { version = "0.31", features = ["bundled"] }  # EventStore
toml        = "0.8"           # конфиг
tracing     = "0.1"           # логирование
tracing-subscriber = "0.3"
thiserror   = "1"             # удобные ошибки
uuid        = { version = "1", features = ["v4"] }
dashmap     = "5"             # concurrent HashMap для реестра
tokio-util  = "0.7"           # framed codec для length-prefix

[build-dependencies]
prost-build = "0.12"          # кодогенерация из .proto
```

---

## Первые шаги

1. `proto/veyron_protocol.proto` → уже есть, это контракт
2. `build.rs` → настроить `prost-build` для генерации Rust кода из proto
3. `transport/framing.rs` + `transport/server.rs` → принять первое подключение
4. `registry/plugin_registry.rs` → зарегистрировать тестовый плагин
5. Написать `tests/fixtures/mock_plugin.rs` → заглушка плагина для тестов
6. `permissions/checker.rs` → проверка манифеста
7. `dispatcher/action_dispatcher.rs` → первые два действия (ping, http_get)
8. `event_bus/bus.rs` → pub/sub
9. `health/monitor.rs` → watchdog

Итого: рабочее ядро за ~2–3 недели при фокусе на одном разработчике.
