//! Parallel Bulk Block / Arena Reset Diff Engine (YZX flattened layout).
//!
//! Scans millions of block state IDs across CPU cores via Rayon to produce ONLY
//! the changed `(rel_x, rel_y, rel_z, new_block_id)` coordinates for Skyblock
//! island pasting, minigame arena resets, or custom schematic builders.

use rayon::prelude::*;

/// Maximum volume supported in a single call (256 x 256 x 256 = 16.7M blocks).
pub const MAX_VOLUME_BLOCKS: usize = 16_777_216;
const PARALLEL_THRESHOLD: usize = 16_384;
const PARALLEL_CHUNK_SIZE: usize = 8_192;

/// Compute changed blocks between `current_blocks` and `target_blocks` in YZX order:
/// `idx = (y * depth + z) * width + x`.
///
/// - If output slices have length `0` (count-only mode), returns total number of
///   differing blocks without writing coordinates.
/// - Otherwise writes up to `min(out_rel_x, out_rel_y, out_rel_z, out_new_block_id)`
///   changed block entries and returns the number of entries written.
/// - Returns `-1` on invalid dimensions or undersized input slices.
#[allow(clippy::too_many_arguments)]
pub fn compute_block_diff(
    width: usize,
    height: usize,
    depth: usize,
    current_blocks: &[i32],
    target_blocks: &[i32],
    out_rel_x: &mut [i32],
    out_rel_y: &mut [i32],
    out_rel_z: &mut [i32],
    out_new_block_id: &mut [i32],
) -> i32 {
    if width == 0 || height == 0 || depth == 0 {
        return 0;
    }

    let Some(plane) = width.checked_mul(depth) else {
        return -1;
    };
    let Some(total) = plane.checked_mul(height) else {
        return -1;
    };
    if total > MAX_VOLUME_BLOCKS || total > i32::MAX as usize {
        return -1;
    }
    if current_blocks.len() < total || target_blocks.len() < total {
        return -1;
    }

    let cur = &current_blocks[..total];
    let tgt = &target_blocks[..total];
    let max_diffs = out_rel_x
        .len()
        .min(out_rel_y.len())
        .min(out_rel_z.len())
        .min(out_new_block_id.len());

    // Fast count-only pass when caller passes empty output slices
    if max_diffs == 0 {
        let count: usize = if total >= PARALLEL_THRESHOLD {
            cur.par_chunks(PARALLEL_CHUNK_SIZE)
                .zip(tgt.par_chunks(PARALLEL_CHUNK_SIZE))
                .map(|(c_chunk, t_chunk)| {
                    c_chunk
                        .iter()
                        .zip(t_chunk.iter())
                        .filter(|(&a, &b)| a != b)
                        .count()
                })
                .sum()
        } else {
            cur.iter()
                .zip(tgt.iter())
                .filter(|(&a, &b)| a != b)
                .count()
        };
        return count.min(i32::MAX as usize) as i32;
    }

    if total < PARALLEL_THRESHOLD {
        let mut written = 0usize;
        for idx in 0..total {
            let new_id = tgt[idx];
            if cur[idx] != new_id {
                let x = idx % width;
                let rem = idx / width;
                let z = rem % depth;
                let y = rem / depth;
                out_rel_x[written] = x as i32;
                out_rel_y[written] = y as i32;
                out_rel_z[written] = z as i32;
                out_new_block_id[written] = new_id;
                written += 1;
                if written >= max_diffs {
                    break;
                }
            }
        }
        return written as i32;
    }

    // Parallel chunk diff extraction -> deterministic ordered merge into output slices
    let per_chunk_diffs: Vec<Vec<(usize, i32)>> = cur
        .par_chunks(PARALLEL_CHUNK_SIZE)
        .zip(tgt.par_chunks(PARALLEL_CHUNK_SIZE))
        .enumerate()
        .map(|(chunk_idx, (c_chunk, t_chunk))| {
            let base = chunk_idx * PARALLEL_CHUNK_SIZE;
            let mut local = Vec::new();
            for offset in 0..c_chunk.len() {
                let new_id = t_chunk[offset];
                if c_chunk[offset] != new_id {
                    local.push((base + offset, new_id));
                }
            }
            local
        })
        .collect();

    let mut written = 0usize;
    'outer: for chunk in per_chunk_diffs {
        for (idx, new_id) in chunk {
            let x = idx % width;
            let rem = idx / width;
            let z = rem % depth;
            let y = rem / depth;
            out_rel_x[written] = x as i32;
            out_rel_y[written] = y as i32;
            out_rel_z[written] = z as i32;
            out_new_block_id[written] = new_id;
            written += 1;
            if written >= max_diffs {
                break 'outer;
            }
        }
    }

    written as i32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_identical_volumes_produce_zero_diffs() {
        let a = vec![1i32; 64];
        let b = vec![1i32; 64];
        let mut rx = [0i32; 16];
        let mut ry = [0i32; 16];
        let mut rz = [0i32; 16];
        let mut nid = [0i32; 16];

        let written = compute_block_diff(4, 4, 4, &a, &b, &mut rx, &mut ry, &mut rz, &mut nid);
        assert_eq!(written, 0);
    }

    #[test]
    fn test_yzx_coordinate_decoding_and_values() {
        let (w, h, d) = (4, 3, 5);
        let cur = vec![0i32; w * h * d];
        let mut tgt = cur.clone();

        // idx = (y * d + z) * w + x
        let idx1 = (2 * d + 4) * w + 3; // (x=3, y=2, z=4)
        let idx2 = (1 * d + 0) * w + 2; // (x=2, y=1, z=0)
        tgt[idx2] = 42;
        tgt[idx1] = 99;

        let mut rx = [0i32; 4];
        let mut ry = [0i32; 4];
        let mut rz = [0i32; 4];
        let mut nid = [0i32; 4];

        let written = compute_block_diff(w, h, d, &cur, &tgt, &mut rx, &mut ry, &mut rz, &mut nid);
        assert_eq!(written, 2);
        // idx2 < idx1 so (2, 1, 0) comes first
        assert_eq!((rx[0], ry[0], rz[0], nid[0]), (2, 1, 0, 42));
        assert_eq!((rx[1], ry[1], rz[1], nid[1]), (3, 2, 4, 99));
    }

    #[test]
    fn test_count_only_mode_and_parallel_cap() {
        let (w, h, d) = (32, 32, 32); // 32,768 blocks -> triggers Rayon parallel path
        let cur = vec![1i32; w * h * d];
        let mut tgt = cur.clone();
        for i in (0..tgt.len()).step_by(32) {
            tgt[i] = 7;
        }
        let expected_total = tgt.len() / 32; // 1024

        // Count-only mode (empty outputs)
        let total_changed = compute_block_diff(w, h, d, &cur, &tgt, &mut [], &mut [], &mut [], &mut []);
        assert_eq!(total_changed, expected_total as i32);

        // Capped output mode (max_diffs = 50)
        let mut rx = vec![0i32; 50];
        let mut ry = vec![0i32; 50];
        let mut rz = vec![0i32; 50];
        let mut nid = vec![0i32; 50];
        let written = compute_block_diff(w, h, d, &cur, &tgt, &mut rx, &mut ry, &mut rz, &mut nid);
        assert_eq!(written, 50);
        assert_eq!((rx[0], ry[0], rz[0], nid[0]), (0, 0, 0, 7));
        assert_eq!((rx[1], ry[1], rz[1], nid[1]), (0, 0, 1, 7));
    }

    #[test]
    fn test_invalid_inputs_return_error() {
        let cur = vec![0i32; 10];
        let tgt = vec![1i32; 10];
        assert_eq!(
            compute_block_diff(4, 4, 4, &cur, &tgt, &mut [], &mut [], &mut [], &mut []),
            -1
        );
        assert_eq!(
            compute_block_diff(0, 4, 4, &cur, &tgt, &mut [], &mut [], &mut [], &mut []),
            0
        );
    }
}
