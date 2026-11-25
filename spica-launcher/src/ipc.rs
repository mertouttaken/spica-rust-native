use crate::brain::{BrainState, TelemetryPayload};
use serde::Deserialize;
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{watch, Semaphore};
use tokio::task::JoinHandle;

const MAX_IPC_LINE_BYTES: usize = 64 * 1024;
const HELLO_TIMEOUT: Duration = Duration::from_secs(10);
const IPC_IDLE_READ_TIMEOUT: Duration = Duration::from_secs(30);
const IPC_WRITE_TIMEOUT: Duration = Duration::from_secs(3);
const MAX_CONCURRENT_IPC_CONNS: usize = 8;
const IPC_PROTOCOL_VERSION: u32 = 2;

#[derive(Debug, Deserialize)]
struct HelloMessage {
    #[serde(rename = "type")]
    msg_type: String,
    #[serde(rename = "clientNonce", default)]
    client_nonce: String,
    #[serde(rename = "clientProof", default)]
    client_proof: String,
    #[serde(default)]
    server: String,
    #[serde(rename = "pluginVersion", default)]
    plugin_version: String,
}

#[derive(Debug, Deserialize)]
struct TaggedHeader {
    #[serde(rename = "type")]
    msg_type: String,
}

/// Constant-time string comparison to avoid timing side-channels on token validation.
fn constant_time_eq(a: &str, b: &str) -> bool {
    let a_bytes = a.as_bytes();
    let b_bytes = b.as_bytes();
    if a_bytes.len() != b_bytes.len() {
        return false;
    }
    let mut diff: u8 = 0;
    for (&x, &y) in a_bytes.iter().zip(b_bytes.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// Pure-Rust FIPS 180-4 SHA-256 (zero external crypto dependencies).
fn sha256(data: &[u8]) -> [u8; 32] {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];

    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];

    let bit_len = (data.len() as u64).wrapping_mul(8);
    let mut padded = Vec::with_capacity(((data.len() + 9 + 63) / 64) * 64);
    padded.extend_from_slice(data);
    padded.push(0x80);
    while (padded.len() % 64) != 56 {
        padded.push(0);
    }
    padded.extend_from_slice(&bit_len.to_be_bytes());

    let mut w = [0u32; 64];
    for chunk in padded.chunks_exact(64) {
        for i in 0..16 {
            w[i] = u32::from_be_bytes([
                chunk[i * 4],
                chunk[i * 4 + 1],
                chunk[i * 4 + 2],
                chunk[i * 4 + 3],
            ]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }

        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh] = h;
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let temp1 = hh
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let temp2 = s0.wrapping_add(maj);

            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(temp1);
            d = c;
            c = b;
            b = a;
            a = temp1.wrapping_add(temp2);
        }

        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
        h[5] = h[5].wrapping_add(f);
        h[6] = h[6].wrapping_add(g);
        h[7] = h[7].wrapping_add(hh);
    }

    let mut out = [0u8; 32];
    for (i, word) in h.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&word.to_be_bytes());
    }
    out
}

/// Computes lowercase hex RFC 2104 HMAC-SHA256(key, msg) for mutual `hello_ack` proof (Y7).
fn hmac_sha256_hex(key: &[u8], msg: &[u8]) -> String {
    let mut key_block = [0u8; 64];
    if key.len() > 64 {
        let hashed = sha256(key);
        key_block[..32].copy_from_slice(&hashed);
    } else {
        key_block[..key.len()].copy_from_slice(key);
    }

    let mut inner = Vec::with_capacity(64 + msg.len());
    for &b in &key_block {
        inner.push(b ^ 0x36);
    }
    inner.extend_from_slice(msg);
    let inner_hash = sha256(&inner);

    let mut outer = [0u8; 96];
    for i in 0..64 {
        outer[i] = key_block[i] ^ 0x5c;
    }
    outer[64..96].copy_from_slice(&inner_hash);
    let mac = sha256(&outer);

    let mut hex = String::with_capacity(64);
    for b in mac {
        use std::fmt::Write;
        let _ = write!(&mut hex, "{:02x}", b);
    }
    hex
}

fn generate_server_nonce_hex() -> Result<String, String> {
    let mut bytes = [0u8; 16];
    getrandom::getrandom(&mut bytes)
        .map_err(|e| format!("CSPRNG server nonce uretilemedi: {}", e))?;
    let mut hex = String::with_capacity(32);
    for b in bytes {
        use std::fmt::Write;
        let _ = write!(&mut hex, "{:02x}", b);
    }
    Ok(hex)
}

fn is_valid_nonce(nonce: &str) -> bool {
    nonce.len() >= 16
        && nonce.len() <= 128
        && nonce.bytes().all(|b| b.is_ascii_alphanumeric())
}

/// R5: Zero-disclosure client authentication proof: never transmits the shared secret over TCP.
fn compute_client_proof(token: &str, server_nonce: &str, client_nonce: &str) -> String {
    let msg = format!("spica-ipc-v2-client:{}:{}", server_nonce, client_nonce);
    hmac_sha256_hex(token.as_bytes(), msg.as_bytes())
}

/// R5: Mutual server authentication proof bound to both nonces and the allocated `conn_id`.
fn compute_server_proof(token: &str, client_nonce: &str, server_nonce: &str, conn_id: u64) -> String {
    let msg = format!(
        "spica-ipc-v2-server:{}:{}:{}",
        client_nonce, server_nonce, conn_id
    );
    hmac_sha256_hex(token.as_bytes(), msg.as_bytes())
}

fn normalize_bind_host(host: &str) -> &str {
    let trimmed = host.trim();
    if trimmed.starts_with('[') && trimmed.ends_with(']') && trimmed.len() >= 2 {
        &trimmed[1..trimmed.len() - 1]
    } else {
        trimmed
    }
}

fn is_loopback_host(host: &str) -> bool {
    let normalized = normalize_bind_host(host);
    if normalized.eq_ignore_ascii_case("localhost") {
        return true;
    }
    normalized
        .parse::<IpAddr>()
        .map(|ip| ip.is_loopback())
        .unwrap_or(false)
}

async fn write_line_timeout<W: tokio::io::AsyncWrite + Unpin>(
    writer: &mut W,
    bytes: &[u8],
) -> tokio::io::Result<()> {
    match tokio::time::timeout(IPC_WRITE_TIMEOUT, async {
        writer.write_all(bytes).await?;
        writer.flush().await
    })
    .await
    {
        Ok(res) => res,
        Err(_) => Err(tokio::io::Error::new(
            tokio::io::ErrorKind::TimedOut,
            "IPC write timeout",
        )),
    }
}

async fn read_bounded_line<R: AsyncBufRead + Unpin>(
    reader: &mut R,
    out: &mut Vec<u8>,
    max_bytes: usize,
) -> tokio::io::Result<usize> {
    out.clear();
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            return Ok(out.len());
        }
        if let Some(pos) = available.iter().position(|&b| b == b'\n') {
            let to_take = pos + 1;
            if out.len() + to_take > max_bytes {
                return Err(tokio::io::Error::new(
                    tokio::io::ErrorKind::InvalidData,
                    "IPC line exceeds maximum allowed size",
                ));
            }
            out.extend_from_slice(&available[..to_take]);
            reader.consume(to_take);
            return Ok(out.len());
        } else {
            let len = available.len();
            if out.len() + len > max_bytes {
                return Err(tokio::io::Error::new(
                    tokio::io::ErrorKind::InvalidData,
                    "IPC line exceeds maximum allowed size",
                ));
            }
            out.extend_from_slice(available);
            reader.consume(len);
        }
    }
}

/// Binds the Brain IPC TCP server and spawns the bounded async accept loop.
/// Enforces loopback-only binding (pre-bind AND post-bind resolved IP verification) by default
/// unless `allow_non_loopback` is explicitly set (Y7). Supports IPv4, IPv6 (`::1` / `[::1]`), and `localhost`.
pub async fn start_ipc_server(
    host: &str,
    port: u16,
    allow_non_loopback: bool,
    expected_token: String,
    brain: Arc<Mutex<BrainState>>,
    mut shutdown_rx: watch::Receiver<bool>,
) -> Result<JoinHandle<()>, String> {
    let normalized_host = normalize_bind_host(host);
    if !allow_non_loopback && !is_loopback_host(normalized_host) {
        return Err(format!(
            "GUVENLIK HATASI: Brain IPC '{}' loopback disi adrese baglanamaz (allow_non_loopback_ipc = true olmadan engellendi).",
            host
        ));
    }

    let listener = TcpListener::bind((normalized_host, port))
        .await
        .map_err(|e| {
            format!(
                "HATA: Brain IPC sunucusu '{}:{}' adresine baglanamadi: {}",
                normalized_host, port, e
            )
        })?;

    // Post-bind verification: prevents `/etc/hosts` or DNS redirecting `localhost` to a non-loopback IP
    if !allow_non_loopback {
        let local_addr = listener.local_addr().map_err(|e| {
            format!("HATA: Brain IPC yerel adres dogrulanamadi: {}", e)
        })?;
        if !local_addr.ip().is_loopback() {
            drop(listener);
            return Err(format!(
                "GUVENLIK HATASI: '{}' loopback disi adrese cozuldu ({}); allow_non_loopback_ipc = true olmadan engellendi.",
                host, local_addr
            ));
        }
    }

    let conn_limiter = Arc::new(Semaphore::new(MAX_CONCURRENT_IPC_CONNS));

    let handle = tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = shutdown_rx.changed() => {
                    if *shutdown_rx.borrow() {
                        break;
                    }
                }
                accept_res = listener.accept() => {
                    match accept_res {
                        Ok((stream, peer)) => {
                            let Ok(permit) = Arc::clone(&conn_limiter).try_acquire_owned() else {
                                eprintln!(
                                    "[SpicaBrain IPC] Maksimum eszamanli baglanti siniri ({}) asildi, {} reddedildi.",
                                    MAX_CONCURRENT_IPC_CONNS, peer
                                );
                                drop(stream);
                                continue;
                            };
                            let token_clone = expected_token.clone();
                            let brain_clone = Arc::clone(&brain);
                            let client_shutdown = shutdown_rx.clone();
                            tokio::spawn(async move {
                                let _permit = permit;
                                handle_client(stream, peer, token_clone, brain_clone, client_shutdown).await;
                            });
                        }
                        Err(err) => {
                            eprintln!("[SpicaBrain IPC] Baglanti kabul hatasi: {}", err);
                            tokio::time::sleep(Duration::from_millis(100)).await;
                        }
                    }
                }
            }
        }
    });

    Ok(handle)
}

async fn handle_client(
    stream: TcpStream,
    peer: SocketAddr,
    expected_token: String,
    brain: Arc<Mutex<BrainState>>,
    mut shutdown_rx: watch::Receiver<bool>,
) {
    let _ = stream.set_nodelay(true);
    let (read_half, mut write_half) = stream.into_split();
    let mut reader = BufReader::new(read_half);
    let mut buf = Vec::with_capacity(1024);

    // Step 1a (R5): Send server challenge nonce first so the shared token is NEVER transmitted in plaintext
    let server_nonce = match generate_server_nonce_hex() {
        Ok(n) => n,
        Err(e) => {
            eprintln!("[SpicaBrain IPC] {} {}", peer, e);
            return;
        }
    };
    let challenge_line = format!(
        "{{\"type\":\"hello_challenge\",\"protocol\":{},\"nonce\":\"{}\"}}\n",
        IPC_PROTOCOL_VERSION, server_nonce
    );
    if let Err(e) = write_line_timeout(&mut write_half, challenge_line.as_bytes()).await {
        eprintln!("[SpicaBrain IPC] {} hello_challenge yazma hatasi: {}", peer, e);
        return;
    }

    // Step 1b: Mandatory hello handshake line with zero-disclosure HMAC-SHA256 clientProof
    let hello_read = tokio::time::timeout(
        HELLO_TIMEOUT,
        read_bounded_line(&mut reader, &mut buf, MAX_IPC_LINE_BYTES),
    )
    .await;

    let n = match hello_read {
        Ok(Ok(bytes)) if bytes > 0 => bytes,
        Ok(Ok(_)) => return,
        Ok(Err(e)) => {
            eprintln!("[SpicaBrain IPC] {} el sikisma okuma hatasi: {}", peer, e);
            return;
        }
        Err(_) => {
            eprintln!("[SpicaBrain IPC] {} el sikisma zaman asimi.", peer);
            return;
        }
    };

    let hello_line = String::from_utf8_lossy(&buf[..n]);
    let trimmed_hello = hello_line.trim();
    let hello: HelloMessage = match serde_json::from_str(trimmed_hello) {
        Ok(msg) => msg,
        Err(e) => {
            eprintln!(
                "[SpicaBrain IPC] {} gecersiz hello JSON: {} (baglanti reddedildi)",
                peer, e
            );
            return;
        }
    };

    let expected_client_proof =
        compute_client_proof(&expected_token, &server_nonce, &hello.client_nonce);
    if hello.msg_type != "hello"
        || !is_valid_nonce(&hello.client_nonce)
        || !constant_time_eq(&hello.client_proof, &expected_client_proof)
    {
        eprintln!(
            "[SpicaBrain IPC] {} gecersiz HMAC istemci kaniti/nonce veya mesaj tipi (baglanti reddedildi)",
            peer
        );
        return;
    }

    let server_label = if hello.server.trim().is_empty() {
        "Purpur".to_string()
    } else {
        hello.server
    };
    let plugin_ver = if hello.plugin_version.trim().is_empty() {
        "unknown".to_string()
    } else {
        hello.plugin_version
    };

    let conn_id = match brain.lock() {
        Ok(mut guard) => guard.register_connection(server_label.clone(), plugin_ver.clone()),
        Err(poisoned) => poisoned
            .into_inner()
            .register_connection(server_label.clone(), plugin_ver.clone()),
    };

    // Step 1c (R5): Mutual server proof bound to both nonces and conn_id
    let proof = compute_server_proof(&expected_token, &hello.client_nonce, &server_nonce, conn_id);
    let ack = format!(
        "{{\"type\":\"hello_ack\",\"status\":\"ok\",\"protocol\":{},\"connId\":{},\"proof\":\"{}\"}}\n",
        IPC_PROTOCOL_VERSION, conn_id, proof
    );
    if let Err(e) = write_line_timeout(&mut write_half, ack.as_bytes()).await {
        eprintln!("[SpicaBrain IPC] {} hello_ack yazma hatasi: {}", peer, e);
        match brain.lock() {
            Ok(mut guard) => guard.clear_connection(conn_id),
            Err(poisoned) => poisoned.into_inner().clear_connection(conn_id),
        }
        return;
    }

    println!(
        "[SpicaBrain IPC] Purpur baglandi: {} | connId={} | server={} | plugin=v{}",
        peer, conn_id, server_label, plugin_ver
    );

    // Step 2: Continuous NDJSON telemetry -> directive loop with idle & write deadlines (Y8)
    loop {
        let bytes_read = tokio::select! {
            _ = shutdown_rx.changed() => {
                if *shutdown_rx.borrow() {
                    break;
                }
                continue;
            }
            res = tokio::time::timeout(
                IPC_IDLE_READ_TIMEOUT,
                read_bounded_line(&mut reader, &mut buf, MAX_IPC_LINE_BYTES)
            ) => {
                match res {
                    Ok(Ok(0)) => break,
                    Ok(Ok(count)) => count,
                    Ok(Err(e)) => {
                        eprintln!("[SpicaBrain IPC] {} okuma hatasi: {}", peer, e);
                        break;
                    }
                    Err(_) => {
                        eprintln!("[SpicaBrain IPC] {} bosta kalma zaman asimi ({}s).", peer, IPC_IDLE_READ_TIMEOUT.as_secs());
                        break;
                    }
                }
            }
        };

        let line = String::from_utf8_lossy(&buf[..bytes_read]);
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let header: TaggedHeader = match serde_json::from_str(trimmed) {
            Ok(h) => h,
            Err(e) => {
                eprintln!("[SpicaBrain IPC] {} gecersiz JSON satiri: {}", peer, e);
                continue;
            }
        };

        match header.msg_type.as_str() {
            "telemetry" => {
                let telemetry: TelemetryPayload = match serde_json::from_str(trimmed) {
                    Ok(t) => t,
                    Err(e) => {
                        eprintln!(
                            "[SpicaBrain IPC] {} telemetri ayristirma hatasi: {}",
                            peer, e
                        );
                        continue;
                    }
                };

                // Y7: Reject telemetry if this session has been superseded by a newer authenticated session
                let directive_opt = match brain.lock() {
                    Ok(mut guard) => guard.ingest_telemetry_for(conn_id, &telemetry),
                    Err(poisoned) => poisoned
                        .into_inner()
                        .ingest_telemetry_for(conn_id, &telemetry),
                };
                let Some(directive) = directive_opt else {
                    println!(
                        "[SpicaBrain IPC] Eski oturum (connId={}, {}) yeni oturum tarafindan gecersiz kilindi, kapatiliyor.",
                        conn_id, peer
                    );
                    break;
                };

                let mut serialized = match serde_json::to_string(&directive) {
                    Ok(s) => s,
                    Err(e) => {
                        eprintln!("[SpicaBrain IPC] Direktif serilestirme hatasi: {}", e);
                        continue;
                    }
                };
                serialized.push('\n');

                if let Err(e) = write_line_timeout(&mut write_half, serialized.as_bytes()).await {
                    eprintln!("[SpicaBrain IPC] {} yazma hatasi: {}", peer, e);
                    break;
                }
            }
            "ping" => {
                let still_active = match brain.lock() {
                    Ok(guard) => guard.is_active_connection(conn_id),
                    Err(poisoned) => poisoned.into_inner().is_active_connection(conn_id),
                };
                if !still_active {
                    break;
                }
                if write_line_timeout(&mut write_half, b"{\"type\":\"pong\"}\n")
                    .await
                    .is_err()
                {
                    break;
                }
            }
            other => {
                eprintln!(
                    "[SpicaBrain IPC] {} bilinmeyen mesaj tipi yoksayildi: '{}'",
                    peer, other
                );
            }
        }
    }

    match brain.lock() {
        Ok(mut guard) => guard.clear_connection(conn_id),
        Err(poisoned) => poisoned.into_inner().clear_connection(conn_id),
    }

    println!("[SpicaBrain IPC] Baglanti kapandi: {}", peer);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_constant_time_eq() {
        assert!(constant_time_eq("abcdef123456", "abcdef123456"));
        assert!(!constant_time_eq("abcdef123456", "abcdef123457"));
        assert!(!constant_time_eq("short", "longer"));
    }

    #[test]
    fn test_loopback_host_validation() {
        assert!(is_loopback_host("127.0.0.1"));
        assert!(is_loopback_host("::1"));
        assert!(is_loopback_host("[::1]"));
        assert!(is_loopback_host("localhost"));
        assert!(!is_loopback_host("0.0.0.0"));
        assert!(!is_loopback_host("[::]"));
        assert!(!is_loopback_host("192.168.1.50"));
    }

    #[test]
    fn test_hmac_sha256_rfc4231_vector() {
        // RFC 4231 Test Case 2: key = "Jefe", data = "what do ya want for nothing?"
        let mac = hmac_sha256_hex(b"Jefe", b"what do ya want for nothing?");
        assert_eq!(
            mac,
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
    }

    #[test]
    fn test_zero_disclosure_mutual_challenge_response_vectors() {
        let token = "secret-spica-token-256bit";
        let server_nonce = "0123456789abcdef0123456789abcdef";
        let client_nonce = "fedcba9876543210fedcba9876543210";

        assert!(is_valid_nonce(server_nonce));
        assert!(is_valid_nonce(client_nonce));
        assert!(!is_valid_nonce("short"));
        assert!(!is_valid_nonce("bad-nonce-with-dashes!"));

        let c_proof = compute_client_proof(token, server_nonce, client_nonce);
        let c_proof_wrong_token = compute_client_proof("wrong-token", server_nonce, client_nonce);
        let c_proof_swapped = compute_client_proof(token, client_nonce, server_nonce);
        assert_eq!(c_proof.len(), 64);
        assert!(!constant_time_eq(&c_proof, &c_proof_wrong_token));
        assert!(!constant_time_eq(&c_proof, &c_proof_swapped));

        let s_proof_1 = compute_server_proof(token, client_nonce, server_nonce, 1);
        let s_proof_2 = compute_server_proof(token, client_nonce, server_nonce, 2);
        assert_eq!(s_proof_1.len(), 64);
        assert!(!constant_time_eq(&s_proof_1, &s_proof_2));
    }
}
