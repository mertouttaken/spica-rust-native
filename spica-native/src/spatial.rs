//! Cache-friendly, O(N) Uniform Spatial Hash Grid (CSR flat layout).
//!
//! Avoids HashMap/Vec-per-cell heap churn by packing all entity indices into
//! two contiguous slices (`cell_offsets` and `sorted_indices`).
//!
//! Correctness guarantee (O3):
//! - Both `for_each_neighbor_3x3x3` and `for_each_in_radius_cells` deduplicate
//!   hashed bucket indices before iterating `sorted_indices`, guaranteeing every
//!   entity index is visited AT MOST ONCE even when neighboring cells hash-collide.

/// Flat 3D spatial hash grid for fast neighborhood queries.
pub struct SpatialHashGrid {
    inv_cell_size: f32,
    table_mask: usize,
    /// Prefix-summed offsets of length `table_size + 1`.
    cell_offsets: Vec<u32>,
    /// Entity indices grouped by bucket hash, length `entity_count`.
    sorted_indices: Vec<u32>,
}

impl SpatialHashGrid {
    /// Build a spatial hash grid from SoA entity positions `(xs, ys, zs)`.
    pub fn build(xs: &[f32], ys: &[f32], zs: &[f32], cell_size: f32) -> Self {
        let n = xs.len();
        let safe_cell = if cell_size.is_finite() && cell_size > 0.05 {
            cell_size
        } else {
            2.0
        };
        let inv_cell_size = 1.0 / safe_cell;

        if n == 0 {
            return Self {
                inv_cell_size,
                table_mask: 0,
                cell_offsets: vec![0, 0],
                sorted_indices: Vec::new(),
            };
        }

        // Load factor <= 0.5 for minimal hash collisions, power-of-two for fast bitmasking.
        let table_size = (n.saturating_mul(2))
            .next_power_of_two()
            .clamp(1024, 262_144);
        let table_mask = table_size - 1;

        let mut cell_counts = vec![0u32; table_size];
        let mut entity_buckets = vec![0u32; n];

        // Pass 1: Hash each entity and count bucket occupancy.
        for i in 0..n {
            let cx = (xs[i] * inv_cell_size).floor() as i32;
            let cy = (ys[i] * inv_cell_size).floor() as i32;
            let cz = (zs[i] * inv_cell_size).floor() as i32;
            let bucket = hash_cell(cx, cy, cz, table_mask);
            entity_buckets[i] = bucket as u32;
            cell_counts[bucket] += 1;
        }

        // Pass 2: Exclusive prefix sum -> cell_offsets.
        let mut cell_offsets = vec![0u32; table_size + 1];
        let mut running = 0u32;
        for b in 0..table_size {
            cell_offsets[b] = running;
            running += cell_counts[b];
        }
        cell_offsets[table_size] = running;

        // Pass 3: Scatter entity indices into contiguous sorted_indices.
        let mut cursors = cell_offsets[..table_size].to_vec();
        let mut sorted_indices = vec![0u32; n];
        for i in 0..n {
            let b = entity_buckets[i] as usize;
            let pos = cursors[b] as usize;
            sorted_indices[pos] = i as u32;
            cursors[b] += 1;
        }

        Self {
            inv_cell_size,
            table_mask,
            cell_offsets,
            sorted_indices,
        }
    }

    #[inline(always)]
    pub fn cell_coords(&self, x: f32, y: f32, z: f32) -> (i32, i32, i32) {
        (
            (x * self.inv_cell_size).floor() as i32,
            (y * self.inv_cell_size).floor() as i32,
            (z * self.inv_cell_size).floor() as i32,
        )
    }

    /// Visit every entity index in the 27 neighboring cells (3x3x3 cube) around `(x, y, z)`.
    ///
    /// Deduplicates hashed bucket indices on the stack (zero heap allocation) so even if two
    /// neighboring cells collide onto the same hash bucket, every entity index is yielded AT MOST ONCE.
    /// Callers still perform exact distance checks against returned candidate indices.
    #[inline(always)]
    pub fn for_each_neighbor_3x3x3<F>(&self, x: f32, y: f32, z: f32, mut f: F)
    where
        F: FnMut(usize),
    {
        if self.sorted_indices.is_empty() {
            return;
        }
        let (cx, cy, cz) = self.cell_coords(x, y, z);
        let mut visited = [usize::MAX; 27];
        let mut visited_len = 0usize;

        for dx in -1..=1 {
            for dy in -1..=1 {
                for dz in -1..=1 {
                    let bucket = hash_cell(
                        cx.wrapping_add(dx),
                        cy.wrapping_add(dy),
                        cz.wrapping_add(dz),
                        self.table_mask,
                    );
                    let start = self.cell_offsets[bucket] as usize;
                    let end = self.cell_offsets[bucket + 1] as usize;
                    if start == end {
                        continue;
                    }
                    // O3: Skip already-visited hash buckets so hash collisions never double-count neighbors
                    if visited[..visited_len].contains(&bucket) {
                        continue;
                    }
                    visited[visited_len] = bucket;
                    visited_len += 1;

                    for &idx in &self.sorted_indices[start..end] {
                        f(idx as usize);
                    }
                }
            }
        }
    }

    /// Visit every entity index in a bounding box of cells covering radius `r`,
    /// deduplicating hashed buckets so no entity index is yielded twice.
    #[inline]
    #[allow(dead_code)]
    pub fn for_each_in_radius_cells<F>(&self, x: f32, y: f32, z: f32, r: f32, mut f: F)
    where
        F: FnMut(usize),
    {
        if self.sorted_indices.is_empty()
            || !x.is_finite()
            || !y.is_finite()
            || !z.is_finite()
            || !r.is_finite()
            || r < 0.0
        {
            return;
        }
        let min_cx = ((x - r) * self.inv_cell_size).floor();
        let max_cx = ((x + r) * self.inv_cell_size).floor();
        let min_cy = ((y - r) * self.inv_cell_size).floor();
        let max_cy = ((y + r) * self.inv_cell_size).floor();
        let min_cz = ((z - r) * self.inv_cell_size).floor();
        let max_cz = ((z + r) * self.inv_cell_size).floor();

        // O3 guard: if the queried bounding box spans more cells than total entities or table buckets
        // (e.g. huge radius r), scan sorted_indices directly in O(N) instead of iterating billions of cells.
        let span_x = ((max_cx as i64) - (min_cx as i64)).saturating_add(1).max(0) as u128;
        let span_y = ((max_cy as i64) - (min_cy as i64)).saturating_add(1).max(0) as u128;
        let span_z = ((max_cz as i64) - (min_cz as i64)).saturating_add(1).max(0) as u128;
        let total_cells = span_x.saturating_mul(span_y).saturating_mul(span_z);
        if total_cells > (self.sorted_indices.len() as u128).min((self.table_mask + 1) as u128)
            || !min_cx.is_finite()
            || !max_cx.is_finite()
            || !min_cy.is_finite()
            || !max_cy.is_finite()
            || !min_cz.is_finite()
            || !max_cz.is_finite()
        {
            for &idx in &self.sorted_indices {
                f(idx as usize);
            }
            return;
        }

        let min_cx = min_cx as i32;
        let max_cx = max_cx as i32;
        let min_cy = min_cy as i32;
        let max_cy = max_cy as i32;
        let min_cz = min_cz as i32;
        let max_cz = max_cz as i32;

        // Stack-dedup up to 64 non-empty buckets; fall back to bitset for very large radii.
        let mut stack_visited = [usize::MAX; 64];
        let mut stack_len = 0usize;
        let mut overflow_bitset: Option<Vec<u64>> = None;

        for cx in min_cx..=max_cx {
            for cy in min_cy..=max_cy {
                for cz in min_cz..=max_cz {
                    let bucket = hash_cell(cx, cy, cz, self.table_mask);
                    let start = self.cell_offsets[bucket] as usize;
                    let end = self.cell_offsets[bucket + 1] as usize;
                    if start == end {
                        continue;
                    }

                    if let Some(bits) = overflow_bitset.as_mut() {
                        let word = bucket >> 6;
                        let mask = 1u64 << (bucket & 63);
                        if (bits[word] & mask) != 0 {
                            continue;
                        }
                        bits[word] |= mask;
                    } else if stack_visited[..stack_len].contains(&bucket) {
                        continue;
                    } else if stack_len < stack_visited.len() {
                        stack_visited[stack_len] = bucket;
                        stack_len += 1;
                    } else {
                        let table_size = self.table_mask + 1;
                        let mut bits = vec![0u64; (table_size + 63) >> 6];
                        for &b in &stack_visited {
                            bits[b >> 6] |= 1u64 << (b & 63);
                        }
                        let word = bucket >> 6;
                        let mask = 1u64 << (bucket & 63);
                        bits[word] |= mask;
                        overflow_bitset = Some(bits);
                    }

                    for &idx in &self.sorted_indices[start..end] {
                        f(idx as usize);
                    }
                }
            }
        }
    }
}

/// Fast large-prime spatial hash (Teschner et al., 2003).
#[inline(always)]
fn hash_cell(cx: i32, cy: i32, cz: i32, mask: usize) -> usize {
    let h = (cx.wrapping_mul(73_856_093))
        ^ (cy.wrapping_mul(19_349_663))
        ^ (cz.wrapping_mul(83_492_791));
    (h as usize) & mask
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn test_3x3x3_deduplicates_colliding_buckets() {
        // Force a tiny table (mask=3 -> 4 buckets) so 27 neighboring cells MUST collide heavily
        let xs = [0.0f32, 0.5, 1.5, -1.2, 0.2];
        let ys = [64.0f32, 64.0, 65.5, 63.1, 64.2];
        let zs = [0.0f32, 0.5, -0.8, 1.2, 0.1];
        let mut grid = SpatialHashGrid::build(&xs, &ys, &zs, 2.0);

        // Rebuild with a deliberately tiny 4-bucket mask to guarantee heavy bucket collisions
        let mask = 3usize;
        let mut counts = vec![0u32; 4];
        let mut buckets = vec![0u32; xs.len()];
        for i in 0..xs.len() {
            let (cx, cy, cz) = grid.cell_coords(xs[i], ys[i], zs[i]);
            let b = hash_cell(cx, cy, cz, mask);
            buckets[i] = b as u32;
            counts[b] += 1;
        }
        let mut offsets = vec![0u32; 5];
        for b in 0..4 {
            offsets[b + 1] = offsets[b] + counts[b];
        }
        let mut cursors = offsets[..4].to_vec();
        let mut sorted = vec![0u32; xs.len()];
        for i in 0..xs.len() {
            let b = buckets[i] as usize;
            sorted[cursors[b] as usize] = i as u32;
            cursors[b] += 1;
        }
        grid.table_mask = mask;
        grid.cell_offsets = offsets;
        grid.sorted_indices = sorted;

        let mut seen = Vec::new();
        grid.for_each_neighbor_3x3x3(0.0, 64.0, 0.0, |idx| seen.push(idx));
        let unique: HashSet<usize> = seen.iter().copied().collect();
        assert_eq!(
            seen.len(),
            unique.len(),
            "Every entity index must be visited at most once even under hash collisions"
        );
        assert_eq!(unique.len(), xs.len());

        let mut seen_r = Vec::new();
        grid.for_each_in_radius_cells(0.0, 64.0, 0.0, 4.0, |idx| seen_r.push(idx));
        let unique_r: HashSet<usize> = seen_r.iter().copied().collect();
        assert_eq!(seen_r.len(), unique_r.len());
    }
}
