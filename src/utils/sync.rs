use std::sync::{MutexGuard, PoisonError};
use tracing::warn;

/// ma-12: mutex poison means a thread panicked while holding the lock —
/// log it instead of silently recovering, so the panic shows up in operator
/// logs. pass as `.unwrap_or_else(recover_poison)` on `Mutex::lock`
pub fn recover_poison<T>(poison: PoisonError<MutexGuard<'_, T>>) -> MutexGuard<'_, T> {
    warn!("mutex poisoned by a panicking thread — recovering");
    poison.into_inner()
}
