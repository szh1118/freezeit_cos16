package io.github.jark006.freezeit.hook;

public final class ScopedHealthReport {
    private ScopedHealthReport() {
    }

    public static String systemServer(boolean systemServerReady, boolean configReady,
                                      boolean screenReady, boolean wakeLockReady,
                                      boolean networkReady, String hookHealthJson) {
        String systemStatus = systemServerReady && configReady ? "active" : "degraded";
        return "{"
                + "\"status\":\"degraded\","
                + "\"system_control_status\":\"" + systemStatus + "\","
                + "\"athena_status\":\"unknown\","
                + "\"scopes\":{"
                + "\"system\":{\"status\":\"" + systemStatus + "\",\"process\":\"system_server\"},"
                + "\"athena\":{\"status\":\"unknown\",\"reason\":\"separate_process\"}},"
                + "\"system_server_ready\":" + systemServerReady + ','
                + "\"config_ready\":" + configReady + ','
                + "\"screen_ready\":" + screenReady + ','
                + "\"wakelock_ready\":" + wakeLockReady + ','
                + "\"network_ready\":" + networkReady + ','
                + "\"hook_health\":" + hookHealthJson
                + "}";
    }
}
