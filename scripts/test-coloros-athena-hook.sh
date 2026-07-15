#!/usr/bin/env sh
set -eu

repo_root=${FREEZEIT_TEST_ROOT:-$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)}

require_text() {
  file=$1
  text=$2
  message=$3
  if ! grep -Fq "$text" "$file"; then
    echo "$message" >&2
    echo "missing text: $text" >&2
    echo "file: $file" >&2
    exit 1
  fi
}

require_line() {
  file=$1
  text=$2
  message=$3
  if ! grep -Fxq "$text" "$file"; then
    echo "$message" >&2
    echo "missing line: $text" >&2
    echo "file: $file" >&2
    exit 1
  fi
}

require_compact_text() {
  content=$1
  text=$2
  message=$3
  if ! printf '%s\n' "$content" | grep -Fq "$text"; then
    echo "$message" >&2
    echo "missing compact text: $text" >&2
    exit 1
  fi
}

require_compact_regex() {
  content=$1
  regex=$2
  message=$3
  if ! printf '%s\n' "$content" | grep -Eq "$regex"; then
    echo "$message" >&2
    echo "missing compact regex: $regex" >&2
    exit 1
  fi
}

strip_java_comments_and_literals() {
  python3 - "$1" <<'PY'
import re
import sys

source = open(sys.argv[1], encoding="utf-8").read()

# Java translates Unicode escapes before it recognizes comments or literals. Decode
# conservatively until stable so an escaped slash cannot masquerade as live code.
escape = re.compile(r"\\u+([0-9a-fA-F]{4})")
while True:
    translated = escape.sub(lambda match: chr(int(match.group(1), 16)), source)
    if translated == source:
        break
    source = translated

result = []
index = 0
state = "code"
while index < len(source):
    character = source[index]
    next_character = source[index + 1] if index + 1 < len(source) else ""

    if state == "line_comment":
        if character == "\n":
            result.append(character)
            state = "code"
        index += 1
        continue
    if state == "block_comment":
        if character == "*" and next_character == "/":
            state = "code"
            index += 2
            continue
        if character == "\n":
            result.append(character)
        index += 1
        continue
    if state in ("string", "character"):
        delimiter = '"' if state == "string" else "'"
        if character == "\\" and index + 1 < len(source):
            index += 2
            continue
        if character == delimiter:
            state = "code"
        elif character == "\n":
            result.append(character)
        index += 1
        continue

    if character == "/" and next_character == "/":
        state = "line_comment"
        index += 2
    elif character == "/" and next_character == "*":
        state = "block_comment"
        index += 2
    elif character == '"':
        state = "string"
        index += 1
    elif character == "'":
        state = "character"
        index += 1
    else:
        result.append(character)
        index += 1

sys.stdout.write("".join(result))
PY
}

extract_java_method() {
  file=$1
  signature=$2
  strip_java_comments_and_literals "$file" | awk -v signature="$signature" '
    index($0, signature) { capture = 1 }
    capture {
      print
      line = $0
      opens = gsub(/\{/, "", line)
      closes = gsub(/\}/, "", line)
      depth += opens - closes
      if (depth > 0) body_started = 1
      if (body_started && depth == 0) exit
    }
  '
}

require_unique_external_clear_hook() {
  content=$1
  hook_count=$(printf '%s\n' "$content" | awk '
    {
      line = $0
      while (match(line, /(^|[^[:alnum:]_$])hook\(/)) {
        count++
        line = substr(line, RSTART + RLENGTH)
      }
    }
    END { print count + 0 }
  ')
  [ "$hook_count" -eq 1 ] || {
    echo "External-clear strategy helper must contain exactly one hook(...) call" >&2
    exit 1
  }
}

enum_java="$repo_root/freezeitApp/app/src/main/java/io/github/jark006/freezeit/hook/Enum.java"
entry_java="$repo_root/freezeitApp/app/src/main/java/io/github/jark006/freezeit/hook/FreezeitHookEntry.java"
modern_java="$repo_root/freezeitApp/app/src/main/java/io/github/jark006/freezeit/hook/ModernHook.java"
athena_java="$repo_root/freezeitApp/app/src/main/java/io/github/jark006/freezeit/hook/app/OplusAthena.java"
signature_java="$repo_root/freezeitApp/app/src/main/java/io/github/jark006/freezeit/hook/AthenaSignatureTable.java"
legacy_java="$repo_root/freezeitApp/app/src/main/java/io/github/jark006/freezeit/hook/LegacyXposedBackend.java"
arrays_xml="$repo_root/freezeitApp/app/src/main/res/values/arrays.xml"
scope_list="$repo_root/freezeitApp/app/src/main/resources/META-INF/xposed/scope.list"

athena_registration_compact="$(extract_java_method "$athena_java" 'private static void installHookRegistrations(ClassLoader classLoader,' | tr -d '[:space:]')"
external_clear_helper_compact="$(extract_java_method "$athena_java" 'private static void hookExternalClearStrategy(ClassLoader classLoader' | tr -d '[:space:]')"

require_text "$enum_java" 'oplusAthena = "com.oplus.athena"' "Athena package constant is missing"
require_text "$arrays_xml" '<item>com.oplus.athena</item>' "Legacy Xposed recommended scope is missing Athena"
require_line "$scope_list" 'com.oplus.athena' "Modern Xposed scope list is missing Athena"

require_text "$entry_java" 'case Enum.Package.oplusAthena:' "Legacy package dispatch is missing Athena"
require_text "$entry_java" 'OplusAthena.Hook(classLoader);' "Athena hook is not dispatched"
require_text "$modern_java" 'Enum.Package.oplusAthena.equals(packageName)' "Modern hook allowlist is missing Athena"

require_text "$athena_java" 'OplusForceStopStrategy' "ForceStopStrategy hook target is missing"
require_text "$athena_java" 'OplusKillPidStrategy' "KillPidStrategy hook target is missing"
require_text "$athena_java" 'OplusKillUidStrategy' "KillUidStrategy hook target is missing"
require_text "$athena_java" 'OplusForceStopOrKillStrategy' "Fourth externalclear.c strategy hook is missing"
require_text "$athena_java" 'getForceStopOrKillValueClass()' "Fourth strategy descriptor is not ROM-specific"
require_unique_external_clear_hook "$external_clear_helper_compact"
require_compact_regex "$external_clear_helper_compact" '(^|[^[:alnum:]_$])hook\(true,classLoader,blockList\([^;]*className,Enum\.Method\.oplusExternalClear,' "External-clear strategy helper must bind its target to the blocking list callback"
for external_clear_strategy in OplusForceStopStrategy OplusKillPidStrategy OplusKillUidStrategy; do
  require_compact_text "$athena_registration_compact" "hookExternalClearStrategy(classLoader,signatures,Enum.Class.$external_clear_strategy);" "External-clear strategy $external_clear_strategy is not registered through the blocking helper"
done
require_compact_regex "$athena_registration_compact" '(^|[^[:alnum:]_$])hook\(true,classLoader,blockVoid\([^;]*Enum\.Class\.OplusForceStopOrKillStrategy,Enum\.Method\.oplusForceStopOrKill,' "ForceStopOrKillStrategy must be bound to a blocking callback"
require_text "$signature_java" 'ba3266b5aec591e5d3c16416a730489beefe327f76f0a31a1b173ceaafb028d9' "Verified CN Athena SHA is missing"
require_text "$signature_java" '332abad6eefcb6a0cd552f01542c585c2a9708abddb1264b8b706d5df15d6326' "Verified EEA Athena SHA is missing"
require_text "$signature_java" '2dd4301e118c759c9138ce615ff0da552100c91544c1b45bfbe593f364bc48a6' "Verified EU F90 Athena SHA is missing"
require_text "$signature_java" 'verifiedMethodSignatures.length == 11' "Complete Athena method signature gate is missing"
require_text "$legacy_java" 'HookHealthRegistry.recordClassResolved(hookId)' "Legacy backend does not record class resolution"
require_text "$legacy_java" 'HookHealthRegistry.recordMethodMatched(hookId)' "Legacy backend does not record method matching"
require_text "$legacy_java" 'HookHealthRegistry.recordRegistered(hookId)' "Legacy backend does not record registration"
require_text "$legacy_java" 'HookHealthRegistry.recordRuntimeInvocation(hookId)' "Legacy backend does not record runtime invocation"
require_text "$athena_java" 'Enum.Method.oplusForceStop' "Athena force-stop utility hook is missing"
require_text "$athena_java" 'Enum.Method.oplusForceStopWithFlag' "Athena force-stop flag utility hook is missing"
require_text "$athena_java" 'Enum.Method.oplusKillSimple' "Athena simple kill utility hook is missing"
require_text "$athena_java" 'Enum.Method.oplusKill' "Athena kill utility hook is missing"
require_text "$athena_java" 'Enum.Method.oplusClearActionKill' "Athena clear action kill wrapper hook is missing"
require_text "$athena_java" 'onPowerProtectPolicyChange' "GuardElf policy diagnostic hook is missing"
require_text "$athena_java" 'setGuardElfSwitch' "GuardElf switch diagnostic hook is missing"
require_text "$athena_java" 'param.setResult(new ArrayList<>())' "External clear strategy hook must return an empty list"
require_text "$athena_java" 'param.setResult(false)' "Kill utility hook must return false"

require_text "$entry_java" 'case Enum.Package.powerkeeper:' "Existing MIUI PowerKeeper dispatch was removed"
require_text "$entry_java" 'case Enum.Package.android:' "Existing Android system dispatch was removed"
