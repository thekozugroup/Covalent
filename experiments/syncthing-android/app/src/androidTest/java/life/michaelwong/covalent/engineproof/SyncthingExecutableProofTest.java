package life.michaelwong.covalent.engineproof;

import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertFalse;
import static org.junit.Assert.assertNotNull;
import static org.junit.Assert.assertTrue;
import static org.junit.Assert.fail;

import android.content.Context;
import android.content.pm.ApplicationInfo;
import android.content.pm.PackageManager;
import android.system.Os;
import android.system.StructStat;

import androidx.test.ext.junit.runners.AndroidJUnit4;
import androidx.test.platform.app.InstrumentationRegistry;

import org.json.JSONObject;
import org.junit.Test;
import org.junit.runner.RunWith;
import org.xml.sax.SAXException;
import org.w3c.dom.Document;
import org.w3c.dom.Element;
import org.w3c.dom.Node;
import org.w3c.dom.NodeList;

import java.io.ByteArrayOutputStream;
import java.io.ByteArrayInputStream;
import java.io.File;
import java.io.FileInputStream;
import java.io.FileOutputStream;
import java.io.IOException;
import java.io.InputStream;
import java.net.HttpURLConnection;
import java.net.InetAddress;
import java.net.ServerSocket;
import java.net.URL;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.StandardCopyOption;
import java.security.MessageDigest;
import java.security.SecureRandom;
import java.util.ArrayList;
import java.util.Arrays;
import java.util.HashMap;
import java.util.Iterator;
import java.util.List;
import java.util.Locale;
import java.util.Map;
import java.util.concurrent.TimeUnit;

import javax.xml.parsers.DocumentBuilderFactory;
import javax.xml.transform.OutputKeys;
import javax.xml.transform.Transformer;
import javax.xml.transform.TransformerFactory;
import javax.xml.transform.dom.DOMSource;
import javax.xml.transform.stream.StreamResult;

/**
 * Experimental API-37 feasibility gate for an APK-embedded Syncthing helper.
 *
 * <p>This proves only packaging, execution, private offline configuration, loopback API
 * authentication, shutdown, and identity retention. It is not a shipped engine, foreground
 * service, DocumentsProvider, sync-convergence, or release-security test.
 */
@RunWith(AndroidJUnit4.class)
public final class SyncthingExecutableProofTest {
    private static final String EXPECTED_VERSION = "v2.1.3";
    private static final int API_BODY_LIMIT = 128 * 1024;
    private static final int LOG_LIMIT = 128 * 1024;
    private static final long START_TIMEOUT_MS = 30_000;
    private static final long PROCESS_TIMEOUT_MS = 30_000;
    private static final long SHUTDOWN_TIMEOUT_MS = 20_000;

    @Test
    public void extractedHelperRunsPrivateOfflineApiAndRetainsIdentityAfterColdRestart()
            throws Exception {
        Context context = InstrumentationRegistry.getInstrumentation().getTargetContext();
        File helper = extractedHelper(context);
        assertInstalledHash(context, helper);
        assertProofManifestIsOffline(context);

        String versionOutput = runToCompletion(
                Arrays.asList(helper.getCanonicalPath(), "--version"), PROCESS_TIMEOUT_MS);
        assertTrue(versionOutput, versionOutput.contains("syncthing " + EXPECTED_VERSION));
        assertTrue(versionOutput, versionOutput.contains("android-"));

        File proofRoot = new File(context.getNoBackupFilesDir(), "syncthing-engine-proof");
        deleteRecursively(proofRoot);
        assertTrue(proofRoot.mkdirs());
        Os.chmod(proofRoot.getCanonicalPath(), 0700);
        File configDir = new File(proofRoot, "config");
        File dataDir = new File(proofRoot, "data");
        assertTrue(configDir.mkdir());
        assertTrue(dataDir.mkdir());
        Os.chmod(configDir.getCanonicalPath(), 0700);
        Os.chmod(dataDir.getCanonicalPath(), 0700);

        runToCompletion(
                Arrays.asList(
                        helper.getCanonicalPath(),
                        "--config", configDir.getCanonicalPath(),
                        "--data", dataDir.getCanonicalPath(),
                        "generate", "--no-port-probing"),
                PROCESS_TIMEOUT_MS);

        File configFile = new File(configDir, "config.xml");
        File certFile = new File(configDir, "cert.pem");
        File keyFile = new File(configDir, "key.pem");
        assertTrue(configFile.isFile());
        assertTrue(certFile.isFile());
        assertTrue(keyFile.isFile());
        assertTrue(configFile.getCanonicalPath().startsWith(proofRoot.getCanonicalPath() + File.separator));
        String certHashBefore = sha256(certFile);

        int port = reserveLoopbackPort();
        String apiKey = randomHex(32);
        writeOfflineConfig(configFile, port, apiKey);
        assertOfflineConfig(configFile, port, apiKey);

        RunningProcess first = null;
        RunningProcess second = null;
        try {
            first = startServer(helper, configDir, dataDir, apiKey);
            JSONObject firstStatus = awaitAuthenticatedStatus(first, port, apiKey);
            String deviceId = firstStatus.getString("myID");
            assertFalse(deviceId.isEmpty());
            assertEquals(EXPECTED_VERSION, authenticatedVersion(port, apiKey));
            assertUnauthenticatedRejected(port);
            gracefulShutdown(first, port, apiKey);
            first = null;
            assertLoopbackPortReleased(port);
            assertOfflineConfig(configFile, port, apiKey);

            second = startServer(helper, configDir, dataDir, apiKey);
            JSONObject secondStatus = awaitAuthenticatedStatus(second, port, apiKey);
            assertEquals(deviceId, secondStatus.getString("myID"));
            assertEquals(certHashBefore, sha256(certFile));
            assertUnauthenticatedRejected(port);
            gracefulShutdown(second, port, apiKey);
            second = null;
            assertLoopbackPortReleased(port);
            assertOfflineConfig(configFile, port, apiKey);
        } finally {
            if (first != null) first.forceStop();
            if (second != null) second.forceStop();
            deleteRecursively(proofRoot);
        }
    }

    private static File extractedHelper(Context context) throws Exception {
        ApplicationInfo info = context.getApplicationInfo();
        assertTrue(
                "APK must request package-manager native-library extraction",
                (info.flags & ApplicationInfo.FLAG_EXTRACT_NATIVE_LIBS) != 0);
        File nativeDir = new File(info.nativeLibraryDir).getCanonicalFile();
        File helper = new File(nativeDir, "libsyncthing.so").getCanonicalFile();
        assertTrue(helper.isFile());
        assertTrue(helper.canExecute());
        assertFalse("Installed helper must be immutable to the app UID", helper.canWrite());
        assertTrue(helper.getPath().startsWith(nativeDir.getPath() + File.separator));
        assertFalse(
                "Executable must not be copied into writable app data",
                helper.getPath().startsWith(context.getApplicationInfo().dataDir + File.separator));
        return helper;
    }

    private static void assertInstalledHash(Context context, File helper) throws Exception {
        Map<String, BuildRecord> records = new HashMap<>();
        try (InputStream input = context.getAssets().open("syncthing-sha256.txt")) {
            String text = new String(readBounded(input, 4096), StandardCharsets.US_ASCII);
            for (String line : text.split("\\n")) {
                if (line.trim().isEmpty()) continue;
                String[] fields = line.trim().split(" +");
                assertEquals(3, fields.length);
                records.put(fields[0], new BuildRecord(fields[1], Long.parseLong(fields[2])));
            }
        }
        String abi = android.os.Build.SUPPORTED_ABIS[0];
        BuildRecord expected = records.get(abi);
        assertNotNull("No build record for installed ABI " + abi, expected);
        assertEquals(expected.size, helper.length());
        assertEquals(expected.sha256, sha256(helper));
    }

    private static void assertProofManifestIsOffline(Context context) throws Exception {
        String[] requested = context.getPackageManager()
                .getPackageInfo(context.getPackageName(), PackageManager.GET_PERMISSIONS)
                .requestedPermissions;
        List<String> permissions = requested == null
                ? new ArrayList<>()
                : Arrays.asList(requested);
        assertTrue(permissions.contains(android.Manifest.permission.INTERNET));
        assertFalse(permissions.contains("android.permission.ACCESS_LOCAL_NETWORK"));
    }

    private static RunningProcess startServer(
            File helper,
            File configDir,
            File dataDir,
            String apiKey) throws Exception {
        List<String> command = Arrays.asList(
                helper.getCanonicalPath(),
                "--config", configDir.getCanonicalPath(),
                "--data", dataDir.getCanonicalPath(),
                "serve",
                "--no-browser",
                "--no-port-probing",
                "--no-restart",
                "--no-upgrade",
                "--log-file=-",
                "--log-max-size=0");
        for (String argument : command) assertFalse(argument.contains(apiKey));
        ProcessBuilder builder = new ProcessBuilder(command).redirectErrorStream(true);
        scrubSyncthingEnvironment(builder.environment(), apiKey);
        // Tagged main.go otherwise enters monitorMain(), which owns a second engine process.
        // This non-secret sentinel selects syncthingMain() directly so the returned Process is
        // the socket/database owner that the Android service would have to reap.
        builder.environment().put("STMONITORED", "1");
        assertEquals("1", builder.environment().get("STMONITORED"));
        return new RunningProcess(builder.start(), apiKey);
    }

    private static void scrubSyncthingEnvironment(Map<String, String> environment, String apiKey) {
        Iterator<Map.Entry<String, String>> iterator = environment.entrySet().iterator();
        while (iterator.hasNext()) {
            Map.Entry<String, String> entry = iterator.next();
            if (entry.getKey().toUpperCase(Locale.ROOT).startsWith("ST")
                    || (!apiKey.isEmpty() && apiKey.equals(entry.getValue()))) {
                iterator.remove();
            }
        }
        for (Map.Entry<String, String> entry : environment.entrySet()) {
            assertFalse(entry.getKey().toUpperCase(Locale.ROOT).startsWith("ST"));
            if (!apiKey.isEmpty()) assertFalse(apiKey.equals(entry.getValue()));
        }
    }

    private static JSONObject awaitAuthenticatedStatus(
            RunningProcess process, int port, String apiKey) throws Exception {
        long deadline = System.nanoTime() + TimeUnit.MILLISECONDS.toNanos(START_TIMEOUT_MS);
        IOException last = null;
        while (System.nanoTime() < deadline) {
            if (!process.process.isAlive()) {
                fail("Syncthing exited during startup: " + process.safeLog(apiKey));
            }
            try {
                HttpResult response = request(port, "/rest/system/status", apiKey, "GET");
                if (response.code == 200) return new JSONObject(response.body);
                last = new IOException("status endpoint returned " + response.code);
            } catch (IOException error) {
                last = error;
            }
            Thread.sleep(100);
        }
        throw new IOException("Timed out waiting for authenticated loopback API", last);
    }

    private static String authenticatedVersion(int port, String apiKey) throws Exception {
        HttpResult result = request(port, "/rest/system/version", apiKey, "GET");
        assertEquals(200, result.code);
        return new JSONObject(result.body).getString("version");
    }

    private static void assertUnauthenticatedRejected(int port) throws Exception {
        HttpResult result = request(port, "/rest/system/status", null, "GET");
        assertEquals(403, result.code);
    }

    private static void gracefulShutdown(
            RunningProcess process, int port, String apiKey) throws Exception {
        HttpResult result = request(port, "/rest/system/shutdown", apiKey, "POST");
        assertEquals(200, result.code);
        if (!process.process.waitFor(SHUTDOWN_TIMEOUT_MS, TimeUnit.MILLISECONDS)) {
            fail("Syncthing did not stop after authenticated shutdown: " + process.safeLog(apiKey));
        }
        assertEquals("Syncthing shutdown exit code", 0, process.process.exitValue());
        process.finishCollector();
        assertFalse("Syncthing log exposed the API credential", process.containsRaw(apiKey));
    }

    private static HttpResult request(
            int port, String path, String apiKey, String method) throws Exception {
        HttpURLConnection connection = (HttpURLConnection) new URL(
                "http://127.0.0.1:" + port + path).openConnection();
        connection.setConnectTimeout(1000);
        connection.setReadTimeout(3000);
        connection.setRequestMethod(method);
        connection.setUseCaches(false);
        if (apiKey != null) connection.setRequestProperty("X-API-Key", apiKey);
        try {
            if ("POST".equals(method)) {
                connection.setDoOutput(true);
                connection.setFixedLengthStreamingMode(0);
            }
            int code = connection.getResponseCode();
            InputStream body = code >= 400
                    ? connection.getErrorStream()
                    : connection.getInputStream();
            String text = body == null
                    ? ""
                    : new String(readBounded(body, API_BODY_LIMIT), StandardCharsets.UTF_8);
            return new HttpResult(code, text);
        } finally {
            connection.disconnect();
        }
    }

    private static void writeOfflineConfig(File configFile, int port, String apiKey)
            throws Exception {
        Document document = parseConfig(configFile);
        Element root = document.getDocumentElement();
        for (Element folder : directChildren(root, "folder")) root.removeChild(folder);

        Element gui = requireDirectChild(root, "gui");
        gui.setAttribute("enabled", "true");
        gui.setAttribute("tls", "false");
        setSingleChild(document, gui, "address", "127.0.0.1:" + port);
        setSingleChild(document, gui, "apikey", apiKey);
        setSingleChild(document, gui, "insecureAdminAccess", "false");
        setSingleChild(document, gui, "insecureSkipHostcheck", "false");
        setSingleChild(document, gui, "metricsWithoutAuth", "false");

        Element options = requireDirectChild(root, "options");
        setSingleChild(document, options, "listenAddress", "");
        setSingleChild(document, options, "globalAnnounceServer", "");
        setSingleChild(document, options, "stunServer", "");
        setSingleChild(document, options, "globalAnnounceEnabled", "false");
        setSingleChild(document, options, "localAnnounceEnabled", "false");
        setSingleChild(document, options, "relaysEnabled", "false");
        setSingleChild(document, options, "natEnabled", "false");
        setSingleChild(document, options, "announceLANAddresses", "false");
        setSingleChild(document, options, "startBrowser", "false");
        setSingleChild(document, options, "autoUpgradeIntervalH", "0");
        setSingleChild(document, options, "urAccepted", "-1");
        setSingleChild(document, options, "crashReportingEnabled", "false");

        File temporary = new File(configFile.getParentFile(), "config.xml.proof-new");
        assertFalse(temporary.exists());
        TransformerFactory factory = TransformerFactory.newInstance();
        Transformer transformer = factory.newTransformer();
        transformer.setOutputProperty(OutputKeys.INDENT, "yes");
        try (FileOutputStream output = new FileOutputStream(temporary)) {
            transformer.transform(new DOMSource(document), new StreamResult(output));
            output.getFD().sync();
        }
        Os.chmod(temporary.getCanonicalPath(), 0600);
        Files.move(
                temporary.toPath(),
                configFile.toPath(),
                StandardCopyOption.ATOMIC_MOVE,
                StandardCopyOption.REPLACE_EXISTING);
        Os.chmod(configFile.getCanonicalPath(), 0600);
    }

    private static void assertOfflineConfig(File configFile, int port, String apiKey)
            throws Exception {
        Document document = parseConfig(configFile);
        Element root = document.getDocumentElement();
        assertEquals(0, directChildren(root, "folder").size());
        assertEquals(1, directChildren(root, "device").size());
        Element gui = requireDirectChild(root, "gui");
        assertTrueOrAbsentAttribute(gui, "enabled");
        assertFalseOrAbsentAttribute(gui, "tls");
        assertEquals("127.0.0.1:" + port, childText(gui, "address"));
        assertTrue("Config API credential changed", apiKey.equals(childText(gui, "apikey")));
        assertFalseOrAbsentChild(gui, "insecureAdminAccess");
        assertFalseOrAbsentChild(gui, "insecureSkipHostcheck");
        assertFalseOrAbsentChild(gui, "metricsWithoutAuth");

        Element options = requireDirectChild(root, "options");
        assertAllBlank(options, "listenAddress");
        assertAllBlank(options, "globalAnnounceServer");
        assertAllBlank(options, "stunServer");
        assertEquals("false", childText(options, "globalAnnounceEnabled"));
        assertEquals("false", childText(options, "localAnnounceEnabled"));
        assertEquals("false", childText(options, "relaysEnabled"));
        assertEquals("false", childText(options, "natEnabled"));
        assertEquals("false", childText(options, "announceLANAddresses"));
        assertEquals("false", childText(options, "startBrowser"));
        assertEquals("0", childText(options, "autoUpgradeIntervalH"));
        assertEquals("-1", childText(options, "urAccepted"));
        assertEquals("false", childText(options, "crashReportingEnabled"));
    }

    private static Document parseConfig(File configFile) throws Exception {
        assertTrue(configFile.isFile());
        assertTrue("Config unexpectedly large", configFile.length() <= 1024 * 1024);
        StructStat stat = Os.stat(configFile.getCanonicalPath());
        assertEquals("Config must not grant group/other permissions", 0, stat.st_mode & 0077);
        byte[] bytes;
        try (InputStream input = new FileInputStream(configFile)) {
            bytes = readBounded(input, 1024 * 1024);
        }
        String lexical = new String(bytes, StandardCharsets.UTF_8).toUpperCase(Locale.ROOT);
        if (lexical.contains("<!DOCTYPE") || lexical.contains("<!ENTITY")) {
            throw new SAXException("DTD/entity declarations are forbidden in proof config");
        }
        DocumentBuilderFactory factory = DocumentBuilderFactory.newInstance();
        factory.setNamespaceAware(false);
        factory.setExpandEntityReferences(false);
        javax.xml.parsers.DocumentBuilder builder = factory.newDocumentBuilder();
        builder.setEntityResolver((publicId, systemId) -> {
            throw new SAXException("External XML entities are forbidden in proof config");
        });
        return builder.parse(new ByteArrayInputStream(bytes));
    }

    private static Element requireDirectChild(Element parent, String name) {
        List<Element> matches = directChildren(parent, name);
        assertEquals("Expected exactly one " + name, 1, matches.size());
        return matches.get(0);
    }

    private static List<Element> directChildren(Element parent, String name) {
        List<Element> result = new ArrayList<>();
        NodeList children = parent.getChildNodes();
        for (int index = 0; index < children.getLength(); index++) {
            Node child = children.item(index);
            if (child instanceof Element && name.equals(child.getNodeName())) {
                result.add((Element) child);
            }
        }
        return result;
    }

    private static void setSingleChild(
            Document document, Element parent, String name, String value) {
        for (Element existing : directChildren(parent, name)) parent.removeChild(existing);
        Element child = document.createElement(name);
        child.setTextContent(value);
        parent.appendChild(child);
    }

    private static String childText(Element parent, String name) {
        return requireDirectChild(parent, name).getTextContent().trim();
    }

    private static void assertFalseOrAbsentChild(Element parent, String name) {
        List<Element> values = directChildren(parent, name);
        assertTrue("Expected zero or one " + name, values.size() <= 1);
        if (!values.isEmpty()) assertEquals("false", values.get(0).getTextContent().trim());
    }

    private static void assertTrueOrAbsentAttribute(Element parent, String name) {
        String value = parent.getAttribute(name).trim();
        assertTrue(value.isEmpty() || "true".equals(value));
    }

    private static void assertFalseOrAbsentAttribute(Element parent, String name) {
        String value = parent.getAttribute(name).trim();
        assertTrue(value.isEmpty() || "false".equals(value));
    }

    private static void assertAllBlank(Element parent, String name) {
        List<Element> values = directChildren(parent, name);
        assertFalse("Expected explicit disabled " + name, values.isEmpty());
        for (Element value : values) assertTrue(value.getTextContent().trim().isEmpty());
    }

    private static int reserveLoopbackPort() throws Exception {
        try (ServerSocket socket = new ServerSocket(0, 1, InetAddress.getByName("127.0.0.1"))) {
            return socket.getLocalPort();
        }
    }

    private static void assertLoopbackPortReleased(int port) throws Exception {
        try (ServerSocket ignored = new ServerSocket(
                port, 1, InetAddress.getByName("127.0.0.1"))) {
            // Exact immediate bind is the assertion.
        }
    }

    private static String runToCompletion(List<String> command, long timeoutMs) throws Exception {
        ProcessBuilder builder = new ProcessBuilder(command).redirectErrorStream(true);
        scrubSyncthingEnvironment(builder.environment(), "");
        Process process = builder.start();
        RunningProcess running = new RunningProcess(process, "");
        try {
            if (!process.waitFor(timeoutMs, TimeUnit.MILLISECONDS)) {
                fail("Process timed out: " + running.safeLog(""));
            }
            running.finishCollector();
            assertEquals(running.safeLog(""), 0, process.exitValue());
            return running.safeLog("");
        } finally {
            running.forceStop();
        }
    }

    private static byte[] readBounded(InputStream input, int limit) throws IOException {
        try (InputStream source = input; ByteArrayOutputStream output = new ByteArrayOutputStream()) {
            byte[] buffer = new byte[8192];
            int total = 0;
            while (true) {
                int count = source.read(buffer);
                if (count < 0) break;
                total += count;
                if (total > limit) throw new IOException("Bounded response exceeded limit");
                output.write(buffer, 0, count);
            }
            return output.toByteArray();
        }
    }

    private static String sha256(File file) throws Exception {
        MessageDigest digest = MessageDigest.getInstance("SHA-256");
        try (InputStream input = new FileInputStream(file)) {
            byte[] buffer = new byte[64 * 1024];
            int count;
            while ((count = input.read(buffer)) >= 0) digest.update(buffer, 0, count);
        }
        StringBuilder result = new StringBuilder(64);
        for (byte value : digest.digest()) {
            result.append(String.format(Locale.ROOT, "%02x", value & 0xff));
        }
        return result.toString();
    }

    private static String randomHex(int byteCount) {
        byte[] bytes = new byte[byteCount];
        new SecureRandom().nextBytes(bytes);
        StringBuilder result = new StringBuilder(byteCount * 2);
        for (byte value : bytes) {
            result.append(String.format(Locale.ROOT, "%02x", value & 0xff));
        }
        Arrays.fill(bytes, (byte) 0);
        return result.toString();
    }

    private static void deleteRecursively(File file) throws IOException {
        if (!file.exists()) return;
        if (Files.isSymbolicLink(file.toPath())) throw new IOException("Refusing test symlink cleanup");
        File[] children = file.listFiles();
        if (children != null) {
            for (File child : children) deleteRecursively(child);
        }
        if (!file.delete()) throw new IOException("Could not clean proof fixture");
    }

    private static final class RunningProcess {
        private final Process process;
        private final ByteArrayOutputStream log = new ByteArrayOutputStream();
        private final Thread collector;
        private final String forbidden;
        private volatile boolean forbiddenObserved;

        RunningProcess(Process process, String forbidden) {
            this.process = process;
            this.forbidden = forbidden;
            collector = new Thread(() -> {
                byte[] buffer = new byte[4096];
                String tail = "";
                try (InputStream input = process.getInputStream()) {
                    while (true) {
                        int count = input.read(buffer);
                        if (count < 0) return;
                        if (!forbidden.isEmpty()) {
                            String chunk = tail + new String(
                                    buffer, 0, count, StandardCharsets.UTF_8);
                            if (chunk.contains(forbidden)) forbiddenObserved = true;
                            int retainedTail = Math.min(forbidden.length() - 1, chunk.length());
                            tail = chunk.substring(chunk.length() - retainedTail);
                        }
                        synchronized (log) {
                            int retained = Math.min(count, Math.max(0, LOG_LIMIT - log.size()));
                            if (retained > 0) log.write(buffer, 0, retained);
                        }
                    }
                } catch (IOException ignored) {
                    // The owning assertion reports process state and retained bounded output.
                }
            }, "syncthing-proof-log");
            collector.setDaemon(true);
            collector.start();
        }

        void finishCollector() throws InterruptedException {
            collector.join(5000);
            assertFalse("Log collector did not finish", collector.isAlive());
        }

        String safeLog(String forbidden) {
            synchronized (log) {
                String text = new String(log.toByteArray(), StandardCharsets.UTF_8);
                if ((!forbidden.isEmpty() && text.contains(forbidden)) || forbiddenObserved) {
                    return "[log contained forbidden API credential and was redacted]";
                }
                return text;
            }
        }

        boolean containsRaw(String value) {
            synchronized (log) {
                return forbiddenObserved || (!value.isEmpty()
                        && new String(log.toByteArray(), StandardCharsets.UTF_8).contains(value));
            }
        }

        void forceStop() throws InterruptedException {
            if (process.isAlive()) {
                process.destroy();
                if (!process.waitFor(2, TimeUnit.SECONDS)) {
                    process.destroyForcibly();
                    process.waitFor(5, TimeUnit.SECONDS);
                }
            }
            collector.join(1000);
            assertFalse("Proof process was not reaped", process.isAlive());
            assertFalse("Proof log collector was not reaped", collector.isAlive());
        }
    }

    private static final class HttpResult {
        final int code;
        final String body;

        HttpResult(int code, String body) {
            this.code = code;
            this.body = body;
        }
    }

    private static final class BuildRecord {
        final String sha256;
        final long size;

        BuildRecord(String sha256, long size) {
            this.sha256 = sha256;
            this.size = size;
        }
    }
}
