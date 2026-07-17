package io.github.jark006.freezeit;

import static org.junit.Assert.assertFalse;
import static org.junit.Assert.assertTrue;

import org.junit.Test;

import java.io.IOException;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.List;

public class ManagerRequestIsolationContractTest {
    private static final List<String> REQUEST_CLIENTS = List.of(
            "activity/AppTime.java",
            "activity/Settings.java",
            "fragment/Config.java",
            "fragment/Home.java",
            "fragment/Logcat.java"
    );

    @Test
    public void managerClientsUseRequestPrivateTaskResults() throws IOException {
        for (String relativePath : REQUEST_CLIENTS) {
            String source = readSource(mainJavaRoot().resolve(relativePath));
            assertFalse(relativePath, source.contains("StaticData.response"));
            assertFalse(relativePath, source.contains("Utils.freezeitTask("));
            assertTrue(relativePath, source.contains("Utils.freezeitTaskResult("));
        }
    }

    @Test
    public void asynchronousPagesDeliverPayloadThroughMessages() throws IOException {
        String home = readSource(mainJavaRoot().resolve("fragment/Home.java"));
        String logcat = readSource(mainJavaRoot().resolve("fragment/Logcat.java"));
        String appTime = readSource(mainJavaRoot().resolve("activity/AppTime.java"));

        assertTrue(home.contains("msg.obj"));
        assertTrue(logcat.contains("msg.obj"));
        assertTrue(appTime.contains("msg.obj"));
    }

    @Test
    public void managerTransportUsesAnAuthenticatedAbstractUnixSocket() throws IOException {
        String utils = readSource(mainJavaRoot().resolve("Utils.java"));

        assertTrue(utils.contains("LocalSocket"));
        assertTrue(utils.contains("LocalSocketAddress.Namespace.ABSTRACT"));
        assertTrue(utils.contains("FreezeitManager"));
        assertFalse(utils.contains("InetSocketAddress"));
    }

    @Test
    public void updateDownloadUsesThePublishedArchiveDigestAndBoundedFetcher() throws IOException {
        String home = readSource(mainJavaRoot().resolve("fragment/Home.java"));
        String staticData = readSource(mainJavaRoot().resolve("StaticData.java"));

        assertTrue(staticData.contains("zipSha256"));
        assertTrue(home.contains("zipSha256"));
        assertTrue(home.contains("ReleaseArchiveVerifier"));
        assertFalse(home.contains("ByteArrayOutputStream"));
    }

    private static Path mainJavaRoot() {
        Path workingDirectory = Path.of(System.getProperty("user.dir"));
        Path moduleRelative = Path.of("src/main/java/io/github/jark006/freezeit");
        for (Path prefix : List.of(
                Path.of(""),
                Path.of("app"),
                Path.of("freezeitApp/app")
        )) {
            Path candidate = workingDirectory.resolve(prefix).resolve(moduleRelative);
            if (Files.isDirectory(candidate))
                return candidate;
        }
        throw new IllegalStateException("Cannot locate Manager Java sources from " + workingDirectory);
    }

    private static String readSource(Path path) throws IOException {
        return new String(Files.readAllBytes(path), StandardCharsets.UTF_8);
    }
}
