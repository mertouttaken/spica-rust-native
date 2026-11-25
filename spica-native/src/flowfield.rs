//! 2D Grid Dijkstra + Flow-Field Batch Pathfinding for Purpur/Paper 1.21.4.
//!
//! Computes an integration field (shortest-path distance from a goal cell across an
//! 8-connected weighted grid with strict diagonal corner-clipping prevention) and a
//! smooth normalized steering vector field for hundreds of mobs simultaneously.

use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::f32::consts::FRAC_1_SQRT_2;

use rayon::prelude::*;

/// Hard safety cap on grid cells (1024 x 1024) to prevent accidental OOM from oversized requests.
pub const MAX_FLOW_FIELD_CELLS: usize = 1_048_576;

const UNVISITED_FP: u32 = u32::MAX;
const MAX_VALID_FP: u32 = u32::MAX - 1;
const PARALLEL_THRESHOLD: usize = 4096;

/// 8-neighbor offsets: (dx, dz, is_diagonal).
const NEIGHBORS_8: [(isize, isize, bool); 8] = [
    (1, 0, false),
    (-1, 0, false),
    (0, 1, false),
    (0, -1, false),
    (1, 1, true),
    (-1, 1, true),
    (1, -1, true),
    (-1, -1, true),
];

/// Compute a 2D Dijkstra distance field and normalized steering flow-field toward `(goal_x, goal_z)`.
///
/// Grid layout is row-major: `idx = z * width + x`.
/// - `cost_grid[idx] == 0` marks an impassable wall; `1..=255` is the cell traversal cost.
/// - Diagonal movement is only permitted when both adjacent cardinal neighbors are walkable,
///   ensuring mobs never clip through wall corners.
/// - Unreachable cells receive `out_dist[idx] = f32::INFINITY` and `(out_dir_x[idx], out_dir_z[idx]) = (0.0, 0.0)`.
///
/// Returns the number of reachable cells (including the goal cell), or `0` on invalid input / blocked goal.
#[allow(clippy::too_many_arguments)]
pub fn compute_flow_field(
    width: usize,
    depth: usize,
    cost_grid: &[u8],
    goal_x: usize,
    goal_z: usize,
    out_dist: &mut [f32],
    out_dir_x: &mut [f32],
    out_dir_z: &mut [f32],
) -> i32 {
    if width == 0 || depth == 0 || goal_x >= width || goal_z >= depth {
        return 0;
    }
    let Some(len) = width.checked_mul(depth) else {
        return 0;
    };
    if len > MAX_FLOW_FIELD_CELLS
        || cost_grid.len() < len
        || out_dist.len() < len
        || out_dir_x.len() < len
        || out_dir_z.len() < len
    {
        return 0;
    }

    out_dist[..len].fill(f32::INFINITY);
    out_dir_x[..len].fill(0.0);
    out_dir_z[..len].fill(0.0);

    let goal_idx = goal_z * width + goal_x;
    if cost_grid[goal_idx] == 0 {
        return 0;
    }

    // Fixed-point Dijkstra (distance scaled by 100) for fast Ord and zero NaN hazards.
    let mut dist_fp = vec![UNVISITED_FP; len];
    let mut heap: BinaryHeap<Reverse<(u32, u32)>> = BinaryHeap::with_capacity((width + depth) * 4);

    dist_fp[goal_idx] = 0;
    heap.push(Reverse((0, goal_idx as u32)));

    let w_isize = width as isize;
    let d_isize = depth as isize;

    while let Some(Reverse((cur_dist, cur_idx_u32))) = heap.pop() {
        let cur_idx = cur_idx_u32 as usize;
        if cur_dist > dist_fp[cur_idx] {
            continue;
        }

        let cx = (cur_idx % width) as isize;
        let cz = (cur_idx / width) as isize;

        for &(dx, dz, is_diag) in &NEIGHBORS_8 {
            let nx = cx + dx;
            let nz = cz + dz;
            if nx < 0 || nx >= w_isize || nz < 0 || nz >= d_isize {
                continue;
            }

            let nx_u = nx as usize;
            let nz_u = nz as usize;
            let next_idx = nz_u * width + nx_u;
            let cell_cost = cost_grid[next_idx];
            if cell_cost == 0 {
                continue;
            }

            if is_diag {
                let adj_x_idx = (cz as usize) * width + nx_u;
                let adj_z_idx = nz_u * width + (cx as usize);
                if cost_grid[adj_x_idx] == 0 || cost_grid[adj_z_idx] == 0 {
                    continue;
                }
            }

            let step_fp = if is_diag {
                // Rounded fixed-point representation of cell_cost * 100 * sqrt(2) (141.42135).
                ((cell_cost as u32) * 14142 + 50) / 100
            } else {
                (cell_cost as u32) * 100
            };

            let cand = cur_dist.saturating_add(step_fp).min(MAX_VALID_FP);
            if cand < dist_fp[next_idx] {
                dist_fp[next_idx] = cand;
                heap.push(Reverse((cand, next_idx as u32)));
            }
        }
    }

    let mut reachable = 0i32;
    for i in 0..len {
        let fp = dist_fp[i];
        if fp != UNVISITED_FP {
            out_dist[i] = (fp as f32) * 0.01;
            reachable += 1;
        }
    }

    let dist_view: &[f32] = &out_dist[..len];

    let compute_dir = |idx: usize| -> (f32, f32) {
        if idx == goal_idx {
            return (0.0, 0.0);
        }
        let my_dist = dist_view[idx];
        if !my_dist.is_finite() {
            return (0.0, 0.0);
        }

        let cx = (idx % width) as isize;
        let cz = (idx / width) as isize;

        let mut best_dist = my_dist;
        let mut best_dx = 0.0f32;
        let mut best_dz = 0.0f32;
        let mut found = false;

        for &(dx, dz, is_diag) in &NEIGHBORS_8 {
            let nx = cx + dx;
            let nz = cz + dz;
            if nx < 0 || nx >= w_isize || nz < 0 || nz >= d_isize {
                continue;
            }
            let nx_u = nx as usize;
            let nz_u = nz as usize;
            let n_idx = nz_u * width + nx_u;
            if cost_grid[n_idx] == 0 {
                continue;
            }
            if is_diag {
                let adj_x_idx = (cz as usize) * width + nx_u;
                let adj_z_idx = nz_u * width + (cx as usize);
                if cost_grid[adj_x_idx] == 0 || cost_grid[adj_z_idx] == 0 {
                    continue;
                }
            }
            let nd = dist_view[n_idx];
            if nd < best_dist {
                best_dist = nd;
                best_dx = dx as f32;
                best_dz = dz as f32;
                found = true;
            }
        }

        if !found {
            return (0.0, 0.0);
        }

        let best_norm = if best_dx != 0.0 && best_dz != 0.0 {
            (best_dx * FRAC_1_SQRT_2, best_dz * FRAC_1_SQRT_2)
        } else {
            (best_dx, best_dz)
        };

        // In open interior cells (all 8 neighbors walkable & reachable), refine with the
        // continuous central-difference distance gradient for smooth steering angles,
        // falling back to best-neighbor near obstacles or saddle points.
        if cx > 0 && cx + 1 < w_isize && cz > 0 && cz + 1 < d_isize {
            let x = cx as usize;
            let z = cz as usize;
            let all_open = cost_grid[(z - 1) * width + (x - 1)] != 0
                && cost_grid[(z - 1) * width + x] != 0
                && cost_grid[(z - 1) * width + (x + 1)] != 0
                && cost_grid[z * width + (x - 1)] != 0
                && cost_grid[z * width + (x + 1)] != 0
                && cost_grid[(z + 1) * width + (x - 1)] != 0
                && cost_grid[(z + 1) * width + x] != 0
                && cost_grid[(z + 1) * width + (x + 1)] != 0;

            if all_open {
                let dl = dist_view[z * width + (x - 1)];
                let dr = dist_view[z * width + (x + 1)];
                let du = dist_view[(z - 1) * width + x];
                let dd = dist_view[(z + 1) * width + x];

                if dl.is_finite() && dr.is_finite() && du.is_finite() && dd.is_finite() {
                    let gx = dl - dr;
                    let gz = du - dd;
                    let mag_sq = gx * gx + gz * gz;
                    if mag_sq > 1e-6 {
                        let inv_mag = mag_sq.sqrt().recip();
                        let sx = gx * inv_mag;
                        let sz = gz * inv_mag;
                        if sx * best_norm.0 + sz * best_norm.1 > 0.5 {
                            return (sx, sz);
                        }
                    }
                }
            }
        }

        best_norm
    };

    if len >= PARALLEL_THRESHOLD {
        out_dir_x[..len]
            .par_iter_mut()
            .zip(out_dir_z[..len].par_iter_mut())
            .enumerate()
            .for_each(|(idx, (dx_out, dz_out))| {
                let (dx, dz) = compute_dir(idx);
                *dx_out = dx;
                *dz_out = dz;
            });
    } else {
        for idx in 0..len {
            let (dx, dz) = compute_dir(idx);
            out_dir_x[idx] = dx;
            out_dir_z[idx] = dz;
        }
    }

    reachable
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_obstacle_wall_detour_and_no_corner_clipping() {
        // 9x9 grid with a vertical wall at x = 4 for z = 0..8, leaving a gap at (4, 8).
        let w = 9usize;
        let d = 9usize;
        let mut cost = vec![1u8; w * d];
        for z in 0..8 {
            cost[z * w + 4] = 0;
        }

        let mut dist = vec![0.0f32; w * d];
        let mut dir_x = vec![0.0f32; w * d];
        let mut dir_z = vec![0.0f32; w * d];

        let reachable = compute_flow_field(w, d, &cost, 7, 2, &mut dist, &mut dir_x, &mut dir_z);
        assert_eq!(reachable, (w * d - 8) as i32);

        // Goal cell has distance 0 and direction (0, 0).
        let goal_idx = 2 * w + 7;
        assert_eq!(dist[goal_idx], 0.0);
        assert_eq!(dir_x[goal_idx], 0.0);
        assert_eq!(dir_z[goal_idx], 0.0);

        // Cell (2, 2) is blocked from straight-line travel by the wall at x = 4.
        let start_idx = 2 * w + 2;
        assert!(dist[start_idx].is_finite());
        assert!(dist[start_idx] > 10.0);
        // Must steer downward (+z) toward the gap at z = 8, not directly into the wall.
        assert!(dir_z[start_idx] > 0.5);

        // Cell (3, 7) is diagonally adjacent to gap (4, 8), but (4, 7) is a wall:
        // must NOT clip diagonally through the wall corner — must steer straight down to (3, 8).
        let corner_idx = 7 * w + 3;
        assert!((dir_x[corner_idx]).abs() < 1e-5);
        assert!((dir_z[corner_idx] - 1.0).abs() < 1e-5);
    }

    #[test]
    fn test_unreachable_cells_stay_infinite_with_zero_dir() {
        // 7x7 grid split completely in half by an unbroken wall at x = 3.
        let w = 7usize;
        let d = 7usize;
        let mut cost = vec![1u8; w * d];
        for z in 0..d {
            cost[z * w + 3] = 0;
        }

        let mut dist = vec![0.0f32; w * d];
        let mut dir_x = vec![99.0f32; w * d];
        let mut dir_z = vec![99.0f32; w * d];

        let reachable = compute_flow_field(w, d, &cost, 5, 3, &mut dist, &mut dir_x, &mut dir_z);
        // Right side has 3 columns * 7 rows = 21 reachable cells.
        assert_eq!(reachable, 21);

        // Every left-side and wall cell must remain INFINITY with (0, 0) direction.
        for z in 0..d {
            for x in 0..=3 {
                let idx = z * w + x;
                assert!(dist[idx].is_infinite());
                assert_eq!(dir_x[idx], 0.0);
                assert_eq!(dir_z[idx], 0.0);
            }
        }

        // Right-side non-goal cells must have finite distance and unit-length direction.
        for z in 0..d {
            for x in 4..w {
                let idx = z * w + x;
                assert!(dist[idx].is_finite());
                if x != 5 || z != 3 {
                    let mag = (dir_x[idx] * dir_x[idx] + dir_z[idx] * dir_z[idx]).sqrt();
                    assert!((mag - 1.0).abs() < 1e-4);
                }
            }
        }
    }

    #[test]
    fn test_blocked_goal_and_invalid_inputs() {
        let cost = [1u8, 0, 1, 1];
        let mut dist = [1.0f32; 4];
        let mut dx = [1.0f32; 4];
        let mut dz = [1.0f32; 4];

        // Goal on wall cell (1, 0) -> returns 0 and resets outputs.
        let r = compute_flow_field(2, 2, &cost, 1, 0, &mut dist, &mut dx, &mut dz);
        assert_eq!(r, 0);
        assert!(dist.iter().all(|v| v.is_infinite()));
        assert!(dx.iter().all(|&v| v == 0.0));
        assert!(dz.iter().all(|&v| v == 0.0));

        // Out-of-bounds goal -> 0.
        assert_eq!(
            compute_flow_field(2, 2, &cost, 2, 0, &mut dist, &mut dx, &mut dz),
            0
        );
    }
}
