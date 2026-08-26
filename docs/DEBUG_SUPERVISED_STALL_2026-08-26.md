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

## Следующий шаг диагностики

```bash
# терминал 1: ядро с киллсвитчами + debug-лог
RUST_LOG=vynkor=debug VYN_DEBUG_SKIP_RLIMITS=1 vyn start --foreground ...
# терминал 2: strace на процесс плагина во время вызова
strace -f -p <plugin_pid> -e trace=write,writev,futex,recvfrom,sendto
```
Сравнить: уходит ли `write()` ActionResponse в UDS; что отвечает ядро;
где блокируется. Затем читать reply-лег `src/ipc/protocol.rs`
(ветка `ActionResponse`, pending-реестр) в сравнении «db vs tts».

Фиксы по пп. 1–2 просить/делать в `develop` отдельными PR; эта ветка —
диагностическая, мерджить опционально.
