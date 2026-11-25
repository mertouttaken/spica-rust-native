#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PLUGIN_DIR="$ROOT_DIR/spica-rust-native-plugin"

ARCH="$(uname -m)"
case "$ARCH" in
  x86_64|amd64) ARCH_KEY="x86_64" ;;
  aarch64|arm64) ARCH_KEY="aarch64" ;;
  *) ARCH_KEY="$ARCH" ;;
esac

NATIVE_OUT_DIR="$PLUGIN_DIR/src/main/resources/native/linux-${ARCH_KEY}"
DIST_DIR="$ROOT_DIR/dist/linux-${ARCH_KEY}"

echo "[1/4] Rust Workspace derleniyor (linux-${ARCH_KEY}: spica-native .so + spica-launcher)..."
cd "$ROOT_DIR"
RUSTFLAGS="${RUSTFLAGS:--C target-cpu=native}" cargo test --workspace --release
RUSTFLAGS="${RUSTFLAGS:--C target-cpu=native}" cargo build --workspace --release

echo "[2/4] libspica_rust_native.so plugin kaynaklarina kopyalaniyor..."
mkdir -p "$NATIVE_OUT_DIR"
cp -f "$ROOT_DIR/target/release/libspica_rust_native.so" "$NATIVE_OUT_DIR/libspica_rust_native.so"

echo "[3/4] Purpur 1.21.4 Java 21 eklentisi paketleniyor (Maven)..."
cd "$PLUGIN_DIR"
mvn -B clean package

echo "[4/4] Ciktilar $DIST_DIR klasorune toplaniyor..."
mkdir -p "$DIST_DIR"
cp -f "$PLUGIN_DIR/target/SpicaRustNative-2.3.0.jar" "$DIST_DIR/SpicaRustNative-2.3.0.jar"
cp -f "$ROOT_DIR/target/release/spica-launcher" "$DIST_DIR/spica-launcher"
cp -f "$ROOT_DIR/target/release/libspica_rust_native.so" "$DIST_DIR/libspica_rust_native.so"
chmod +x "$DIST_DIR/spica-launcher"

echo ""
echo "TAMAMLANDI!"
echo "  Rust Launcher : $DIST_DIR/spica-launcher"
echo "  Purpur Plugin : $DIST_DIR/SpicaRustNative-2.3.0.jar"
echo "  Native Engine : $DIST_DIR/libspica_rust_native.so"
