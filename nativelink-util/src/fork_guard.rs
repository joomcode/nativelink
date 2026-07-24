// Copyright 2026 The NativeLink Authors. All rights reserved.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//    http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! Process-global guard that serializes creation of executable files (which
//! briefly hold a writable file descriptor) against process spawning, to close
//! the residual `ETXTBSY` race in remote execution.
//!
//! # Background
//!
//! `execve(2)` fails with `ETXTBSY` ("Text file busy", `os error 26`) when the
//! target file's inode is open for writing by *any* process. Materializing an
//! executable input necessarily opens a writable fd (the `std::fs::copy` that
//! produces the 0o555 variant in `filesystem_store`). That fd is `O_CLOEXEC`,
//! so it is closed on `execve` — **but `O_CLOEXEC` is not closed on `fork`.**
//!
//! A child forked while the writable fd is open inherits a copy of it and holds
//! it until the child's own `execve` closes it. On Linux the worker installs a
//! `pre_exec` namespace-configuration hook, which forces the spawn onto the
//! `fork`+`exec` path and *widens* that fork→exec window. If another task
//! `execve`s a hardlink of the just-written inode while an inheriting child is
//! still in its window, the exec fails with `ETXTBSY`. This is exactly the
//! burst that happens when many single-flighted actions exec a hot executable
//! immediately after its variant is first created.
//!
//! # Contract
//!
//! Executable creation takes the **exclusive** (writer) side for exactly the
//! span its writable fd is open; process spawning takes the **shared** (reader)
//! side across the `fork`→`exec`. Because the two sides are mutually exclusive,
//! no child is ever forked while an executable's writable fd is open, so no
//! child can inherit it. Spawns stay fully concurrent with one another, and
//! executable creation is rare (once per digest), so steady state is
//! contention-free and the cold-path cost is a brief spawn quiesce bounded by a
//! single copy.
//!
//! # Opt-in
//!
//! The guard is **disabled by default** and enabled process-wide via
//! [`set_enabled`] (wired to the `enable_exec_fork_guard` worker config). While
//! disabled, [`spawn_guard`] and [`exec_write_guard`] acquire nothing and add
//! no synchronization. The flag MUST be set once at startup before any action
//! runs: both the spawn side and the executable-write side read this single
//! flag, so enabling it turns on mutual exclusion on both sides at once. A
//! mismatched setting (one side guarding, the other not) would provide no
//! exclusion, which is why the switch is a single global rather than threaded
//! independently into each call site.

use core::sync::atomic::{AtomicBool, Ordering};

use tokio::sync::{RwLock, RwLockReadGuard, RwLockWriteGuard};

/// Whether the fork guard is active. Off by default; flipped once at startup by
/// [`set_enabled`]. Read on every spawn/exec-write, so kept as a cheap atomic.
static ENABLED: AtomicBool = AtomicBool::new(false);

/// The process-global fork guard. Reader = process spawn, writer = executable
/// write. There is exactly one instance per worker process; `fork`/`ETXTBSY`
/// is inherently a process-wide concern, so a `static` is the correct scope.
static FORK_GUARD: RwLock<()> = RwLock::const_new(());

/// Enable or disable the fork guard for the whole process. Call once at worker
/// startup from config, before any action executes. Applies to both guard
/// sides at once (see the module-level opt-in note).
pub fn set_enabled(enabled: bool) {
    ENABLED.store(enabled, Ordering::Relaxed);
}

/// Whether the fork guard is currently enabled.
#[must_use]
pub fn is_enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

/// RAII guard for the shared (spawn) side. Holds a real read guard only when
/// the fork guard is enabled; otherwise it is a no-op. Either way, callers hold
/// it across `Command::spawn` and drop it afterward with identical code.
#[must_use = "the guard releases as soon as it is dropped; bind it across the spawn"]
pub enum SpawnGuard {
    Held(RwLockReadGuard<'static, ()>),
    Disabled,
}

/// RAII guard for the exclusive (executable-writer) side. Holds a real write
/// guard only when the fork guard is enabled; otherwise it is a no-op.
#[must_use = "the guard releases as soon as it is dropped; bind it across the writable-fd span"]
pub enum ExecWriteGuard {
    Held(RwLockWriteGuard<'static, ()>),
    Disabled,
}

/// Acquire the shared (spawn) side. Hold the returned guard across the
/// `fork`+`exec` of a child process — i.e. across the `Command::spawn` call —
/// and drop it once `spawn` returns. While held (and enabled), no
/// executable-write can begin, so the forked child cannot inherit a writable fd
/// on an about-to-be-executed inode. A no-op when the guard is disabled.
pub async fn spawn_guard() -> SpawnGuard {
    if is_enabled() {
        SpawnGuard::Held(FORK_GUARD.read().await)
    } else {
        SpawnGuard::Disabled
    }
}

/// Acquire the exclusive (executable-writer) side. Hold the returned guard for
/// exactly the span a writable fd is open on an inode that will later be
/// hardlinked and executed (e.g. the `std::fs::copy` that materializes an
/// executable variant) — never across the subsequent `fsync`/`rename`, which
/// hold no writable fd. While held (and enabled), no new process can be
/// spawned, so no child can inherit this write's fd across `fork`. A no-op when
/// the guard is disabled.
pub async fn exec_write_guard() -> ExecWriteGuard {
    if is_enabled() {
        ExecWriteGuard::Held(FORK_GUARD.write().await)
    } else {
        ExecWriteGuard::Disabled
    }
}

#[cfg(test)]
mod tests {
    use futures::FutureExt;
    use nativelink_macro::nativelink_test;

    use super::*;

    /// The guard's ETXTBSY contract, stated as lock semantics, plus its opt-in
    /// behavior. Deterministic — uses `now_or_never` rather than timing. This
    /// is the only test touching the process-global flag/lock, and it runs its
    /// phases sequentially, so there is no cross-test interference.
    #[nativelink_test("crate")]
    async fn opt_in_and_mutual_exclusion() {
        // Disabled (the default): both sides are no-ops and never block, even
        // when a "writer" is held — so no synchronization cost is imposed on
        // workers that do not opt in.
        assert!(!is_enabled());
        let writer = exec_write_guard().await;
        assert!(
            spawn_guard().now_or_never().is_some(),
            "while disabled, a spawn must never block on an executable-write"
        );
        drop(writer);

        // Enabled: the two sides become mutually exclusive.
        set_enabled(true);
        assert!(is_enabled());

        // A held executable-write blocks any process spawn: a child forked
        // while the write's fd is open is exactly the ETXTBSY hazard.
        let writer = exec_write_guard().await;
        assert!(
            spawn_guard().now_or_never().is_none(),
            "a spawn must not proceed while an executable-write is in flight"
        );
        drop(writer);

        // Once the write is done, spawns proceed — and multiple spawns run
        // concurrently (the common steady-state path must not serialize).
        let reader1 = spawn_guard().await;
        assert!(
            spawn_guard().now_or_never().is_some(),
            "concurrent spawns must not block one another"
        );

        // A held spawn blocks an executable-write, so no copy's fd can be open
        // while a child is mid fork→exec.
        assert!(
            exec_write_guard().now_or_never().is_none(),
            "an executable-write must not proceed while a spawn is in flight"
        );
        drop(reader1);

        // With everything released the write side is acquirable again.
        assert!(
            exec_write_guard().now_or_never().is_some(),
            "the write side must be acquirable once all guards are released"
        );

        // Restore the default so the process-global flag does not leak to any
        // other test in this binary.
        set_enabled(false);
    }
}
