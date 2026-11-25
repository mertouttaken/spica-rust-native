//! Async Minecraft 1.21.4 (Protocol 769) offline-mode bot state machine
//! (Handshaking -> Login -> Configuration -> Play) and thread-safe telemetry counters.

use crate::protocol::{
    offline_uuid, read_i32_be, read_i64_be, read_packet, read_varint_slice, send_packet, write_f32,
    write_i32, write_i64, write_mc_string, write_u16, write_varint, PROTOCOL_VERSION_1_21_4,
};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpStream;
use tokio::sync::watch;

// Bit-verified Purpur 1.21.4 (Protocol 769) Packet IDs
// Handshaking
const SB_HANDSHAKE: i32 = 0x00;

// Login
const SB_LOGIN_HELLO: i32 = 0x00;
const SB_LOGIN_CUSTOM_QUERY_ANSWER: i32 = 0x02;
const SB_LOGIN_ACKNOWLEDGED: i32 = 0x03;

const CB_LOGIN_DISCONNECT: i32 = 0x00;
const CB_LOGIN_ENCRYPTION_REQ: i32 = 0x01;
const CB_LOGIN_FINISHED: i32 = 0x02;
const CB_LOGIN_SET_COMPRESSION: i32 = 0x03;
const CB_LOGIN_CUSTOM_QUERY: i32 = 0x04;

// Configuration
const SB_CONFIG_CLIENT_INFO: i32 = 0x00;
const SB_CONFIG_FINISH: i32 = 0x03;
const SB_CONFIG_KEEP_ALIVE: i32 = 0x04;
const SB_CONFIG_PONG: i32 = 0x05;
const SB_CONFIG_SELECT_KNOWN_PACKS: i32 = 0x07;

const CB_CONFIG_DISCONNECT: i32 = 0x02;
const CB_CONFIG_FINISH: i32 = 0x03;
const CB_CONFIG_KEEP_ALIVE: i32 = 0x04;
const CB_CONFIG_PING: i32 = 0x05;
const CB_CONFIG_SELECT_KNOWN_PACKS: i32 = 0x0E;

// Play (Game)
const SB_PLAY_ACCEPT_TELEPORT: i32 = 0x00;
const SB_PLAY_CHUNK_BATCH_RECEIVED: i32 = 0x09;
const SB_PLAY_KEEP_ALIVE: i32 = 0x1A;
const SB_PLAY_MOVE_ROT: i32 = 0x1E;
const SB_PLAY_PONG: i32 = 0x2B;

const CB_PLAY_ADD_ENTITY: i32 = 0x01;
const CB_PLAY_CHUNK_BATCH_FINISHED: i32 = 0x0C;
const CB_PLAY_KEEP_ALIVE: i32 = 0x27;
const CB_PLAY_MOVE_ENTITY_POS: i32 = 0x2F;
const CB_PLAY_MOVE_ENTITY_POS_ROT: i32 = 0x30;
const CB_PLAY_MOVE_ENTITY_ROT: i32 = 0x32;
const CB_PLAY_PING: i32 = 0x37;
const CB_PLAY_PLAYER_POSITION: i32 = 0x42;
const CB_PLAY_REMOVE_ENTITIES: i32 = 0x47;
const CB_PLAY_ROTATE_HEAD: i32 = 0x4D;
const CB_PLAY_SET_ENTITY_DATA: i32 = 0x5D;
const CB_PLAY_SET_ENTITY_MOTION: i32 = 0x5F;
const CB_PLAY_TELEPORT_ENTITY: i32 = 0x77;

#[derive(Default)]
pub struct BenchMetrics {
    pub attempted: AtomicU64,
    pub connected_play: AtomicU64,
    pub active_play: AtomicU64,
    pub failed_connect: AtomicU64,
    pub disconnected_early: AtomicU64,
    pub entity_spawn: AtomicU64,
    pub entity_move: AtomicU64,
    pub entity_meta: AtomicU64,
    pub entity_remove: AtomicU64,
    pub keepalives_acked: AtomicU64,
    pub total_packets_in: AtomicU64,
    pub total_bytes_in: AtomicU64,
}

impl BenchMetrics {
    pub fn total_entity_packets(&self) -> u64 {
        self.entity_spawn.load(Ordering::Relaxed)
            + self.entity_move.load(Ordering::Relaxed)
            + self.entity_meta.load(Ordering::Relaxed)
            + self.entity_remove.load(Ordering::Relaxed)
    }
}

pub async fn run_bot(
    host: String,
    port: u16,
    username: String,
    metrics: Arc<BenchMetrics>,
    mut stop_rx: watch::Receiver<bool>,
) {
    metrics.attempted.fetch_add(1, Ordering::Relaxed);

    if *stop_rx.borrow() {
        return;
    }

    let addr = format!("{}:{}", host, port);
    let mut stream = match tokio::time::timeout(Duration::from_secs(10), TcpStream::connect(&addr)).await {
        Ok(Ok(s)) => s,
        _ => {
            metrics.failed_connect.fetch_add(1, Ordering::Relaxed);
            return;
        }
    };
    let _ = stream.set_nodelay(true);

    let mut compression: Option<usize> = None;

    // 1. Handshake (Intent = 2: Login)
    let mut hs = Vec::with_capacity(32);
    write_varint(&mut hs, PROTOCOL_VERSION_1_21_4);
    write_mc_string(&mut hs, &host);
    write_u16(&mut hs, port);
    write_varint(&mut hs, 2);
    if send_packet(&mut stream, SB_HANDSHAKE, &hs, compression)
        .await
        .is_err()
    {
        metrics.failed_connect.fetch_add(1, Ordering::Relaxed);
        return;
    }

    // 2. Login Hello (username + 16-byte offline UUID)
    let mut hello = Vec::with_capacity(36);
    write_mc_string(&mut hello, &username);
    hello.extend_from_slice(&offline_uuid(&username));
    if send_packet(&mut stream, SB_LOGIN_HELLO, &hello, compression)
        .await
        .is_err()
    {
        metrics.failed_connect.fetch_add(1, Ordering::Relaxed);
        return;
    }

    // Login Phase Loop
    loop {
        let pkt = tokio::select! {
            _ = stop_rx.changed() => {
                if *stop_rx.borrow() {
                    return;
                }
                continue;
            }
            res = tokio::time::timeout(Duration::from_secs(15), read_packet(&mut stream, compression)) => {
                match res {
                    Ok(Ok(p)) => p,
                    _ => {
                        metrics.failed_connect.fetch_add(1, Ordering::Relaxed);
                        return;
                    }
                }
            }
        };

        match pkt.id {
            CB_LOGIN_SET_COMPRESSION => {
                if let Ok((threshold, _)) = read_varint_slice(&pkt.payload) {
                    compression = if threshold >= 0 {
                        Some(threshold as usize)
                    } else {
                        None
                    };
                }
            }
            CB_LOGIN_FINISHED => {
                if send_packet(&mut stream, SB_LOGIN_ACKNOWLEDGED, &[], compression)
                    .await
                    .is_err()
                {
                    metrics.failed_connect.fetch_add(1, Ordering::Relaxed);
                    return;
                }
                break;
            }
            CB_LOGIN_CUSTOM_QUERY => {
                if let Ok((msg_id, _)) = read_varint_slice(&pkt.payload) {
                    let mut ans = Vec::with_capacity(8);
                    write_varint(&mut ans, msg_id);
                    ans.push(0); // has_payload = false
                    let _ =
                        send_packet(&mut stream, SB_LOGIN_CUSTOM_QUERY_ANSWER, &ans, compression)
                            .await;
                }
            }
            CB_LOGIN_ENCRYPTION_REQ | CB_LOGIN_DISCONNECT => {
                metrics.failed_connect.fetch_add(1, Ordering::Relaxed);
                return;
            }
            _ => {}
        }
    }

    // 3. Configuration Phase: Send Client Information (1.21.4 format)
    let mut client_info = Vec::with_capacity(32);
    write_mc_string(&mut client_info, "en_US");
    client_info.push(8u8); // view distance
    write_varint(&mut client_info, 0); // chat mode = enabled
    client_info.push(1u8); // chat colors = true
    client_info.push(0x7Fu8); // displayed skin parts
    write_varint(&mut client_info, 1); // main hand = right
    client_info.push(0u8); // text filtering = false
    client_info.push(1u8); // allow server listings = true
    write_varint(&mut client_info, 0); // particle status = all (1.21.2+)
    if send_packet(&mut stream, SB_CONFIG_CLIENT_INFO, &client_info, compression)
        .await
        .is_err()
    {
        metrics.failed_connect.fetch_add(1, Ordering::Relaxed);
        return;
    }

    // Configuration Phase Loop
    loop {
        let pkt = tokio::select! {
            _ = stop_rx.changed() => {
                if *stop_rx.borrow() {
                    return;
                }
                continue;
            }
            res = tokio::time::timeout(Duration::from_secs(15), read_packet(&mut stream, compression)) => {
                match res {
                    Ok(Ok(p)) => p,
                    _ => {
                        metrics.failed_connect.fetch_add(1, Ordering::Relaxed);
                        return;
                    }
                }
            }
        };

        match pkt.id {
            CB_CONFIG_SELECT_KNOWN_PACKS => {
                // Acknowledge with 0 known packs so server sends all default registries
                let mut resp = Vec::with_capacity(2);
                write_varint(&mut resp, 0);
                let _ = send_packet(
                    &mut stream,
                    SB_CONFIG_SELECT_KNOWN_PACKS,
                    &resp,
                    compression,
                )
                .await;
            }
            CB_CONFIG_KEEP_ALIVE => {
                if let Ok(ka_id) = read_i64_be(&pkt.payload) {
                    let mut resp = Vec::with_capacity(8);
                    write_i64(&mut resp, ka_id);
                    let _ = send_packet(&mut stream, SB_CONFIG_KEEP_ALIVE, &resp, compression).await;
                }
            }
            CB_CONFIG_PING => {
                if let Ok(ping_id) = read_i32_be(&pkt.payload) {
                    let mut resp = Vec::with_capacity(4);
                    write_i32(&mut resp, ping_id);
                    let _ = send_packet(&mut stream, SB_CONFIG_PONG, &resp, compression).await;
                }
            }
            CB_CONFIG_FINISH => {
                if send_packet(&mut stream, SB_CONFIG_FINISH, &[], compression)
                    .await
                    .is_err()
                {
                    metrics.failed_connect.fetch_add(1, Ordering::Relaxed);
                    return;
                }
                break;
            }
            CB_CONFIG_DISCONNECT => {
                metrics.failed_connect.fetch_add(1, Ordering::Relaxed);
                return;
            }
            _ => {}
        }
    }

    // 4. Play Phase — Bot is fully in-world!
    // Split TCP stream so `read_packet` is NEVER cancelled mid-frame by the 250ms look-rotation timer.
    metrics.connected_play.fetch_add(1, Ordering::Relaxed);
    metrics.active_play.fetch_add(1, Ordering::Relaxed);

    let (mut reader, writer) = stream.into_split();
    let writer = Arc::new(tokio::sync::Mutex::new(writer));
    let (tp_tx, mut tp_rx) = watch::channel(false);

    let rot_writer = Arc::clone(&writer);
    let mut rot_stop_rx = stop_rx.clone();
    let rot_task = tokio::spawn(async move {
        // Wait until server sends initial Player Position teleport before rotating look vector
        while !*tp_rx.borrow() {
            tokio::select! {
                _ = rot_stop_rx.changed() => {
                    if *rot_stop_rx.borrow() {
                        return;
                    }
                }
                res = tp_rx.changed() => {
                    if res.is_err() {
                        return;
                    }
                }
            }
        }

        let mut look_ticker = tokio::time::interval(Duration::from_millis(250));
        look_ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut yaw: f32 = 0.0;

        loop {
            tokio::select! {
                _ = rot_stop_rx.changed() => {
                    if *rot_stop_rx.borrow() {
                        break;
                    }
                }
                _ = look_ticker.tick() => {
                    yaw = (yaw + 22.5) % 360.0;
                    let pitch = (yaw.to_radians().sin()) * 15.0;
                    let mut rot = Vec::with_capacity(9);
                    write_f32(&mut rot, yaw);
                    write_f32(&mut rot, pitch);
                    rot.push(0x01); // flags: onGround = true
                    let mut w = rot_writer.lock().await;
                    if send_packet(&mut *w, SB_PLAY_MOVE_ROT, &rot, compression).await.is_err() {
                        break;
                    }
                }
            }
        }
    });

    loop {
        tokio::select! {
            _ = stop_rx.changed() => {
                if *stop_rx.borrow() {
                    break;
                }
            }
            res = read_packet(&mut reader, compression) => {
                let pkt = match res {
                    Ok(p) => p,
                    Err(_) => {
                        if !*stop_rx.borrow() {
                            metrics.disconnected_early.fetch_add(1, Ordering::Relaxed);
                        }
                        break;
                    }
                };

                metrics.total_packets_in.fetch_add(1, Ordering::Relaxed);
                metrics.total_bytes_in.fetch_add(pkt.wire_bytes as u64, Ordering::Relaxed);

                match pkt.id {
                    CB_PLAY_KEEP_ALIVE => {
                        if let Ok(ka_id) = read_i64_be(&pkt.payload) {
                            let mut resp = Vec::with_capacity(8);
                            write_i64(&mut resp, ka_id);
                            let mut w = writer.lock().await;
                            let _ = send_packet(&mut *w, SB_PLAY_KEEP_ALIVE, &resp, compression).await;
                            metrics.keepalives_acked.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                    CB_PLAY_PING => {
                        if let Ok(ping_id) = read_i32_be(&pkt.payload) {
                            let mut resp = Vec::with_capacity(4);
                            write_i32(&mut resp, ping_id);
                            let mut w = writer.lock().await;
                            let _ = send_packet(&mut *w, SB_PLAY_PONG, &resp, compression).await;
                        }
                    }
                    CB_PLAY_PLAYER_POSITION => {
                        if let Ok((tp_id, _)) = read_varint_slice(&pkt.payload) {
                            let mut resp = Vec::with_capacity(5);
                            write_varint(&mut resp, tp_id);
                            let mut w = writer.lock().await;
                            let _ = send_packet(&mut *w, SB_PLAY_ACCEPT_TELEPORT, &resp, compression).await;
                            let _ = tp_tx.send(true);
                        }
                    }
                    CB_PLAY_CHUNK_BATCH_FINISHED => {
                        let mut resp = Vec::with_capacity(4);
                        write_f32(&mut resp, 20.0);
                        let mut w = writer.lock().await;
                        let _ = send_packet(&mut *w, SB_PLAY_CHUNK_BATCH_RECEIVED, &resp, compression).await;
                    }
                    CB_PLAY_ADD_ENTITY => {
                        metrics.entity_spawn.fetch_add(1, Ordering::Relaxed);
                    }
                    CB_PLAY_MOVE_ENTITY_POS
                    | CB_PLAY_MOVE_ENTITY_POS_ROT
                    | CB_PLAY_MOVE_ENTITY_ROT
                    | CB_PLAY_ROTATE_HEAD
                    | CB_PLAY_SET_ENTITY_MOTION
                    | CB_PLAY_TELEPORT_ENTITY => {
                        metrics.entity_move.fetch_add(1, Ordering::Relaxed);
                    }
                    CB_PLAY_SET_ENTITY_DATA => {
                        metrics.entity_meta.fetch_add(1, Ordering::Relaxed);
                    }
                    CB_PLAY_REMOVE_ENTITIES => {
                        metrics.entity_remove.fetch_add(1, Ordering::Relaxed);
                    }
                    _ => {}
                }
            }
        }
    }

    rot_task.abort();
    metrics.active_play.fetch_sub(1, Ordering::Relaxed);
}
