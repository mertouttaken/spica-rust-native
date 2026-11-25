//! Pure C ABI exports for Java 22+ Project Panama (FFM), C/C++, or external tooling.
//!
//! # Safety Contract (All Exports)
//! - All non-zero-length pointer spans are validated BEFORE any Rust slice is constructed:
//!   null check, `align_of::<T>()` alignment check, `count * size_of::<T>() <= isize::MAX`,
//!   address wraparound check, and zero read-write / write-write aliasing (`SpanValidator`).
//! - Callers must guarantee backing allocations remain valid, initialized for `T`, and
//!   exclusively owned (for mutable outputs) for the full duration of the FFI call.
//! - Every export wraps execution in `catch_unwind` and returns `-1` on any contract violation.

use std::panic::{catch_unwind, AssertUnwindSafe};

use crate::ffi_guard::SpanValidator;
use crate::{block_diff, farm_governor, governor, kernels, noise};

/// Initialize the Rayon worker thread pool. Pass `0` to auto-select optimal thread count.
/// Returns active thread count, or `-1` on error.
#[no_mangle]
pub extern "C" fn spica_init_pool(num_threads: i32) -> i32 {
    catch_unwind(AssertUnwindSafe(|| {
        crate::init_thread_pool(num_threads.max(0) as usize) as i32
    }))
    .unwrap_or(-1)
}

/// Compute batch entity collisions via raw C pointers (zero-copy).
///
/// # Safety
/// All pointers must be valid, non-null, properly aligned for `f32`/`i32`, non-overlapping on
/// writable outputs, and point to at least `count` elements for the duration of the call.
#[no_mangle]
#[allow(clippy::too_many_arguments)]
pub unsafe extern "C" fn spica_compute_collisions(
    count: usize,
    xs: *const f32,
    ys: *const f32,
    zs: *const f32,
    radii: *const f32,
    cell_size: f32,
    push_strength: f32,
    max_push: f32,
    out_push_x: *mut f32,
    out_push_y: *mut f32,
    out_push_z: *mut f32,
    out_neighbor_counts: *mut i32,
) -> i64 {
    if count == 0 {
        return 0;
    }

    catch_unwind(AssertUnwindSafe(|| {
        let mut guard = SpanValidator::new();
        let Ok(xs_r) = guard.register_read(xs, count, None) else { return -1; };
        let Ok(ys_r) = guard.register_read(ys, count, None) else { return -1; };
        let Ok(zs_r) = guard.register_read(zs, count, None) else { return -1; };
        let Ok(rad_r) = guard.register_read(radii, count, None) else { return -1; };
        let Ok(mut opx_w) = guard.register_write(out_push_x, count, None) else { return -1; };
        let Ok(mut opy_w) = guard.register_write(out_push_y, count, None) else { return -1; };
        let Ok(mut opz_w) = guard.register_write(out_push_z, count, None) else { return -1; };
        let Ok(mut oc_w) = guard.register_write(out_neighbor_counts, count, None) else { return -1; };

        let xs_s = xs_r.as_slice();
        let ys_s = ys_r.as_slice();
        let zs_s = zs_r.as_slice();
        let rad_s = rad_r.as_slice();
        let opx = opx_w.as_mut_slice();
        let opy = opy_w.as_mut_slice();
        let opz = opz_w.as_mut_slice();
        let oc = oc_w.as_mut_slice();

        crate::run_in_pool(|| {
            kernels::compute_entity_collisions(
                xs_s,
                ys_s,
                zs_s,
                rad_s,
                cell_size,
                push_strength,
                max_push,
                opx,
                opy,
                opz,
                oc,
            )
        })
    }))
    .unwrap_or(-1)
}

/// Compute batch AI nearest-target + LOD tiers via raw C pointers (zero-copy).
///
/// # Safety
/// All mob pointers must have `mob_count` elements; target pointers must have `target_count`
/// elements; all output spans must be non-overlapping and exclusively writable.
#[no_mangle]
#[allow(clippy::too_many_arguments)]
pub unsafe extern "C" fn spica_compute_ai_targets(
    mob_count: usize,
    mob_x: *const f32,
    mob_y: *const f32,
    mob_z: *const f32,
    target_count: usize,
    target_x: *const f32,
    target_y: *const f32,
    target_z: *const f32,
    full_range: f32,
    reduced_range: f32,
    out_nearest_idx: *mut i32,
    out_dist_sq: *mut f32,
    out_dir_x: *mut f32,
    out_dir_y: *mut f32,
    out_dir_z: *mut f32,
    out_lod_tier: *mut i32,
) -> i32 {
    if mob_count == 0 {
        return 0;
    }
    if mob_count > i32::MAX as usize || target_count > i32::MAX as usize {
        return -1;
    }

    catch_unwind(AssertUnwindSafe(|| {
        let mut guard = SpanValidator::new();
        let Ok(mx_r) = guard.register_read(mob_x, mob_count, None) else { return -1; };
        let Ok(my_r) = guard.register_read(mob_y, mob_count, None) else { return -1; };
        let Ok(mz_r) = guard.register_read(mob_z, mob_count, None) else { return -1; };
        let Ok(tx_r) = guard.register_read(target_x, target_count, None) else { return -1; };
        let Ok(ty_r) = guard.register_read(target_y, target_count, None) else { return -1; };
        let Ok(tz_r) = guard.register_read(target_z, target_count, None) else { return -1; };
        let Ok(mut oi_w) = guard.register_write(out_nearest_idx, mob_count, None) else { return -1; };
        let Ok(mut od_w) = guard.register_write(out_dist_sq, mob_count, None) else { return -1; };
        let Ok(mut odx_w) = guard.register_write(out_dir_x, mob_count, None) else { return -1; };
        let Ok(mut ody_w) = guard.register_write(out_dir_y, mob_count, None) else { return -1; };
        let Ok(mut odz_w) = guard.register_write(out_dir_z, mob_count, None) else { return -1; };
        let Ok(mut ot_w) = guard.register_write(out_lod_tier, mob_count, None) else { return -1; };

        let mx = mx_r.as_slice();
        let my = my_r.as_slice();
        let mz = mz_r.as_slice();
        let tx = tx_r.as_slice();
        let ty = ty_r.as_slice();
        let tz = tz_r.as_slice();
        let oi = oi_w.as_mut_slice();
        let od = od_w.as_mut_slice();
        let odx = odx_w.as_mut_slice();
        let ody = ody_w.as_mut_slice();
        let odz = odz_w.as_mut_slice();
        let ot = ot_w.as_mut_slice();

        crate::run_in_pool(|| {
            kernels::compute_ai_targets(
                mx,
                my,
                mz,
                tx,
                ty,
                tz,
                full_range,
                reduced_range,
                oi,
                od,
                odx,
                ody,
                odz,
                ot,
            )
        })
    }))
    .unwrap_or(-1)
}

/// Evaluate the adaptive MSPT/TPS Governor via raw C pointers.
///
/// Writes:
/// - `out_int_state[0..4]` = `[state_code, view_dist, sim_dist, enable_item_merge]`
/// - `out_float_params[0..4]` = `[full_ai_range, hibernate_range, merge_radius, ewma_mspt]`
///
/// Returns `state_code`, or `-1` on error.
///
/// # Safety
/// `out_int_state` and `out_float_params` must be non-null, aligned, non-overlapping, and
/// writable for at least 4 elements each.
#[no_mangle]
#[allow(clippy::too_many_arguments)]
pub unsafe extern "C" fn spica_evaluate_governor(
    current_mspt: f32,
    ewma_mspt: f32,
    loaded_entities: i32,
    online_players: i32,
    base_full_range: f32,
    base_hibernate_range: f32,
    min_view_dist: i32,
    max_view_dist: i32,
    min_sim_dist: i32,
    max_sim_dist: i32,
    out_int_state: *mut i32,
    out_float_params: *mut f32,
) -> i32 {
    catch_unwind(AssertUnwindSafe(|| {
        let mut guard = SpanValidator::new();
        let Ok(mut oi_w) = guard.register_write(out_int_state, 4, None) else { return -1; };
        let Ok(mut of_w) = guard.register_write(out_float_params, 4, None) else { return -1; };

        let out_ints = oi_w.as_mut_slice();
        let out_floats = of_w.as_mut_slice();

        let result = governor::evaluate_governor(
            current_mspt,
            ewma_mspt,
            loaded_entities,
            online_players,
            base_full_range,
            base_hibernate_range,
            min_view_dist,
            max_view_dist,
            min_sim_dist,
            max_sim_dist,
        );

        out_ints[0] = result.state_code;
        out_ints[1] = result.view_dist;
        out_ints[2] = result.sim_dist;
        out_ints[3] = result.enable_item_merge;

        out_floats[0] = result.full_ai_range;
        out_floats[1] = result.hibernate_range;
        out_floats[2] = result.merge_radius;
        out_floats[3] = result.ewma_mspt;

        result.state_code
    }))
    .unwrap_or(-1)
}

/// Evaluate the Farm & Villager Governor via raw C pointers (zero-copy).
///
/// Returns the number of throttled entities (`out_actions[i] != ACTION_NORMAL`), or `-1` on error.
///
/// # Safety
/// All entity pointers must be non-null, properly aligned, and valid for `entity_count` elements.
/// If `player_count > 0`, all player pointers must be valid for `player_count` elements.
/// Writable outputs must not overlap any input or each other.
#[no_mangle]
#[allow(clippy::too_many_arguments)]
pub unsafe extern "C" fn spica_evaluate_farm_throttling(
    entity_count: usize,
    xs: *const f32,
    ys: *const f32,
    zs: *const f32,
    entity_flags: *const i32,
    player_count: usize,
    player_xs: *const f32,
    player_ys: *const f32,
    player_zs: *const f32,
    crowd_radius: f32,
    mild_crowd_threshold: i32,
    severe_crowd_threshold: i32,
    chunk_mob_limit: i32,
    villager_wake_range: f32,
    villager_crowd_threshold: i32,
    brain_state_code: i32,
    out_actions: *mut i32,
    out_neighbor_counts: *mut i32,
) -> i32 {
    if entity_count == 0 {
        return 0;
    }
    if entity_count > i32::MAX as usize || player_count > i32::MAX as usize {
        return -1;
    }

    catch_unwind(AssertUnwindSafe(|| {
        let mut guard = SpanValidator::new();
        let Ok(xs_r) = guard.register_read(xs, entity_count, None) else { return -1; };
        let Ok(ys_r) = guard.register_read(ys, entity_count, None) else { return -1; };
        let Ok(zs_r) = guard.register_read(zs, entity_count, None) else { return -1; };
        let Ok(fl_r) = guard.register_read(entity_flags, entity_count, None) else { return -1; };
        let Ok(px_r) = guard.register_read(player_xs, player_count, None) else { return -1; };
        let Ok(py_r) = guard.register_read(player_ys, player_count, None) else { return -1; };
        let Ok(pz_r) = guard.register_read(player_zs, player_count, None) else { return -1; };
        let Ok(mut oa_w) = guard.register_write(out_actions, entity_count, None) else { return -1; };
        let Ok(mut on_w) = guard.register_write(out_neighbor_counts, entity_count, None) else {
            return -1;
        };

        let xs_s = xs_r.as_slice();
        let ys_s = ys_r.as_slice();
        let zs_s = zs_r.as_slice();
        let flags_s = fl_r.as_slice();
        let pxs_s = px_r.as_slice();
        let pys_s = py_r.as_slice();
        let pzs_s = pz_r.as_slice();
        let oa = oa_w.as_mut_slice();
        let on = on_w.as_mut_slice();

        crate::run_in_pool(|| {
            farm_governor::evaluate_farm_throttling(
                xs_s,
                ys_s,
                zs_s,
                flags_s,
                pxs_s,
                pys_s,
                pzs_s,
                crowd_radius,
                mild_crowd_threshold,
                severe_crowd_threshold,
                chunk_mob_limit,
                villager_wake_range,
                villager_crowd_threshold,
                brain_state_code,
                oa,
                on,
            )
        })
    }))
    .unwrap_or(-1)
}

/// Generate deterministic parallel 2D FBM gradient noise via raw C pointers (zero-copy).
///
/// Returns the number of generated samples (`width * depth`), or `-1` on error.
///
/// # Safety
/// `out_heights` must be non-null, properly aligned for `f32`, and exclusively writable for at
/// least `width * depth` elements (`width * depth * 4 <= isize::MAX`).
#[no_mangle]
#[allow(clippy::too_many_arguments)]
pub unsafe extern "C" fn spica_generate_noise_2d(
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
    out_heights: *mut f32,
) -> i32 {
    if width == 0 || depth == 0 {
        return 0;
    }
    let Some(total) = width.checked_mul(depth) else {
        return -1;
    };
    if total > i32::MAX as usize {
        return -1;
    }

    catch_unwind(AssertUnwindSafe(|| {
        let mut guard = SpanValidator::new();
        let Ok(mut out_w) = guard.register_write(out_heights, total, None) else { return -1; };
        let out = out_w.as_mut_slice();

        crate::run_in_pool(|| {
            noise::generate_noise_2d(
                origin_x,
                origin_z,
                width,
                depth,
                step,
                seed,
                octaves,
                frequency,
                lacunarity,
                persistence,
                out,
            )
        })
    }))
    .unwrap_or(-1)
}

/// Compute a parallel 3D arena/region block diff in YZX layout via raw C pointers (zero-copy).
///
/// Return value semantics:
/// - Count-only mode (`max_diffs == 0`): returns the total number of differing blocks.
/// - Extraction mode (`max_diffs > 0`): writes up to `max_diffs` entries and returns the number
///   of entries written.
/// - Returns `-1` on contract violation (null, unaligned, overflow, or aliasing buffers).
///
/// # Safety
/// `current_blocks` and `target_blocks` must be valid, non-null, aligned for `i32`, and have at
/// least `width * height * depth` elements. When `max_diffs > 0`, all four output pointers must
/// be non-null, aligned for `i32`, mutually non-overlapping (and non-overlapping with inputs),
/// and writable for at least `max_diffs` elements.
#[no_mangle]
#[allow(clippy::too_many_arguments)]
pub unsafe extern "C" fn spica_compute_block_diff(
    width: usize,
    height: usize,
    depth: usize,
    current_blocks: *const i32,
    target_blocks: *const i32,
    max_diffs: usize,
    out_rel_x: *mut i32,
    out_rel_y: *mut i32,
    out_rel_z: *mut i32,
    out_new_block_id: *mut i32,
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
    if total > block_diff::MAX_VOLUME_BLOCKS {
        return -1;
    }

    catch_unwind(AssertUnwindSafe(|| {
        let mut guard = SpanValidator::new();
        let Ok(cur_r) = guard.register_read(current_blocks, total, None) else { return -1; };
        let Ok(tgt_r) = guard.register_read(target_blocks, total, None) else { return -1; };
        let Ok(mut rx_w) = guard.register_write(out_rel_x, max_diffs, None) else { return -1; };
        let Ok(mut ry_w) = guard.register_write(out_rel_y, max_diffs, None) else { return -1; };
        let Ok(mut rz_w) = guard.register_write(out_rel_z, max_diffs, None) else { return -1; };
        let Ok(mut nid_w) = guard.register_write(out_new_block_id, max_diffs, None) else { return -1; };

        let cur = cur_r.as_slice();
        let tgt = tgt_r.as_slice();
        let rx = rx_w.as_mut_slice();
        let ry = ry_w.as_mut_slice();
        let rz = rz_w.as_mut_slice();
        let nid = nid_w.as_mut_slice();

        crate::run_in_pool(|| {
            block_diff::compute_block_diff(width, height, depth, cur, tgt, rx, ry, rz, nid)
        })
    }))
    .unwrap_or(-1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_c_abi_rejects_aliasing_and_unaligned_pointers() {
        let mut a = [1.0f32; 8];
        let mut b = [2.0f32; 8];
        let mut c = [3.0f32; 8];
        let r = [0.5f32; 8];
        let mut out_c = [0i32; 8];

        // Aliasing: out_push_x points to same buffer as xs
        let res_alias = unsafe {
            spica_compute_collisions(
                8,
                a.as_ptr(),
                b.as_ptr(),
                c.as_ptr(),
                r.as_ptr(),
                1.5,
                0.5,
                0.4,
                a.as_mut_ptr(),
                b.as_mut_ptr(),
                c.as_mut_ptr(),
                out_c.as_mut_ptr(),
            )
        };
        assert_eq!(res_alias, -1);

        // Unaligned pointer for governor output
        let mut raw = [0u8; 32];
        let bad_ints = unsafe { raw.as_mut_ptr().add(1) } as *mut i32;
        let mut good_floats = [0.0f32; 4];
        let res_unaligned = unsafe {
            spica_evaluate_governor(
                20.0,
                20.0,
                100,
                5,
                32.0,
                56.0,
                4,
                10,
                3,
                8,
                bad_ints,
                good_floats.as_mut_ptr(),
            )
        };
        assert_eq!(res_unaligned, -1);

        // Aliasing outputs in block_diff
        let cur = [1i32; 8];
        let tgt = [2i32; 8];
        let mut ox = [0i32; 8];
        let mut oy = [0i32; 8];
        let mut oz = [0i32; 8];
        let res_bd_alias = unsafe {
            spica_compute_block_diff(
                2,
                2,
                2,
                cur.as_ptr(),
                tgt.as_ptr(),
                8,
                ox.as_mut_ptr(),
                oy.as_mut_ptr(),
                oz.as_mut_ptr(),
                ox.as_mut_ptr(), // aliases ox!
            )
        };
        assert_eq!(res_bd_alias, -1);
    }
}
