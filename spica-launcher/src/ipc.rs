use crate::brain::{BrainState, TelemetryPayload};
use serde::Deserialize;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::watch;
use tokio::task::JoinHandle;

const MAX_IPC_LINE_BYTES: usize = 64 * 1024;
const HELLO_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Deserialize)]
struct HelloMessage {
    #[serde(rename = "type")]
    msg_type: String,
    #[serde(default)]
    token: String,
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

/// Binds the Brain IPC TCP server and spawns the async accept loop.
/// Binding happens before returning so the port is guaranteed ready before Purpur boots.
pub async fn start_ipc_server(
    host: &str,
    port: u16,
    expected_token: String,
    brain: Arc<Mutex<BrainState>>,
    mut shutdown_rx: watch::Receiver<bool>,
) -> Result<JoinHandle<()>, String> {
    let addr = format!("{}:{}", host, port);
    let listener = TcpListener::bind(&addr)
        .await
        .map_err(|e| format!("HATA: Brain IPC sunucusu '{}' adresine baglanamadi: {}", addr, e))?;

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
                            let token_clone = expected_token.clone();
                            let brain_clone = Arc::clone(&brain);
                            let client_shutdown = shutdown_rx.clone();
                            tokio::spawn(async move {
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

    // Step 1: Mandatory hello handshake line with token verification
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

    if hello.msg_type != "hello" || !constant_time_eq(&hello.token, &expected_token) {
        eprintln!(
            "[SpicaBrain IPC] {} gecersiz token veya mesaj tipi (baglanti reddedildi)",
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

    println!(
        "[SpicaBrain IPC] Purpur baglandi: {} | server={} | plugin=v{}",
        peer, server_label, plugin_ver
    );

    // Step 2: Continuous NDJSON telemetry -> directive loop
    loop {
        let bytes_read = tokio::select! {
            _ = shutdown_rx.changed() => {
                if *shutdown_rx.borrow() {
                    break;
                }
                continue;
            }
            res = read_bounded_line(&mut reader, &mut buf, MAX_IPC_LINE_BYTES) => {
                match res {
                    Ok(0) => break,
                    Ok(count) => count,
                    Err(e) => {
                        eprintln!("[SpicaBrain IPC] {} okuma hatasi: {}", peer, e);
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

                let directive = match brain.lock() {
                    Ok(mut guard) => guard.ingest_telemetry(&telemetry),
                    Err(poisoned) => poisoned.into_inner().ingest_telemetry(&telemetry),
                };

                let mut serialized = match serde_json::to_string(&directive) {
                    Ok(s) => s,
                    Err(e) => {
                        eprintln!("[SpicaBrain IPC] Direktif serilestirme hatasi: {}", e);
                        continue;
                    }
                };
                serialized.push('\n');

                if let Err(e) = write_half.write_all(serialized.as_bytes()).await {
                    eprintln!("[SpicaBrain IPC] {} yazma hatasi: {}", peer, e);
                    break;
                }
                if let Err(e) = write_half.flush().await {
                    eprintln!("[SpicaBrain IPC] {} flush hatasi: {}", peer, e);
                    break;
                }
            }
            "ping" => {
                if write_half.write_all(b"{\"type\":\"pong\"}\n").await.is_err()
                    || write_half.flush().await.is_err()
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
}
