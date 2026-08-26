# Debug: supervised plugin stall — bisection 2026-08-26 (sherpa TTS)

Ветка `debug/sherpa-supervised-stall`. Контекст: локальный sherpa-синтез
(tts/speech) под супервайзером грузит модель и никогда не завершает
инференс; вне ядра тот же бинарник/env/rlimit — 80–100 мс. Полная матрица —
в vynkor-plugins `plugins/speech/ROADMAP.md` и
`docs/P0_BATCH_2026-08-26.md` (§3).

## Что добавлено в этой ветке

Env-киллсвитчи на процессе **ядра** (не на плагине):

| Переменная | Эффект |
|---|---|
| `VYN_DEBUG_SKIP_CGROUP=1` | не создавать/джойнить per-plugin cgroup v2 (включается RLIMIT_NPROC-фолбэк!) |
| `VYN_DEBUG_SKIP_RLIMITS=1` | pre_exec не вызывает `apply_resource_limits` вообще |

Оба флага также зеркалят stdout/stderr ребёнка в kernel log с префиксом
`[plugin-stderr]` — иначе stderr незарегистрированного/упавшего плагина
недоступен (`/plugins/{id}/logs` отвечает 404 до регистрации).

## Факты, добытые бисектом

1. **NPROC-фолбэк ломает плагины (подтверждённый баг).** При скипе cgroup
   ставится `RLIMIT_NPROC = max_procs` (дефолт 64). NPROC считает потоки всех
   процессов uid; рабочая станция превышает 64 сразу ⇒ любой
   thread-heavy плагин падает: `pthread_create → EAGAIN` → tokio panic
   `OS can't spawn worker thread` (видно в `[plugin-stderr]`).
   **Фикс:** не применять NPROC из `max_procs` (cgroup уже покрывает), либо
   считать от текущего потребления, либо сильно поднять дефолт.
2. **`DEFAULT_MAX_VMEM_MB = 512` мал для ONNX-плагинов**: модель не
   аллоцируется уже на загрузке. tts/speech требуют ≥1536M.
3. **Главная открытая загадка:** с полностью скипнутыми лимитами хендлер
   завершается мгновенно (`time-to-first-audio: 0 ms` в зеркале), чанки
   отправлены, но `ActionResponse` не доходит до вызывающего. db_* через тот
   же клиент работает. Исключены: rlimits, cgroup, action_id дубликаты,
   sandbox/seccomp/cwd/пайпы/ONNX threads.

## Resolved 2026-08-26 — fix/supervised-stall-and-limits (PR → develop)

Ветка диагностическая закрыта; фиксы влиты в `develop` одним PR.

### Problem A — главная загадка: `ActionResponse` не доходит

**Root cause:** `src/ipc/protocol.rs` — per-connection error-budget
throttling (`max_conn_errors = 16`). Каждый denied `tts_speak` audio-chunk
forward (`ipc_targets` не содержит `sound` → `ERR_PERMISSION_DENIED`,
`errored = true`) инкрементил `error_counts[tts_conn]`. После 16 denied
чанков соединение помечалось `throttled` и **все** дальнейшие сообщения от
того же `conn_id` дропались молча (`continue` без `send_error`), включая
легитимный финальный `ActionResponse` провайдера. `db_*` не шлёт
peer-to-peer фреймов перед ответом, поэтому не триггерит throttling и
работает. С полностью скипнутыми лимитами `time-to-first-audio: 0 ms`
подтверждало что хендлер завершился, но ответ уже был в дроп-зоне.

**Fix:** `is_throttle_exempt()` — `ActionResponse`,
`ActionResponseChunk`, `ActionRequestChunk`, `SessionClose`, `Pong`
проходят мимо throttling-гейта и сбрасывают бюджет (`errored == false →
error_counts.remove`). Это сохраняет защиту от amplification (VULN-007)
для error-генерирующих сообщений, но не блокирует легитимные replies и
watchdog `Pong`.

Live verification (scratch `/tmp/opencode/vynfix`, без `VYN_DEBUG_*`,
cgroup `pids.max` активен, `DEFAULT_MAX_VMEM_MB = 2048`):
`tts_speak` (sherpa piper `ru_RU-denis-medium`, "Один два три.")
→ 47 Opus пакетов, `duration 1.0s`, `TTFA 0 ms`, `elapsed 1633 ms` (cold)
и `0 ms` (warm); `tts_speak_stream` (3 предложения, 162 пакета)
→ `TTFA stream 0 ms`, оба `status=OK`. `speech` plugin (swap drop-in) —
аналогично `tts_speak` / `tts_speak_stream` с теми же TTFA.

### Problem B — `RLIMIT_NPROC` fallback

`RLIMIT_NPROC` считает треды всего uid (десктоп > 64), поэтому `max_procs`
64 ронял любой thread-heavy плагин `EAGAIN` → tokio panic. Фикс:
`apply_resource_limits` больше не ставит `RLIMIT_NPROC` вообще; при
`!joined_cgroup` только `warn!` ("process accounting degraded"), cgroup
`pids.max` остаётся единственным лимитом. Юнит-тесты в `runner.rs`.

### Problem C — `DEFAULT_MAX_VMEM_MB = 512`

ONNX Runtime резервирует ~500 MiB VA → модель не mmap-илась. Дефолт
поднят до **2048**, `0` = unlimited (skip `setrlimit`). Обновлены
`src/utils/config.rs`, `plugins/{tts,speech}/config.example.yaml` и
`README.md`.

Kill-switches `VYN_DEBUG_SKIP_CGROUP` / `VYN_DEBUG_SKIP_RLIMITS` сохранены
как есть (требование миссии), зеркалирование `[plugin-stderr]` тоже.
