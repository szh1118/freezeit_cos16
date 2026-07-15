package io.github.jark006.freezeit.hook;

import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertFalse;
import static org.junit.Assert.assertTrue;

import org.junit.Before;
import org.junit.Test;

public class HookHealthRegistryTest {
    @Before
    public void resetRegistry() {
        HookHealthRegistry.resetForTests();
        HookHealthRegistry.beginScope("test", "test.process");
    }

    @Test
    public void recordsEveryRegistrationStageAndRuntimeCount() {
        String hookId = "athena#kill(int)";
        HookHealthRegistry.recordClassResolved(hookId);
        HookHealthRegistry.recordMethodMatched(hookId);
        HookHealthRegistry.recordRegistered(hookId);
        HookHealthRegistry.recordRuntimeInvocation(hookId);
        HookHealthRegistry.recordRuntimeInvocation(hookId);

        String json = HookHealthRegistry.toJson();
        assertTrue(json.contains("\"id\":\"athena#kill(int)\""));
        assertTrue(json.contains("\"class_resolved\":1"));
        assertTrue(json.contains("\"method_matched\":1"));
        assertTrue(json.contains("\"registered\":1"));
        assertTrue(json.contains("\"runtime_invocations\":2"));
        assertTrue(json.contains("\"status\":\"active\""));
    }

    @Test
    public void registrationFailureIsStructuredAndFailClosed() {
        HookHealthRegistry.recordClassResolutionFailure("athena#missing", new LinkageError("broken"));

        String json = HookHealthRegistry.toJson();
        assertTrue(json.contains("\"status\":\"degraded\""));
        assertTrue(json.contains("\"stage\":\"class_resolution\""));
        assertTrue(json.contains("\"error_type\":\"java.lang.LinkageError\""));
        assertFalse(HookHealthRegistry.hasSuccessfulRegistration("athena#missing"));
        assertTrue(HookHealthRegistry.isDegraded());
        assertEquals(1, HookHealthRegistry.hookCount());
    }

    @Test
    public void emptyRegistryIsNotReportedActive() {
        String json = HookHealthRegistry.toJson();

        assertTrue(json.contains("\"status\":\"inactive\""));
        assertTrue(json.contains("\"scope\":\"test\""));
        assertTrue(json.contains("\"process\":\"test.process\""));
    }

    @Test
    public void optionalFailureDoesNotDegradeCriticalHealth() {
        String optionalHook = "athena#guard-log";
        HookHealthRegistry.declareHook(optionalHook, false);
        HookHealthRegistry.recordRegistrationFailure(optionalHook, new IllegalStateException("missing"));

        String json = HookHealthRegistry.toJson();
        assertTrue(json.contains("\"status\":\"inactive\""));
        assertTrue(json.contains("\"critical\":false"));
        assertFalse(HookHealthRegistry.isDegraded());
    }

    @Test
    public void unsupportedModernBroadcastKeepsSystemControlActive() {
        HookHealthRegistry.recordRegistered("system#core");

        FreezeitHookEntry.recordUnsupportedModernBroadcast(
                new UnsupportedOperationException("not implemented"));

        String hookHealth = HookHealthRegistry.toJson();
        String report = ScopedHealthReport.systemServer(true, true, true, true, true, hookHealth);
        assertTrue(hookHealth.contains("\"critical\":false"));
        assertFalse(HookHealthRegistry.isDegraded());
        assertTrue(report.contains("\"system_control_status\":\"active\""));
    }

    @Test
    public void successfulRetryClearsPreviousFailure() {
        String criticalHook = "athena#critical";
        HookHealthRegistry.declareHook(criticalHook, true);
        HookHealthRegistry.recordRegistrationFailure(criticalHook, new IllegalStateException("first"));
        HookHealthRegistry.recordClassResolved(criticalHook);
        HookHealthRegistry.recordMethodMatched(criticalHook);
        HookHealthRegistry.recordRegistered(criticalHook);

        String json = HookHealthRegistry.toJson();
        assertTrue(json.contains("\"status\":\"active\""));
        assertTrue(json.contains("\"stage\":\"\""));
        assertFalse(HookHealthRegistry.isDegraded());
    }

    @Test
    public void systemCapabilityDoesNotPromoteUnknownAthenaToOverallActive() {
        String json = ScopedHealthReport.systemServer(true, true, true, true, true,
                "{\"status\":\"active\"}");
        assertTrue(json.contains("\"status\":\"degraded\""));
        assertTrue(json.contains("\"system_control_status\":\"active\""));
        assertTrue(json.contains("\"athena_status\":\"unknown\""));
        assertFalse(json.contains("\"athena_status\":\"active\""));
    }
}
