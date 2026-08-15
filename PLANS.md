# PLANS — Распространение плагинов через Cloudflare R2 + платный маркетплейс (будущее)

> Статус: **запланировано / фаза 1 готова к выполнению (0 изменений кода).**
> Фаза 2 (платные плагины) — намеренно отложена до появления реальных платных плагинов.

---

## 1. Контекст и цель

Сейчас ядро Veyron скачивает плагины с **raw.githubusercontent.com** (реестр `registry.json`
+ zip-архивы по прямым HTTPS-ссылкам). Цель:

1. **Сейчас** — перевести раздачу плагинов на **Cloudflare R2** (открытый бакет):
   - бесплатный egress (у R2 нет платы за исходящий трафик);
   - стабильный кастомный домен вместо raw.githubusercontent;
   - **ноль изменений в ядре** — это конфигурация, а не код.
2. **В будущем** — платный маркетплейс: платные плагины по ключу/аккаунту,
   бесплатные — открыты всем. Для этого понадобится тонкий бэкенд (Cloudflare Worker)
   поверх R2 + изолированные изменения в ядре (токен доступа в 2 местах).

Ключевой принцип: **безопасность целостности уже решена в ядре** (Ed25519-подпись +
sha256), поэтому открытая раздача не снижает доверия. Бэкенд нужен только для
**контроля доступа**, не для целостности.

---

## 2. Как ядро распространяет плагины сегодня (карта кода)

### 2.1 Поток данных

```
vyn plugin list|search|install <slug>
  │
  ├─ fetch_registry_with_url(url)            src/marketplace/registry.rs:478
  │     └─ fetch_from_network(url)           src/marketplace/registry.rs:271
  │           = reqwest::get(url)  →  registry.json  (обычный HTTPS GET)
  │     └─ resolve_relative_archive_urls()   src/marketplace/registry.rs:507
  │           ← относительные archive_url склеиваются с базой реестра
  │     └─ verify_entries()                  ← Ed25519-проверка сигнатур
  │     └─ кэш: registry-cache.json          (state_dir, TTL registry_cache_ttl_secs)
  │
  └─ install(entries, target, ...)           src/marketplace/installer.rs:159
        └─ download_with_progress(archive_url)  src/marketplace/installer.rs:374
              = reqwest::get(url) → zip (обычный HTTPS GET, лимит max_archive_bytes)
        ├─ шаг 4:  sha256-проверка архива
        ├─ шаг 4b: Ed25519-проверка подписи записи реестра
        ├─ шаг 5:  распаковка (zip-slip защита, manifest v2 allowlist)
        └─ шаг 6:  атомарный перенос в plugin_dir + запись installed.json
```

### 2.2 Ключевые точки кода

| Что | Файл / строка |
|---|---|
| Дефолтный URL реестра (raw GitHub) | `src/marketplace/registry.rs:15-16` (`DEFAULT_REGISTRY_URL`) |
| Поле конфига `registry_url` | `src/utils/config.rs:130` |
| Поле конфига `marketplace_public_key` | `src/utils/config.rs:136` |
| Поле конфига `registry_cache_ttl_secs` | `src/utils/config.rs:140` |
| Прокидывание `registry_url` из конфига в CLI | `src/main.rs:129` |
| Выбор URL реестра в CLI (пустой → дефолт) | `src/cli/plugin.rs:80-96` |
| HTTPS GET реестра | `src/marketplace/registry.rs:271-295` (`fetch_from_network`) |
| HTTPS GET архива | `src/marketplace/installer.rs:374-427` (`download_with_progress`) |
| Схема записи реестра | `src/marketplace/registry.rs:41-81` (`RegistryEntry`) |
| Резолв относительных `archive_url` | `src/marketplace/registry.rs:507-522` |
| Пайплайн установки (8 шагов) | `src/marketplace/installer.rs:159` (`install`) |

### 2.3 Модель целостности (не зависит от канала раздачи)

- Каждая запись реестра: `{slug, version, archive_url, sha256, signature, status, ...}`
  (`RegistryEntry`, `registry.rs:41`).
- **Ed25519-подпись** над строкой `"{slug}:{version}:{sha256}"`, публичный ключ
  зашит в ядре (`MAINTAINER_PUBLIC_KEY_HEX`, `registry.rs:32`) или переопределён
  через `marketplace_public_key` в конфиге.
- Подпись **не покрывает URL** → перенос реестра/архивов на другой хост не требует
  переподписи (sha256 не меняется).
- Утверждение из кода: «attacker who only compromises the registry host/CDN cannot
  forge it» — открытый канал раздачи безопасен по дизайну.

### 2.4 Важные свойства

- И реестр, и архив качаются **обычным HTTPS GET** (`reqwest` 0.12, фича `json`).
  Никакого S3/SigV4 в ядре нет — R2 выступает как статический файловый сервер.
- `archive_url` может быть **относительным** — ядро само резолвит его относительно
  base URL реестра (`resolve_relative_archive_urls`). Написано специально для
  сценария «одна строка `registry_url` и ничего перепубликовывать».
- Serde игнорирует лишние поля JSON → в реестр можно добавлять новые поля
  (`access`, `license`, ...) без поломки старых ядер.
- Кэш реестра: `registry-cache.json` в state_dir, свежесть по `registry_cache_ttl_secs`.
  Архивы не кэшируются (скачиваются при каждом install).

---

## 3. Фаза 1 — открытый R2-бакет (готово к выполнению, 0 кода)

### 3.1 Настройка Cloudflare

1. Дашборд → **R2 → Create bucket**, имя например `veyron-plugins`.
2. **Публичный доступ** (обязательно кастомный домен, не r2.dev):
   - Бакет → Settings → Public access → **Connect domain** → `plugins.veyron.dev`.
   - Домен должен быть на DNS Cloudflare — TLS-сертификат выпустится автоматически.
   - r2.dev (`pub-<hash>.r2.dev`) — только для локальных тестов (rate limits, не для прода).
3. **S3 API-токен**: R2 → Manage API Tokens → создать токен с правами на бакет
   (нужен только для загрузки, не для раздачи).

### 3.2 Layout бакета (стабильный, не менять потом)

```
plugins.veyron.dev/
├── registry.json                  ← корень, фиксированный путь
└── plugins/
    └── <slug>/
        └── <slug>-v<version>.zip
```

### 3.3 registry.json

- Относительные `archive_url`, чтобы переезд реестра не трогал архивы:

```json
[
  {
    "slug": "weather",
    "name": "Weather",
    "version": "1.2.3",
    "archive_url": "plugins/weather/weather-v1.2.3.zip",
    "sha256": "<sha256 архива>",
    "signature": "<ed25519 hex>",
    "min_kernel_version": "0.1.0",
    "max_kernel_version": "*"
  }
]
```

- Сигнатуры пересоздавать **не нужно** (покрывают `slug:version:sha256`, не URL).
- Новые поля (`access`, `license`) добавлять уже можно — старые ядра их проигнорируют.

### 3.4 config.yaml

```yaml
registry_url: https://plugins.veyron.dev/registry.json
```

### 3.5 Загрузка и CI

- Вручную: aws CLI / rclone / drag-and-drop в дашборде.
- CI: aws CLI с R2-endpoint и креденшелами из секретов:

```bash
aws s3 cp registry.json s3://veyron-plugins/registry.json \
  --endpoint-url https://<ACCOUNT_ID>.r2.cloudflarestorage.com
aws s3 cp plugins/weather/weather-v1.2.3.zip \
  s3://veyron-plugins/plugins/weather/weather-v1.2.3.zip \
  --endpoint-url https://<ACCOUNT_ID>.r2.cloudflarestorage.com
```

### 3.6 Чек-лист проверки

- [ ] `curl https://plugins.veyron.dev/registry.json` отдаёт JSON (200, не пустой)
- [ ] `curl https://plugins.veyron.dev/plugins/<slug>/<slug>-v<ver>.zip` отдаёт архив
- [ ] `vyn plugin search weather --refresh` показывает записи
- [ ] `vyn plugin install weather` скачивает с R2, sha256 и подпись проходят
- [ ] В `installed.json` `source_url` указывает на новый реестр

---

## 4. Фаза 2 — платные плагины по ключу/аккаунту (будущее)

**Когда делать:** когда появятся реальные платные плагины. Строить сейчас — строить
по требованиям, которых ещё нет (биллинг, лицензии, аккаунты).

### 4.1 Архитектура

```
Kernel (reqwest GET)
   │  registry_url
   ▼
Cloudflare Worker  ──►  D1 / KV   (аккаунты, ключи, лицензии)
   │
   ├─ отдаёт ОТФИЛЬТРОВАННЫЙ registry.json
   │    (только плагины, к которым есть доступ у предъявленного ключа)
   │    free-плагины → обычные открытые URL (открытый бакет)
   │    paid-плагины → presigned R2 URL (приватный бакет)
   ▼
R2 (приватный для paid) — байты льются напрямую в ядро, egress = 0
```

- **Бэкенд = тонкий слой контроля доступа**, не прокси байтов. Cloudflare-стек:
  Worker + D1/KV + R2. Отдельный VM/backend-фреймворк не нужен.
- **Presigned URL** — это обычный HTTPS URL с `?X-Amz-...`; `reqwest::get` ядра
  принимает его без изменений. Сигнатура не ломается (не покрывает URL).
- Free-плагины остаются в открытом бакете — Worker просто отдаёт их всем.

### 4.2 Изменения в ядре (изолированный скоуп)

| Что | Где |
|---|---|
| Опциональный токен доступа к маркетплейсу | новое поле конфига (напр. `marketplace_token`) + CLI-флаг `vyn plugin ... --token` |
| Передача токена при запросе реестра | `fetch_from_network` (`registry.rs:271`) — заголовок/query |
| Передача токена при скачивании архива | `download_with_progress` (`installer.rs:374`) |
| Кэш vs истечение presigned URL | короткий `registry_cache_ttl_secs` или принудительный `--refresh` при install (флаг уже есть) |

### 4.3 Открытые вопросы (решить в фазе 2)

- Формат токена: статический ключ из конфига vs per-account JWT из CLI.
- Поддержка нескольких реестров (free-реестр + paid-реестр) в одном ядре.
- Лицензионная модель: entitlement только через серверную фильтрацию или ещё
  и подписанные entitlements (криптографическая привязка плагина к аккаунту).
- Роль veyron-web (веб-UI) в маркетплейсе: каталог, биллинг, выдача ключей.

---

## 5. Принятые решения и обоснование

| Решение | Почему |
|---|---|
| Сейчас — открытый бакет, бэкенд не строим | Ядро не умеет и не должно уметь «лицензии»; путь установки анонимный. Миграция на бэкенд = одна строка `registry_url`. YAGNI. |
| Целостность оставляем на Ed25519 + sha256 | Спроектировано именно под недоверенный канал раздачи (`registry.rs:22-27`). Бэкенд не усилит безопасность free-плагинов. |
| Публичный бакет безопасен | R2 public — только GET; записать через публичный эндпоинт нельзя. Секреты (access keys) нужны только на стороне загрузки/CI. |
| Кастомный домен, не r2.dev | Стабильные URL; переезд на бэкенд/Worker потом не ломает кэши и `installed.json`. |
| Относительные `archive_url` | Ядро само склеивает их с базой реестра — смена хоста не трогает архивы. |
| Presigned URL вместо проксирования | Байты идут R2 → ядро напрямую; egress R2 = 0; Worker не платит за трафик. |

## 6. Связанные документы

- `docs/PLUGIN_REGISTRY_SCHEMA.md` — схема реестра
- `docs/FRAMING.md` — формат кадров (не затрагивается)
- `ROADMAP.md` — общий роадмап ядра
- Cloudflare: [R2 S3 API](https://developers.cloudflare.com/r2/api/s3/api/),
  [API tokens](https://developers.cloudflare.com/r2/api/tokens/),
  [presigned URLs](https://developers.cloudflare.com/r2/examples/aws/aws-sdk-rust/),
  [публичные бакеты](https://developers.cloudflare.com/r2/buckets/public-buckets/)
