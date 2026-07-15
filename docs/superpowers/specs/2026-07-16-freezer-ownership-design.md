# Freezer ownership reconciliation design

## Goal

Fix the Android 16 failure in which an application is successfully resumed with
`SIGCONT`, but Freezeit continues to record it as frozen.  Until the later audit
runs, a subsequent background transition is skipped, so the process remains
running while the manager reports it as frozen.  X makes this conspicuous, but
the defect applies to any configured application, including Douyin.

The change must preserve the conservative handling of a real cgroup/Binder
freezer transaction: successful `SIGCONT` alone is not evidence that all
freezer resources were released.

## Observed failure

On the affected OnePlus Android 16 device, Binder freezer ioctls return
`EINVAL`; the daemon correctly falls back to `SIGSTOP` for mode 30.  The
runtime process object still has a usable cgroup path.  On the next foreground
pass, rediscovery supplies that path to the generic thaw path, which runs:

1. cgroup thaw;
2. Binder unfreeze, which fails; and
3. `SIGCONT`, which succeeds.

The generic operation returns the Binder failure, so the controller retains
the identity in `frozen_apps`.  A background pass before the periodic audit
sees that stale marker and does not schedule a new freeze.

## Chosen approach

Record the **actual successful freeze ownership** for each runtime identity,
rather than inferring it later from the process's cgroup capability.

`RuntimeControlState` will retain its existing frozen identity and timestamp
tracking and gain a synchronized ownership map keyed by the same
`(package_name, uid)` identity:

```rust
enum FrozenOwnership {
    SignalOnly,
    CgroupBinder,
    ResidualUnknown,
}
```

`track_frozen` will set both the frozen marker and its ownership; `clear_frozen`
will remove both.  All paths that replace or remove a frozen identity must use
these helpers so the map cannot become stale.

The ownership rules are deliberately conservative:

| Freeze result | Recorded ownership |
| --- | --- |
| Direct signal policy, or a Binder-unavailable fallback that reaches only `SIGSTOP` | `SignalOnly` |
| Complete cgroup + Binder freezer transaction | `CgroupBinder` |
| Any primary freezer attempt that could have changed state before a failure, rollback uncertainty, or primary-then-signal fallback | `ResidualUnknown` |

`ResidualUnknown` has precedence over later signal success.  It must never be
overwritten as `SignalOnly`, because that would make a partial freezer
transaction look fully released.

## Thaw behavior

The controller will select a thaw plan from the recorded ownership, not from
the newly discovered process's cgroup path.

| Recorded ownership | Per-process thaw plan | When the marker is cleared |
| --- | --- | --- |
| `SignalOnly` | Clone the process through `signal_control_process`, so only `SIGCONT` is attempted | Every validated process succeeds with that signal-only plan |
| `CgroupBinder` | Existing cgroup + Binder + `SIGCONT` path | Every required step for every validated process succeeds |
| `ResidualUnknown` | Existing conservative generic path, which attempts all possible cleanup | Every generic cleanup step succeeds |

This selection applies to foreground thaw, periodic wake-up thaw, policy-removal
thaw, and any shared helper used by those flows.  A failure in a
`CgroupBinder` or `ResidualUnknown` thaw retains ownership and remains an
operation failure/partial result even if `SIGCONT` happened to succeed.

For a confirmed `SignalOnly` identity, a successful `SIGCONT` clears the
marker immediately.  The next background pass can therefore schedule and
execute a fresh `SIGSTOP`, without waiting for the 60-second reconciliation
audit.

Restart recovery will likewise treat a process recorded in the existing
Freezeit-owned SIGSTOP ledger with a thawed cgroup as signal-owned recovery;
it must not use mere cgroup-path availability as evidence that a Binder thaw is
needed.  A cgroup observed as frozen, or uncertain evidence, keeps the
conservative generic recovery behavior.

## Operations and diagnostics

Operation log backend names will report the plan actually used:

- signal freeze: `signal.stop`;
- signal-owned thaw: `signal.cont`;
- generic freezer paths: their existing cgroup/Binder backend name.

The socket layer keeps its existing failure semantics: a Binder failure after a
successful `SIGCONT` remains an error for a generic cgroup/Binder thaw.  The
controller avoids that path only when ownership proves that no cgroup/Binder
freeze was performed.  This avoids masking legitimate partial thaw failures.

## Tests and acceptance criteria

Before implementation, add a failing controller regression that models mode
30 with Binder unavailable and a rediscovered process that still has a cgroup
path.  Its three passes are background freeze, foreground thaw, then immediate
background freeze.  It must prove that:

1. the first freeze uses signal-only control;
2. foreground thaw receives a process with no cgroup path and succeeds with
   `SIGCONT`;
3. frozen ownership is cleared; and
4. the immediate next background pass sends a second `SIGSTOP` and only then
   reports the identity frozen.

Add a guard regression for a partial primary freezer transaction followed by a
signal fallback: its ownership remains `ResidualUnknown`, and a generic thaw
error does not clear it.  Add a socket-level guard that confirms the existing
generic cgroup/Binder path reports Binder failure even when `SIGCONT` succeeds.
Keep or extend integration coverage so the status record agrees with the
actual second stop, not a stale ownership marker.

The finished change is accepted when the new tests are red before the fix,
green after it, all related daemon tests pass, the release build succeeds, and
on-device testing shows both X and Douyin can be frozen, brought foreground,
and frozen again without a stale frozen state or a permanently stopped
foreground process.

## Scope boundaries

This is a lifecycle correctness fix.  It does not claim to make Binder freezer
available on this Android kernel, nor does it change policy modes or weaken
the current fallback to `SIGSTOP`/`SIGCONT`.  It only makes the controller
honest about which mechanism actually owns a freeze and releases it with the
matching plan.
