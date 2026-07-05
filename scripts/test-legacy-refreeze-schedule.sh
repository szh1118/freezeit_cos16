#!/usr/bin/env sh
set -eu

freezer_hpp="freezeitVS/include/freezer.hpp"

if grep -q 'refreezeSecRemain = 3600' "$freezer_hpp"; then
  echo "legacy refreeze audit is still hard-coded to 3600 seconds" >&2
  exit 1
fi

grep -q 'STARTUP_REFREEZE_AUDIT_WINDOW_SECONDS' "$freezer_hpp" || {
  echo "missing startup refreeze audit window" >&2
  exit 1
}

grep -q 'nextRefreezeAuditSeconds' "$freezer_hpp" || {
  echo "missing centralized refreeze audit schedule" >&2
  exit 1
}

grep -q 'settings.getRefreezeTimeout()' "$freezer_hpp" || {
  echo "refreeze audit does not use configured timeout" >&2
  exit 1
}

cycle_start=$(grep -n 'void cycleThreadFunc' "$freezer_hpp" | head -n 1 | cut -d: -f1)
doze_continue_line=$(awk -v start="$cycle_start" 'NR > start && /doze\.isScreenOffStandby\) *continue/ { print NR; exit }' "$freezer_hpp")
check_unfreeze_line=$(awk -v start="$cycle_start" 'NR > start && /checkUnFreeze\(\)/ { print NR; exit }' "$freezer_hpp")

if [ -z "$doze_continue_line" ] || [ -z "$check_unfreeze_line" ]; then
  echo "missing cycle loop doze guard or refreeze audit call" >&2
  exit 1
fi

if [ "$check_unfreeze_line" -gt "$doze_continue_line" ]; then
  echo "refreeze audit is skipped during screen-off standby" >&2
  exit 1
fi

check_unfreeze_func_start=$(grep -n 'void checkUnFreeze' "$freezer_hpp" | head -n 1 | cut -d: -f1)
check_unfreeze_end=$(awk -v start="$check_unfreeze_func_start" 'NR > start && /bool mountFreezerV1/ { print NR; exit }' "$freezer_hpp")
standby_requeue_line=$(awk -v start="$check_unfreeze_func_start" -v end="$check_unfreeze_end" 'NR > start && NR < end && /doze\.isScreenOffStandby/ { seen_standby = 1 } seen_standby && /pendingHandleList\[uid\] = 1/ { print NR; exit }' "$freezer_hpp")

if [ -z "$standby_requeue_line" ]; then
  echo "screen-off standby abnormal-thaw audit does not requeue apps for freezing" >&2
  exit 1
fi

if awk -v start="$check_unfreeze_func_start" -v end="$check_unfreeze_end" 'NR > start && NR < end && /unFreezerTemporary\(naughtyApp\)/ { found = 1 } END { exit found ? 0 : 1 }' "$freezer_hpp"; then
  echo "abnormal-thaw audit still depends on foreground refresh before freezing" >&2
  exit 1
fi
