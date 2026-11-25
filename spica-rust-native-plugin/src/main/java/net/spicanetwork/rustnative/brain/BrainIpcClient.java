package net.spicanetwork.rustnative.brain;

import com.google.gson.JsonElement;
import com.google.gson.JsonObject;
import com.google.gson.JsonParser;

import java.io.BufferedReader;
import java.io.BufferedWriter;
import java.io.IOException;
import java.io.InputStreamReader;
import java.io.OutputStreamWriter;
import java.net.InetSocketAddress;
import java.net.Socket;
import java.net.SocketTimeoutException;
import java.nio.charset.StandardCharsets;
import java.util.concurrent.atomic.AtomicBoolean;
import java.util.concurrent.atomic.AtomicReference;
import java.util.logging.Logger;

/**
 * Non-blocking asynchronous NDJSON TCP client connecting Purpur 1.21.4 to the external
 * {@code spica-launcher} Rust Brain daemon.
 *
 * Guarantees:
 * - Never blocks the Minecraft Main Thread (single-slot {@link AtomicReference} telemetry handoff)
 * - Bounded memory (at most 1 pending telemetry JSON string in flight)
 * - Automatic reconnect every 5 seconds if {@code spica-launcher} restarts or starts later
 */
public final class BrainIpcClient {

    public static final String PLUGIN_VERSION = "2.0.0";
    public static final String SERVER_FLAVOR = "purpur-1.21.4";

    private static final int CONNECT_TIMEOUT_MS = 3_000;
    private static final int READ_TIMEOUT_MS = 150;
    private static final long RECONNECT_DELAY_MS = 5_000L;

    private final String host;
    private final int port;
    private final String token;
    private final boolean enabled;
    private final Logger logger;

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
            this.host = propHost.trim();
        } else if (fallbackHost != null && !fallbackHost.isBlank()) {
            this.host = fallbackHost.trim();
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

        String propToken = System.getProperty("spica.ipc.token");
        this.token = propToken != null ? propToken.trim() : "";

        this.enabled = this.port > 0;
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

    /**
     * Non-blocking single-slot telemetry submission safe to invoke directly from the Main Thread.
     */
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
            try {
                thread.join(500L);
            } catch (InterruptedException ignored) {
                Thread.currentThread().interrupt();
            }
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
                socket.setSoTimeout(READ_TIMEOUT_MS);

                BufferedWriter writer = new BufferedWriter(
                    new OutputStreamWriter(socket.getOutputStream(), StandardCharsets.UTF_8)
                );
                BufferedReader reader = new BufferedReader(
                    new InputStreamReader(socket.getInputStream(), StandardCharsets.UTF_8)
                );

                sendHandshake(writer);

                connected.set(true);
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
                        String line = reader.readLine();
                        if (line == null) {
                            break;
                        }
                        if (!line.isBlank()) {
                            handleIncomingLine(line);
                        }
                    } catch (SocketTimeoutException ignored) {
                        // Normal non-blocking poll timeout; loop back to flush pending telemetry
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

    private void sendHandshake(BufferedWriter writer) throws IOException {
        JsonObject hello = new JsonObject();
        hello.addProperty("type", "hello");
        hello.addProperty("token", token);
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
