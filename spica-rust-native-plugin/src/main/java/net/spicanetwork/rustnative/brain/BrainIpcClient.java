package net.spicanetwork.rustnative.brain;

import com.google.gson.JsonElement;
import com.google.gson.JsonObject;
import com.google.gson.JsonParser;

import javax.crypto.Mac;
import javax.crypto.spec.SecretKeySpec;
import java.io.BufferedInputStream;
import java.io.BufferedWriter;
import java.io.ByteArrayOutputStream;
import java.io.IOException;
import java.io.InputStream;
import java.io.OutputStreamWriter;
import java.net.InetSocketAddress;
import java.net.Socket;
import java.net.SocketTimeoutException;
import java.nio.charset.StandardCharsets;
import java.security.GeneralSecurityException;
import java.security.MessageDigest;
import java.security.SecureRandom;
import java.util.HexFormat;
import java.util.concurrent.atomic.AtomicBoolean;
import java.util.concurrent.atomic.AtomicReference;
import java.util.logging.Logger;

/**
 * Non-blocking asynchronous NDJSON TCP client connecting Purpur 1.21.4 to the external
 * {@code spica-launcher} Rust Brain daemon.
 *
 * Guarantees:
 * - Never blocks the Minecraft Main Thread (single-slot {@link AtomicReference} telemetry handoff,
 *   non-blocking {@link #shutdown()})
 * - Bounded memory and wall-clock framing (64 KiB max line length, 5s Slowloris frame deadline,
 *   partial-frame preservation across {@code SO_TIMEOUT} polls)
 * - Mutual cryptographic authentication: reads token from {@code SPICA_IPC_TOKEN} env var (fallback
 *   {@code -Dspica.ipc.token}), sends a fresh CSPRNG {@code nonce}, and verifies the launcher's
 *   HMAC-SHA256 {@code proof} in {@code hello_ack} in constant time before accepting directives
 * - Automatic reconnect every 5 seconds or immediately on stale directives
 */
public final class BrainIpcClient {

    public static final String PLUGIN_VERSION = "2.6.0";
    public static final String SERVER_FLAVOR = "purpur-1.21.4";

    private static final int CONNECT_TIMEOUT_MS = 3_000;
    private static final int READ_TIMEOUT_MS = 150;
    private static final int HANDSHAKE_ACK_TIMEOUT_MS = 5_000;
    private static final long FRAME_READ_DEADLINE_NANOS = 5_000_000_000L;
    private static final int MAX_INCOMING_LINE_BYTES = 64 * 1024;
    private static final long RECONNECT_DELAY_MS = 5_000L;

    private final String host;
    private final int port;
    private final String token;
    private final boolean enabled;
    private final Logger logger;
    private final SecureRandom secureRandom = new SecureRandom();

    private final AtomicBoolean running = new AtomicBoolean(false);
    private final AtomicBoolean connected = new AtomicBoolean(false);
    private final AtomicReference<String> pendingTelemetry = new AtomicReference<>(null);
    private final AtomicReference<BrainDirective> latestDirective = new AtomicReference<>(null);
    private final AtomicReference<Socket> activeSocket = new AtomicReference<>(null);

    private volatile Thread workerThread;
    private volatile long lastDirectiveTimestampMs = 0L;

    public BrainIpcClient(String fallbackHost, int fallbackPort, Logger logger) {
        this.logger = logger;

        String propHost = System.getProperty("spica.ipc.host");
        if (propHost != null && !propHost.isBlank()) {
            this.host = normalizeHost(propHost);
        } else if (fallbackHost != null && !fallbackHost.isBlank()) {
            this.host = normalizeHost(fallbackHost);
        } else {
            this.host = "127.0.0.1";
        }

        int resolvedPort = 0;
        String propPort = System.getProperty("spica.ipc.port");
        if (propPort != null && !propPort.isBlank()) {
            try {
                resolvedPort = Integer.parseInt(propPort.trim());
            } catch (NumberFormatException ignored) {
                resolvedPort = 0;
            }
        }
        if (resolvedPort <= 0) {
            resolvedPort = fallbackPort;
        }
        this.port = (resolvedPort > 0 && resolvedPort <= 65535) ? resolvedPort : 0;

        // Y7: Prefer process environment variable (not exposed in /proc/<pid>/cmdline or ps/WMI);
        // keep -Dspica.ipc.token only as backwards-compatible standalone fallback.
        String envToken = System.getenv("SPICA_IPC_TOKEN");
        if (envToken != null && !envToken.isBlank()) {
            this.token = envToken.trim();
        } else {
            String propToken = System.getProperty("spica.ipc.token");
            this.token = propToken != null ? propToken.trim() : "";
        }

        this.enabled = this.port > 0;
    }

    private static String normalizeHost(String raw) {
        String trimmed = raw.trim();
        if (trimmed.startsWith("[") && trimmed.endsWith("]") && trimmed.length() >= 2) {
            return trimmed.substring(1, trimmed.length() - 1);
        }
        return trimmed;
    }

    public synchronized void start() {
        if (!enabled) {
            logger.info("[SpicaRustNative] Launcher IPC devre disi (port=0). In-process Rust Brain aktif.");
            return;
        }
        if (!running.compareAndSet(false, true)) {
            return;
        }

        Thread thread = new Thread(this::runLoop, "spica-brain-ipc");
        thread.setDaemon(true);
        this.workerThread = thread;
        thread.start();
        logger.info("[SpicaRustNative] Launcher IPC istemcisi baslatildi -> " + host + ":" + port);
    }

    public void submitTelemetry(
        double mspt,
        double tps,
        int entities,
        int hibernated,
        int players,
        int loadedChunks,
        long heapUsedMb,
        long heapMaxMb
    ) {
        submitTelemetry(mspt, tps, entities, hibernated, players, loadedChunks, heapUsedMb, heapMaxMb, 0, 0);
    }

    public void submitTelemetry(
        double mspt,
        double tps,
        int entities,
        int hibernated,
        int players,
        int loadedChunks,
        long heapUsedMb,
        long heapMaxMb,
        int farmThrottled,
        int villagersThrottled
    ) {
        submitTelemetry(
            mspt, tps, entities, hibernated, players, loadedChunks, heapUsedMb, heapMaxMb,
            farmThrottled, villagersThrottled, 0L, 0L
        );
    }

    public void submitTelemetry(
        double mspt,
        double tps,
        int entities,
        int hibernated,
        int players,
        int loadedChunks,
        long heapUsedMb,
        long heapMaxMb,
        int farmThrottled,
        int villagersThrottled,
        long elytraClamps,
        long explosionItemsMerged
    ) {
        submitTelemetry(
            mspt, tps, entities, hibernated, players, loadedChunks, heapUsedMb, heapMaxMb,
            farmThrottled, villagersThrottled, elytraClamps, explosionItemsMerged, 0, 0
        );
    }

    public void submitTelemetry(
        double mspt,
        double tps,
        int entities,
        int hibernated,
        int players,
        int loadedChunks,
        long heapUsedMb,
        long heapMaxMb,
        int farmThrottled,
        int villagersThrottled,
        long elytraClamps,
        long explosionItemsMerged,
        int trackedPairsHidden,
        int trackingTransitionsPerSec
    ) {
        submitTelemetry(
            mspt, tps, entities, hibernated, players, loadedChunks, heapUsedMb, heapMaxMb,
            farmThrottled, villagersThrottled, elytraClamps, explosionItemsMerged,
            trackedPairsHidden, trackingTransitionsPerSec, 0, 0L
        );
    }

    public void submitTelemetry(
        double mspt,
        double tps,
        int entities,
        int hibernated,
        int players,
        int loadedChunks,
        long heapUsedMb,
        long heapMaxMb,
        int farmThrottled,
        int villagersThrottled,
        long elytraClamps,
        long explosionItemsMerged,
        int trackedPairsHidden,
        int trackingTransitionsPerSec,
        int redstoneChunksThrottled,
        long redstoneOpsBlocked
    ) {
        if (!enabled || !running.get() || !connected.get()) {
            return;
        }

        JsonObject json = new JsonObject();
        json.addProperty("type", "telemetry");
        json.addProperty("mspt", sanitizeDouble(mspt));
        json.addProperty("tps", sanitizeDouble(tps));
        json.addProperty("entities", Math.max(0, entities));
        json.addProperty("hibernated", Math.max(0, hibernated));
        json.addProperty("players", Math.max(0, players));
        json.addProperty("loadedChunks", Math.max(0, loadedChunks));
        json.addProperty("heapUsedMb", Math.max(0L, heapUsedMb));
        json.addProperty("heapMaxMb", Math.max(0L, heapMaxMb));
        json.addProperty("farmThrottled", Math.max(0, farmThrottled));
        json.addProperty("villagersThrottled", Math.max(0, villagersThrottled));
        json.addProperty("elytraClamps", Math.max(0L, elytraClamps));
        json.addProperty("explosionItemsMerged", Math.max(0L, explosionItemsMerged));
        json.addProperty("trackedPairsHidden", Math.max(0, trackedPairsHidden));
        json.addProperty("trackingTransitionsPerSec", Math.max(0, trackingTransitionsPerSec));
        json.addProperty("redstoneChunksThrottled", Math.max(0, redstoneChunksThrottled));
        json.addProperty("redstoneOpsBlocked", Math.max(0L, redstoneOpsBlocked));

        pendingTelemetry.set(json.toString());
    }

    public boolean isEnabled() {
        return enabled;
    }

    public boolean isConnected() {
        return enabled && connected.get();
    }

    public String getEndpoint() {
        return enabled ? (host + ":" + port) : "disabled";
    }

    public BrainDirective getLatestDirective() {
        return latestDirective.get();
    }

    public long getLastDirectiveTimestampMs() {
        return lastDirectiveTimestampMs;
    }

    public void forceReconnect() {
        connected.set(false);
        latestDirective.set(null);
        lastDirectiveTimestampMs = 0L;
        Socket socket = activeSocket.getAndSet(null);
        closeQuietly(socket);
    }

    /**
     * Non-blocking shutdown safe to invoke on the Bukkit Main Thread during {@code onDisable()} / reload.
     */
    public synchronized void shutdown() {
        running.set(false);
        connected.set(false);
        pendingTelemetry.set(null);
        latestDirective.set(null);

        Socket socket = activeSocket.getAndSet(null);
        closeQuietly(socket);

        Thread thread = this.workerThread;
        this.workerThread = null;
        if (thread != null) {
            thread.interrupt();
        }
    }

    private void runLoop() {
        boolean loggedConnectFailure = false;

        while (running.get()) {
            Socket socket = null;
            try {
                socket = new Socket();
                activeSocket.set(socket);
                socket.connect(new InetSocketAddress(host, port), CONNECT_TIMEOUT_MS);
                socket.setTcpNoDelay(true);
                socket.setSoTimeout(HANDSHAKE_ACK_TIMEOUT_MS);

                BufferedWriter writer = new BufferedWriter(
                    new OutputStreamWriter(socket.getOutputStream(), StandardCharsets.UTF_8)
                );
                InputStream in = new BufferedInputStream(socket.getInputStream(), 4096);
                FrameReader frameReader = new FrameReader(MAX_INCOMING_LINE_BYTES);

                String nonce = generateNonceHex();
                sendHandshake(writer, nonce);

                // Y7: Wait for mutually-authenticated HMAC-SHA256 hello_ack within strict wall-clock deadline
                long ackDeadline = System.nanoTime() + (HANDSHAKE_ACK_TIMEOUT_MS * 1_000_000L);
                String ackLine = frameReader.readNextLine(in, ackDeadline);
                if (ackLine == null || !verifyHelloAck(ackLine, nonce, token)) {
                    throw new IOException("spica-launcher hello_ack HMAC dogrulamasi basarisiz oldu");
                }

                socket.setSoTimeout(READ_TIMEOUT_MS);
                connected.set(true);
                lastDirectiveTimestampMs = System.currentTimeMillis();
                loggedConnectFailure = false;
                logger.info("[SpicaRustNative] spica-launcher Brain IPC baglantisi kuruldu (" + host + ":" + port + ")");

                while (running.get() && !socket.isClosed()) {
                    String payload = pendingTelemetry.getAndSet(null);
                    if (payload != null) {
                        writer.write(payload);
                        writer.write('\n');
                        writer.flush();
                    }

                    try {
                        String line = frameReader.pollLine(in, FRAME_READ_DEADLINE_NANOS);
                        if (line == FrameReader.EOF) {
                            break;
                        }
                        if (line != null && !line.isBlank()) {
                            handleIncomingLine(line);
                        }
                    } catch (SocketTimeoutException ignored) {
                        // Partial frame bytes are preserved inside frameReader; check wall-clock deadline
                        if (frameReader.isPartialFrameExpired(FRAME_READ_DEADLINE_NANOS)) {
                            throw new IOException("IPC frame read deadline exceeded (slow peer)");
                        }
                    }
                }
            } catch (IOException ex) {
                if (running.get() && !loggedConnectFailure) {
                    logger.info(
                        "[SpicaRustNative] spica-launcher IPC erisilemedi (" + host + ":" + port
                            + "): " + ex.getMessage() + " — in-process Rust Brain ile devam ediliyor (5s yeniden deneme)."
                    );
                    loggedConnectFailure = true;
                }
            } finally {
                boolean wasConnected = connected.getAndSet(false);
                latestDirective.set(null);
                activeSocket.compareAndSet(socket, null);
                closeQuietly(socket);

                if (wasConnected && running.get()) {
                    logger.warning(
                        "[SpicaRustNative] spica-launcher IPC baglantisi koptu; in-process Rust Brain'e gecildi (5s sonra yeniden baglanilacak)."
                    );
                }
            }

            if (running.get()) {
                try {
                    Thread.sleep(RECONNECT_DELAY_MS);
                } catch (InterruptedException ignored) {
                    if (!running.get()) {
                        break;
                    }
                }
            }
        }
    }

    /**
     * Y8: Stateful bounded NDJSON line reader that preserves partial frames across {@code SO_TIMEOUT}
     * polls and enforces strict wall-clock deadlines against slowloris peers.
     */
    private static final class FrameReader {
        static final String EOF = new String();
        private final int maxBytes;
        private final ByteArrayOutputStream buf = new ByteArrayOutputStream(512);
        private long partialStartNanos = 0L;

        FrameReader(int maxBytes) {
            this.maxBytes = maxBytes;
        }

        String readNextLine(InputStream in, long deadlineNanos) throws IOException {
            while (true) {
                if (System.nanoTime() > deadlineNanos) {
                    throw new IOException("IPC handshake/frame wall-clock deadline exceeded");
                }
                int b = in.read();
                if (b < 0) {
                    if (buf.size() == 0) {
                        return null;
                    }
                    return flushLine();
                }
                if (b == '\n') {
                    return flushLine();
                }
                if (b != '\r') {
                    appendByte(b);
                }
            }
        }

        String pollLine(InputStream in, long maxFrameNanos) throws IOException {
            while (true) {
                if (partialStartNanos != 0L && (System.nanoTime() - partialStartNanos) > maxFrameNanos) {
                    throw new IOException("IPC partial frame deadline exceeded");
                }
                int b = in.read();
                if (b < 0) {
                    if (buf.size() == 0) {
                        return EOF;
                    }
                    return flushLine();
                }
                if (partialStartNanos == 0L) {
                    partialStartNanos = System.nanoTime();
                }
                if (b == '\n') {
                    return flushLine();
                }
                if (b != '\r') {
                    appendByte(b);
                }
            }
        }

        boolean isPartialFrameExpired(long maxFrameNanos) {
            return partialStartNanos != 0L && (System.nanoTime() - partialStartNanos) > maxFrameNanos;
        }

        private void appendByte(int b) throws IOException {
            if (buf.size() >= maxBytes) {
                throw new IOException("IPC frame exceeds max allowed size (" + maxBytes + " bytes)");
            }
            buf.write(b);
        }

        private String flushLine() {
            String result = buf.toString(StandardCharsets.UTF_8);
            buf.reset();
            partialStartNanos = 0L;
            return result;
        }
    }

    private String generateNonceHex() {
        byte[] bytes = new byte[16];
        secureRandom.nextBytes(bytes);
        return HexFormat.of().formatHex(bytes);
    }

    private static boolean verifyHelloAck(String line, String nonce, String expectedToken) {
        if (expectedToken == null || expectedToken.isEmpty() || nonce == null || nonce.isEmpty()) {
            return false;
        }
        try {
            JsonElement parsed = JsonParser.parseString(line.trim());
            if (!parsed.isJsonObject()) {
                return false;
            }
            JsonObject obj = parsed.getAsJsonObject();
            if (!"hello_ack".equalsIgnoreCase(getString(obj, "type", ""))
                || !"ok".equalsIgnoreCase(getString(obj, "status", ""))) {
                return false;
            }
            long connId = getLong(obj, "connId", 0L);
            String proof = getString(obj, "proof", "");
            if (connId <= 0L || proof.isEmpty()) {
                return false;
            }
            String expectedProof = computeHmacSha256Hex(expectedToken, "spica-ipc-v1:" + nonce + ":" + connId);
            return MessageDigest.isEqual(
                proof.getBytes(StandardCharsets.UTF_8),
                expectedProof.getBytes(StandardCharsets.UTF_8)
            );
        } catch (RuntimeException | GeneralSecurityException ex) {
            return false;
        }
    }

    private static String computeHmacSha256Hex(String secretKey, String message) throws GeneralSecurityException {
        Mac mac = Mac.getInstance("HmacSHA256");
        mac.init(new SecretKeySpec(secretKey.getBytes(StandardCharsets.UTF_8), "HmacSHA256"));
        byte[] digest = mac.doFinal(message.getBytes(StandardCharsets.UTF_8));
        return HexFormat.of().formatHex(digest);
    }

    private void sendHandshake(BufferedWriter writer, String nonce) throws IOException {
        JsonObject hello = new JsonObject();
        hello.addProperty("type", "hello");
        hello.addProperty("token", token);
        hello.addProperty("nonce", nonce);
        hello.addProperty("server", SERVER_FLAVOR);
        hello.addProperty("pluginVersion", PLUGIN_VERSION);

        writer.write(hello.toString());
        writer.write('\n');
        writer.flush();
    }

    private void handleIncomingLine(String line) {
        try {
            JsonElement parsed = JsonParser.parseString(line);
            if (!parsed.isJsonObject()) {
                return;
            }
            JsonObject obj = parsed.getAsJsonObject();
            String type = getString(obj, "type", "");
            if (!"directive".equalsIgnoreCase(type)) {
                return;
            }

            int stateCode;
            String state;
            if (hasPrimitive(obj, "stateCode")) {
                stateCode = Math.clamp(getInt(obj, "stateCode", BrainDirective.STATE_GREEN), 0, 4);
                state = getString(obj, "state", BrainDirective.stateNameForCode(stateCode));
            } else {
                state = getString(obj, "state", "GREEN");
                stateCode = BrainDirective.codeForStateName(state);
            }

            float ewmaMspt = sanitizeFloat(getFloat(obj, "ewmaMspt", 0f), 0f);
            int viewDistance = Math.clamp(getInt(obj, "viewDistance", 8), 2, 32);
            int simDistance = Math.clamp(getInt(obj, "simDistance", 6), 2, 32);
            float fullAiRange = sanitizeFloat(getFloat(obj, "fullAiRange", 32.0f), 32.0f);
            float hibernateRange = sanitizeFloat(getFloat(obj, "hibernateRange", 56.0f), 56.0f);
            boolean itemMergeEnabled = getBoolean(obj, "itemMergeEnabled", false);
            float itemMergeRadius = sanitizeFloat(getFloat(obj, "itemMergeRadius", 3.5f), 3.5f);
            String reason = getString(obj, "reason", "spica-launcher directive");

            BrainDirective directive = new BrainDirective(
                state,
                stateCode,
                ewmaMspt,
                viewDistance,
                simDistance,
                fullAiRange,
                hibernateRange,
                itemMergeEnabled,
                itemMergeRadius,
                reason,
                true
            );
            latestDirective.set(directive);
            lastDirectiveTimestampMs = System.currentTimeMillis();
        } catch (RuntimeException ex) {
            logger.fine("[SpicaRustNative] Gecersiz IPC JSON satiri yoksayildi: " + ex.getMessage());
        }
    }

    private static boolean hasPrimitive(JsonObject obj, String key) {
        return obj.has(key) && !obj.get(key).isJsonNull() && obj.get(key).isJsonPrimitive();
    }

    private static String getString(JsonObject obj, String key, String fallback) {
        if (!hasPrimitive(obj, key)) {
            return fallback;
        }
        try {
            return obj.get(key).getAsString();
        } catch (RuntimeException ignored) {
            return fallback;
        }
    }

    private static int getInt(JsonObject obj, String key, int fallback) {
        if (!hasPrimitive(obj, key)) {
            return fallback;
        }
        try {
            return obj.get(key).getAsInt();
        } catch (RuntimeException ignored) {
            return fallback;
        }
    }

    private static long getLong(JsonObject obj, String key, long fallback) {
        if (!hasPrimitive(obj, key)) {
            return fallback;
        }
        try {
            return obj.get(key).getAsLong();
        } catch (RuntimeException ignored) {
            return fallback;
        }
    }

    private static float getFloat(JsonObject obj, String key, float fallback) {
        if (!hasPrimitive(obj, key)) {
            return fallback;
        }
        try {
            return obj.get(key).getAsFloat();
        } catch (RuntimeException ignored) {
            return fallback;
        }
    }

    private static boolean getBoolean(JsonObject obj, String key, boolean fallback) {
        if (!hasPrimitive(obj, key)) {
            return fallback;
        }
        try {
            return obj.get(key).getAsBoolean();
        } catch (RuntimeException ignored) {
            return fallback;
        }
    }

    private static double sanitizeDouble(double value) {
        return Double.isFinite(value) && value >= 0.0 ? value : 0.0;
    }

    private static float sanitizeFloat(float value, float fallback) {
        return Float.isFinite(value) && value >= 0f ? value : fallback;
    }

    private static void closeQuietly(Socket socket) {
        if (socket != null) {
            try {
                socket.close();
            } catch (IOException ignored) {
                // Ignore close errors
            }
        }
    }
}
