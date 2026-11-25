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
 * Lookup order:
 * 1. Override file in `plugins/SpicaRustNative/native/<libName>` (great for quick dev swaps)
 * 2. Embedded JAR resource `/native/<os>-<arch>/<libName>` extracted to a temp file
 */
public final class NativeLoader {

    private static volatile boolean loaded = false;
    private static volatile String loadedPath = "unloaded";

    private NativeLoader() {
    }

    public static synchronized void load(Path pluginDataDir, Logger logger) throws IOException {
        if (loaded) {
            return;
        }

        String os = detectOs();
        String arch = detectArch();
        String libFileName = mapLibraryFileName(os);
        String platformKey = os + "-" + arch;

        // 1) Check external override in plugins/SpicaRustNative/native/
        Path externalOverride = pluginDataDir.resolve("native").resolve(libFileName);
        if (Files.isRegularFile(externalOverride)) {
            Path absolute = externalOverride.toAbsolutePath();
            logger.info("[SpicaRustNative] Harici native kutuphane yukleniyor: " + absolute);
            System.load(absolute.toString());
            loaded = true;
            loadedPath = absolute.toString();
            return;
        }

        // 2) Extract bundled library from JAR resources
        String resourcePath = "/native/" + platformKey + "/" + libFileName;
        try (InputStream in = NativeLoader.class.getResourceAsStream(resourcePath)) {
            if (in == null) {
                throw new IOException(
                    "Native Rust kutuphanesi bulunamadi! Aranan kaynak: " + resourcePath
                        + " veya harici yol: " + externalOverride.toAbsolutePath()
                        + ". Lutfen build-windows.ps1 (Windows) veya build-linux.sh (Linux) ile derleyin."
                );
            }

            Path tempDir = Files.createTempDirectory("spica-rust-native-");
            Path extracted = tempDir.resolve(libFileName);
            Files.copy(in, extracted, StandardCopyOption.REPLACE_EXISTING);
            extracted.toFile().deleteOnExit();
            tempDir.toFile().deleteOnExit();

            System.load(extracted.toAbsolutePath().toString());
            loaded = true;
            loadedPath = "embedded:" + resourcePath + " -> " + extracted;
            logger.info("[SpicaRustNative] Gomulu Rust kutuphanesi yuklendi (" + platformKey + ")");
        }
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
