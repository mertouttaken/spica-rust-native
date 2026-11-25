//! High-Performance Rust Farm & Villager Governor (v2.1.0).
//!
//! Evaluates mob farm crowding, chunk entity density, and boxed/trading-hall villagers
//! in O(N) using the flat `SpatialHashGrid`, per-chunk density map, and Rayon parallelism.
//! Adapts thresholds dynamically based on the MSPT/TPS Governor brain state while
//! preserving instant player-approach wakeup for villagers and immunity for protected mobs.

use std::collections::HashMap;

use rayon::prelude::*;

use crate::spatial::SpatialHashGrid;

/// Threshold below which sequential execution avoids Rayon thread-sync overhead.
const PARALLEL_THRESHOLD: usize = 512;

// ---------------------------------------------------------------------------
// Output Action Bitmask Constants
// ---------------------------------------------------------------------------

/// Entity behaves normally (no throttling applied).
pub const ACTION_NORMAL: i32 = 0;
/// Bit 0: Disable entity-entity collision pushes (mild mob farm crowding).
pub const ACTION_DISABLE_COLLISION: i32 = 1;
/// Bit 1: Throttle mob AI / pathfinding ticks.
pub const ACTION_THROTTLE_AI: i32 = 2;
/// Bit 0 | Bit 1: Full farm suppression (disable collision + throttle AI).
pub const ACTION_FARM_SUPPRESS: i32 = 3;

// ---------------------------------------------------------------------------
// Entity Input Flag Bitmask Constants
// ---------------------------------------------------------------------------

/// Protected entity (boss, named, leashed, tamed, vehicle, etc.) — never throttled.
pub const FLAG_PROTECTED: i32 = 1;
/// Villager entity — eligible for boxed-hall suppression and instant player-proximity wake.
pub const FLAG_VILLAGER: i32 = 2;
/// Entity is confined in a 1x1 / trapped trading box.
pub const FLAG_TRAPPED_BOX: i32 = 4;
/// Entity was recently interacted with by a player — keep awake (`ACTION_NORMAL`).
pub const FLAG_RECENT_INTERACT: i32 = 8;

/// Evaluate per-entity farm & villager throttling actions in O(N).
///
/// For each entity `i` in `0..n`:
/// - Counts neighbors `j != i` within `crowd_radius` via `SpatialHashGrid`.
/// - Looks up entity `i`'s 16x16 chunk `(floor(x) >> 4, floor(z) >> 4)` mob density.
/// - Scales thresholds dynamically using `brain_state_code` (`0=GREEN`, `1=YELLOW`,
///   `2=ORANGE`, `>=3=RED/EMERGENCY`).
/// - Writes the resulting action bitmask into `out_actions[i]` and neighbor count
///   into `out_neighbor_counts[i]`.
///
/// Returns the total number of entities with `out_actions[i] != ACTION_NORMAL`.
#[allow(clippy::too_many_arguments)]
pub fn evaluate_farm_throttling(
    xs: &[f32],
    ys: &[f32],
    zs: &[f32],
    entity_flags: &[i32],
    player_xs: &[f32],
    player_ys: &[f32],
    player_zs: &[f32],
    crowd_radius: f32,
    mild_crowd_threshold: i32,
    severe_crowd_threshold: i32,
    chunk_mob_limit: i32,
    villager_wake_range: f32,
    villager_crowd_threshold: i32,
    brain_state_code: i32,
    out_actions: &mut [i32],
    out_neighbor_counts: &mut [i32],
) -> i32 {
    let n = xs
        .len()
        .min(ys.len())
        .min(zs.len())
        .min(entity_flags.len())
        .min(out_actions.len())
        .min(out_neighbor_counts.len());
    if n == 0 {
        return 0;
    }

    let p_count = player_xs.len().min(player_ys.len()).min(player_zs.len());

    let safe_crowd_radius = if crowd_radius.is_finite() && crowd_radius > 0.0 {
        crowd_radius
    } else {
        0.0
    };
    let crowd_radius_sq = safe_crowd_radius * safe_crowd_radius;

    let safe_wake_range = if villager_wake_range.is_finite() && villager_wake_range > 0.0 {
        villager_wake_range
    } else {
        0.0
    };
    let wake_range_sq = safe_wake_range * safe_wake_range;

    let grid = SpatialHashGrid::build(&xs[..n], &ys[..n], &zs[..n], safe_crowd_radius.max(1.5));

    // O(N) chunk entity count map: ((floor(x) as i32) >> 4, (floor(z) as i32) >> 4) -> count
    let mut chunk_counts: HashMap<(i32, i32), i32> =
        HashMap::with_capacity((n / 8).clamp(16, 2048));
    for i in 0..n {
        let x = xs[i];
        let z = zs[i];
        if x.is_finite() && z.is_finite() {
            let cx = (x.floor() as i32) >> 4;
            let cz = (z.floor() as i32) >> 4;
            *chunk_counts.entry((cx, cz)).or_insert(0) += 1;
        }
    }

    // Dynamic threshold scaling based on MSPT/TPS Governor state
    let scale: f32 = match brain_state_code {
        code if code <= 0 => 1.0,
        1 => 0.85,
        2 => 0.70,
        _ => 0.55,
    };

    let eff_mild = ((mild_crowd_threshold as f32) * scale).round().max(2.0) as i32;
    let eff_severe = ((severe_crowd_threshold as f32) * scale)
        .round()
        .max(eff_mild as f32 + 1.0) as i32;
    let eff_chunk_limit = ((chunk_mob_limit as f32) * scale).round().max(12.0) as i32;
    let eff_villager_crowd = ((villager_crowd_threshold as f32) * scale)
        .round()
        .max(2.0) as i32;

    let eval_entity = |i: usize| -> (i32, i32) {
        let xi = xs[i];
        let yi = ys[i];
        let zi = zs[i];

        let mut neighbors = 0i32;
        if xi.is_finite() && yi.is_finite() && zi.is_finite() && safe_crowd_radius > 0.0 {
            grid.for_each_neighbor_3x3x3(xi, yi, zi, |j| {
                if i == j {
                    return;
                }
                let dx = xi - xs[j];
                let dy = yi - ys[j];
                let dz = zi - zs[j];
                let dist_sq = dx * dx + dy * dy + dz * dz;
                if dist_sq <= crowd_radius_sq {
                    neighbors += 1;
                }
            });
        }

        let chunk_count = if xi.is_finite() && zi.is_finite() {
            let cx = (xi.floor() as i32) >> 4;
            let cz = (zi.floor() as i32) >> 4;
            chunk_counts.get(&(cx, cz)).copied().unwrap_or(0)
        } else {
            0
        };

        let flags = entity_flags[i];
        let action = if (flags & (FLAG_PROTECTED | FLAG_RECENT_INTERACT)) != 0 {
            ACTION_NORMAL
        } else if (flags & FLAG_VILLAGER) != 0 {
            let player_near = p_count > 0 && {
                let mut min_player_d2 = f32::MAX;
                for p in 0..p_count {
                    let dx = player_xs[p] - xi;
                    let dy = player_ys[p] - yi;
                    let dz = player_zs[p] - zi;
                    let d2 = dx * dx + dy * dy + dz * dz;
                    if d2 < min_player_d2 {
                        min_player_d2 = d2;
                    }
                }
                min_player_d2 <= wake_range_sq
            };

            if player_near {
                ACTION_NORMAL
            } else if (flags & FLAG_TRAPPED_BOX) != 0
                || neighbors >= eff_villager_crowd
                || chunk_count >= eff_chunk_limit
            {
                ACTION_FARM_SUPPRESS
            } else {
                ACTION_NORMAL
            }
        } else if neighbors >= eff_severe || chunk_count >= eff_chunk_limit {
            ACTION_FARM_SUPPRESS
        } else if neighbors >= eff_mild {
            ACTION_DISABLE_COLLISION
        } else {
            ACTION_NORMAL
        };

        (action, neighbors)
    };

    if n >= PARALLEL_THRESHOLD {
        out_actions[..n]
            .par_iter_mut()
            .zip(out_neighbor_counts[..n].par_iter_mut())
            .enumerate()
            .map(|(i, (oa, on))| {
                let (action, neighbors) = eval_entity(i);
                *oa = action;
                *on = neighbors;
                if action != ACTION_NORMAL { 1i32 } else { 0i32 }
            })
            .sum()
    } else {
        let mut throttled = 0i32;
        for i in 0..n {
            let (action, neighbors) = eval_entity(i);
            out_actions[i] = action;
            out_neighbor_counts[i] = neighbors;
            if action != ACTION_NORMAL {
                throttled += 1;
            }
        }
        throttled
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_dense_mob_farm_mild_severe_and_chunk_overflow() {
        // 5 mobs in a tight mild cluster (each has 4 neighbors; mild=4, severe=8)
        // 9 mobs in a tight severe cluster (each has 8 neighbors; severe=8)
        // 1 isolated mob (0 neighbors)
        // 16 mobs stacked vertically in one chunk (chunk_mob_limit=16, 0 neighbors each)
        let mut xs = Vec::new();
        let mut ys = Vec::new();
        let mut zs = Vec::new();

        // Mild cluster at chunk (0, 0): indices 0..5
        for i in 0..5 {
            xs.push(2.0 + (i as f32) * 0.1);
            ys.push(64.0);
            zs.push(2.0);
        }
        // Severe cluster at chunk (10, 0): indices 5..14
        for i in 0..9 {
            xs.push(162.0 + (i as f32) * 0.1);
            ys.push(64.0);
            zs.push(2.0);
        }
        // Isolated mob at chunk (20, 0): index 14
        xs.push(322.0);
        ys.push(64.0);
        zs.push(2.0);
        // Chunk-overflow mobs at chunk (30, 0): indices 15..31 (spaced 5 blocks vertically -> 0 neighbors)
        for i in 0..16 {
            xs.push(482.0);
            ys.push(64.0 + (i as f32) * 5.0);
            zs.push(2.0);
        }

        let n = xs.len();
        let flags = vec![0i32; n];
        let mut out_actions = vec![0i32; n];
        let mut out_neighbors = vec![0i32; n];

        let throttled = evaluate_farm_throttling(
            &xs,
            &ys,
            &zs,
            &flags,
            &[],
            &[],
            &[],
            2.0, // crowd_radius
            4,   // mild_crowd_threshold
            8,   // severe_crowd_threshold
            16,  // chunk_mob_limit
            10.0,
            4,
            0, // GREEN (scale = 1.0)
            &mut out_actions,
            &mut out_neighbors,
        );

        // Mild cluster -> ACTION_DISABLE_COLLISION
        for i in 0..5 {
            assert_eq!(out_neighbors[i], 4);
            assert_eq!(out_actions[i], ACTION_DISABLE_COLLISION);
        }
        // Severe cluster -> ACTION_FARM_SUPPRESS
        for i in 5..14 {
            assert_eq!(out_neighbors[i], 8);
            assert_eq!(out_actions[i], ACTION_FARM_SUPPRESS);
        }
        // Isolated mob -> ACTION_NORMAL
        assert_eq!(out_neighbors[14], 0);
        assert_eq!(out_actions[14], ACTION_NORMAL);
        // Chunk overflow -> ACTION_FARM_SUPPRESS even with 0 neighbors
        for i in 15..31 {
            assert_eq!(out_neighbors[i], 0);
            assert_eq!(out_actions[i], ACTION_FARM_SUPPRESS);
        }

        assert_eq!(throttled, 5 + 9 + 16);
    }

    #[test]
    fn test_boxed_villager_suppression_and_instant_wake() {
        let xs = [10.0f32, 100.0, 200.0];
        let ys = [64.0f32, 64.0, 64.0];
        let zs = [10.0f32, 100.0, 200.0];
        // Villager 0: boxed, far or near player depending on scenario
        // Villager 1: boxed + recent interact -> always awake
        // Villager 2: free (unboxed, uncrowded) -> always normal
        let flags = [
            FLAG_VILLAGER | FLAG_TRAPPED_BOX,
            FLAG_VILLAGER | FLAG_TRAPPED_BOX | FLAG_RECENT_INTERACT,
            FLAG_VILLAGER,
        ];

        let mut out_actions = [0i32; 3];
        let mut out_neighbors = [0i32; 3];

        // Case A: No players online -> boxed villager 0 suppressed, villager 1 awake via RECENT_INTERACT
        let throttled_empty = evaluate_farm_throttling(
            &xs,
            &ys,
            &zs,
            &flags,
            &[],
            &[],
            &[],
            2.5,
            6,
            12,
            32,
            10.0,
            4,
            0,
            &mut out_actions,
            &mut out_neighbors,
        );
        assert_eq!(throttled_empty, 1);
        assert_eq!(out_actions[0], ACTION_FARM_SUPPRESS);
        assert_eq!(out_actions[1], ACTION_NORMAL);
        assert_eq!(out_actions[2], ACTION_NORMAL);

        // Case B: Player far away (50 blocks from villager 0) -> still suppressed
        let throttled_far = evaluate_farm_throttling(
            &xs,
            &ys,
            &zs,
            &flags,
            &[60.0],
            &[64.0],
            &[10.0],
            2.5,
            6,
            12,
            32,
            10.0,
            4,
            0,
            &mut out_actions,
            &mut out_neighbors,
        );
        assert_eq!(throttled_far, 1);
        assert_eq!(out_actions[0], ACTION_FARM_SUPPRESS);
        assert_eq!(out_actions[1], ACTION_NORMAL);
        assert_eq!(out_actions[2], ACTION_NORMAL);

        // Case C: Player approaches within villager_wake_range (6 blocks <= 10 blocks) -> instant wake!
        let throttled_near = evaluate_farm_throttling(
            &xs,
            &ys,
            &zs,
            &flags,
            &[16.0],
            &[64.0],
            &[10.0],
            2.5,
            6,
            12,
            32,
            10.0,
            4,
            0,
            &mut out_actions,
            &mut out_neighbors,
        );
        assert_eq!(throttled_near, 0);
        assert_eq!(out_actions[0], ACTION_NORMAL);
        assert_eq!(out_actions[1], ACTION_NORMAL);
        assert_eq!(out_actions[2], ACTION_NORMAL);
    }

    #[test]
    fn test_protected_mob_immune_inside_dense_farm() {
        // 20 mobs crammed into 1 block: index 0 is FLAG_PROTECTED, index 1 is FLAG_PROTECTED | FLAG_VILLAGER | FLAG_TRAPPED_BOX
        let xs = [5.0f32; 20];
        let ys = [64.0f32; 20];
        let zs = [5.0f32; 20];
        let mut flags = [0i32; 20];
        flags[0] = FLAG_PROTECTED;
        flags[1] = FLAG_PROTECTED | FLAG_VILLAGER | FLAG_TRAPPED_BOX;

        let mut out_actions = [0i32; 20];
        let mut out_neighbors = [0i32; 20];

        let throttled = evaluate_farm_throttling(
            &xs,
            &ys,
            &zs,
            &flags,
            &[],
            &[],
            &[],
            2.5,
            4,
            8,
            16,
            10.0,
            4,
            3, // RED/EMERGENCY
            &mut out_actions,
            &mut out_neighbors,
        );

        assert_eq!(out_actions[0], ACTION_NORMAL);
        assert_eq!(out_actions[1], ACTION_NORMAL);
        assert_eq!(out_neighbors[0], 19);
        assert_eq!(out_neighbors[1], 19);
        assert!(out_actions[2..20]
            .iter()
            .all(|&a| a == ACTION_FARM_SUPPRESS));
        assert_eq!(throttled, 18);
    }

    #[test]
    fn test_brain_state_scaling_triggers_earlier_under_red() {
        // 6 mobs in a tight cluster -> each has 5 neighbors.
        // mild=8, severe=12:
        //   GREEN (0, scale=1.00): eff_mild=8, eff_severe=12 -> 5 neighbors => ACTION_NORMAL
        //   RED   (3, scale=0.55): eff_mild=4, eff_severe=7  -> 5 neighbors => ACTION_DISABLE_COLLISION
        let xs = [0.0f32, 0.2, 0.4, 0.6, 0.8, 1.0];
        let ys = [64.0f32; 6];
        let zs = [0.0f32; 6];
        let flags = [0i32; 6];

        let mut actions_green = [0i32; 6];
        let mut neighbors_green = [0i32; 6];
        let throttled_green = evaluate_farm_throttling(
            &xs,
            &ys,
            &zs,
            &flags,
            &[],
            &[],
            &[],
            2.0,
            8,
            12,
            40,
            10.0,
            6,
            0, // GREEN
            &mut actions_green,
            &mut neighbors_green,
        );
        assert_eq!(throttled_green, 0);
        assert!(actions_green.iter().all(|&a| a == ACTION_NORMAL));

        let mut actions_red = [0i32; 6];
        let mut neighbors_red = [0i32; 6];
        let throttled_red = evaluate_farm_throttling(
            &xs,
            &ys,
            &zs,
            &flags,
            &[],
            &[],
            &[],
            2.0,
            8,
            12,
            40,
            10.0,
            6,
            3, // RED
            &mut actions_red,
            &mut neighbors_red,
        );
        assert_eq!(throttled_red, 6);
        assert!(actions_red.iter().all(|&a| a == ACTION_DISABLE_COLLISION));
    }

    #[test]
    fn test_empty_and_parallel_threshold_paths() {
        let mut empty_a: [i32; 0] = [];
        let mut empty_n: [i32; 0] = [];
        assert_eq!(
            evaluate_farm_throttling(
                &[],
                &[],
                &[],
                &[],
                &[],
                &[],
                &[],
                2.5,
                6,
                12,
                32,
                10.0,
                4,
                0,
                &mut empty_a,
                &mut empty_n,
            ),
            0
        );

        // Exercise Rayon parallel path (n >= 512)
        let n = 600usize;
        let xs = vec![4.0f32; n];
        let ys = vec![64.0f32; n];
        let zs = vec![4.0f32; n];
        let mut flags = vec![0i32; n];
        flags[0] = FLAG_PROTECTED;
        let mut out_actions = vec![0i32; n];
        let mut out_neighbors = vec![0i32; n];

        let throttled = evaluate_farm_throttling(
            &xs,
            &ys,
            &zs,
            &flags,
            &[],
            &[],
            &[],
            2.0,
            6,
            12,
            48,
            10.0,
            4,
            0,
            &mut out_actions,
            &mut out_neighbors,
        );
        assert_eq!(throttled, (n - 1) as i32);
        assert_eq!(out_actions[0], ACTION_NORMAL);
        assert_eq!(out_actions[1], ACTION_FARM_SUPPRESS);
        assert_eq!(out_neighbors[1], (n - 1) as i32);
    }
}
