//! High-Performance Rust Redstone & Hopper Lag-Machine Governor (v2.5.0).
//!
//! Tracks per-chunk redstone event rates and hopper transfer rates using an
//! asymmetric EWMA thermal model (fast attack for sudden lag machines, smooth
//! cooldown decay once clocks stop), dynamically scaled by the MSPT/TPS Governor
//! brain state (`GREEN`..`EMERGENCY`). Outputs per-chunk action bitmasks,
//! updated heat scores, proportional event drop ratios (`0.0..=0.95`), and a
//! deterministically ranked list of the hottest chunks for diagnostics.

use rayon::prelude::*;

/// Threshold below which sequential execution avoids Rayon thread-sync overhead.
const PARALLEL_THRESHOLD: usize = 512;

// ---------------------------------------------------------------------------
// Output Action Bitmask Constants
// ---------------------------------------------------------------------------

/// Chunk redstone and hopper activity is within safe bounds.
pub const REDSTONE_ACTION_NORMAL: i32 = 0;
/// Bit 0: Throttle redstone block updates / clocks in this chunk.
pub const REDSTONE_ACTION_THROTTLE_REDSTONE: i32 = 1;
/// Bit 1: Throttle hopper item transfers / lookups in this chunk.
pub const REDSTONE_ACTION_THROTTLE_HOPPERS: i32 = 2;
/// Bit 0 | Bit 1: Full lag-machine lockdown (throttle both redstone and hoppers).
pub const REDSTONE_ACTION_LOCKDOWN: i32 = 3;

/// Evaluate per-chunk redstone & hopper activity and compute thermal throttling actions.
///
/// For each chunk `i` in `0..n`:
/// - Scales `redstone_limit` and `hopper_limit` dynamically according to `brain_state_code`
///   (`<=0=GREEN 1.0`, `1=YELLOW 0.85`, `2=ORANGE 0.70`, `3=RED 0.55`, `>=4=EMERGENCY 0.40`).
/// - Updates chunk heat via asymmetric EWMA (attack `0.40/0.60`, cooldown `0.70/0.30`).
/// - Emits an action bitmask (`REDSTONE_ACTION_*`) and proportional throttle drop ratio
///   in `0.0..=0.95` (`0.50` at 1.0x limit, `0.725` at 2.0x limit, capped at `0.95` at >=3.0x).
/// - Populates `out_top_indices` with the hottest active chunk indices in deterministic
///   descending order, filling any unused slots with `-1`.
///
/// Returns the total number of chunks where `out_actions[i] != REDSTONE_ACTION_NORMAL`.
#[allow(clippy::too_many_arguments)]
pub fn evaluate_redstone_chunks(
    chunk_xs: &[i32],
    chunk_zs: &[i32],
    redstone_per_sec: &[i32],
    hopper_per_sec: &[i32],
    prev_heat: &[f32],
    redstone_limit: i32,
    hopper_limit: i32,
    brain_state_code: i32,
    out_actions: &mut [i32],
    out_new_heat: &mut [f32],
    out_throttle_ratio: &mut [f32],
    out_top_indices: &mut [i32],
) -> i32 {
    let n = chunk_xs
        .len()
        .min(chunk_zs.len())
        .min(redstone_per_sec.len())
        .min(hopper_per_sec.len())
        .min(prev_heat.len())
        .min(out_actions.len())
        .min(out_new_heat.len())
        .min(out_throttle_ratio.len());

    if n == 0 {
        out_top_indices.fill(-1);
        return 0;
    }

    // Dynamic Brain State scaling
    let scale: f32 = match brain_state_code {
        code if code <= 0 => 1.0,
        1 => 0.85,
        2 => 0.70,
        3 => 0.55,
        _ => 0.40,
    };

    let eff_rs_lim = ((redstone_limit.max(20) as f32) * scale).round().max(40.0);
    let eff_hop_lim = ((hopper_limit.max(20) as f32) * scale).round().max(30.0);

    let eval_chunk = |i: usize| -> (i32, f32, f32) {
        let rs = redstone_per_sec[i].max(0) as f32;
        let hop = hopper_per_sec[i].max(0) as f32;
        let prev = if prev_heat[i].is_finite() {
            prev_heat[i].clamp(0.0, 1000.0)
        } else {
            0.0
        };

        let rs_ratio = rs / eff_rs_lim;
        let hop_ratio = hop / eff_hop_lim;
        let peak_ratio = rs_ratio.max(hop_ratio);
        let instant_heat = (peak_ratio * 100.0).min(1000.0);

        // Asymmetric EWMA: fast attack, smooth cooldown
        let new_heat = if instant_heat > prev {
            (prev * 0.40 + instant_heat * 0.60).min(1000.0)
        } else {
            (prev * 0.70 + instant_heat * 0.30).max(0.0)
        };

        let mut action = REDSTONE_ACTION_NORMAL;
        if (rs_ratio >= 1.0 && new_heat >= 85.0) || rs_ratio >= 2.0 {
            action |= REDSTONE_ACTION_THROTTLE_REDSTONE;
        }
        if (hop_ratio >= 1.0 && new_heat >= 85.0) || hop_ratio >= 2.0 {
            action |= REDSTONE_ACTION_THROTTLE_HOPPERS;
        }

        let ratio = if action == REDSTONE_ACTION_NORMAL {
            0.0
        } else {
            let excess = (peak_ratio - 1.0).max(0.0);
            (0.50 + excess * 0.225).clamp(0.50, 0.95)
        };

        (action, new_heat, ratio)
    };

    let throttled_count: i32 = if n >= PARALLEL_THRESHOLD {
        out_actions[..n]
            .par_iter_mut()
            .zip(out_new_heat[..n].par_iter_mut())
            .zip(out_throttle_ratio[..n].par_iter_mut())
            .enumerate()
            .map(|(i, ((oa, oh), or))| {
                let (action, new_heat, ratio) = eval_chunk(i);
                *oa = action;
                *oh = new_heat;
                *or = ratio;
                if action != REDSTONE_ACTION_NORMAL {
                    1i32
                } else {
                    0i32
                }
            })
            .sum()
    } else {
        let mut count = 0i32;
        for i in 0..n {
            let (action, new_heat, ratio) = eval_chunk(i);
            out_actions[i] = action;
            out_new_heat[i] = new_heat;
            out_throttle_ratio[i] = ratio;
            if action != REDSTONE_ACTION_NORMAL {
                count += 1;
            }
        }
        count
    };

    // Rank hottest active chunks into `out_top_indices`
    if !out_top_indices.is_empty() {
        let mut ranked: Vec<(i32, f32, i32)> = Vec::new();
        for i in 0..n {
            let heat = out_new_heat[i];
            let rs = redstone_per_sec[i];
            let hop = hopper_per_sec[i];
            if heat > 0.0 || rs > 0 || hop > 0 {
                let total_ops = rs.max(0).saturating_add(hop.max(0));
                ranked.push((i as i32, heat, total_ops));
            }
        }

        let cmp_desc = |a: &(i32, f32, i32), b: &(i32, f32, i32)| {
            let ia = a.0 as usize;
            let ib = b.0 as usize;
            b.1.total_cmp(&a.1)
                .then_with(|| b.2.cmp(&a.2))
                .then_with(|| chunk_xs[ib].cmp(&chunk_xs[ia]))
                .then_with(|| chunk_zs[ib].cmp(&chunk_zs[ia]))
                .then_with(|| a.0.cmp(&b.0))
        };

        let limit = out_top_indices.len();
        let count = ranked.len().min(limit);
        if count < ranked.len() {
            ranked.select_nth_unstable_by(count, cmp_desc);
            ranked.truncate(count);
        }
        ranked.sort_unstable_by(cmp_desc);

        for (slot, &(idx, _, _)) in ranked.iter().take(count).enumerate() {
            out_top_indices[slot] = idx;
        }
        for slot in &mut out_top_indices[count..] {
            *slot = -1;
        }
    }

    throttled_count
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normal_redstone_and_hopper_activity_stays_unthrottled() {
        let chunk_xs = [0, 1, 2, 3];
        let chunk_zs = [0, 0, 1, 1];
        let rs_per_sec = [40, 199, 0, -10];
        let hop_per_sec = [20, 159, 0, 0];
        let prev_heat = [0.0f32, 80.0, 0.0, f32::NAN];

        let mut actions = [99i32; 4];
        let mut new_heat = [0.0f32; 4];
        let mut ratios = [1.0f32; 4];
        let mut top = [99i32; 4];

        let throttled = evaluate_redstone_chunks(
            &chunk_xs,
            &chunk_zs,
            &rs_per_sec,
            &hop_per_sec,
            &prev_heat,
            200,
            160,
            0, // GREEN (1.0)
            &mut actions,
            &mut new_heat,
            &mut ratios,
            &mut top,
        );

        assert_eq!(throttled, 0);
        for i in 0..4 {
            assert_eq!(actions[i], REDSTONE_ACTION_NORMAL);
            assert_eq!(ratios[i], 0.0);
        }
        assert!((new_heat[0] - 12.0).abs() < 1e-4);
        assert!((new_heat[1] - 91.7).abs() < 1e-4);
        assert_eq!(new_heat[2], 0.0);
        assert_eq!(new_heat[3], 0.0);
        assert_eq!(top, [1, 0, -1, -1]);

        // Empty slice edge case
        let mut empty_top = [42i32; 2];
        assert_eq!(
            evaluate_redstone_chunks(
                &[],
                &[],
                &[],
                &[],
                &[],
                200,
                160,
                0,
                &mut [],
                &mut [],
                &mut [],
                &mut empty_top,
            ),
            0
        );
        assert_eq!(empty_top, [-1, -1]);
    }

    #[test]
    fn test_sustained_lag_machine_builds_ewma_heat_and_scales_drop_ratio() {
        let chunk_xs = [0, 1];
        let chunk_zs = [0, 0];
        let hop_per_sec = [0, 0];
        let mut heat = [0.0f32; 2];
        let mut actions = [0i32; 2];
        let mut next_heat = [0.0f32; 2];
        let mut ratios = [0.0f32; 2];
        let mut top = [0i32; 2];

        // Tick 1: moderate overflow (1.3x and 1.25x) builds heat but stays below 85.0 threshold.
        let rs_tick1 = [260, 250];
        let t1 = evaluate_redstone_chunks(
            &chunk_xs,
            &chunk_zs,
            &rs_tick1,
            &hop_per_sec,
            &heat,
            200,
            160,
            0,
            &mut actions,
            &mut next_heat,
            &mut ratios,
            &mut top,
        );
        assert_eq!(t1, 0);
        assert_eq!(actions[0], REDSTONE_ACTION_NORMAL);
        assert_eq!(actions[1], REDSTONE_ACTION_NORMAL);
        assert!((next_heat[0] - 78.0).abs() < 1e-4);
        assert!((next_heat[1] - 75.0).abs() < 1e-4);

        // Tick 2: sustained clock pushes chunk 0 over 85.0 heat, and chunk 1 spikes to 3.5x (0.95 cap).
        heat.copy_from_slice(&next_heat);
        let rs_tick2 = [260, 700];
        let t2 = evaluate_redstone_chunks(
            &chunk_xs,
            &chunk_zs,
            &rs_tick2,
            &hop_per_sec,
            &heat,
            200,
            160,
            0,
            &mut actions,
            &mut next_heat,
            &mut ratios,
            &mut top,
        );
        assert_eq!(t2, 2);
        assert_eq!(actions[0], REDSTONE_ACTION_THROTTLE_REDSTONE);
        assert!((next_heat[0] - 109.2).abs() < 1e-3);
        // 1.3x -> 0.50 + 0.3 * 0.225 = 0.5675
        assert!((ratios[0] - 0.5675).abs() < 1e-4);

        assert_eq!(actions[1], REDSTONE_ACTION_THROTTLE_REDSTONE);
        assert!((next_heat[1] - 240.0).abs() < 1e-3);
        // 3.5x -> capped at 0.95
        assert!((ratios[1] - 0.95).abs() < 1e-5);
        assert_eq!(top, [1, 0]);
    }

    #[test]
    fn test_extreme_instant_spike_throttles_tick1_and_lockdown() {
        let chunk_xs = [0, 1, 2];
        let chunk_zs = [0, 0, 0];
        // Chunk 0: 2.0x redstone + 2.0x hopper -> instant LOCKDOWN (0.725 drop)
        // Chunk 1: 0 redstone + 2.0x hopper -> instant THROTTLE_HOPPERS (0.725 drop)
        // Chunk 2: 3.0x redstone + 1.0x hopper -> heat=180>=85 so both fire -> LOCKDOWN (0.95 drop)
        let rs_per_sec = [400, 0, 600];
        let hop_per_sec = [320, 320, 160];
        let prev_heat = [0.0f32; 3];

        let mut actions = [0i32; 3];
        let mut new_heat = [0.0f32; 3];
        let mut ratios = [0.0f32; 3];
        let mut top = [0i32; 3];

        let throttled = evaluate_redstone_chunks(
            &chunk_xs,
            &chunk_zs,
            &rs_per_sec,
            &hop_per_sec,
            &prev_heat,
            200,
            160,
            0,
            &mut actions,
            &mut new_heat,
            &mut ratios,
            &mut top,
        );

        assert_eq!(throttled, 3);
        assert_eq!(actions[0], REDSTONE_ACTION_LOCKDOWN);
        assert!((ratios[0] - 0.725).abs() < 1e-5);
        assert_eq!(actions[1], REDSTONE_ACTION_THROTTLE_HOPPERS);
        assert!((ratios[1] - 0.725).abs() < 1e-5);
        assert_eq!(actions[2], REDSTONE_ACTION_LOCKDOWN);
        assert!((ratios[2] - 0.95).abs() < 1e-5);
        assert_eq!(top, [2, 0, 1]);
    }

    #[test]
    fn test_smooth_cooldown_decay_returns_to_normal() {
        let chunk_xs = [5];
        let chunk_zs = [-3];
        let rs_active = [400];
        let rs_idle = [0];
        let hop_idle = [0];

        let mut prev_heat = [200.0f32];
        let mut actions = [0i32; 1];
        let mut new_heat = [0.0f32; 1];
        let mut ratios = [0.0f32; 1];
        let mut top = [0i32; 1];

        // While active at 2.0x, chunk is throttled.
        let t_active = evaluate_redstone_chunks(
            &chunk_xs,
            &chunk_zs,
            &rs_active,
            &hop_idle,
            &prev_heat,
            200,
            160,
            0,
            &mut actions,
            &mut new_heat,
            &mut ratios,
            &mut top,
        );
        assert_eq!(t_active, 1);
        assert_eq!(actions[0], REDSTONE_ACTION_THROTTLE_REDSTONE);
        assert!((new_heat[0] - 200.0).abs() < 1e-4);

        // Once the clock stops, action returns to NORMAL immediately and heat decays by 0.70x/tick.
        let expected_heats = [140.0f32, 98.0, 68.6];
        prev_heat[0] = new_heat[0];
        for &expected in &expected_heats {
            let throttled = evaluate_redstone_chunks(
                &chunk_xs,
                &chunk_zs,
                &rs_idle,
                &hop_idle,
                &prev_heat,
                200,
                160,
                0,
                &mut actions,
                &mut new_heat,
                &mut ratios,
                &mut top,
            );
            assert_eq!(throttled, 0);
            assert_eq!(actions[0], REDSTONE_ACTION_NORMAL);
            assert_eq!(ratios[0], 0.0);
            assert!((new_heat[0] - expected).abs() < 1e-3);
            prev_heat[0] = new_heat[0];
        }
        assert!(prev_heat[0] < 85.0);
    }

    #[test]
    fn test_brain_state_scaling_and_deterministic_top_ranking() {
        let chunk_xs = [1, -2, 3, 3, 10];
        let chunk_zs = [0, 5, 1, 4, 10];
        let rs_per_sec = [120, 240, 100, 100, 0];
        let hop_per_sec = [0, 0, 50, 50, 0];
        let prev_heat = [60.0f32, 0.0, 0.0, 0.0, 0.0];

        let mut actions_green = [0i32; 5];
        let mut heat_green = [0.0f32; 5];
        let mut ratios_green = [0.0f32; 5];
        let mut top_green = [0i32; 5];

        // GREEN (0): eff_rs_lim = 200 -> chunk 0 (120) is well under limit, chunk 1 (240) builds heat=72 < 85.
        let throttled_green = evaluate_redstone_chunks(
            &chunk_xs,
            &chunk_zs,
            &rs_per_sec,
            &hop_per_sec,
            &prev_heat,
            200,
            160,
            0,
            &mut actions_green,
            &mut heat_green,
            &mut ratios_green,
            &mut top_green,
        );
        assert_eq!(throttled_green, 0);
        assert_eq!(actions_green[0], REDSTONE_ACTION_NORMAL);
        assert_eq!(actions_green[1], REDSTONE_ACTION_NORMAL);

        let mut actions_red = [0i32; 5];
        let mut heat_red = [0.0f32; 5];
        let mut ratios_red = [0.0f32; 5];
        let mut top_red = [0i32; 5];

        // RED (3): scale=0.55 -> eff_rs_lim = 110, eff_hop_lim = 88.
        // Chunk 0 (120/110=1.091, heat=89.45>=85) and Chunk 1 (240/110=2.182>=2.0) both throttle!
        let throttled_red = evaluate_redstone_chunks(
            &chunk_xs,
            &chunk_zs,
            &rs_per_sec,
            &hop_per_sec,
            &prev_heat,
            200,
            160,
            3,
            &mut actions_red,
            &mut heat_red,
            &mut ratios_red,
            &mut top_red,
        );
        assert_eq!(throttled_red, 2);
        assert_eq!(actions_red[0], REDSTONE_ACTION_THROTTLE_REDSTONE);
        assert_eq!(actions_red[1], REDSTONE_ACTION_THROTTLE_REDSTONE);
        assert_eq!(actions_red[2], REDSTONE_ACTION_NORMAL);
        assert_eq!(actions_red[3], REDSTONE_ACTION_NORMAL);
        assert_eq!(actions_red[4], REDSTONE_ACTION_NORMAL);

        // Deterministic ranking: chunk 1 (hottest) > chunk 0 > chunk 3 (cz=4) > chunk 2 (cz=1) > -1 (chunk 4 idle)
        assert_eq!(top_red, [1, 0, 3, 2, -1]);

        // Also verify parallel path (>= 512 chunks) with partial top-K selection
        let n = 600usize;
        let p_xs: Vec<i32> = (0..n as i32).collect();
        let p_zs = vec![0i32; n];
        let mut p_rs = vec![10i32; n];
        let p_hop = vec![0i32; n];
        let p_prev = vec![0.0f32; n];
        p_rs[42] = 500;
        p_rs[512] = 600;
        let mut p_act = vec![0i32; n];
        let mut p_heat = vec![0.0f32; n];
        let mut p_ratio = vec![0.0f32; n];
        let mut p_top = [0i32; 2];

        let p_throttled = evaluate_redstone_chunks(
            &p_xs,
            &p_zs,
            &p_rs,
            &p_hop,
            &p_prev,
            200,
            160,
            0,
            &mut p_act,
            &mut p_heat,
            &mut p_ratio,
            &mut p_top,
        );
        assert_eq!(p_throttled, 2);
        assert_eq!(p_act[42], REDSTONE_ACTION_THROTTLE_REDSTONE);
        assert_eq!(p_act[512], REDSTONE_ACTION_THROTTLE_REDSTONE);
        assert_eq!(p_top, [512, 42]);
    }
}
