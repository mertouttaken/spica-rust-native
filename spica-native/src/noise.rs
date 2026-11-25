//! Fast Parallel 2D Gradient Noise + Fractal Brownian Motion (FBM) WorldGen Kernel.
//!
//! Deterministic, allocation-free 2D quintic-fade gradient noise layered across up to
//! 10 FBM octaves and normalized to `[-1.0, 1.0]`. Parallelizes per row with Rayon
//! for chunk/region-scale batches (`width * depth >= 4096`).

use std::f32::consts::{FRAC_1_SQRT_2, SQRT_2};

use rayon::prelude::*;

const PARALLEL_THRESHOLD: usize = 4096;

/// 8 unit-length gradient directions (4 cardinal + 4 diagonal).
const GRADIENTS_2D: [(f32, f32); 8] = [
    (1.0, 0.0),
    (-1.0, 0.0),
    (0.0, 1.0),
    (0.0, -1.0),
    (FRAC_1_SQRT_2, FRAC_1_SQRT_2),
    (-FRAC_1_SQRT_2, FRAC_1_SQRT_2),
    (FRAC_1_SQRT_2, -FRAC_1_SQRT_2),
    (-FRAC_1_SQRT_2, -FRAC_1_SQRT_2),
];

#[inline(always)]
fn fade(t: f32) -> f32 {
    t * t * t * (t * (t * 6.0 - 15.0) + 10.0)
}

#[inline(always)]
fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + t * (b - a)
}

#[inline(always)]
fn hash_2d(ix: i32, iz: i32, seed: i32) -> usize {
    let mut h = (ix as u32).wrapping_mul(374_761_393)
        ^ (iz as u32).wrapping_mul(668_265_263)
        ^ (seed as u32).wrapping_mul(2_246_822_519);
    h = (h ^ (h >> 13)).wrapping_mul(1_274_126_177);
    h ^= h >> 16;
    (h & 7) as usize
}

#[inline(always)]
fn grad_noise_2d(x: f32, z: f32, seed: i32) -> f32 {
    let x0f = x.floor();
    let z0f = z.floor();
    let ix0 = x0f as i32;
    let iz0 = z0f as i32;
    let ix1 = ix0.wrapping_add(1);
    let iz1 = iz0.wrapping_add(1);

    let fx = x - x0f;
    let fz = z - z0f;

    let u = fade(fx);
    let v = fade(fz);

    let (g00x, g00z) = GRADIENTS_2D[hash_2d(ix0, iz0, seed)];
    let (g10x, g10z) = GRADIENTS_2D[hash_2d(ix1, iz0, seed)];
    let (g01x, g01z) = GRADIENTS_2D[hash_2d(ix0, iz1, seed)];
    let (g11x, g11z) = GRADIENTS_2D[hash_2d(ix1, iz1, seed)];

    let n00 = g00x * fx + g00z * fz;
    let n10 = g10x * (fx - 1.0) + g10z * fz;
    let n01 = g01x * fx + g01z * (fz - 1.0);
    let n11 = g11x * (fx - 1.0) + g11z * (fz - 1.0);

    let nx0 = lerp(n00, n10, u);
    let nx1 = lerp(n01, n11, u);
    let val = lerp(nx0, nx1, v) * SQRT_2;
    if val.is_finite() {
        val.clamp(-1.0, 1.0)
    } else {
        0.0
    }
}

#[inline(always)]
fn fbm_2d(
    x: f32,
    z: f32,
    seed: i32,
    octaves: u32,
    frequency: f32,
    lacunarity: f32,
    persistence: f32,
) -> f32 {
    let mut amp = 1.0f32;
    let mut freq = frequency;
    let mut sum = 0.0f32;
    let mut norm = 0.0f32;

    for oct in 0..octaves {
        let sx = x * freq;
        let sz = z * freq;
        if !sx.is_finite() || !sz.is_finite() || !amp.is_finite() || amp <= 0.0 {
            break;
        }
        let oct_seed = seed.wrapping_add((oct as i32).wrapping_mul(1_013_904_223));
        sum += grad_noise_2d(sx, sz, oct_seed) * amp;
        norm += amp;
        freq *= lacunarity;
        amp *= persistence;
    }

    if norm > 0.0 && sum.is_finite() && norm.is_finite() {
        (sum / norm).clamp(-1.0, 1.0)
    } else {
        0.0
    }
}

/// Generate a row-major `width x depth` grid of deterministic 2D FBM gradient noise
/// normalized to `[-1.0, 1.0]`.
///
/// Sample `(x, z)` is evaluated at `(origin_x + x * step, origin_z + z * step)` and
/// written to `out_heights[z * width + x]`.
///
/// Returns the number of generated samples (`width * depth`), or `0` on invalid input.
#[allow(clippy::too_many_arguments)]
pub fn generate_noise_2d(
    origin_x: f32,
    origin_z: f32,
    width: usize,
    depth: usize,
    step: f32,
    seed: i32,
    octaves: i32,
    frequency: f32,
    lacunarity: f32,
    persistence: f32,
    out_heights: &mut [f32],
) -> i32 {
    if width == 0 || depth == 0 {
        return 0;
    }
    let Some(total) = width.checked_mul(depth) else {
        return 0;
    };
    if out_heights.len() < total {
        return 0;
    }

    let safe_origin_x = if origin_x.is_finite() { origin_x } else { 0.0 };
    let safe_origin_z = if origin_z.is_finite() { origin_z } else { 0.0 };
    let safe_step = if step.is_finite() && step > 0.0 {
        step
    } else {
        1.0
    };
    let safe_octaves = octaves.clamp(1, 10) as u32;
    let safe_freq = if frequency.is_finite() && frequency > 0.0 {
        frequency
    } else {
        0.01
    };
    let safe_lac = if lacunarity.is_finite() && lacunarity > 0.0 {
        lacunarity
    } else {
        2.0
    };
    let safe_pers = if persistence.is_finite() && persistence > 0.0 {
        persistence
    } else {
        0.5
    };

    let compute_row = |z: usize, row: &mut [f32]| {
        let wz = safe_origin_z + (z as f32) * safe_step;
        for (x, cell) in row.iter_mut().enumerate() {
            let wx = safe_origin_x + (x as f32) * safe_step;
            *cell = fbm_2d(
                wx,
                wz,
                seed,
                safe_octaves,
                safe_freq,
                safe_lac,
                safe_pers,
            );
        }
    };

    if total >= PARALLEL_THRESHOLD {
        out_heights[..total]
            .par_chunks_mut(width)
            .enumerate()
            .for_each(|(z, row)| compute_row(z, row));
    } else {
        out_heights[..total]
            .chunks_mut(width)
            .enumerate()
            .for_each(|(z, row)| compute_row(z, row));
    }

    i32::try_from(total).unwrap_or(i32::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_determinism_and_seed_variation() {
        let w = 64usize;
        let d = 64usize;
        let mut a = vec![0.0f32; w * d];
        let mut b = vec![0.0f32; w * d];
        let mut c = vec![0.0f32; w * d];

        let na = generate_noise_2d(-128.0, 64.0, w, d, 1.0, 42, 5, 0.02, 2.0, 0.5, &mut a);
        let nb = generate_noise_2d(-128.0, 64.0, w, d, 1.0, 42, 5, 0.02, 2.0, 0.5, &mut b);
        let nc = generate_noise_2d(-128.0, 64.0, w, d, 1.0, 999, 5, 0.02, 2.0, 0.5, &mut c);

        assert_eq!(na, (w * d) as i32);
        assert_eq!(nb, (w * d) as i32);
        assert_eq!(nc, (w * d) as i32);

        // Same seed produces bit-identical output (including across Rayon threads).
        for i in 0..(w * d) {
            assert_eq!(a[i].to_bits(), b[i].to_bits());
        }

        // Different seed produces distinct terrain values.
        let diffs = a
            .iter()
            .zip(c.iter())
            .filter(|(va, vc)| (**va - **vc).abs() > 1e-4)
            .count();
        assert!(diffs > (w * d) / 2);
    }

    #[test]
    fn test_range_bounds_and_finite_values() {
        let w = 80usize;
        let d = 80usize;
        let mut out = vec![0.0f32; w * d];

        let count = generate_noise_2d(0.0, 0.0, w, d, 1.5, -1337, 10, 0.035, 2.1, 0.55, &mut out);
        assert_eq!(count, (w * d) as i32);

        let mut min_v = f32::INFINITY;
        let mut max_v = f32::NEG_INFINITY;
        for &v in &out {
            assert!(v.is_finite());
            assert!((-1.0..=1.0).contains(&v));
            min_v = min_v.min(v);
            max_v = max_v.max(v);
        }
        // Ensure non-degenerate variation across the surface.
        assert!(max_v - min_v > 0.25);
    }

    #[test]
    fn test_zero_and_short_buffer_guards() {
        let mut small = [0.0f32; 4];
        assert_eq!(
            generate_noise_2d(0.0, 0.0, 0, 10, 1.0, 1, 3, 0.01, 2.0, 0.5, &mut small),
            0
        );
        assert_eq!(
            generate_noise_2d(0.0, 0.0, 4, 4, 1.0, 1, 3, 0.01, 2.0, 0.5, &mut small),
            0
        );
    }
}
