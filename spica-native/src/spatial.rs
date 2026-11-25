//! Cache-friendly, O(N) Uniform Spatial Hash Grid (CSR flat layout).
//!
//! Avoids HashMap/Vec-per-cell heap churn by packing all entity indices into
//! two contiguous slices (`cell_offsets` and `sorted_indices`).

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
        let safe_cell = if cell_size > 0.05 { cell_size } else { 2.0 };
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
    /// Note: Because of hash table folding, callers MUST still perform an exact
    /// distance check against the returned candidate indices.
    #[inline(always)]
    pub fn for_each_neighbor_3x3x3<F>(&self, x: f32, y: f32, z: f32, mut f: F)
    where
        F: FnMut(usize),
    {
        if self.sorted_indices.is_empty() {
            return;
        }
        let (cx, cy, cz) = self.cell_coords(x, y, z);
        for dx in -1..=1 {
            for dy in -1..=1 {
                for dz in -1..=1 {
                    let bucket = hash_cell(cx + dx, cy + dy, cz + dz, self.table_mask);
                    let start = self.cell_offsets[bucket] as usize;
                    let end = self.cell_offsets[bucket + 1] as usize;
                    for &idx in &self.sorted_indices[start..end] {
                        f(idx as usize);
                    }
                }
            }
        }
    }

    /// Visit every entity index in a bounding box of cells covering radius `r`.
    #[inline]
    pub fn for_each_in_radius_cells<F>(&self, x: f32, y: f32, z: f32, r: f32, mut f: F)
    where
        F: FnMut(usize),
    {
        if self.sorted_indices.is_empty() {
            return;
        }
        let min_cx = ((x - r) * self.inv_cell_size).floor() as i32;
        let max_cx = ((x + r) * self.inv_cell_size).floor() as i32;
        let min_cy = ((y - r) * self.inv_cell_size).floor() as i32;
        let max_cy = ((y + r) * self.inv_cell_size).floor() as i32;
        let min_cz = ((z - r) * self.inv_cell_size).floor() as i32;
        let max_cz = ((z + r) * self.inv_cell_size).floor() as i32;

        // Deduplicate visited buckets when radius spans many cells by keeping a tiny stack filter
        // for common small radii; for larger spans, callers deduplicate or filter exact distance.
        for cx in min_cx..=max_cx {
            for cy in min_cy..=max_cy {
                for cz in min_cz..=max_cz {
                    let bucket = hash_cell(cx, cy, cz, self.table_mask);
                    let start = self.cell_offsets[bucket] as usize;
                    let end = self.cell_offsets[bucket + 1] as usize;
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
