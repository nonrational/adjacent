<!-- Lesson for PR #50. Teaches one concept grounded in the real diff. -->

# PR #50 — Proxy follow-ups: deadline timing, BootGate GC, NAT-replacement warning

> **Rust lesson:** Store `Weak` references as a map's values so the map garbage-collects itself — an entry lives only while some in-flight boot still holds the strong `Arc`, and `HashMap::retain` sweeps the dead ones in one in-place pass.
> **Tags:** `weak-references` · `hashmap-retain`
> **Merged:** 2026-06-13 · +48/−6 · [View PR](https://github.com/nonrational/adjacent/pull/50)

## The situation

The proxy's `BootGate` keeps one lock per app name so concurrent first-requests boot the app exactly once (that mechanism is PR #24). But the original map held an `Arc<Mutex<()>>` per name, and nothing ever removed entries. Every name the daemon ever routed left a permanent row in the map. The daemon is long-lived, and there is **no app-removal RPC** to hook a cleanup call onto — so the map only ever grew. A slow leak, but a leak. This PR makes the map bound itself to boots that are actually in flight.

## The Rust idea

`Arc<T>` is a *strong* reference: as long as one `Arc` exists, the value stays alive. `Weak<T>` is a *non-owning* reference to the same value — it points at the data but does **not** count toward keeping it alive. When the last `Arc` drops, the value is freed even if `Weak`s still point at where it used to be.

Because a `Weak` might be dangling, you can't dereference it directly. You call `.upgrade()`, which returns `Option<Arc<T>>`: `Some` if the value is still alive (and hands you a fresh strong ref), `None` if it's gone. That `Option` is Rust forcing you to handle "the thing I referenced no longer exists" — there is no way to accidentally touch freed memory.

Two more pieces the diff uses:

- `Arc::downgrade(&arc)` mints a `Weak` from an `Arc` — that's what you store in the map.
- `weak.strong_count()` asks how many strong `Arc`s are still out there. Zero means the value is dead and the entry is garbage.
- `HashMap::retain(|k, v| bool)` walks every entry once and drops those where the closure returns `false`, editing the map **in place** — no rebuild, no second allocation.

Put together: store `Weak` values, and an entry becomes collectable the moment its last `Arc` drops. `retain(|_, w| w.strong_count() > 0)` is the sweep.

## In this PR

The map's value type flips from `Arc` to `Weak`:

```rust
// crates/adj/src/proxy.rs
#[derive(Default)]
pub struct BootGate {
    locks: Mutex<HashMap<String, std::sync::Weak<Mutex<()>>>>,
}
```

And `lock_for` prunes on the way in, then either upgrades the surviving entry or mints a fresh lock:

```rust
// crates/adj/src/proxy.rs
async fn lock_for(&self, name: &str) -> Arc<Mutex<()>> {
    let mut map = self.locks.lock().await;
    map.retain(|_, weak| weak.strong_count() > 0);
    if let Some(existing) = map.get(name).and_then(std::sync::Weak::upgrade) {
        return existing;
    }
    let lock = Arc::new(Mutex::new(()));
    map.insert(name.to_string(), Arc::downgrade(&lock));
    lock
}
```

Trace the lifetime. `lock_for` returns an `Arc` — the caller in `ensure_running` holds it (`let name_lock = gate.lock_for(name).await;`) for the whole boot. So while a boot is in flight, at least one strong `Arc` exists, `strong_count()` is `> 0`, and the map entry survives `retain`. Concurrent requests for the *same* name `upgrade()` that same live `Weak` and get the *same* `Arc` back — single-flight identity is preserved (the new unit test asserts `Arc::ptr_eq(&lock_a, &lock_a2)`). Once the boot finishes and every caller drops its `Arc`, `strong_count()` falls to zero, and the *next* `lock_for` call sweeps the dead entry. The map is now bounded by the number of boots happening right now, not by every name ever seen.

The new test spells out the whole arc — hold two locks, confirm neither is pruned while alive, drop them all, then confirm the next lookup collects them:

```rust
// crates/adj/src/proxy.rs
drop(lock_a);
drop(lock_a2);
drop(lock_b);
// All strong refs gone — the next lock_for sweeps the dead entries.
let _lock_c = gate.lock_for("c").await;
let map = gate.locks.lock().await;
assert_eq!(map.len(), 1);
assert!(map.contains_key("c"));
```

## Why it matters

The trap is thinking of a map as a passive container and forgetting it's a *strong* owner. In the original code the `HashMap` held `Arc`s, so those apps could never be freed — the map itself was the reason they stayed alive. This is exactly the situation a garbage collector *can't* rescue you from either: a GC won't collect an object the map still strongly references, so a Java `HashMap` here would leak the same way. The tool for "reference it, but don't be the reason it lives" is a weak reference — `Weak<T>` in Rust, `WeakHashMap` in Java. Reaching for it turns an ever-growing map into a self-cleaning one, and the safety of `upgrade()` returning an `Option` means the "it might be gone" case is impossible to forget.

## Related lessons

- PR #24 owns the single-flight boot itself — the per-name lock and the double-check that make concurrent first-requests boot an app exactly once. This lesson is the follow-up that stops the lock *map* from growing forever; read #24 first for what the map is *for*.
- Same PR, second thread (monotonic time): `ensure_running` now captures its boot deadline as `let deadline = tokio::time::Instant::now() + boot_timeout;` **before** calling `up()`, so a slow spawn counts against the boot budget instead of being added on top of it. `Instant` is a *monotonic* clock — it only moves forward and is immune to wall-clock jumps (NTP steps, DST), which is why it's the right type for a deadline. Prefer it over `SystemTime` for measuring elapsed time, and reach for `saturating_duration_since` / checked arithmetic when subtracting two `Instant`s so a reordering can't underflow.

## Dig deeper

- [`std::sync::Weak`](https://doc.rust-lang.org/std/sync/struct.Weak.html) — `upgrade`, `strong_count`, and the `Arc`/`Weak` ownership split.
- [`HashMap::retain`](https://doc.rust-lang.org/std/collections/struct.HashMap.html#method.retain) — in-place filtering in a single pass.
- [`std::time::Instant`](https://doc.rust-lang.org/std/time/struct.Instant.html) — the monotonic clock behind the deadline math.
