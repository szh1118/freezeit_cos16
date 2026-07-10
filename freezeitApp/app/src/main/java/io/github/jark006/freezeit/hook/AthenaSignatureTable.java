package io.github.jark006.freezeit.hook;

public final class AthenaSignatureTable {
    private static final String VERSION = "6.0.1";
    private static final String SHA_CN = "ba3266b5aec591e5d3c16416a730489beefe327f76f0a31a1b173ceaafb028d9";
    private static final String SHA_EEA = "332abad6eefcb6a0cd552f01542c585c2a9708abddb1264b8b706d5df15d6326";
    private static final String SHA_EU_F90 = "2dd4301e118c759c9138ce615ff0da552100c91544c1b45bfbe593f364bc48a6";

    private static final SignatureSet[] KNOWN_SIGNATURES = {
            new SignatureSet("cn13", VERSION, SHA_CN,
                    "oplus/ossi/ossi:16/BP2A.250605.015/1782962310967:user/release-keys",
                    "com.oplus.athena.systemservice.utils.q", "b4.l", "b4.w1", "b4.n0", "e4.z"),
            new SignatureSet("cn15", VERSION, SHA_CN,
                    "oplus/ossi/ossi:16/BP2A.250605.015/1782313553701:user/release-keys",
                    "com.oplus.athena.systemservice.utils.q", "b4.l", "b4.w1", "b4.n0", "e4.z"),
            new SignatureSet("eea13", VERSION, SHA_EEA,
                    "oplus/ossi/ossi:16/BP2A.250605.015/1764959785638:user/release-keys",
                    "com.oplus.athena.systemservice.utils.s", "n1.m", "n1.w1", "n1.o0", "q1.s"),
            new SignatureSet("eu13f90", VERSION, SHA_EU_F90,
                    "oplus/ossi/ossi:16/BP2A.250605.015/1780491741931:user/release-keys",
                    "com.oplus.athena.systemservice.utils.s", "x3.m", "x3.x1", "x3.o0", "a4.z")
    };

    private AthenaSignatureTable() {
    }

    public static Selection select(String apkVersion, String apkSha256, String romFingerprint) {
        for (SignatureSet signatureSet : KNOWN_SIGNATURES) {
            if (signatureSet.matches(apkVersion, apkSha256, romFingerprint)) {
                return signatureSet.isComplete()
                        ? Selection.allowed(signatureSet)
                        : Selection.denied("incomplete_signature_table_entry");
            }
        }
        return Selection.denied("unknown_signature:apk=" + safe(apkVersion)
                + ",sha256=" + safe(apkSha256) + ",rom=" + safe(romFingerprint));
    }

    static Selection selectForTests(SignatureSet signatureSet, String apkVersion,
                                    String apkSha256, String romFingerprint) {
        if (!signatureSet.matches(apkVersion, apkSha256, romFingerprint)) {
            return Selection.denied("unknown_signature");
        }
        return signatureSet.isComplete()
                ? Selection.allowed(signatureSet)
                : Selection.denied("incomplete_signature_table_entry");
    }

    private static String safe(String value) {
        return value == null || value.isEmpty() ? "unknown" : value;
    }

    public static final class Selection {
        private final SignatureSet signatureSet;
        private final String reason;

        private Selection(SignatureSet signatureSet, String reason) {
            this.signatureSet = signatureSet;
            this.reason = reason;
        }

        private static Selection allowed(SignatureSet signatureSet) {
            return new Selection(signatureSet, "exact_signature_match");
        }

        private static Selection denied(String reason) {
            return new Selection(null, reason);
        }

        public boolean isHookingAllowed() {
            return signatureSet != null && signatureSet.isComplete();
        }

        public SignatureSet getSignatureSet() {
            return signatureSet;
        }

        public String getReason() {
            return reason;
        }
    }

    public static final class SignatureSet {
        private final String id;
        private final String apkVersion;
        private final String apkSha256;
        private final String romFingerprint;
        private final String clearUtilsClass;
        private final String clearActionClass;
        private final String externalClearRequestClass;
        private final String externalClearContextClass;
        private final String forceStopOrKillValueClass;
        private final String[] verifiedMethodSignatures;

        private SignatureSet(String id, String apkVersion, String apkSha256, String romFingerprint,
                             String clearUtilsClass, String clearActionClass,
                             String externalClearRequestClass, String externalClearContextClass,
                             String forceStopOrKillValueClass) {
            this.id = id;
            this.apkVersion = apkVersion;
            this.apkSha256 = apkSha256;
            this.romFingerprint = romFingerprint;
            this.clearUtilsClass = clearUtilsClass;
            this.clearActionClass = clearActionClass;
            this.externalClearRequestClass = externalClearRequestClass;
            this.externalClearContextClass = externalClearContextClass;
            this.forceStopOrKillValueClass = forceStopOrKillValueClass;
            String externalParameters = "(java.util.List," + externalClearRequestClass + ','
                    + externalClearContextClass + ",com.oplus.app.athena.ClearRecord,"
                    + "com.oplus.app.athena.KeepRecord)";
            this.verifiedMethodSignatures = new String[] {
                    "com.oplus.athena.systemservice.action.prockill.clear.externalclear.ForceStopStrategy#a" + externalParameters,
                    "com.oplus.athena.systemservice.action.prockill.clear.externalclear.KillPidStrategy#a" + externalParameters,
                    "com.oplus.athena.systemservice.action.prockill.clear.externalclear.KillUidStrategy#a" + externalParameters,
                    "com.oplus.athena.systemservice.action.prockill.clear.externalclear.c#e("
                            + forceStopOrKillValueClass + ",com.oplus.app.athena.ClearRecord,"
                            + "com.oplus.athena.systemservice.action.prockill.clear.externalclear."
                            + "ForceStopStrategy$ForceStopItemResult)",
                    clearUtilsClass + "#b(android.content.Context,java.lang.String,int,int,int,java.lang.String,java.lang.String)",
                    clearUtilsClass + "#c(android.content.Context,java.lang.String,int,int,int,java.lang.String,java.lang.String,boolean)",
                    clearUtilsClass + "#d(int,int,java.lang.String,int,int,int,java.lang.String,java.lang.String)",
                    clearUtilsClass + "#e(int,int,java.lang.String,int,int,int,java.lang.String,java.lang.String,java.util.concurrent.Callable,java.util.concurrent.Callable)",
                    clearActionClass + "#h(int,int,java.lang.String,int,int,int,java.lang.String,java.lang.String,java.lang.String)",
                    "com.oplus.athena.client.action.oplusguardelf.RemoteGuardElfService$1#onPowerProtectPolicyChange(java.lang.String,int)",
                    "com.oplus.athena.client.action.oplusguardelf.RemoteGuardElfService$1#setGuardElfSwitch(boolean,java.lang.String)"
            };
        }

        static SignatureSet completeForTests(String id, String apkVersion, String apkSha256,
                                             String romFingerprint, String targetClass,
                                             String targetMethod) {
            return new SignatureSet(id, apkVersion, apkSha256, romFingerprint,
                    targetClass, targetMethod, "fixture.Request", "fixture.Context", "fixture.Value");
        }

        private boolean matches(String version, String sha256, String fingerprint) {
            return apkVersion.equals(version) && apkSha256.equals(sha256)
                    && romFingerprint.equals(fingerprint);
        }

        public boolean isComplete() {
            return !id.isEmpty() && !clearUtilsClass.isEmpty() && !clearActionClass.isEmpty()
                    && !externalClearRequestClass.isEmpty() && !externalClearContextClass.isEmpty()
                    && !forceStopOrKillValueClass.isEmpty() && verifiedMethodSignatures.length == 11;
        }

        public String getClearUtilsClass() {
            return clearUtilsClass;
        }

        public String getClearActionClass() {
            return clearActionClass;
        }

        public String getExternalClearRequestClass() {
            return externalClearRequestClass;
        }

        public String getExternalClearContextClass() {
            return externalClearContextClass;
        }

        public String getForceStopOrKillValueClass() {
            return forceStopOrKillValueClass;
        }

        public boolean hasVerifiedMethodSignature(String signature) {
            for (String verified : verifiedMethodSignatures) {
                if (verified.equals(signature)) return true;
            }
            return false;
        }

        public String[] getVerifiedMethodSignatures() {
            return verifiedMethodSignatures.clone();
        }
    }
}
