//! O(N) Chunk Density & Crowding Heatmap Analyzer.
//!
//! Aggregates entity counts and crowded-entity counts (entities with >= 6 colliding neighbors)
//! per Minecraft 16x16 chunk `(x >> 4, z >> 4)` and extracts the hottest chunks in descending
//! severity order for lag diagnostics and proactive farm throttling.

use std::collections::HashMap;

/// Neighbor count threshold above which an entity is flagged as crowded.
const CROWDED_NEIGHBOR_THRESHOLD: i32 = 6;

/// Analyze entity positions and optional collision neighbor counts to find the hottest chunks.
///
/// Writes up to `min(unique_chunks, max_hot_chunks)` entries sorted descending by
/// `(crowded_count * 4 + entity_count, entity_count)` into the output slices and returns
/// the number of hot chunks written.
#[allow(clippy::too_many_arguments)]
pub fn analyze_chunks(
    xs: &[f32],
    zs: &[f32],
    neighbor_counts: &[i32],
    max_hot_chunks: usize,
    out_chunk_x: &mut [i32],
    out_chunk_z: &mut [i32],
    out_entity_count: &mut [i32],
    out_crowded_count: &mut [i32],
) -> i32 {
    let n = xs.len().min(zs.len());
    let limit = max_hot_chunks
        .min(out_chunk_x.len())
        .min(out_chunk_z.len())
        .min(out_entity_count.len())
        .min(out_crowded_count.len());

    if n == 0 || limit == 0 {
        return 0;
    }

    let has_neighbors = neighbor_counts.len() == xs.len();
    let mut chunks: HashMap<(i32, i32), (i32, i32)> =
        HashMap::with_capacity((n / 8).clamp(16, 2048));

    for i in 0..n {
        let x = xs[i];
        let z = zs[i];
        if !x.is_finite() || !z.is_finite() {
            continue;
        }
        let cx = (x.floor() as i32) >> 4;
        let cz = (z.floor() as i32) >> 4;
        let entry = chunks.entry((cx, cz)).or_insert((0, 0));
        entry.0 += 1;
        if has_neighbors && neighbor_counts[i] >= CROWDED_NEIGHBOR_THRESHOLD {
            entry.1 += 1;
        }
    }

    if chunks.is_empty() {
        return 0;
    }

    let mut ranked: Vec<(i32, i32, i32, i32)> = chunks
        .into_iter()
        .map(|((cx, cz), (entity_count, crowded_count))| (cx, cz, entity_count, crowded_count))
        .collect();

    let cmp_desc = |a: &(i32, i32, i32, i32), b: &(i32, i32, i32, i32)| {
        let score_a = (a.3 as i64) * 4 + (a.2 as i64);
        let score_b = (b.3 as i64) * 4 + (b.2 as i64);
        score_b
            .cmp(&score_a)
            .then_with(|| b.2.cmp(&a.2))
            .then_with(|| a.0.cmp(&b.0))
            .then_with(|| a.1.cmp(&b.1))
    };

    let count = ranked.len().min(limit);
    if count < ranked.len() {
        ranked.select_nth_unstable_by(count, cmp_desc);
        ranked.truncate(count);
    }
    ranked.sort_unstable_by(cmp_desc);

    for (idx, &(cx, cz, entity_count, crowded_count)) in ranked.iter().take(count).enumerate() {
        out_chunk_x[idx] = cx;
        out_chunk_z[idx] = cz;
        out_entity_count[idx] = entity_count;
        out_crowded_count[idx] = crowded_count;
    }

    count as i32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_analyze_chunks_ranks_crowded_chunk_first() {
        // Chunk (0, 0): 4 uncrowded entities -> score = 4
        // Chunk (-1, -1): 3 entities, 2 crowded (>= 6 neighbors) -> score = 2*4 + 3 = 11
        // Chunk (2, 1): 5 uncrowded entities -> score = 5
        let xs = [
            1.0, 2.0, 3.0, 4.0, // chunk (0, 0)
            -0.5, -15.9, -8.0, // chunk (-1, -1)
            32.0, 33.0, 34.0, 35.0, 36.0, // chunk (2, 1)
        ];
        let zs = [
            1.0, 2.0, 3.0, 4.0, // chunk (0, 0)
            -0.1, -16.0, -5.0, // chunk (-1, -1)
            16.0, 17.0, 18.0, 19.0, 20.0, // chunk (2, 1)
        ];
        let neighbors = [
            0, 1, 2, 5, // none >= 6
            6, 10, 2, // two >= 6
            0, 0, 0, 0, 0,
        ];

        let mut out_cx = [0i32; 2];
        let mut out_cz = [0i32; 2];
        let mut out_ec = [0i32; 2];
        let mut out_cc = [0i32; 2];

        let written = analyze_chunks(
            &xs,
            &zs,
            &neighbors,
            2,
            &mut out_cx,
            &mut out_cz,
            &mut out_ec,
            &mut out_cc,
        );

        assert_eq!(written, 2);
        // First: chunk (-1, -1) with score 11
        assert_eq!((out_cx[0], out_cz[0]), (-1, -1));
        assert_eq!(out_ec[0], 3);
        assert_eq!(out_cc[0], 2);
        // Second: chunk (2, 1) with score 5
        assert_eq!((out_cx[1], out_cz[1]), (2, 1));
        assert_eq!(out_ec[1], 5);
        assert_eq!(out_cc[1], 0);
    }

    #[test]
    fn test_analyze_chunks_without_neighbor_counts_and_empty() {
        let xs = [0.0f32, 15.9, 16.0];
        let zs = [0.0f32, 15.9, 0.0];
        let mut out_cx = [0i32; 4];
        let mut out_cz = [0i32; 4];
        let mut out_ec = [0i32; 4];
        let mut out_cc = [0i32; 4];

        // Mismatched/empty neighbor_counts -> crowded_count must be 0.
        let written = analyze_chunks(
            &xs,
            &zs,
            &[],
            4,
            &mut out_cx,
            &mut out_cz,
            &mut out_ec,
            &mut out_cc,
        );
        assert_eq!(written, 2);
        assert_eq!((out_cx[0], out_cz[0]), (0, 0));
        assert_eq!(out_ec[0], 2);
        assert_eq!(out_cc[0], 0);

        let zero = analyze_chunks(
            &[],
            &[],
            &[],
            4,
            &mut out_cx,
            &mut out_cz,
            &mut out_ec,
            &mut out_cc,
        );
        assert_eq!(zero, 0);
    }
}
