//! Parallel Mob Tracking & Visibility Culling Engine (v2.4.0).
//!
//! Evaluates per-player mob visibility across `(player, mob)` pairs using Rayon
//! parallelism, combining:
//! - Always-visible immunity for bosses, combat targets, tamed/named/mounted mobs
//! - Safety near-zone protection (exempt from cave/vertical and rear-FOV culling)
//! - Cave / vertical delta culling (`|dy| > eff_vert`)
//! - Rear-FOV dot-product culling behind each player's look vector
//! - Hysteresis margins for previously hidden mobs to prevent boundary flicker
//! - Per-player Top-K visibility budget with hostile-over-passive priority
//! - Bitset-synchronized delta output (`HIDE` / `SHOW`) with backpressure safety

use rayon::prelude::*;

// ---------------------------------------------------------------------------
// Mob Input Flag Bitmask Constants (`mob_flags`)
// ---------------------------------------------------------------------------

/// Bosses, combat targets, tamed/named/mounted mobs — NEVER hidden.
pub const TRACK_FLAG_ALWAYS_VISIBLE: i32 = 1;
/// Hostile mob — higher priority in per-player Top-K visibility budget.
pub const TRACK_FLAG_HOSTILE: i32 = 2;
/// Passive mob — lower priority in per-player Top-K visibility budget.
pub const TRACK_FLAG_PASSIVE: i32 = 4;

// ---------------------------------------------------------------------------
// Output Delta Action Constants (`out_delta_action`)
// ---------------------------------------------------------------------------

/// Transition mob from visible -> hidden for this player.
pub const TRACK_ACTION_HIDE: i32 = 0;
/// Transition mob from hidden -> visible for this player.
pub const TRACK_ACTION_SHOW: i32 = 1;

/// Minimum `(players * mobs)` pair count before fanning out across Rayon workers.
const PARALLEL_PAIR_THRESHOLD: usize = 512;

/// Priority boost keeping safety-near mobs ranked ahead of all mid/far mobs
/// while still allowing Top-K budget capping inside dense nearby mob farms.
const SAFETY_NEAR_SCORE_BIAS: f32 = -500_000.0;

/// Priority boost for `TRACK_FLAG_ALWAYS_VISIBLE` mobs (also immune from budget cap).
const ALWAYS_VISIBLE_SCORE_BIAS: f32 = -1_000_000.0;

struct PlayerTrackResult {
    show_mobs: Vec<usize>,
    hide_mobs: Vec<usize>,
    hidden_count: i32,
    visible_count: i32,
    culled_by_budget: i32,
}

/// Per-worker reusable scratch buffers (O2) — allocated once per Rayon worker thread
/// instead of once per player, eliminating per-player heap churn on large servers.
struct WorkerScratch {
    candidates: Vec<(f32, usize, bool)>,
    desired_visible: Vec<u8>,
}

impl WorkerScratch {
    fn new(m: usize, cap_hint: usize) -> Self {
        Self {
            candidates: Vec::with_capacity(m.min(cap_hint)),
            desired_visible: vec![0u8; m],
        }
    }
}

/// Evaluate per-player mob visibility and emit bitset-synchronized HIDE/SHOW deltas.
///
/// `hidden_bitset` stores 1 bit per `(player_idx, mob_idx)` pair at bit offset
/// `player_idx * mob_count + mob_idx` (`0` = currently visible, `1` = currently hidden).
/// Bits are updated in-place **only** for deltas that fit within
/// `max_deltas = min(out_delta_player_idx.len(), out_delta_mob_idx.len(), out_delta_action.len())`,
/// keeping Java and Rust state 100% synchronized across ticks under backpressure.
///
/// Writes summary statistics into `out_stats[0..4]` (when `out_stats.len() >= 4`):
/// - `out_stats[0]` = `total_hidden`
/// - `out_stats[1]` = `total_visible`
/// - `out_stats[2]` = `deltas_written`
/// - `out_stats[3]` = `total_culled_by_budget`
///
/// Returns `deltas_written as i32`, or `-1` if `hidden_bitset` is too small or overflows.
#[allow(clippy::too_many_arguments)]
pub fn compute_mob_tracking(
    mob_xs: &[f32],
    mob_ys: &[f32],
    mob_zs: &[f32],
    mob_flags: &[i32],
    player_xs: &[f32],
    player_ys: &[f32],
    player_zs: &[f32],
    player_look_xs: &[f32],
    player_look_ys: &[f32],
    player_look_zs: &[f32],
    max_track_range: f32,
    safety_near_range: f32,
    max_vertical_delta: f32,
    rear_cull_range: f32,
    rear_cull_dot_threshold: f32,
    max_mobs_per_player: i32,
    brain_state_code: i32,
    hidden_bitset: &mut [u8],
    out_delta_player_idx: &mut [i32],
    out_delta_mob_idx: &mut [i32],
    out_delta_action: &mut [i32],
    out_stats: &mut [i32],
) -> i32 {
    let m = mob_xs
        .len()
        .min(mob_ys.len())
        .min(mob_zs.len())
        .min(mob_flags.len());
    let p = player_xs
        .len()
        .min(player_ys.len())
        .min(player_zs.len())
        .min(player_look_xs.len())
        .min(player_look_ys.len())
        .min(player_look_zs.len());

    if m == 0 || p == 0 {
        if out_stats.len() >= 4 {
            out_stats[0] = 0;
            out_stats[1] = 0;
            out_stats[2] = 0;
            out_stats[3] = 0;
        }
        return 0;
    }

    let Some(total_bits) = p.checked_mul(m) else {
        return -1;
    };
    let needed_bytes = (total_bits >> 3) + usize::from((total_bits & 7) != 0);
    if hidden_bitset.len() < needed_bytes {
        return -1;
    }

    // Dynamic Brain State Scaling:
    //   <= 0 (GREEN)    : 1.00 / 1.00 / 1.00
    //      1 (YELLOW)   : 0.90 / 0.85 / 0.90
    //      2 (ORANGE)   : 0.78 / 0.70 / 0.80
    //      3 (RED)      : 0.65 / 0.55 / 0.70
    //   >= 4 (EMERGENCY): 0.52 / 0.40 / 0.60
    let (range_scale, budget_scale, vert_scale): (f32, f32, f32) = match brain_state_code {
        c if c <= 0 => (1.0, 1.0, 1.0),
        1 => (0.90, 0.85, 0.90),
        2 => (0.78, 0.70, 0.80),
        3 => (0.65, 0.55, 0.70),
        _ => (0.52, 0.40, 0.60),
    };

    // Sanitized effective bounds
    let safe_near = if safety_near_range.is_finite() {
        safety_near_range.max(4.0)
    } else {
        12.0
    };
    let eff_range = ((if max_track_range.is_finite() && max_track_range > 0.0 {
        max_track_range
    } else {
        48.0
    }) * range_scale)
        .max(safe_near + 4.0);
    let eff_vert = ((if max_vertical_delta.is_finite() && max_vertical_delta > 0.0 {
        max_vertical_delta
    } else {
        24.0
    }) * vert_scale)
        .max(8.0);
    let eff_rear = if rear_cull_range.is_finite() && rear_cull_range > 0.0 {
        rear_cull_range.max(safe_near)
    } else {
        safe_near + 2.0
    };
    let eff_dot = if rear_cull_dot_threshold.is_finite() {
        rear_cull_dot_threshold.clamp(-0.95, 0.5)
    } else {
        -0.25
    };
    let eff_budget = (((max_mobs_per_player.max(8)) as f32) * budget_scale)
        .round()
        .max(8.0) as usize;

    // Precompute squared bounds and hysteresis thresholds
    let safe_near_sq = safe_near * safe_near;
    let eff_rear_sq = eff_rear * eff_rear;

    let r_vis_sq = eff_range * eff_range;
    let r_hid = (eff_range - 3.0).max(safe_near);
    let r_hid_sq = r_hid * r_hid;

    let v_vis = eff_vert;
    let v_hid = (eff_vert - 2.0).max(6.0);

    let dot_vis = eff_dot;
    let dot_hid = (eff_dot + 0.12).min(0.6);

    // Parallel per-player evaluation (reads `hidden_bitset` immutably) using per-worker scratch (O2).
    let per_player_results: Vec<PlayerTrackResult> = {
        let prev_bitset: &[u8] = &*hidden_bitset;
        let cap_hint = eff_budget.saturating_mul(2).max(16);

        let eval_player = |scratch: &mut WorkerScratch, pi: usize| -> PlayerTrackResult {
            let px = player_xs[pi];
            let py = player_ys[pi];
            let pz = player_zs[pi];
            let player_finite = px.is_finite() && py.is_finite() && pz.is_finite();

            let raw_lx = player_look_xs[pi];
            let raw_ly = player_look_ys[pi];
            let raw_lz = player_look_zs[pi];
            let look_len_sq = raw_lx * raw_lx + raw_ly * raw_ly + raw_lz * raw_lz;
            let (has_look, lx, ly, lz) = if raw_lx.is_finite()
                && raw_ly.is_finite()
                && raw_lz.is_finite()
                && look_len_sq > 1e-12
            {
                let inv_len = 1.0 / look_len_sq.sqrt();
                (true, raw_lx * inv_len, raw_ly * inv_len, raw_lz * inv_len)
            } else {
                (false, 0.0, 0.0, 0.0)
            };

            let base_bit = pi * m;
            scratch.candidates.clear();
            scratch.desired_visible.fill(0);

            // Pass A: Spatial + Cave/Vertical + Rear-FOV + Hysteresis filter
            for mi in 0..m {
                let bit = base_bit + mi;
                let prev_hidden = (prev_bitset[bit >> 3] & (1u8 << (bit & 7))) != 0;
                let flags = mob_flags[mi];

                if !player_finite {
                    scratch.candidates.push((0.0, mi, true));
                    continue;
                }

                let dx = mob_xs[mi] - px;
                let dy = mob_ys[mi] - py;
                let dz = mob_zs[mi] - pz;
                let d2 = dx * dx + dy * dy + dz * dz;

                if (flags & TRACK_FLAG_ALWAYS_VISIBLE) != 0 {
                    let finite_d2 = if d2.is_finite() { d2 } else { 0.0 };
                    scratch
                        .candidates
                        .push((ALWAYS_VISIBLE_SCORE_BIAS + finite_d2, mi, true));
                    continue;
                }

                if !d2.is_finite() {
                    continue;
                }

                let base_score = if (flags & TRACK_FLAG_HOSTILE) != 0 {
                    d2 * 0.65
                } else {
                    d2
                };

                // Safety near zone (Y1): exempt from cave/vertical, rear-FOV, AND Top-K budget culling
                if d2 <= safe_near_sq {
                    scratch
                        .candidates
                        .push((SAFETY_NEAR_SCORE_BIAS + base_score, mi, true));
                    continue;
                }

                let (r_lim_sq, v_lim, dot_lim) = if prev_hidden {
                    (r_hid_sq, v_hid, dot_hid)
                } else {
                    (r_vis_sq, v_vis, dot_vis)
                };

                if d2 > r_lim_sq {
                    continue;
                }
                if dy.abs() > v_lim {
                    continue;
                }
                if has_look && d2 > eff_rear_sq {
                    let dot = (dx * lx + dy * ly + dz * lz) / d2.sqrt();
                    if dot < dot_lim {
                        continue;
                    }
                }

                scratch.candidates.push((base_score, mi, false));
            }

            // Pass B: Per-player Top-K visibility budget cap
            let mut culled_by_budget = 0i32;

            if scratch.candidates.len() <= eff_budget {
                for &(_, mi, _) in &scratch.candidates {
                    scratch.desired_visible[mi] = 1;
                }
            } else {
                let mut immune_count = 0usize;
                for idx in 0..scratch.candidates.len() {
                    if scratch.candidates[idx].2 {
                        scratch.candidates.swap(immune_count, idx);
                        immune_count += 1;
                    }
                }

                for &(_, mi, _) in &scratch.candidates[..immune_count] {
                    scratch.desired_visible[mi] = 1;
                }

                let remaining_slots = eff_budget.saturating_sub(immune_count);
                let non_immune = &mut scratch.candidates[immune_count..];

                if remaining_slots == 0 {
                    culled_by_budget = non_immune.len() as i32;
                } else if non_immune.len() > remaining_slots {
                    non_immune.select_nth_unstable_by(remaining_slots - 1, |a, b| {
                        a.0.total_cmp(&b.0).then_with(|| a.1.cmp(&b.1))
                    });
                    for &(_, mi, _) in &non_immune[..remaining_slots] {
                        scratch.desired_visible[mi] = 1;
                    }
                    culled_by_budget = (non_immune.len() - remaining_slots) as i32;
                } else {
                    for &(_, mi, _) in non_immune.iter() {
                        scratch.desired_visible[mi] = 1;
                    }
                }
            }

            // Pass C: Diff desired state against `prev_hidden` into separated SHOW / HIDE queues
            let mut show_mobs: Vec<usize> = Vec::new();
            let mut hide_mobs: Vec<usize> = Vec::new();
            let mut hidden_count = 0i32;
            let mut visible_count = 0i32;

            for mi in 0..m {
                let bit = base_bit + mi;
                let prev_hidden = (prev_bitset[bit >> 3] & (1u8 << (bit & 7))) != 0;
                let want_hidden = scratch.desired_visible[mi] == 0;

                if want_hidden {
                    hidden_count += 1;
                } else {
                    visible_count += 1;
                }

                if want_hidden != prev_hidden {
                    if want_hidden {
                        hide_mobs.push(mi);
                    } else {
                        show_mobs.push(mi);
                    }
                }
            }

            PlayerTrackResult {
                show_mobs,
                hide_mobs,
                hidden_count,
                visible_count,
                culled_by_budget,
            }
        };

        if p >= 2 && total_bits >= PARALLEL_PAIR_THRESHOLD {
            (0..p)
                .into_par_iter()
                .map_init(|| WorkerScratch::new(m, cap_hint), eval_player)
                .collect()
        } else {
            let mut scratch = WorkerScratch::new(m, cap_hint);
            (0..p).map(|pi| eval_player(&mut scratch, pi)).collect()
        }
    };

    // Sequential fair round-robin merge (Y1 + O2):
    // Pass 1 emits TRACK_ACTION_SHOW across players in round-robin order BEFORE any HIDE.
    // Pass 2 emits TRACK_ACTION_HIDE across players in round-robin order with remaining budget,
    // preventing player 0 from starving players 1..p-1 under backpressure.
    let max_deltas = out_delta_player_idx
        .len()
        .min(out_delta_mob_idx.len())
        .min(out_delta_action.len());

    let mut deltas_written = 0usize;
    let mut total_hidden = 0i32;
    let mut total_visible = 0i32;
    let mut total_culled_by_budget = 0i32;

    let mut player_hidden_counts: Vec<i32> = per_player_results
        .iter()
        .map(|r| r.hidden_count)
        .collect();
    let mut player_visible_counts: Vec<i32> = per_player_results
        .iter()
        .map(|r| r.visible_count)
        .collect();
    for res in &per_player_results {
        total_culled_by_budget = total_culled_by_budget.saturating_add(res.culled_by_budget);
    }

    // Pass 1: Fair round-robin SHOW emission across all players (exits early when max_deltas is full)
    let mut show_cursors = vec![0usize; p];
    if deltas_written < max_deltas {
        'show_rr: loop {
            let mut progressed = false;
            for (pi, res) in per_player_results.iter().enumerate() {
                let cursor = show_cursors[pi];
                if cursor >= res.show_mobs.len() {
                    continue;
                }
                progressed = true;
                let mi = res.show_mobs[cursor];
                show_cursors[pi] = cursor + 1;
                let bit = pi * m + mi;
                hidden_bitset[bit >> 3] &= !(1u8 << (bit & 7));
                out_delta_player_idx[deltas_written] = pi as i32;
                out_delta_mob_idx[deltas_written] = mi as i32;
                out_delta_action[deltas_written] = TRACK_ACTION_SHOW;
                deltas_written += 1;
                if deltas_written >= max_deltas {
                    break 'show_rr;
                }
            }
            if !progressed {
                break;
            }
        }
    }
    // O(p) reconciliation for any un-emitted SHOW deltas under backpressure
    for (pi, res) in per_player_results.iter().enumerate() {
        let rem_show = (res.show_mobs.len() - show_cursors[pi]) as i32;
        if rem_show > 0 {
            player_visible_counts[pi] -= rem_show;
            player_hidden_counts[pi] += rem_show;
        }
    }

    // Pass 2: Fair round-robin HIDE emission across all players with remaining budget (early exit)
    let mut hide_cursors = vec![0usize; p];
    if deltas_written < max_deltas {
        'hide_rr: loop {
            let mut progressed = false;
            for (pi, res) in per_player_results.iter().enumerate() {
                let cursor = hide_cursors[pi];
                if cursor >= res.hide_mobs.len() {
                    continue;
                }
                progressed = true;
                let mi = res.hide_mobs[cursor];
                hide_cursors[pi] = cursor + 1;
                let bit = pi * m + mi;
                hidden_bitset[bit >> 3] |= 1u8 << (bit & 7);
                out_delta_player_idx[deltas_written] = pi as i32;
                out_delta_mob_idx[deltas_written] = mi as i32;
                out_delta_action[deltas_written] = TRACK_ACTION_HIDE;
                deltas_written += 1;
                if deltas_written >= max_deltas {
                    break 'hide_rr;
                }
            }
            if !progressed {
                break;
            }
        }
    }
    // O(p) reconciliation for any un-emitted HIDE deltas under backpressure
    for (pi, res) in per_player_results.iter().enumerate() {
        let rem_hide = (res.hide_mobs.len() - hide_cursors[pi]) as i32;
        if rem_hide > 0 {
            player_hidden_counts[pi] -= rem_hide;
            player_visible_counts[pi] += rem_hide;
        }
    }

    for pi in 0..player_hidden_counts.len() {
        total_hidden = total_hidden.saturating_add(player_hidden_counts[pi]);
        total_visible = total_visible.saturating_add(player_visible_counts[pi]);
    }

    let deltas_i32 = deltas_written.min(i32::MAX as usize) as i32;
    if out_stats.len() >= 4 {
        out_stats[0] = total_hidden;
        out_stats[1] = total_visible;
        out_stats[2] = deltas_i32;
        out_stats[3] = total_culled_by_budget;
    }

    deltas_i32
}

#[cfg(test)]
mod tests {
    use super::*;

    fn is_bit_hidden(bitset: &[u8], mob_count: usize, pi: usize, mi: usize) -> bool {
        let bit = pi * mob_count + mi;
        (bitset[bit >> 3] & (1u8 << (bit & 7))) != 0
    }

    #[test]
    fn test_cave_vertical_culling_and_safety_near_zone() {
        // Player at (0, 64, 0) looking +X.
        // Mob 0: directly below in deep cave at y=30 (|dy|=34 > max_vertical_delta=20, d=34 > safe_near=12) -> HIDDEN.
        // Mob 1: below player at y=54 (|dy|=10, d=10 <= safe_near=12) -> VISIBLE even if max_vertical_delta=8.
        // Mob 2: horizontal distance 20, |dy|=10 -> VISIBLE when max_vertical_delta=16, HIDDEN when max_vertical_delta=8.
        let mob_xs = [0.0f32, 0.0, 20.0];
        let mob_ys = [30.0f32, 54.0, 54.0];
        let mob_zs = [0.0f32, 0.0, 0.0];
        let mob_flags = [TRACK_FLAG_PASSIVE, TRACK_FLAG_PASSIVE, TRACK_FLAG_PASSIVE];

        let pxs = [0.0f32];
        let pys = [64.0f32];
        let pzs = [0.0f32];
        let plx = [1.0f32];
        let ply = [0.0f32];
        let plz = [0.0f32];

        let mut bitset = vec![0u8; 1];
        let mut dp = [0i32; 16];
        let mut dm = [0i32; 16];
        let mut da = [0i32; 16];
        let mut stats = [0i32; 4];

        // max_vertical_delta = 8.0, safety_near_range = 12.0
        let deltas = compute_mob_tracking(
            &mob_xs,
            &mob_ys,
            &mob_zs,
            &mob_flags,
            &pxs,
            &pys,
            &pzs,
            &plx,
            &ply,
            &plz,
            48.0,
            12.0,
            8.0,
            16.0,
            -0.25,
            64,
            0, // GREEN
            &mut bitset,
            &mut dp,
            &mut dm,
            &mut da,
            &mut stats,
        );

        // Mob 0 (deep cave) and Mob 2 (outside safe_near & |dy|=10 > 8) should be hidden.
        // Mob 1 (within safe_near=12 even though |dy|=10 > 8) must stay visible!
        assert_eq!(deltas, 2);
        assert_eq!(stats, [2, 1, 2, 0]);
        assert!(is_bit_hidden(&bitset, 3, 0, 0));
        assert!(!is_bit_hidden(&bitset, 3, 0, 1));
        assert!(is_bit_hidden(&bitset, 3, 0, 2));
    }

    #[test]
    fn test_rear_fov_culling_and_always_visible_boss() {
        // Player at (0, 64, 0) looking +X = (1, 0, 0).
        // Mob 0: behind player at x=-25 (> rear_cull_range=16, dot=-1.0 < -0.25) -> HIDDEN
        // Mob 1: in front of player at x=+25 (dot=+1.0) -> VISIBLE
        // Mob 2: boss (TRACK_FLAG_ALWAYS_VISIBLE) behind player at x=-25 -> VISIBLE
        // Mob 3: behind player at x=-14 (<= rear_cull_range=16) -> VISIBLE
        let mob_xs = [-25.0f32, 25.0, -25.0, -14.0];
        let mob_ys = [64.0f32; 4];
        let mob_zs = [0.0f32; 4];
        let mob_flags = [
            TRACK_FLAG_HOSTILE,
            TRACK_FLAG_PASSIVE,
            TRACK_FLAG_ALWAYS_VISIBLE | TRACK_FLAG_HOSTILE,
            TRACK_FLAG_PASSIVE,
        ];

        let pxs = [0.0f32];
        let pys = [64.0f32];
        let pzs = [0.0f32];
        let plx = [1.0f32];
        let ply = [0.0f32];
        let plz = [0.0f32];

        let mut bitset = vec![0u8; 1];
        let mut dp = [0i32; 16];
        let mut dm = [0i32; 16];
        let mut da = [0i32; 16];
        let mut stats = [0i32; 4];

        let deltas = compute_mob_tracking(
            &mob_xs,
            &mob_ys,
            &mob_zs,
            &mob_flags,
            &pxs,
            &pys,
            &pzs,
            &plx,
            &ply,
            &plz,
            48.0,
            12.0,
            24.0,
            16.0,
            -0.25,
            64,
            0, // GREEN
            &mut bitset,
            &mut dp,
            &mut dm,
            &mut da,
            &mut stats,
        );

        assert_eq!(deltas, 1);
        assert_eq!(stats, [1, 3, 1, 0]);
        assert_eq!(dp[0], 0);
        assert_eq!(dm[0], 0);
        assert_eq!(da[0], TRACK_ACTION_HIDE);
        assert!(is_bit_hidden(&bitset, 4, 0, 0));
        assert!(!is_bit_hidden(&bitset, 4, 0, 1));
        assert!(!is_bit_hidden(&bitset, 4, 0, 2));
        assert!(!is_bit_hidden(&bitset, 4, 0, 3));
    }

    #[test]
    fn test_per_player_top_k_priority_budget() {
        // 100 mobs in front of 1 player at (0, 64, 0) looking +X, all inside track range.
        // Indices 0..90: passive mobs at x = 15.0 + i * 0.2 (distances 15.0 .. 32.8).
        // Indices 90..100: hostile mobs at x = 18.0 + j * 0.2 (distances 18.0 .. 19.8;
        //   hostile score = d2 * 0.65 => 210.6 .. 254.826, beating passive mobs from x >= 16.0 => d2 >= 256).
        let mut mob_xs = Vec::with_capacity(100);
        let mob_ys = vec![64.0f32; 100];
        let mob_zs = vec![0.0f32; 100];
        let mut mob_flags = Vec::with_capacity(100);

        for i in 0..90 {
            mob_xs.push(15.0 + (i as f32) * 0.2);
            mob_flags.push(TRACK_FLAG_PASSIVE);
        }
        for j in 0..10 {
            mob_xs.push(18.0 + (j as f32) * 0.2);
            mob_flags.push(TRACK_FLAG_HOSTILE);
        }

        let pxs = [0.0f32];
        let pys = [64.0f32];
        let pzs = [0.0f32];
        let plx = [1.0f32];
        let ply = [0.0f32];
        let plz = [0.0f32];

        let mut bitset = vec![0u8; (100 + 7) / 8];
        let mut dp = vec![0i32; 128];
        let mut dm = vec![0i32; 128];
        let mut da = vec![0i32; 128];
        let mut stats = [0i32; 4];

        let deltas = compute_mob_tracking(
            &mob_xs,
            &mob_ys,
            &mob_zs,
            &mob_flags,
            &pxs,
            &pys,
            &pzs,
            &plx,
            &ply,
            &plz,
            48.0,
            12.0,
            24.0,
            16.0,
            -0.25,
            20, // max_mobs_per_player
            0,  // GREEN
            &mut bitset,
            &mut dp,
            &mut dm,
            &mut da,
            &mut stats,
        );

        assert_eq!(deltas, 80);
        assert_eq!(stats[0], 80); // total_hidden
        assert_eq!(stats[1], 20); // total_visible
        assert_eq!(stats[2], 80); // deltas_written
        assert_eq!(stats[3], 80); // total_culled_by_budget

        // All 10 hostile mobs (indices 90..100) must be visible (prioritized over farther passives).
        for mi in 90..100 {
            assert!(
                !is_bit_hidden(&bitset, 100, 0, mi),
                "hostile mob {mi} should remain visible within Top-20 budget"
            );
        }
        // Closest 10 passive mobs (indices 0..10) must be visible, remaining 80 passives (10..90) hidden.
        for mi in 0..10 {
            assert!(!is_bit_hidden(&bitset, 100, 0, mi));
        }
        for mi in 10..90 {
            assert!(is_bit_hidden(&bitset, 100, 0, mi));
        }
    }

    #[test]
    fn test_safety_near_exempt_from_top_k_and_show_prioritized_over_hide() {
        // 12 ALWAYS_VISIBLE bosses + 4 ordinary mobs inside safety_near_range (x=5..8)
        // with max_mobs_per_player = 8 -> even though bosses alone exceed budget (12 > 8),
        // ALL 4 safety-near mobs MUST remain visible (Y1).
        let mut mob_xs = vec![20.0f32; 12];
        mob_xs.extend_from_slice(&[5.0, 6.0, 7.0, 8.0, 60.0, 61.0, 62.0, 63.0]);
        let mob_ys = vec![64.0f32; 20];
        let mob_zs = vec![0.0f32; 20];
        let mut mob_flags = vec![TRACK_FLAG_ALWAYS_VISIBLE; 12];
        mob_flags.extend_from_slice(&[TRACK_FLAG_HOSTILE; 4]); // indices 12..16: safety-near
        mob_flags.extend_from_slice(&[TRACK_FLAG_PASSIVE; 4]); // indices 16..20: out of range (to HIDE)

        let pxs = [0.0f32];
        let pys = [64.0f32];
        let pzs = [0.0f32];
        let plx = [1.0f32];
        let ply = [0.0f32];
        let plz = [0.0f32];

        // Pretend safety-near mobs (12..16) were previously hidden (bits set to 1) while out-of-range (16..20) were visible (0)
        let mut bitset = vec![0u8; 3];
        for mi in 12..16 {
            bitset[mi >> 3] |= 1u8 << (mi & 7);
        }

        // Tight max_deltas = 5 (4 SHOWs + 4 HIDEs wanted -> all 4 SHOWs MUST come first!)
        let mut dp = [0i32; 5];
        let mut dm = [0i32; 5];
        let mut da = [0i32; 5];
        let mut stats = [0i32; 4];

        let written = compute_mob_tracking(
            &mob_xs,
            &mob_ys,
            &mob_zs,
            &mob_flags,
            &pxs,
            &pys,
            &pzs,
            &plx,
            &ply,
            &plz,
            48.0,
            12.0,
            24.0,
            16.0,
            -0.25,
            8, // budget smaller than 12 bosses
            0,
            &mut bitset,
            &mut dp,
            &mut dm,
            &mut da,
            &mut stats,
        );

        assert_eq!(written, 5);
        // First 4 deltas MUST be TRACK_ACTION_SHOW for safety-near mobs 12..16
        for k in 0..4 {
            assert_eq!(da[k], TRACK_ACTION_SHOW, "delta {k} must prioritize SHOW over HIDE");
        }
        assert_eq!(da[4], TRACK_ACTION_HIDE);
        for mi in 12..16 {
            assert!(!is_bit_hidden(&bitset, 20, 0, mi), "safety-near mob {mi} must be visible");
        }
    }

    #[test]
    fn test_hysteresis_stability_and_delta_only_transitions() {
        // Player at (0, 64, 0) looking +X; max_track_range = 48.0 (hysteresis re-show = 45.0).
        // Mob 0: at x=50.0 (initially hidden on tick 1)
        // Mob 1: at x=20.0 (visible)
        // Mob 2: behind player at x=-30.0 (hidden on tick 1)
        let mut mob_xs = [50.0f32, 20.0, -30.0];
        let mob_ys = [64.0f32; 3];
        let mob_zs = [0.0f32; 3];
        let mob_flags = [TRACK_FLAG_PASSIVE; 3];

        let pxs = [0.0f32];
        let pys = [64.0f32];
        let pzs = [0.0f32];
        let plx = [1.0f32];
        let ply = [0.0f32];
        let plz = [0.0f32];

        let mut bitset = vec![0u8; 1];
        let mut dp = [0i32; 16];
        let mut dm = [0i32; 16];
        let mut da = [0i32; 16];
        let mut stats = [0i32; 4];

        // Tick 1: initial hide transitions for Mob 0 and Mob 2
        let d1 = compute_mob_tracking(
            &mob_xs,
            &mob_ys,
            &mob_zs,
            &mob_flags,
            &pxs,
            &pys,
            &pzs,
            &plx,
            &ply,
            &plz,
            48.0,
            12.0,
            24.0,
            16.0,
            -0.25,
            32,
            0,
            &mut bitset,
            &mut dp,
            &mut dm,
            &mut da,
            &mut stats,
        );
        assert_eq!(d1, 2);
        assert_eq!(stats, [2, 1, 2, 0]);

        // Tick 2: identical positions must emit 0 deltas!
        let d2 = compute_mob_tracking(
            &mob_xs,
            &mob_ys,
            &mob_zs,
            &mob_flags,
            &pxs,
            &pys,
            &pzs,
            &plx,
            &ply,
            &plz,
            48.0,
            12.0,
            24.0,
            16.0,
            -0.25,
            32,
            0,
            &mut bitset,
            &mut dp,
            &mut dm,
            &mut da,
            &mut stats,
        );
        assert_eq!(d2, 0);
        assert_eq!(stats, [2, 1, 0, 0]);

        // Tick 3: move hidden Mob 0 from 50.0 -> 46.5 (inside 48.0, outside hysteresis 45.0) => still hidden, 0 deltas!
        mob_xs[0] = 46.5;
        let d3 = compute_mob_tracking(
            &mob_xs,
            &mob_ys,
            &mob_zs,
            &mob_flags,
            &pxs,
            &pys,
            &pzs,
            &plx,
            &ply,
            &plz,
            48.0,
            12.0,
            24.0,
            16.0,
            -0.25,
            32,
            0,
            &mut bitset,
            &mut dp,
            &mut dm,
            &mut da,
            &mut stats,
        );
        assert_eq!(d3, 0);
        assert!(is_bit_hidden(&bitset, 3, 0, 0));

        // Tick 4: move hidden Mob 0 inside hysteresis threshold (44.0 <= 45.0) => emits 1 SHOW delta!
        mob_xs[0] = 44.0;
        let d4 = compute_mob_tracking(
            &mob_xs,
            &mob_ys,
            &mob_zs,
            &mob_flags,
            &pxs,
            &pys,
            &pzs,
            &plx,
            &ply,
            &plz,
            48.0,
            12.0,
            24.0,
            16.0,
            -0.25,
            32,
            0,
            &mut bitset,
            &mut dp,
            &mut dm,
            &mut da,
            &mut stats,
        );
        assert_eq!(d4, 1);
        assert_eq!(dp[0], 0);
        assert_eq!(dm[0], 0);
        assert_eq!(da[0], TRACK_ACTION_SHOW);
        assert!(!is_bit_hidden(&bitset, 3, 0, 0));
    }

    #[test]
    fn test_brain_state_scaling_red_hides_green_visible_mobs() {
        // max_track_range = 48.0:
        //   GREEN (0): eff_range = 48.0  -> Mob 0 (x=40) visible, Mob 1 (x=28) visible
        //   RED   (3): eff_range = 31.2  -> Mob 0 (x=40) hidden,  Mob 1 (x=28) visible
        //   EMERG (4): eff_range = 24.96 -> Mob 0 (x=40) hidden,  Mob 1 (x=28) hidden
        let mob_xs = [40.0f32, 28.0];
        let mob_ys = [64.0f32; 2];
        let mob_zs = [0.0f32; 2];
        let mob_flags = [TRACK_FLAG_PASSIVE; 2];

        let pxs = [0.0f32];
        let pys = [64.0f32];
        let pzs = [0.0f32];
        let plx = [1.0f32];
        let ply = [0.0f32];
        let plz = [0.0f32];

        let mut bitset = vec![0u8; 1];
        let mut dp = [0i32; 8];
        let mut dm = [0i32; 8];
        let mut da = [0i32; 8];
        let mut stats = [0i32; 4];

        // GREEN: both visible -> 0 deltas
        let d_green = compute_mob_tracking(
            &mob_xs,
            &mob_ys,
            &mob_zs,
            &mob_flags,
            &pxs,
            &pys,
            &pzs,
            &plx,
            &ply,
            &plz,
            48.0,
            12.0,
            24.0,
            16.0,
            -0.25,
            32,
            0, // GREEN
            &mut bitset,
            &mut dp,
            &mut dm,
            &mut da,
            &mut stats,
        );
        assert_eq!(d_green, 0);
        assert_eq!(stats, [0, 2, 0, 0]);

        // RED: Mob 0 (x=40 > 31.2) hidden -> 1 HIDE delta
        let d_red = compute_mob_tracking(
            &mob_xs,
            &mob_ys,
            &mob_zs,
            &mob_flags,
            &pxs,
            &pys,
            &pzs,
            &plx,
            &ply,
            &plz,
            48.0,
            12.0,
            24.0,
            16.0,
            -0.25,
            32,
            3, // RED
            &mut bitset,
            &mut dp,
            &mut dm,
            &mut da,
            &mut stats,
        );
        assert_eq!(d_red, 1);
        assert_eq!(dm[0], 0);
        assert_eq!(da[0], TRACK_ACTION_HIDE);
        assert!(is_bit_hidden(&bitset, 2, 0, 0));
        assert!(!is_bit_hidden(&bitset, 2, 0, 1));

        // EMERGENCY: Mob 1 (x=28 > 24.96) also hidden -> 1 HIDE delta
        let d_emerg = compute_mob_tracking(
            &mob_xs,
            &mob_ys,
            &mob_zs,
            &mob_flags,
            &pxs,
            &pys,
            &pzs,
            &plx,
            &ply,
            &plz,
            48.0,
            12.0,
            24.0,
            16.0,
            -0.25,
            32,
            4, // EMERGENCY
            &mut bitset,
            &mut dp,
            &mut dm,
            &mut da,
            &mut stats,
        );
        assert_eq!(d_emerg, 1);
        assert_eq!(dm[0], 1);
        assert_eq!(da[0], TRACK_ACTION_HIDE);
        assert!(is_bit_hidden(&bitset, 2, 0, 0));
        assert!(is_bit_hidden(&bitset, 2, 0, 1));
    }

    #[test]
    fn test_max_deltas_backpressure_and_parallel_path() {
        // 4 players x 200 mobs = 800 pairs (>= 512 => exercises Rayon parallel path).
        // All mobs far away (x = 200.0) so all 800 pairs want HIDE.
        // Output delta buffers hold only 250 deltas per tick -> must drain cleanly across 4 ticks.
        let p = 4usize;
        let m = 200usize;
        let mob_xs = vec![200.0f32; m];
        let mob_ys = vec![64.0f32; m];
        let mob_zs = vec![0.0f32; m];
        let mob_flags = vec![TRACK_FLAG_PASSIVE; m];

        let pxs = vec![0.0f32; p];
        let pys = vec![64.0f32; p];
        let pzs = vec![0.0f32; p];
        let plx = vec![1.0f32; p];
        let ply = vec![0.0f32; p];
        let plz = vec![0.0f32; p];

        let mut bitset = vec![0u8; (p * m + 7) / 8];
        let mut dp = vec![0i32; 250];
        let mut dm = vec![0i32; 250];
        let mut da = vec![0i32; 250];
        let mut stats = [0i32; 4];

        let mut total_emitted = 0i32;
        for expected in [250i32, 250, 250, 50, 0] {
            let d = compute_mob_tracking(
                &mob_xs,
                &mob_ys,
                &mob_zs,
                &mob_flags,
                &pxs,
                &pys,
                &pzs,
                &plx,
                &ply,
                &plz,
                48.0,
                12.0,
                24.0,
                16.0,
                -0.25,
                64,
                0,
                &mut bitset,
                &mut dp,
                &mut dm,
                &mut da,
                &mut stats,
            );
            assert_eq!(d, expected);
            total_emitted += d;
            assert_eq!(stats[0], total_emitted);
            assert_eq!(stats[1], (p * m) as i32 - total_emitted);
            assert_eq!(stats[2], expected);
        }
        assert_eq!(total_emitted, 800);

        // Verify undersized bitset returns -1
        let mut tiny_bitset = vec![0u8; 4];
        let err = compute_mob_tracking(
            &mob_xs,
            &mob_ys,
            &mob_zs,
            &mob_flags,
            &pxs,
            &pys,
            &pzs,
            &plx,
            &ply,
            &plz,
            48.0,
            12.0,
            24.0,
            16.0,
            -0.25,
            64,
            0,
            &mut tiny_bitset,
            &mut dp,
            &mut dm,
            &mut da,
            &mut stats,
        );
        assert_eq!(err, -1);
    }
}
