# Freezer Ownership Reconciliation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ensure a process frozen through the SIGSTOP fallback is thawed through SIGCONT only, clears its runtime ownership, and can be frozen again immediately.

**Architecture:** `RuntimeControlState` keeps its existing frozen identity/timestamp indexes and adds a per-identity ownership record.  Freeze paths record the backend that actually succeeded; thaw paths select their process shape and log backend from that record.  Unknown or partially applied freezer transactions retain the generic cgroup/Binder cleanup path and cannot be cleared by a bare SIGCONT.

**Tech Stack:** Rust 2021 daemon, Android cgroup v2/Binder freezer, POSIX signals, Cargo tests, Magisk release scripts, ADB, GitHub CLI.

---

## File structure

- Modify: `freezeitDaemon/src/app/controller.rs` — owns runtime freeze state, controller transitions, restart recovery, and focused unit tests.
- Modify: `freezeitDaemon/src/sys/socket.rs` — keeps the generic thaw transaction semantics and gains a regression test proving that a Binder error is not globally swallowed.
- Modify: `freezeitDaemon/tests/integration/freeze_unfreeze_state.rs` — exercises the externally visible status and refreeze cycle.
- Modify: `magisk/module.prop`, `freezeitApp/app/build.gradle`, `README.md`, `freezeitRelease/{changelog.txt,README.md,update.json}` — versions and release metadata for the repaired package.

### Task 1: Write the controller and socket regressions (RED)

**Files:**
- Modify: `freezeitDaemon/src/app/controller.rs:2395-2665`
- Modify: `freezeitDaemon/src/sys/socket.rs:1180-1260`
- Modify: `freezeitDaemon/tests/integration/freeze_unfreeze_state.rs:147-190`

- [ ] **Step 1: Add the three-pass Binder-degraded controller regression**

  In the controller test module, add a test named
  `binderless_signal_fallback_thaws_signal_only_and_refreezes_immediately`.
  Give its discovered process a cgroup path, hold
  `set_test_binder_available(false)`, and model the generic Binder failure in
  the unfreeze closure:

  ```rust
  let _binder_guard = set_test_binder_available(false);
  let mut runtime_process = process(uid, 123, ControlState::Running);
  runtime_process.cgroup_freeze_path = Some("/cgroup/uid_10123/pid_123/cgroup.freeze".to_owned());
  let stopped = RefCell::new(Vec::new());

  // Background: the candidate must be signal-only and records one stop.
  run_control_pass(&mut state, &[config(uid, 30)], |_, _| Ok(vec![runtime_process.clone()]),
      |candidate| { assert!(candidate.cgroup_freeze_path.is_none()); stopped.borrow_mut().push(candidate.pid); Ok(()) },
      |_| Ok(()), &[], 0).expect("initial signal fallback");

  // Foreground: a cgroup path means the old generic path was selected and must fail.
  run_control_pass(&mut state, &[config(uid, 30)], |_, _| Ok(vec![runtime_process.clone()]),
      |_| panic!("foreground must not freeze"),
      |candidate| if candidate.cgroup_freeze_path.is_none() { Ok(()) } else { Err(DaemonError::system("binder unfreeze failed after SIGCONT")) },
      &[uid], 1).expect("signal-owned foreground thaw");

  // Before the 60-second audit, a new background pass must issue another stop.
  run_control_pass(&mut state, &[config(uid, 30)], |_, _| Ok(vec![runtime_process.clone()]),
      |candidate| { assert!(candidate.cgroup_freeze_path.is_none()); stopped.borrow_mut().push(candidate.pid); Ok(()) },
      |_| Ok(()), &[], 2).expect("background pass returns even with stale ownership");
  assert_eq!(&*stopped.borrow(), &[123, 123]);
  ```

- [ ] **Step 2: Add the generic socket failure guard**

  In `socket.rs` next to the existing `run_unfreeze_sequence` tests, add
  `generic_cgroup_unfreeze_reports_binder_failure_after_successful_sigcont`.
  Record the closures in a `RefCell<Vec<&str>>`; use `Some("/cgroup/freeze")`,
  return success for cgroup and SIGCONT, and return
  `Err(DaemonError::system("EINVAL"))` for Binder.  Assert the call order is
  `cgroup`, `binder`, `sigcont` and the returned error contains
  `binder unfreeze failed`.

- [ ] **Step 3: Add an integration assertion for visible status after refreeze**

  Add `binderless_signal_fallback_foreground_thaw_then_background_refreezes` to
  `freeze_unfreeze_state.rs`.  Use the same three passes and cgroup-path-aware
  fake unfreeze closure as the unit regression; after pass three call
  `freeze_status_records` and assert the sole row is state `3` only after the
  second fake SIGSTOP was observed.

- [ ] **Step 4: Run the new regressions and capture RED evidence**

  Run:

  ```bash
  cargo test --manifest-path freezeitDaemon/Cargo.toml binderless_signal_fallback_thaws_signal_only_and_refreezes_immediately -- --exact
  cargo test --manifest-path freezeitDaemon/Cargo.toml generic_cgroup_unfreeze_reports_binder_failure_after_successful_sigcont -- --exact
  cargo test --manifest-path freezeitDaemon/Cargo.toml --test freeze_unfreeze_state binderless_signal_fallback_foreground_thaw_then_background_refreezes -- --exact
  ```

  Expected before implementation: the controller test fails because foreground
  thaw receives a cgroup path, and the integration test fails because the
  second stop is skipped.  The socket guard passes and protects against the
  incorrect "ignore Binder error" fix.

### Task 2: Record actual freezer ownership and route normal thaw (GREEN)

**Files:**
- Modify: `freezeitDaemon/src/app/controller.rs:507-615, 740-865, 1150-1268, 1350-1715`
- Test: `freezeitDaemon/src/app/controller.rs:2395-2665`

- [ ] **Step 1: Define the ownership type and synchronized state helpers**

  Add this private type beside `RuntimeIdentity`:

  ```rust
  #[derive(Debug, Clone, Copy, PartialEq, Eq)]
  enum FrozenOwnership {
      SignalOnly,
      CgroupBinder,
      ResidualUnknown,
  }
  ```

  Add `frozen_ownership: BTreeMap<RuntimeIdentity, FrozenOwnership>` to
  `RuntimeControlState` and initialize it in `Default`.  Change
  `track_frozen` to accept an ownership.  If an existing value is
  `ResidualUnknown`, retain it; otherwise replace it with the supplied value.
  `clear_frozen` must remove the set entry, timestamp, and ownership map entry.
  Add `fn frozen_ownership(&self, identity: &RuntimeIdentity) -> FrozenOwnership`
  that returns `ResidualUnknown` when a legacy/stale set entry has no map value.

- [ ] **Step 2: Make thaw select its plan from ownership**

  Extend `unfreeze_identity_processes` with a `FrozenOwnership` parameter.
  Validate the identity, then pass a cloned `signal_control_process(process)`
  only for `SignalOnly`; pass the original process for all other ownerships.
  Add:

  ```rust
  impl FrozenOwnership {
      fn unfreeze_backend(self, processes: &[RuntimeProcess]) -> &'static str {
          match self {
              Self::SignalOnly => "signal.cont",
              Self::CgroupBinder | Self::ResidualUnknown => backend_name(processes),
          }
      }
  }
  ```

  In `thaw_frozen_identities`, foreground thaw, and periodic wake-up thaw,
  fetch the identity ownership once, pass it into
  `unfreeze_identity_processes`, and use `unfreeze_backend` in the operation
  log.  Clear tracking only when the selected plan succeeds for every validated
  process.

- [ ] **Step 3: Record ownership at every freeze outcome**

  Update every `track_frozen` call with the transaction provenance:

  ```rust
  // Clean generic freezer success.
  state.track_frozen(identity.clone(), timestamp_ms, FrozenOwnership::CgroupBinder);

  // Direct signal policy, or Binder-unavailable decision that selected signal.
  state.track_frozen(identity.clone(), timestamp_ms, FrozenOwnership::SignalOnly);

  // A primary freezer transaction can have left work behind, a rollback failed,
  // or a post-freeze rescan requires a conservative rollback path.
  state.track_frozen(identity.clone(), timestamp_ms, FrozenOwnership::ResidualUnknown);
  ```

  In the primary-freezer failure followed by successful signal fallback, first
  record `ResidualUnknown` when `freeze_outcome.residual_possible` is true;
  the helper's merge rule prevents the later signal success from downgrading
  that uncertainty.  Use `ResidualUnknown` for failed new-PID rollback and for
  signal transaction rollback failures.  Update direct test calls to
  `track_frozen` with the ownership they model.

- [ ] **Step 4: Run the focused green suite**

  Run:

  ```bash
  cargo test --manifest-path freezeitDaemon/Cargo.toml binderless_signal_fallback_thaws_signal_only_and_refreezes_immediately -- --exact
  cargo test --manifest-path freezeitDaemon/Cargo.toml freezer_fallback_to_signal_reports_the_signal_backend -- --exact
  cargo test --manifest-path freezeitDaemon/Cargo.toml generic_cgroup_unfreeze_reports_binder_failure_after_successful_sigcont -- --exact
  cargo test --manifest-path freezeitDaemon/Cargo.toml --test freeze_unfreeze_state binderless_signal_fallback_foreground_thaw_then_background_refreezes -- --exact
  ```

  Expected: every command exits 0; the controller and integration tests observe
  `signal.cont` on foreground thaw and exactly two signal-only freezes.

- [ ] **Step 5: Commit the focused implementation**

  ```bash
  git add freezeitDaemon/src/app/controller.rs freezeitDaemon/src/sys/socket.rs freezeitDaemon/tests/integration/freeze_unfreeze_state.rs
  git commit -m "fix: reconcile signal fallback freeze ownership"
  ```

### Task 3: Preserve restart and partial-transaction safety

**Files:**
- Modify: `freezeitDaemon/src/app/controller.rs:2220-2395`
- Test: `freezeitDaemon/src/app/controller.rs:2395-2665`

- [ ] **Step 1: Write the restart recovery regression**

  Add `restart_signal_owned_stop_with_thawed_cgroup_skips_binder_unfreeze` for
  `recover_process_after_restart`.  Give the process a cgroup path and model a
  Freezeit-owned stopped process with observed `FreezeState::Thawed`.  Record
  calls from the cgroup thaw, Binder unfreeze, and SIGCONT closures.  Assert
  that only SIGCONT is called and the outcome has `signal_resumed == true` with
  no failures.

- [ ] **Step 2: Pass cgroup state into restart recovery**

  Change `recover_process_after_restart` so it receives the observed cgroup
  state as `Option<FreezeState>`.  When `signal_stopped` is true and state is
  `Some(FreezeState::Thawed)`, skip cgroup thaw and Binder unfreeze, then run
  only the existing identity-checked SIGCONT.  When state is frozen or cannot
  be read, retain the current cgroup/Binder plus SIGCONT cleanup behavior.

  In `recover_stopped_managed_processes_after_restart`, read
  `cgroup::read_freeze_state` only when a cgroup path exists and pass `.ok()`
  into the helper.  Do not treat the presence of a path as freezer ownership.

- [ ] **Step 3: Add and run the residual ownership regression**

  Add a controller unit test that starts with
  `state.track_frozen(identity, 0, FrozenOwnership::ResidualUnknown)`, gives a
  process a cgroup path, and makes the generic unfreeze closure return a Binder
  error.  Run a foreground pass and assert the log result is `partial` or
  `failed`, the next immediate background pass does not issue a new stop, and
  a recorded `SignalOnly` thaw test still clears normally.  This proves a
  successful SIGCONT cannot erase uncertain cgroup/Binder ownership.

- [ ] **Step 4: Run controller unit coverage for restart and partial safety**

  ```bash
  cargo test --manifest-path freezeitDaemon/Cargo.toml restart_signal_owned_stop_with_thawed_cgroup_skips_binder_unfreeze -- --exact
  cargo test --manifest-path freezeitDaemon/Cargo.toml residual_unknown_thaw_failure_keeps_freeze_ownership -- --exact
  cargo test --manifest-path freezeitDaemon/Cargo.toml --lib app::controller::tests
  ```

- [ ] **Step 5: Commit the safety coverage**

  ```bash
  git add freezeitDaemon/src/app/controller.rs
  git commit -m "fix: preserve conservative freezer recovery ownership"
  ```

### Task 4: Verify, package, install, release, and validate on device

**Files:**
- Modify: `magisk/module.prop`
- Modify: `freezeitApp/app/build.gradle`
- Modify: `README.md`
- Modify: `freezeitRelease/changelog.txt`
- Modify: `freezeitRelease/README.md`
- Modify: `freezeitRelease/update.json`

- [ ] **Step 1: Run the complete daemon verification suite**

  ```bash
  cargo test --manifest-path freezeitDaemon/Cargo.toml
  cargo clippy --manifest-path freezeitDaemon/Cargo.toml --all-targets -- -D warnings
  ```

  Also run the existing release metadata checks after updating the version.

- [ ] **Step 2: Bump to the next self-use release**

  Change the version consistently to `3.3.5SelfUse` and version code to
  `303005` in the module, APK Gradle configuration, README, update manifest,
  changelog, and release README.  Describe the fix as reconciliation of actual
  signal fallback ownership, including the immediate foreground-to-background
  refreeze regression.

- [ ] **Step 3: Commit and push the source release**

  ```bash
  git add magisk/module.prop freezeitApp/app/build.gradle README.md freezeitRelease/changelog.txt freezeitRelease/README.md freezeitRelease/update.json
  git commit -m "release: prepare 3.3.5SelfUse"
  git push origin main
  ```

- [ ] **Step 4: Build and validate the release ZIP**

  Use the existing signing certificate digest already configured for the prior
  released build:

  ```bash
  FREEZEIT_EXPECTED_APK_SIGNER_SHA256="$FREEZEIT_EXPECTED_APK_SIGNER_SHA256" \
    EXPECTED_VERSION=3.3.5SelfUse EXPECTED_VERSION_CODE=303005 \
    RELEASE_KIND=released scripts/build-release.sh
  scripts/validate-release-zip.sh freezeitRelease/freezeit_oneplus13_android16_selfuse_v3.3.5SelfUse_303005.zip 3.3.5SelfUse 303005
  scripts/test-release-metadata.sh released 3.3.5SelfUse 303005
  ```

- [ ] **Step 5: Publish the GitHub release and make update metadata match its artifact**

  Create annotated tag `v3.3.5SelfUse`, push it, and use `gh release create`
  to upload the validated ZIP with release notes that name the freeze ownership
  fix.  Compute its SHA-256, set `zipUrl` and `zipSha256` in
  `freezeitRelease/update.json` to that exact tag asset, commit the generated
  release metadata, and push `main` again.  Confirm `gh release view
  v3.3.5SelfUse --json tagName,assets,url` reports the ZIP and its expected
  size.

- [ ] **Step 6: Install and run the authorized real-device regression**

  With device `3B1F4LE5MS142WJY` connected, install the validated Magisk ZIP
  using the project’s existing rooted-device install path, reboot, and confirm
  the daemon reports version `3.3.5SelfUse / 303005`.  For X and Douyin, run
  this sequence twice: launch, background until SIGSTOP is observed, foreground
  and verify the same PID becomes `S`, then background before 60 seconds and
  verify a fresh SIGSTOP occurs.  Record cgroup state, process state, manager
  operation backend, and status row on each transition.  End by returning to
  the launcher and force-stopping only the two test apps.

- [ ] **Step 7: Final release evidence and commit check**

  ```bash
  git status --short
  git log --oneline -5
  git ls-remote --tags origin v3.3.5SelfUse
  gh release view v3.3.5SelfUse --repo szh1118/freezeit_cos16 --json tagName,assets,url
  ```

  Expected: a clean worktree, pushed source and release metadata commits, the
  remote tag, one validated ZIP asset, and device observations proving the
  regression sequence for both applications.
