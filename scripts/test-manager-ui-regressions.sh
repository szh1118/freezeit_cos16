#!/usr/bin/env bash
set -euo pipefail

root="${FREEZEIT_TEST_ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)}"
home="$root/freezeitApp/app/src/main/java/io/github/jark006/freezeit/fragment/Home.java"
logcat="$root/freezeitApp/app/src/main/java/io/github/jark006/freezeit/fragment/Logcat.java"

fail=0

if tr -d '[:space:]' < "$home" | grep -Eq 'Utils\.freezeitTask(Result)?\(ManagerCmd\.getHealthReport,'; then
  echo "FAIL: Home must not call unsupported ManagerCmd.getHealthReport through a daemon socket helper."
  fail=1
fi

if ! grep -q 'binding\.forBottom\.requestFocus()' "$logcat"; then
  echo "FAIL: Logcat must focus the bottom sentinel after updating logs."
  fail=1
fi

if ! grep -q 'binding\.forBottom\.clearFocus()' "$logcat"; then
  echo "FAIL: Logcat must clear the bottom sentinel focus after auto-scroll."
  fail=1
fi

if ! grep -q 'binding\.logView\.scrollTo(0, .*scrollAmount' "$logcat"; then
  echo "FAIL: Logcat must directly scroll the log TextView to the newest line."
  fail=1
fi

if ! grep -q 'layout\.getLineTop(binding\.logView\.getLineCount())' "$logcat"; then
  echo "FAIL: Logcat must compute the bottom offset from the TextView layout."
  fail=1
fi

if grep -q 'fullScroll(View\.FOCUS_DOWN)' "$logcat"; then
  echo "FAIL: Logcat must not rely on ScrollView.fullScroll for this layout."
  fail=1
fi

exit "$fail"
