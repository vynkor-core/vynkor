# Veyron & Кайро — Дерево папок

---

## Стратегия репозиториев

```
Вариант A — Монорепо (рекомендуется для 2 разработчиков):

  veyron/
  ├── kernel/         ← Rust
  ├── cairo/          ← Rust или Python
  ├── plugins/        ← все плагины
  ├── sdk/            ← три SDK
  └── proto/          ← единственный источник истины

Вариант B — Отдельные репо:
  veyron-kernel/
  cairo/
  veyron-plugins/
  veyron-sdk/

Рекомендация: Монорепо. На старте синхронизация важнее независимости.
```

---

## Полное дерево (Монорепо)

```
veyron/
│
│  # Общий контракт — единственный источник истины протокола
├── proto/
│   └── veyron_protocol.proto       # все типы сообщений
│
│  # SDK для разработчиков плагинов
├── sdk/
│   │
│   ├── rust/                       # veyron-sdk-rs (crate)
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── client.rs           # подключение к ядру
│   │       ├── framing.rs          # упаковка/распаковка бинарного фрейма
│   │       └── plugin.rs           # трейт Plugin для реализации
│   │
│   ├── cpp/                        # veyron-sdk-cpp (для друга)
│   │   ├── CMakeLists.txt
│   │   ├── include/
│   │   │   └── veyron/
│   │   │       ├── client.hpp
│   │   │       ├── framing.hpp
│   │   │       └── plugin.hpp
│   │   └── src/
│   │       ├── client.cpp
│   │       └── framing.cpp
│   │
│   └── python/                     # veyron-sdk-py (для ML плагинов)
│       ├── pyproject.toml
│       └── veyron/
│           ├── __init__.py
│           ├── client.py           # asyncio UDS клиент
│           ├── framing.py
│           └── plugin.py           # базовый класс Plugin
│
│  # Ядро Veyron
├── kernel/
│   ├── Cargo.toml
│   ├── Cargo.lock
│   ├── build.rs                    # prost-build: генерация Rust из .proto
│   │
│   ├── src/
│   │   ├── main.rs                 # точка входа, сборка компонентов
│   │   │
│   │   ├── kernel/
│   │   │   ├── mod.rs
│   │   │   ├── lifecycle.rs        # start / stop / graceful_shutdown
│   │   │   └── state.rs            # Arc<KernelState> — шарится между компонентами
│   │   │
│   │   ├── transport/
│   │   │   ├── mod.rs
│   │   │   ├── server.rs           # UDS accept loop (/tmp/veyron.sock)
│   │   │   ├── connection.rs       # один плагин = один Connection (read + write half)
│   │   │   └── framing.rs          # Magic+Flags+Len+Target+CRC32 codec
│   │   │
│   │   ├── registry/
│   │   │   ├── mod.rs
│   │   │   ├── plugin_registry.rs  # DashMap<plugin_id, PluginEntry>
│   │   │   └── manifest.rs         # парсинг и валидация plugin.json
│   │   │
│   │   ├── permissions/
│   │   │   ├── mod.rs
│   │   │   └── checker.rs          # check(plugin_id, permission) -> Result
│   │   │
│   │   ├── dispatcher/
│   │   │   ├── mod.rs
│   │   │   ├── action_dispatcher.rs # match action → handler
│   │   │   ├── command_sender.rs    # kernel → plugin: KernelCommand
│   │   │   └── handlers/
│   │   │       ├── network.rs       # http_get, http_post
│   │   │       ├── filesystem.rs    # read_file, write_file
│   │   │       ├── system.rs        # get_metrics, get_info
│   │   │       └── notify.rs        # send_notification
│   │   │
│   │   ├── event_bus/
│   │   │   ├── mod.rs
│   │   │   ├── bus.rs              # HashMap<event_type, Vec<plugin_id>>
│   │   │   ├── subscriptions.rs    # subscribe / unsubscribe
│   │   │   └── store.rs            # SQLite EventStore (at-least-once)
│   │   │
│   │   ├── process_manager/
│   │   │   ├── mod.rs
│   │   │   ├── manager.rs          # spawn / kill / restart плагинов
│   │   │   └── policy.rs           # RestartPolicy: always | on-failure | never
│   │   │
│   │   ├── health/
│   │   │   ├── mod.rs
│   │   │   └── watchdog.rs         # Ping loop, детект зависших плагинов
│   │   │
│   │   ├── rate_limiter/
│   │   │   ├── mod.rs
│   │   │   └── limiter.rs          # token bucket на плагин
│   │   │
│   │   ├── ai/
│   │   │   ├── mod.rs
│   │   │   ├── provider.rs         # trait AiProvider
│   │   │   ├── openrouter.rs
│   │   │   ├── ollama.rs
│   │   │   ├── cache.rs            # LRU кеш ответов
│   │   │   └── semaphore.rs        # GPU семафор
│   │   │
│   │   ├── scheduler/
│   │   │   ├── mod.rs
│   │   │   └── scheduler.rs        # таймеры и аларм
│   │   │
│   │   ├── gateway/                # WebSocket шлюз (внешние клиенты)
│   │   │   ├── mod.rs
│   │   │   ├── server.rs           # Axum WebSocket сервер
│   │   │   ├── auth.rs             # JWT валидация из query param
│   │   │   └── bridge.rs           # WebSocket ↔ UDS трансляция
│   │   │
│   │   ├── config/
│   │   │   ├── mod.rs
│   │   │   └── settings.rs         # kernel.toml + env vars
│   │   │
│   │   └── error.rs                # единый тип ошибок (thiserror)
│   │
│   ├── tests/
│   │   ├── integration/
│   │   │   ├── test_plugin_lifecycle.rs
│   │   │   ├── test_permissions.rs
│   │   │   ├── test_event_delivery.rs
│   │   │   ├── test_framing.rs
│   │   │   └── test_rate_limiter.rs
│   │   └── fixtures/
│   │       └── mock_plugin.rs      # заглушка плагина для тестов
│   │
│   └── configs/
│       └── kernel.toml             # дефолтный конфиг
│
│  # AI-агент Кайро (плагин поверх Veyron)
├── cairo/
│   ├── Cargo.toml                  # или pyproject.toml если Python
│   │
│   └── src/
│       ├── main.rs                 # точка входа, подключение к ядру
│       │
│       ├── agent/
│       │   ├── mod.rs
│       │   ├── router.rs           # Диспетчер: Intent → JSON-RPC вызов
│       │   └── worker.rs           # Собеседник: результат → ответ пользователю
│       │
│       ├── llm/
│       │   ├── mod.rs
│       │   ├── loader.rs           # динамическая загрузка/выгрузка из VRAM
│       │   ├── levels.rs           # Light / Medium / Heavy модели
│       │   └── streaming.rs        # стриминг токенов
│       │
│       ├── memory/
│       │   ├── mod.rs
│       │   ├── context.rs          # краткосрочный контекст диалога
│       │   └── rag.rs              # запросы к плагину knowledge (долгосрочная память)
│       │
│       ├── audio/
│       │   ├── mod.rs
│       │   ├── wake_word.rs        # детектор wake word
│       │   └── pipeline.rs         # wake → STT → LLM → TTS
│       │
│       └── tools/
│           ├── mod.rs              # реестр инструментов для Диспетчера
│           └── definitions.rs      # JSON-схемы инструментов
│
│  # Все плагины
├── plugins/
│   │
│   ├── ai/                         # Python (OpenRouter/Ollama API)
│   │   ├── plugin.json
│   │   ├── main.py
│   │   └── requirements.txt
│   │
│   ├── stt/                        # Python (Whisper)
│   │   ├── plugin.json
│   │   ├── main.py
│   │   └── requirements.txt
│   │
│   ├── tts/                        # Python (Piper / Coqui)
│   │   ├── plugin.json
│   │   ├── main.py
│   │   └── requirements.txt
│   │
│   ├── weather/                    # Rust (простой HTTP)
│   │   ├── plugin.json
│   │   ├── Cargo.toml
│   │   └── src/
│   │       └── main.rs
│   │
│   ├── alarm/                      # Rust
│   │   ├── plugin.json
│   │   ├── Cargo.toml
│   │   └── src/main.rs
│   │
│   ├── timer/                      # Rust
│   │   ├── plugin.json
│   │   ├── Cargo.toml
│   │   └── src/main.rs
│   │
│   ├── calculator/                 # C++ (пример для друга)
│   │   ├── plugin.json
│   │   ├── CMakeLists.txt
│   │   └── src/
│   │       └── main.cpp
│   │
│   ├── browser/                    # C++ (WebKit / CEF)
│   │   ├── plugin.json
│   │   ├── CMakeLists.txt
│   │   └── src/main.cpp
│   │
│   ├── system_monitor/             # Rust
│   │   ├── plugin.json
│   │   ├── Cargo.toml
│   │   └── src/main.rs
│   │
│   ├── knowledge/                  # Python (hnswlib)
│   │   ├── plugin.json
│   │   ├── main.py
│   │   └── requirements.txt
│   │
│   ├── notes/                      # Rust
│   │   ├── plugin.json
│   │   └── src/main.rs
│   │
│   ├── files/                      # Rust
│   │   ├── plugin.json
│   │   └── src/main.rs
│   │
│   ├── notifications/              # Rust (libnotify)
│   │   ├── plugin.json
│   │   └── src/main.rs
│   │
│   ├── currency/                   # Rust
│   │   ├── plugin.json
│   │   └── src/main.rs
│   │
│   ├── apps/                       # Rust
│   │   ├── plugin.json
│   │   └── src/main.rs
│   │
│   ├── quick_commands/             # Rust
│   │   ├── plugin.json
│   │   └── src/main.rs
│   │
│   └── web_ui/                     # Rust (Axum static)
│       ├── plugin.json
│       ├── src/main.rs
│       └── static/
│           ├── index.html
│           └── app.js
│
│  # Документация
├── docs/
│   ├── PROTOCOL.md                 # бинарный фрейм + protobuf схема
│   ├── PLUGIN_GUIDE.md             # как написать плагин
│   ├── ARCHITECTURE.md             # архитектурные решения и их причины
│   └── CAIRO.md                    # документация по AI агенту
│
│  # CI/CD
├── .github/
│   └── workflows/
│       ├── kernel-ci.yml           # cargo fmt + clippy + tests
│       ├── plugin-ci.yml           # проверка каждого плагина
│       └── proto-check.yml         # проверка что .proto не сломан
│
├── Makefile                        # make build / make test / make run
└── README.md
```

---

## Ключевые решения в структуре

**`proto/` на верхнем уровне** — не внутри `kernel/`. Это общий контракт.
Обе стороны (ядро и SDK) импортируют его. Изменение `.proto` = один PR,
оба языка обновляются вместе.

**`sdk/` отдельно от `kernel/`** — разработчик плагина не должен тянуть
весь исходник ядра. SDK это публичный API, ядро это приватная реализация.

**`cairo/` отдельно от `plugins/`** — Кайро архитектурно другой. Это не просто
плагин с несколькими действиями, это AI-агент который оркестрирует другие плагины.
Структурно разные → папки разные.

**ML плагины (stt, tts, ai, knowledge) на Python** — реалистичное решение.
Whisper, FAISS, TTS библиотеки живут в Python экосистеме. Общаются с ядром
через тот же UDS протокол — никаких исключений.

**C++ плагины (calculator, browser)** — папки для друга уже готовы.
Структура идентична Rust плагинам, только `Cargo.toml` → `CMakeLists.txt`.
