package io.github.jark006.freezeit.hook;

import static org.junit.Assert.assertFalse;
import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertTrue;

import org.junit.Test;

import java.io.BufferedReader;
import java.io.InputStreamReader;
import java.nio.charset.StandardCharsets;

public class AthenaSignatureTableTest {
    private static final String[] CURRENT_ROM_FIXTURES = {
            "rom/fingerprint/one", "rom/fingerprint/two",
            "rom/fingerprint/three", "rom/fingerprint/four"
    };

    @Test
    public void athena601WithUnknownFullSignatureAlwaysFailsClosed() {
        for (String romFingerprint : CURRENT_ROM_FIXTURES) {
            AthenaSignatureTable.Selection selection = AthenaSignatureTable.select(
                    "6.0.1", "apk-sha256-unknown", romFingerprint);
            assertFalse(selection.isHookingAllowed());
            assertTrue(selection.getReason().contains("unknown_signature"));
        }
    }

    @Test
    public void exactCompleteCompositeSignatureCanBeSelected() {
        AthenaSignatureTable.SignatureSet signatureSet = AthenaSignatureTable.SignatureSet.completeForTests(
                "fixture", "6.0.1", "apk-sha256", "rom/fingerprint", "example.Target", "run");
        AthenaSignatureTable.Selection selection = AthenaSignatureTable.selectForTests(
                signatureSet, "6.0.1", "apk-sha256", "rom/fingerprint");

        assertTrue(selection.isHookingAllowed());
        assertTrue(selection.getSignatureSet().isComplete());
    }

    @Test
    public void verifiedCn13SignatureSelectsCompleteProductionEntry() {
        AthenaSignatureTable.Selection selection = AthenaSignatureTable.select(
                "6.0.1",
                "ba3266b5aec591e5d3c16416a730489beefe327f76f0a31a1b173ceaafb028d9",
                "oplus/ossi/ossi:16/BP2A.250605.015/1782962310967:user/release-keys");

        assertTrue(selection.isHookingAllowed());
        assertEquals("com.oplus.athena.systemservice.utils.q",
                selection.getSignatureSet().getClearUtilsClass());
        assertEquals("b4.l", selection.getSignatureSet().getClearActionClass());
        assertEquals("b4.n0", selection.getSignatureSet().getExternalClearContextClass());
        assertEquals(11, selection.getSignatureSet().getVerifiedMethodSignatures().length);
        assertEquals("com.oplus.athena.systemservice.utils.q#e(int,int,java.lang.String,int,int,int,java.lang.String,java.lang.String,java.util.concurrent.Callable,java.util.concurrent.Callable)",
                selection.getSignatureSet().getVerifiedMethodSignatures()[7]);
    }

    @Test
    public void sameApkOnUnverifiedFingerprintFailsClosed() {
        AthenaSignatureTable.Selection selection = AthenaSignatureTable.select(
                "6.0.1",
                "ba3266b5aec591e5d3c16416a730489beefe327f76f0a31a1b173ceaafb028d9",
                "unknown/fingerprint");

        assertFalse(selection.isHookingAllowed());
    }

    @Test
    public void allFourRomFixturesExposeExactFourthStrategyDescriptor() throws Exception {
        try (BufferedReader reader = new BufferedReader(new InputStreamReader(
                getClass().getResourceAsStream("/athena/rom-signatures.tsv"), StandardCharsets.UTF_8))) {
            String line;
            int fixtureCount = 0;
            while ((line = reader.readLine()) != null) {
                String[] fixture = line.split("\\t");
                AthenaSignatureTable.Selection selection = AthenaSignatureTable.select(
                        fixture[1], fixture[2], fixture[3]);
                assertTrue(fixture[0], selection.isHookingAllowed());
                AthenaSignatureTable.SignatureSet signatures = selection.getSignatureSet();
                assertEquals(fixture[4], signatures.getClearUtilsClass());
                assertEquals(fixture[5], signatures.getClearActionClass());
                assertEquals(fixture[6], signatures.getExternalClearRequestClass());
                assertEquals(fixture[7], signatures.getExternalClearContextClass());
                assertEquals(fixture[8], signatures.getForceStopOrKillValueClass());
                assertTrue(signatures.hasVerifiedMethodSignature(
                        "com.oplus.athena.systemservice.action.prockill.clear.externalclear.c#e("
                                + fixture[8] + ",com.oplus.app.athena.ClearRecord,"
                                + "com.oplus.athena.systemservice.action.prockill.clear.externalclear."
                                + "ForceStopStrategy$ForceStopItemResult)"));
                fixtureCount++;
            }
            assertEquals(4, fixtureCount);
        }
    }
}
