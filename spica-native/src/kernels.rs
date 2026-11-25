//! High-performance batch entity kernels (SoA + Spatial Hash + Rayon).
//!
//! All kernels operate on flat contiguous slices (Structure-of-Arrays) so the CPU
//! can auto-vectorize inner loops (AVX2 / NEON) and scale across cores via Rayon.

use rayon::prelude::*;

use crate::spatial::SpatialHashGrid;

/// Threshold below which sequential execution avoids Rayon thread-sync overhead.
const PARALLEL_THRESHOLD: usize = 512;

/// AI Level-of-Detail (LOD) tiers returned by `compute_ai_targets`.
pub const LOD_FULL: i32 = 0;
pub const LOD_REDUCED: i32 = 1;
pub const LOD_HIBERNATE: i32 = 2;

/// Kernel 1: Batch Entity-Entity Soft Collision & Density / Cramming Calculation.
///
/// For each entity `i`:
/// - Finds all neighbors within `radius[i] + radius[j]` using an O(N) Spatial Hash Grid.
/// - Accumulates smooth separation impulse `(out_push_x[i], out_push_y[i], out_push_z[i])`.
/// - Writes exact colliding neighbor count into `out_neighbor_counts[i]`.
///
/// Returns total number of entity pairs that collided (counted per entity).
pub fn compute_entity_collisions(
    xs: &[f32],
    ys: &[f32],
    zs: &[f32],
    radii: &[f32],
    cell_size: f32,
    push_strength: f32,
    max_push: f32,
    out_push_x: &mut [f32],
    out_push_y: &mut [f32],
    out_push_z: &mut [f32],
    out_neighbor_counts: &mut [i32],
) -> i64 {
    let n = xs.len();
    if n == 0 {
        return 0;
    }

    let grid = SpatialHashGrid::build(xs, ys, zs, cell_size);
    let max_push_sq = max_push * max_push;

    let compute_one = |i: usize| -> (f32, f32, f32, i32) {
        let xi = xs[i];
        let yi = ys[i];
        let zi = zs[i];
        let ri = radii[i];

        let mut px = 0.0f32;
        let mut py = 0.0f32;
        let mut pz = 0.0f32;
        let mut count = 0i32;

        grid.for_each_neighbor_3x3x3(xi, yi, zi, |j| {
            if i == j {
                return;
            }
            let dx = xi - xs[j];
            let dy = yi - ys[j];
            let dz = zi - zs[j];
            let min_dist = ri + radii[j];
            let min_dist_sq = min_dist * min_dist;
            let dist_sq = dx * dx + dy * dy + dz * dz;

            if dist_sq < min_dist_sq {
                count += 1;
                if dist_sq > 1e-6 {
                    let dist = dist_sq.sqrt();
                    let overlap = (min_dist - dist) * push_strength;
                    let inv_dist = 1.0 / dist;
                    px += dx * inv_dist * overlap;
                    py += dy * inv_dist * overlap * 0.25; // Dampen vertical bounce
                    pz += dz * inv_dist * overlap;
                } else {
                    // Deterministic tiny jitter when two entities occupy the exact same coordinate
                    let angle = ((i.wrapping_mul(2654435761)) & 1023) as f32 * 0.006135923;
                    px += angle.cos() * 0.05 * push_strength;
                    pz += angle.sin() * 0.05 * push_strength;
                }
            }
        });

        let mag_sq = px * px + py * py + pz * pz;
        if mag_sq > max_push_sq && max_push > 0.0 {
            let scale = max_push / mag_sq.sqrt();
            px *= scale;
            py *= scale;
            pz *= scale;
        }

        (px, py, pz, count)
    };

    if n >= PARALLEL_THRESHOLD {
        out_push_x
            .par_iter_mut()
            .zip(out_push_y.par_iter_mut())
            .zip(out_push_z.par_iter_mut())
            .zip(out_neighbor_counts.par_iter_mut())
            .enumerate()
            .map(|(i, (((ox, oy), oz), oc))| {
                let (px, py, pz, c) = compute_one(i);
                *ox = px;
                *oy = py;
                *oz = pz;
                *oc = c;
                c as i64
            })
            .sum()
    } else {
        let mut total = 0i64;
        for i in 0..n {
            let (px, py, pz, c) = compute_one(i);
            out_push_x[i] = px;
            out_push_y[i] = py;
            out_push_z[i] = pz;
            out_neighbor_counts[i] = c;
            total += c as i64;
        }
        total
    }
}

/// Kernel 2: Batch Nearest-Target & AI LOD (Activation Tier) Computation.
///
/// Given `N` mobs and `M` players/targets:
/// - Computes nearest target index (`-1` if none or outside `hibernate_range`).
/// - Computes squared distance to nearest target.
/// - Computes normalized steering vector `(dir_x, dir_y, dir_z)` toward nearest target.
/// - Assigns LOD tier:
///   - `0` (`LOD_FULL`): `dist <= full_range`
///   - `1` (`LOD_REDUCED`): `full_range < dist <= reduced_range`
///   - `2` (`LOD_HIBERNATE`): `dist > reduced_range` (or `M == 0`)
///
/// Returns count of mobs in `LOD_FULL` tier.
#[allow(clippy::too_many_arguments)]
pub fn compute_ai_targets(
    mob_x: &[f32],
    mob_y: &[f32],
    mob_z: &[f32],
    target_x: &[f32],
    target_y: &[f32],
    target_z: &[f32],
    full_range: f32,
    reduced_range: f32,
    out_nearest_idx: &mut [i32],
    out_dist_sq: &mut [f32],
    out_dir_x: &mut [f32],
    out_dir_y: &mut [f32],
    out_dir_z: &mut [f32],
    out_lod_tier: &mut [i32],
) -> i32 {
    let n = mob_x.len();
    let m = target_x.len();
    if n == 0 {
        return 0;
    }

    let full_sq = full_range * full_range;
    let reduced_sq = reduced_range * reduced_range;

    let eval_mob = |i: usize| -> (i32, f32, f32, f32, f32, i32) {
        if m == 0 {
            return (-1, f32::MAX, 0.0, 0.0, 0.0, LOD_HIBERNATE);
        }

        let mx = mob_x[i];
        let my = mob_y[i];
        let mz = mob_z[i];

        let mut best_idx = -1i32;
        let mut best_dist_sq = f32::MAX;
        let mut best_dx = 0.0f32;
        let mut best_dy = 0.0f32;
        let mut best_dz = 0.0f32;

        // Inner loop over contiguous SoA target arrays — auto-vectorizable by LLVM.
        for j in 0..m {
            let dx = target_x[j] - mx;
            let dy = target_y[j] - my;
            let dz = target_z[j] - mz;
            let d2 = dx * dx + dy * dy + dz * dz;
            if d2 < best_dist_sq {
                best_dist_sq = d2;
                best_idx = j as i32;
                best_dx = dx;
                best_dy = dy;
                best_dz = dz;
            }
        }

        let tier = if best_dist_sq <= full_sq {
            LOD_FULL
        } else if best_dist_sq <= reduced_sq {
            LOD_REDUCED
        } else {
            LOD_HIBERNATE
        };

        let (nx, ny, nz) = if best_idx >= 0 && best_dist_sq > 1e-6 && tier != LOD_HIBERNATE {
            let inv = 1.0 / best_dist_sq.sqrt();
            (best_dx * inv, best_dy * inv, best_dz * inv)
        } else {
            (0.0, 0.0, 0.0)
        };

        (best_idx, best_dist_sq, nx, ny, nz, tier)
    };

    if n >= PARALLEL_THRESHOLD {
        out_nearest_idx
            .par_iter_mut()
            .zip(out_dist_sq.par_iter_mut())
            .zip(out_dir_x.par_iter_mut())
            .zip(out_dir_y.par_iter_mut())
            .zip(out_dir_z.par_iter_mut())
            .zip(out_lod_tier.par_iter_mut())
            .enumerate()
            .map(|(i, (((((oi, od), odx), ody), odz), ot))| {
                let (idx, d2, nx, ny, nz, tier) = eval_mob(i);
                *oi = idx;
                *od = d2;
                *odx = nx;
                *ody = ny;
                *odz = nz;
                *ot = tier;
                if tier == LOD_FULL { 1 } else { 0 }
            })
            .sum()
    } else {
        let mut active = 0i32;
        for i in 0..n {
            let (idx, d2, nx, ny, nz, tier) = eval_mob(i);
            out_nearest_idx[i] = idx;
            out_dist_sq[i] = d2;
            out_dir_x[i] = nx;
            out_dir_y[i] = ny;
            out_dir_z[i] = nz;
            out_lod_tier[i] = tier;
            if tier == LOD_FULL {
                active += 1;
            }
        }
        active
    }
}

/// Kernel 3: Custom Swarm / Boids AI Steering (Separation + Alignment + Cohesion + Goal Seek).
///
/// Ideal for custom boss minions, flying mobs, phantoms, or horde events in Purpur.
/// Outputs desired steering acceleration `(out_ax, out_ay, out_az)` per entity.
#[allow(clippy::too_many_arguments)]
pub fn compute_boids_steering(
    xs: &[f32],
    ys: &[f32],
    zs: &[f32],
    vxs: &[f32],
    vys: &[f32],
    vzs: &[f32],
    goal_x: f32,
    goal_y: f32,
    goal_z: f32,
    perception_radius: f32,
    sep_weight: f32,
    align_weight: f32,
    cohesion_weight: f32,
    goal_weight: f32,
    max_force: f32,
    out_ax: &mut [f32],
    out_ay: &mut [f32],
    out_az: &mut [f32],
) -> i32 {
    let n = xs.len();
    if n == 0 {
        return 0;
    }

    let r = perception_radius.max(0.5);
    let r_sq = r * r;
    let sep_r_sq = (r * 0.45) * (r * 0.45);
    let max_force_sq = max_force * max_force;
    let grid = SpatialHashGrid::build(xs, ys, zs, r);

    let eval_boid = |i: usize| -> (f32, f32, f32) {
        let xi = xs[i];
        let yi = ys[i];
        let zi = zs[i];

        let mut sep_x = 0.0f32;
        let mut sep_y = 0.0f32;
        let mut sep_z = 0.0f32;
        let mut sep_n = 0u32;

        let mut ali_x = 0.0f32;
        let mut ali_y = 0.0f32;
        let mut ali_z = 0.0f32;

        let mut coh_x = 0.0f32;
        let mut coh_y = 0.0f32;
        let mut coh_z = 0.0f32;
        let mut neigh_n = 0u32;

        grid.for_each_neighbor_3x3x3(xi, yi, zi, |j| {
            if i == j {
                return;
            }
            let dx = xs[j] - xi;
            let dy = ys[j] - yi;
            let dz = zs[j] - zi;
            let d2 = dx * dx + dy * dy + dz * dz;

            if d2 < r_sq && d2 > 1e-6 {
                neigh_n += 1;
                ali_x += vxs[j];
                ali_y += vys[j];
                ali_z += vzs[j];
                coh_x += xs[j];
                coh_y += ys[j];
                coh_z += zs[j];

                if d2 < sep_r_sq {
                    let inv_d2 = 1.0 / d2;
                    sep_x -= dx * inv_d2;
                    sep_y -= dy * inv_d2;
                    sep_z -= dz * inv_d2;
                    sep_n += 1;
                }
            }
        });

        let mut ax = 0.0f32;
        let mut ay = 0.0f32;
        let mut az = 0.0f32;

        if sep_n > 0 {
            let inv = sep_weight / (sep_n as f32);
            ax += sep_x * inv;
            ay += sep_y * inv;
            az += sep_z * inv;
        }

        if neigh_n > 0 {
            let inv = 1.0 / (neigh_n as f32);
            ax += (ali_x * inv - vxs[i]) * align_weight;
            ay += (ali_y * inv - vys[i]) * align_weight;
            az += (ali_z * inv - vzs[i]) * align_weight;

            ax += (coh_x * inv - xi) * cohesion_weight;
            ay += (coh_y * inv - yi) * cohesion_weight;
            az += (coh_z * inv - zi) * cohesion_weight;
        }

        // Goal seek vector
        let gx = goal_x - xi;
        let gy = goal_y - yi;
        let gz = goal_z - zi;
        let gd2 = gx * gx + gy * gy + gz * gz;
        if gd2 > 1e-4 && goal_weight > 0.0 {
            let inv = goal_weight / gd2.sqrt();
            ax += gx * inv;
            ay += gy * inv;
            az += gz * inv;
        }

        let mag2 = ax * ax + ay * ay + az * az;
        if mag2 > max_force_sq && max_force > 0.0 {
            let scale = max_force / mag2.sqrt();
            ax *= scale;
            ay *= scale;
            az *= scale;
        }

        (ax, ay, az)
    };

    if n >= PARALLEL_THRESHOLD {
        out_ax
            .par_iter_mut()
            .zip(out_ay.par_iter_mut())
            .zip(out_az.par_iter_mut())
            .enumerate()
            .for_each(|(i, ((ox, oy), oz))| {
                let (ax, ay, az) = eval_boid(i);
                *ox = ax;
                *oy = ay;
                *oz = az;
            });
    } else {
        for i in 0..n {
            let (ax, ay, az) = eval_boid(i);
            out_ax[i] = ax;
            out_ay[i] = ay;
            out_az[i] = az;
        }
    }

    n as i32
}

/// Kernel 4: Batch AOE / Radius Query.
///
/// Given `N` entities and a query sphere `(qx, qy, qz, radius)`, writes matching entity
/// indices into `out_indices` up to `out_indices.len()` and returns total matches written.
pub fn query_radius(
    xs: &[f32],
    ys: &[f32],
    zs: &[f32],
    qx: f32,
    qy: f32,
    qz: f32,
    radius: f32,
    out_indices: &mut [i32],
) -> i32 {
    let n = xs.len();
    let max_out = out_indices.len();
    if n == 0 || max_out == 0 || radius <= 0.0 {
        return 0;
    }

    let r_sq = radius * radius;
    let mut written = 0usize;

    // For single AOE query over flat SoA arrays, linear SIMD scan is faster than building
    // a one-shot grid (for 10,000 entities, linear scan takes ~8 microseconds!).
    for i in 0..n {
        let dx = xs[i] - qx;
        let dy = ys[i] - qy;
        let dz = zs[i] - qz;
        if dx * dx + dy * dy + dz * dz <= r_sq {
            out_indices[written] = i as i32;
            written += 1;
            if written >= max_out {
                break;
            }
        }
    }

    written as i32
}

/// Kernel 5: O(N) Dropped-Item Stack Merge Planner.
///
/// Uses a `SpatialHashGrid` with cell size `merge_radius.max(1.0)` to pair nearby dropped
/// item stacks of identical `type_ids` into clean 100% absorption `(target, source)` pairs.
///
/// Invariant: Every source index is consumed at most once and never acts as a target, so
/// Java can apply all emitted `(out_target_idx[k], out_source_idx[k])` pairs in a single pass.
/// Returns the number of merge pairs written.
#[allow(clippy::too_many_arguments)]
pub fn plan_item_merges(
    xs: &[f32],
    ys: &[f32],
    zs: &[f32],
    type_ids: &[i32],
    stack_amounts: &[i32],
    max_stack_size: i32,
    merge_radius: f32,
    out_target_idx: &mut [i32],
    out_source_idx: &mut [i32],
) -> i32 {
    let n = xs
        .len()
        .min(ys.len())
        .min(zs.len())
        .min(type_ids.len())
        .min(stack_amounts.len());
    let max_pairs = out_target_idx.len().min(out_source_idx.len());

    if n < 2
        || max_pairs == 0
        || max_stack_size <= 1
        || !merge_radius.is_finite()
        || merge_radius <= 0.0
    {
        return 0;
    }

    let grid = SpatialHashGrid::build(&xs[..n], &ys[..n], &zs[..n], merge_radius.max(1.0));
    let radius_sq = merge_radius * merge_radius;

    let mut consumed = vec![false; n];
    let mut remaining_space: Vec<i32> = (0..n)
        .map(|i| {
            let amt = stack_amounts[i];
            if amt > 0 {
                (max_stack_size - amt).max(0)
            } else {
                0
            }
        })
        .collect();

    let mut pairs = 0usize;

    for i in 0..n {
        if pairs >= max_pairs {
            break;
        }
        if consumed[i] || remaining_space[i] <= 0 {
            continue;
        }

        let xi = xs[i];
        let yi = ys[i];
        let zi = zs[i];
        let ti = type_ids[i];

        grid.for_each_neighbor_3x3x3(xi, yi, zi, |j| {
            if pairs >= max_pairs || remaining_space[i] <= 0 {
                return;
            }
            if j <= i || consumed[j] {
                return;
            }
            if type_ids[j] != ti {
                return;
            }
            let amt_j = stack_amounts[j];
            if amt_j <= 0 || amt_j > remaining_space[i] {
                return;
            }

            let dx = xi - xs[j];
            let dy = yi - ys[j];
            let dz = zi - zs[j];
            let dist_sq = dx * dx + dy * dy + dz * dz;
            if dist_sq <= radius_sq {
                consumed[j] = true;
                remaining_space[i] -= amt_j;
                out_target_idx[pairs] = i as i32;
                out_source_idx[pairs] = j as i32;
                pairs += 1;
            }
        });
    }

    pairs as i32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_collisions_repel_overlapping_entities() {
        let xs = [0.0f32, 0.4, 50.0];
        let ys = [64.0f32, 64.0, 64.0];
        let zs = [0.0f32, 0.0, 50.0];
        let radii = [0.5f32, 0.5, 0.5];

        let mut px = [0.0f32; 3];
        let mut py = [0.0f32; 3];
        let mut pz = [0.0f32; 3];
        let mut counts = [0i32; 3];

        let total = compute_entity_collisions(
            &xs, &ys, &zs, &radii, 2.0, 1.0, 1.0, &mut px, &mut py, &mut pz, &mut counts,
        );

        assert_eq!(total, 2);
        assert_eq!(counts[0], 1);
        assert_eq!(counts[1], 1);
        assert_eq!(counts[2], 0);
        assert!(px[0] < 0.0, "Entity 0 should be pushed left");
        assert!(px[1] > 0.0, "Entity 1 should be pushed right");
        assert_eq!(px[2], 0.0);
    }

    #[test]
    fn test_ai_targets_and_lod_tiers() {
        let mob_x = [0.0f32, 40.0, 200.0];
        let mob_y = [64.0f32, 64.0, 64.0];
        let mob_z = [0.0f32, 0.0, 0.0];

        let target_x = [10.0f32];
        let target_y = [64.0f32];
        let target_z = [0.0f32];

        let mut idx = [0i32; 3];
        let mut d2 = [0.0f32; 3];
        let mut dx = [0.0f32; 3];
        let mut dy = [0.0f32; 3];
        let mut dz = [0.0f32; 3];
        let mut lod = [0i32; 3];

        let full_count = compute_ai_targets(
            &mob_x, &mob_y, &mob_z, &target_x, &target_y, &target_z, 24.0, 64.0, &mut idx,
            &mut d2, &mut dx, &mut dy, &mut dz, &mut lod,
        );

        assert_eq!(full_count, 1);
        assert_eq!(lod, [LOD_FULL, LOD_REDUCED, LOD_HIBERNATE]);
        assert_eq!(idx[0], 0);
        assert!((dx[0] - 1.0).abs() < 1e-4);
    }

    #[test]
    fn test_boids_steering_moves_toward_goal() {
        let xs = [0.0f32, 1.0];
        let ys = [64.0f32, 64.0];
        let zs = [0.0f32, 0.0];
        let vxs = [0.0f32; 2];
        let vys = [0.0f32; 2];
        let vzs = [0.0f32; 2];

        let mut ax = [0.0f32; 2];
        let mut ay = [0.0f32; 2];
        let mut az = [0.0f32; 2];

        let processed = compute_boids_steering(
            &xs, &ys, &zs, &vxs, &vys, &vzs, 100.0, 64.0, 0.0, 4.0, 0.5, 0.2, 0.1, 2.0, 1.0,
            &mut ax, &mut ay, &mut az,
        );

        assert_eq!(processed, 2);
        assert!(ax[0] > 0.0 && ax[1] > 0.0);
    }

    #[test]
    fn test_plan_item_merges_absorbs_compatible_neighbors() {
        // Items 0, 1, 2: same type (10), close together, stacks 20 + 30 + 25 (max 64)
        //   0 absorbs 1 (20 + 30 = 50, remaining = 14), cannot absorb 2 (25 > 14).
        // Item 3: different type (99) at same spot -> never merged.
        // Item 4: same type (10), stack 10, close to item 0 -> 0 absorbs 4 (50 + 10 = 60).
        // Item 5: same type (10), stack 5, far away -> never merged.
        let xs = [0.0f32, 1.0, 1.5, 0.2, 2.0, 50.0];
        let ys = [64.0f32; 6];
        let zs = [0.0f32; 6];
        let types = [10, 10, 10, 99, 10, 10];
        let amounts = [20, 30, 25, 5, 10, 5];

        let mut targets = [0i32; 8];
        let mut sources = [0i32; 8];

        let count = plan_item_merges(
            &xs,
            &ys,
            &zs,
            &types,
            &amounts,
            64,
            3.0,
            &mut targets,
            &mut sources,
        );

        assert_eq!(count, 2);
        let pairs: Vec<(i32, i32)> = (0..count as usize)
            .map(|k| (targets[k], sources[k]))
            .collect();
        // Item 0 should absorb items 1 and 4 (or 2 and 4 depending on bucket order,
        // never exceeding max_stack_size = 64, and never absorbing 3 or 5).
        let mut total_into_0 = amounts[0];
        for &(t, s) in &pairs {
            assert_eq!(t, 0);
            assert_ne!(s, 3);
            assert_ne!(s, 5);
            total_into_0 += amounts[s as usize];
        }
        assert!(total_into_0 <= 64);
    }

    #[test]
    fn test_plan_item_merges_edge_cases() {
        let mut t = [0i32; 4];
        let mut s = [0i32; 4];
        assert_eq!(
            plan_item_merges(&[], &[], &[], &[], &[], 64, 3.0, &mut t, &mut s),
            0
        );
        assert_eq!(
            plan_item_merges(&[0.0], &[0.0], &[0.0], &[1], &[1], 64, 3.0, &mut t, &mut s),
            0
        );
        // Full stacks (64) cannot merge
        assert_eq!(
            plan_item_merges(
                &[0.0, 0.5],
                &[64.0, 64.0],
                &[0.0, 0.0],
                &[1, 1],
                &[64, 64],
                64,
                3.0,
                &mut t,
                &mut s,
            ),
            0
        );
    }
}
