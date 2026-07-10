#!/usr/bin/env sh
set -eu

repo_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)

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

enum_java="$repo_root/freezeitApp/app/src/main/java/io/github/jark006/freezeit/hook/Enum.java"
entry_java="$repo_root/freezeitApp/app/src/main/java/io/github/jark006/freezeit/hook/FreezeitHookEntry.java"
modern_java="$repo_root/freezeitApp/app/src/main/java/io/github/jark006/freezeit/hook/ModernHook.java"
athena_java="$repo_root/freezeitApp/app/src/main/java/io/github/jark006/freezeit/hook/app/OplusAthena.java"
signature_java="$repo_root/freezeitApp/app/src/main/java/io/github/jark006/freezeit/hook/AthenaSignatureTable.java"
legacy_java="$repo_root/freezeitApp/app/src/main/java/io/github/jark006/freezeit/hook/LegacyXposedBackend.java"
arrays_xml="$repo_root/freezeitApp/app/src/main/res/values/arrays.xml"
scope_list="$repo_root/freezeitApp/app/src/main/resources/META-INF/xposed/scope.list"

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
