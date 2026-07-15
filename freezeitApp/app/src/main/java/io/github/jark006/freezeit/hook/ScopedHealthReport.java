package io.github.jark006.freezeit.hook;

public final class ScopedHealthReport {
    private ScopedHealthReport() {
    }

    public static String systemServer(boolean systemServerReady, boolean configReady,
                                      boolean screenReady, boolean wakeLockReady,
                                      boolean networkReady, String hookHealthJson) {
        String systemStatus = systemServerReady && configReady
                && hasActiveRootHookHealth(hookHealthJson) ? "active" : "degraded";
        String overallStatus = "degraded";
        return "{"
                + "\"status\":\"" + overallStatus + "\","
                + "\"system_control_status\":\"" + systemStatus + "\","
                + "\"athena_status\":\"unknown\","
                + "\"scopes\":{"
                + "\"system\":{\"scope_status\":\"" + systemStatus
                + "\",\"process\":\"system_server\"},"
                + "\"athena\":{\"scope_status\":\"unknown\",\"reason\":\"separate_process\"}},"
                + "\"system_server_ready\":" + systemServerReady + ','
                + "\"config_ready\":" + configReady + ','
                + "\"screen_ready\":" + screenReady + ','
                + "\"wakelock_ready\":" + wakeLockReady + ','
                + "\"network_ready\":" + networkReady + ','
                + "\"hook_health\":" + namespaceStatuses(hookHealthJson)
                + "}";
    }

    private static String namespaceStatuses(String hookHealthJson) {
        if (hookHealthJson == null || hookHealthJson.isEmpty()) return "{}";
        // The daemon classifies this payload by exact status tokens, so nested hook statuses
        // must not override the report's top-level degraded state.
        return hookHealthJson.replace("\"status\"", "\"hook_status\"");
    }

    private static boolean hasActiveRootHookHealth(String hookHealthJson) {
        if (hookHealthJson == null) return false;
        int index = 0;
        while (index < hookHealthJson.length()
                && Character.isWhitespace(hookHealthJson.charAt(index))) {
            index++;
        }
        if (index >= hookHealthJson.length() || hookHealthJson.charAt(index++) != '{') {
            return false;
        }
        while (index < hookHealthJson.length()
                && Character.isWhitespace(hookHealthJson.charAt(index))) {
            index++;
        }
        String activeStatus = "\"status\":\"active\"";
        if (!hookHealthJson.regionMatches(index, activeStatus, 0, activeStatus.length())) {
            return false;
        }
        index += activeStatus.length();
        while (index < hookHealthJson.length()
                && Character.isWhitespace(hookHealthJson.charAt(index))) {
            index++;
        }
        return index < hookHealthJson.length()
                && (hookHealthJson.charAt(index) == ',' || hookHealthJson.charAt(index) == '}');
    }
}
