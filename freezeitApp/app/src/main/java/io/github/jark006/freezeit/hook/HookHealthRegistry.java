package io.github.jark006.freezeit.hook;

import java.util.LinkedHashMap;
import java.util.Map;

public final class HookHealthRegistry {
    private static final Map<String, HookHealth> HOOKS = new LinkedHashMap<>();
    private static String scope = "uninitialized";
    private static String process = "unknown";

    private HookHealthRegistry() {
    }

    public static synchronized void beginScope(String newScope, String newProcess) {
        String normalizedScope = safeIdentity(newScope, "unknown");
        String normalizedProcess = safeIdentity(newProcess, "unknown");
        if (!scope.equals(normalizedScope) || !process.equals(normalizedProcess)) {
            HOOKS.clear();
        }
        scope = normalizedScope;
        process = normalizedProcess;
    }

    public static synchronized void declareHook(String hookId, boolean critical) {
        get(hookId).critical = critical;
    }

    public static synchronized void recordClassResolved(String hookId) {
        get(hookId).classResolved++;
    }

    public static synchronized void recordClassResolutionFailure(String hookId, Throwable error) {
        get(hookId).fail("class_resolution", error);
    }

    public static synchronized void recordMethodMatched(String hookId) {
        get(hookId).methodMatched++;
    }

    public static synchronized void recordMethodMatchFailure(String hookId, Throwable error) {
        get(hookId).fail("method_match", error);
    }

    public static synchronized void recordRegistered(String hookId) {
        HookHealth health = get(hookId);
        health.registered++;
        health.clearFailure();
    }

    public static synchronized void recordRegistrationFailure(String hookId, Throwable error) {
        get(hookId).fail("registration", error);
    }

    public static synchronized void recordRuntimeInvocation(String hookId) {
        get(hookId).runtimeInvocations++;
    }

    public static synchronized boolean hasSuccessfulRegistration(String hookId) {
        HookHealth health = HOOKS.get(hookId);
        return health != null && health.registered > 0 && health.failureStage == null;
    }

    public static synchronized int hookCount() {
        return HOOKS.size();
    }

    public static synchronized boolean isDegraded() {
        for (HookHealth health : HOOKS.values()) {
            if (health.critical && health.failureStage != null) return true;
        }
        return false;
    }

    public static synchronized String toJson() {
        boolean degraded = false;
        int registered = 0;
        long runtimeInvocations = 0;
        StringBuilder hooks = new StringBuilder("[");
        boolean first = true;
        for (HookHealth health : HOOKS.values()) {
            if (!first) hooks.append(',');
            first = false;
            hooks.append(health.toJson());
            degraded |= health.critical && health.failureStage != null;
            registered += health.registered;
            runtimeInvocations += health.runtimeInvocations;
        }
        hooks.append(']');
        return "{"
                + "\"status\":\"" + (degraded ? "degraded" : registered == 0 ? "inactive" : "active") + "\","
                + "\"scope\":\"" + escape(scope) + "\","
                + "\"process\":\"" + escape(process) + "\","
                + "\"hook_count\":" + HOOKS.size() + ','
                + "\"registered_count\":" + registered + ','
                + "\"runtime_invocations\":" + runtimeInvocations + ','
                + "\"hooks\":" + hooks
                + '}';
    }

    static synchronized void resetForTests() {
        HOOKS.clear();
        scope = "uninitialized";
        process = "unknown";
    }

    private static HookHealth get(String hookId) {
        HookHealth health = HOOKS.get(hookId);
        if (health == null) {
            health = new HookHealth(hookId);
            HOOKS.put(hookId, health);
        }
        return health;
    }

    private static String escape(String value) {
        if (value == null) return "";
        return value.replace("\\", "\\\\").replace("\"", "\\\"")
                .replace("\n", "\\n").replace("\r", "\\r");
    }

    private static String safeIdentity(String value, String fallback) {
        return value == null || value.isEmpty() ? fallback : value;
    }

    private static final class HookHealth {
        private final String id;
        private boolean critical = true;
        private int classResolved;
        private int methodMatched;
        private int registered;
        private long runtimeInvocations;
        private String failureStage;
        private String errorType;
        private String errorMessage;

        private HookHealth(String id) {
            this.id = id;
        }

        private void fail(String stage, Throwable error) {
            failureStage = stage;
            errorType = error == null ? "unknown" : error.getClass().getName();
            errorMessage = error == null ? "" : String.valueOf(error.getMessage());
        }

        private void clearFailure() {
            failureStage = null;
            errorType = null;
            errorMessage = null;
        }

        private String toJson() {
            return "{"
                    + "\"id\":\"" + escape(id) + "\","
                    + "\"critical\":" + critical + ','
                    + "\"status\":\"" + (failureStage == null ? "active" : "degraded") + "\","
                    + "\"class_resolved\":" + classResolved + ','
                    + "\"method_matched\":" + methodMatched + ','
                    + "\"registered\":" + registered + ','
                    + "\"runtime_invocations\":" + runtimeInvocations + ','
                    + "\"stage\":\"" + escape(failureStage) + "\","
                    + "\"error_type\":\"" + escape(errorType) + "\","
                    + "\"error_message\":\"" + escape(errorMessage) + "\""
                    + '}';
        }
    }
}
