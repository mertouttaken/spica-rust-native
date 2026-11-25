package net.spicanetwork.rustnative.nativebridge;

import java.io.IOException;
import java.io.InputStream;
import java.nio.file.Files;
import java.nio.file.Path;
import java.nio.file.StandardCopyOption;
import java.util.Locale;
import java.util.logging.Logger;

/**
 * Loads the compiled Rust shared library (.dll on Windows, .so on Linux, .dylib on macOS).
 *
 * Security & Lookup order (O5/O6):
 * 1. Embedded JAR resource `/native/<os>-<arch>/<libName>` extracted to a temp file (default, trusted).
 * 2. Opt-in external override (ONLY when {@code native.allow-external-override: true} in config.yml
 *    or {@code -Dspica.native.allow-override=true}): checks {@code -Dspica.native.dir} first, then
 *    {@code plugins/SpicaRustNative/native/<libName>}.
 * 3. Verifies exact native ABI version token from {@link RustNative#nativeGetInfo()} BEFORE
 *    marking {@code loaded = true} so a mismatched library never poisons runtime state.
 */
public final class NativeLoader {

    public static final int REQUIRED_ABI_REV = 2;

    private static volatile boolean loaded = false;
    private static volatile String loadedPath = "unloaded";

    private NativeLoader() {
    }

    public static synchronized void load(Path pluginDataDir, Logger logger) throws IOException {
        load(pluginDataDir, logger, false, null);
    }

    public static synchronized void load(
        Path pluginDataDir,
        Logger logger,
        boolean configAllowExternalOverride,
        String expectedPluginVersion
    ) throws IOException {
        if (loaded) {
            return;
        }

        String os = detectOs();
        String arch = detectArch();
        String libFileName = mapLibraryFileName(os);
        String platformKey = os + "-" + arch;

        boolean allowOverride = configAllowExternalOverride
            || Boolean.parseBoolean(System.getProperty("spica.native.allow-override", "false"));

        String resolvedSource = null;

        // 1) Opt-in external override (O6 security boundary: never load arbitrary writable DLL/SO by default)
        if (allowOverride) {
            Path overrideFile = resolveExternalOverride(pluginDataDir, libFileName);
            if (overrideFile != null && Files.isRegularFile(overrideFile)) {
                Path absolute = overrideFile.toAbsolutePath().normalize();
                logger.warning("[SpicaRustNative] Opt-in harici native kutuphane yukleniyor: " + absolute);
                System.load(absolute.toString());
                resolvedSource = "override:" + absolute;
            }
        } else {
            String propDir = System.getProperty("spica.native.dir");
            if (propDir != null && !propDir.isBlank()) {
                Path candidate = Path.of(propDir.trim()).resolve(libFileName);
                if (Files.isRegularFile(candidate)) {
                    logger.info(
                        "[SpicaRustNative] Harici native dizini ('" + propDir.trim()
                            + "') guvenlik nedeniyle atlandi; etkinlestirmek icin config.yml 'native.allow-external-override: true' veya '-Dspica.native.allow-override=true' kullanin."
                    );
                }
            }
        }

        // 2) Extract bundled library from JAR resources when no opt-in override was loaded
        if (resolvedSource == null) {
            String resourcePath = "/native/" + platformKey + "/" + libFileName;
            try (InputStream in = NativeLoader.class.getResourceAsStream(resourcePath)) {
                if (in == null) {
                    throw new IOException(
                        "Native Rust kutuphanesi bulunamadi! Aranan kaynak: " + resourcePath
                            + ". Lutfen build-windows.ps1 (Windows) veya build-linux.sh (Linux) ile derleyin."
                    );
                }

                Path tempDir = Files.createTempDirectory("spica-rust-native-");
                Path extracted = tempDir.resolve(libFileName);
                Files.copy(in, extracted, StandardCopyOption.REPLACE_EXISTING);
                extracted.toFile().deleteOnExit();
                tempDir.toFile().deleteOnExit();

                System.load(extracted.toAbsolutePath().toString());
                resolvedSource = "embedded:" + resourcePath + " -> " + extracted;
                logger.info("[SpicaRustNative] Gomulu Rust kutuphanesi yuklendi (" + platformKey + ")");
            }
        }

        // 3) Strict ABI revision + exact SemVer verification BEFORE setting loaded = true (R13)
        String nativeInfo = RustNative.nativeGetInfo();
        int nativeAbiRev = extractNativeAbiRev(nativeInfo);
        if (nativeAbiRev != REQUIRED_ABI_REV) {
            throw new IOException(
                "Native ABI revizyon uyusmazligi: beklenen abi=" + REQUIRED_ABI_REV
                    + ", bulunan='" + nativeInfo + "'"
            );
        }
        if (expectedPluginVersion != null && !expectedPluginVersion.isBlank()) {
            String nativeVersion = extractNativeVersion(nativeInfo);
            String normalizedExpected = normalizeExpectedVersion(expectedPluginVersion);
            if (nativeVersion == null || !nativeVersion.equals(normalizedExpected)) {
                throw new IOException(
                    "Native ABI surum uyusmazligi: beklenen=" + normalizedExpected
                        + ", bulunan=" + (nativeVersion != null ? nativeVersion : String.valueOf(nativeInfo))
                );
            }
        }

        loadedPath = resolvedSource;
        loaded = true;
    }

    /**
     * Extracts exact version token from `"spica-rust-native v2.6.0 | abi=2 | os=... | threads=..."`.
     */
    static String extractNativeVersion(String nativeInfo) {
        if (nativeInfo == null) {
            return null;
        }
        String prefix = "spica-rust-native v";
        int start = nativeInfo.indexOf(prefix);
        if (start < 0) {
            return null;
        }
        int verStart = start + prefix.length();
        int sep = nativeInfo.indexOf(" |", verStart);
        String token = (sep > verStart) ? nativeInfo.substring(verStart, sep) : nativeInfo.substring(verStart);
        return token.trim();
    }

    /**
     * Extracts integer ABI revision from `"| abi=<rev>"`, returning -1 if missing or malformed.
     */
    static int extractNativeAbiRev(String nativeInfo) {
        if (nativeInfo == null) {
            return -1;
        }
        String marker = "| abi=";
        int start = nativeInfo.indexOf(marker);
        if (start < 0) {
            return -1;
        }
        int valStart = start + marker.length();
        int sep = nativeInfo.indexOf(" |", valStart);
        String token = (sep > valStart) ? nativeInfo.substring(valStart, sep) : nativeInfo.substring(valStart);
        try {
            return Integer.parseInt(token.trim());
        } catch (NumberFormatException ex) {
            return -1;
        }
    }

    private static String normalizeExpectedVersion(String pluginVersion) {
        String trimmed = pluginVersion.trim();
        if (trimmed.endsWith("-SNAPSHOT")) {
            return trimmed.substring(0, trimmed.length() - "-SNAPSHOT".length());
        }
        return trimmed;
    }

    private static Path resolveExternalOverride(Path pluginDataDir, String libFileName) {
        String propDir = System.getProperty("spica.native.dir");
        if (propDir != null && !propDir.isBlank()) {
            Path configured = Path.of(propDir.trim()).resolve(libFileName);
            if (Files.isRegularFile(configured)) {
                return configured;
            }
        }
        if (pluginDataDir != null) {
            return pluginDataDir.resolve("native").resolve(libFileName);
        }
        return null;
    }

    public static boolean isLoaded() {
        return loaded;
    }

    public static String getLoadedPath() {
        return loadedPath;
    }

    public static String detectPlatformKey() {
        return detectOs() + "-" + detectArch();
    }

    private static String detectOs() {
        String name = System.getProperty("os.name", "").toLowerCase(Locale.ROOT);
        if (name.contains("win")) {
            return "windows";
        }
        if (name.contains("linux")) {
            return "linux";
        }
        if (name.contains("mac") || name.contains("darwin")) {
            return "macos";
        }
        return name.replaceAll("\\s+", "");
    }

    private static String detectArch() {
        String arch = System.getProperty("os.arch", "").toLowerCase(Locale.ROOT);
        if (arch.equals("amd64") || arch.equals("x86_64")) {
            return "x86_64";
        }
        if (arch.equals("aarch64") || arch.equals("arm64")) {
            return "aarch64";
        }
        return arch;
    }

    private static String mapLibraryFileName(String os) {
        return switch (os) {
            case "windows" -> "spica_rust_native.dll";
            case "macos" -> "libspica_rust_native.dylib";
            default -> "libspica_rust_native.so";
        };
    }
}
