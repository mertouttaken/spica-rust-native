//! Pure C ABI exports for Java 22+ Project Panama (FFM), C/C++, or external tooling.

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::slice;

use crate::{farm_governor, governor, kernels};

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
/// All pointers must be valid, non-null, properly aligned for `f32`/`i32`, and point to at least `count` elements.
#[no_mangle]
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
    if xs.is_null()
        || ys.is_null()
        || zs.is_null()
        || radii.is_null()
        || out_push_x.is_null()
        || out_push_y.is_null()
        || out_push_z.is_null()
        || out_neighbor_counts.is_null()
    {
        return -1;
    }

    catch_unwind(AssertUnwindSafe(|| {
        let xs_s = slice::from_raw_parts(xs, count);
        let ys_s = slice::from_raw_parts(ys, count);
        let zs_s = slice::from_raw_parts(zs, count);
        let rad_s = slice::from_raw_parts(radii, count);
        let opx = slice::from_raw_parts_mut(out_push_x, count);
        let opy = slice::from_raw_parts_mut(out_push_y, count);
        let opz = slice::from_raw_parts_mut(out_push_z, count);
        let oc = slice::from_raw_parts_mut(out_neighbor_counts, count);

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
/// All mob pointers must have `mob_count` elements; target pointers must have `target_count` elements.
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
    if mob_x.is_null()
        || mob_y.is_null()
        || mob_z.is_null()
        || out_nearest_idx.is_null()
        || out_dist_sq.is_null()
        || out_dir_x.is_null()
        || out_dir_y.is_null()
        || out_dir_z.is_null()
        || out_lod_tier.is_null()
        || (target_count > 0 && (target_x.is_null() || target_y.is_null() || target_z.is_null()))
    {
        return -1;
    }

    catch_unwind(AssertUnwindSafe(|| {
        let mx = slice::from_raw_parts(mob_x, mob_count);
        let my = slice::from_raw_parts(mob_y, mob_count);
        let mz = slice::from_raw_parts(mob_z, mob_count);

        let (tx, ty, tz): (&[f32], &[f32], &[f32]) = if target_count > 0 {
            (
                slice::from_raw_parts(target_x, target_count),
                slice::from_raw_parts(target_y, target_count),
                slice::from_raw_parts(target_z, target_count),
            )
        } else {
            (&[], &[], &[])
        };

        let oi = slice::from_raw_parts_mut(out_nearest_idx, mob_count);
        let od = slice::from_raw_parts_mut(out_dist_sq, mob_count);
        let odx = slice::from_raw_parts_mut(out_dir_x, mob_count);
        let ody = slice::from_raw_parts_mut(out_dir_y, mob_count);
        let odz = slice::from_raw_parts_mut(out_dir_z, mob_count);
        let ot = slice::from_raw_parts_mut(out_lod_tier, mob_count);

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
/// `out_int_state` must be non-null, aligned, and writable for at least 4 `i32` elements;
/// `out_float_params` must be non-null, aligned, and writable for at least 4 `f32` elements.
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
    if out_int_state.is_null() || out_float_params.is_null() {
        return -1;
    }

    catch_unwind(AssertUnwindSafe(|| {
        let out_ints = slice::from_raw_parts_mut(out_int_state, 4);
        let out_floats = slice::from_raw_parts_mut(out_float_params, 4);

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
/// All entity pointers must be non-null, properly aligned for `f32`/`i32`, and valid for
/// `entity_count` elements. If `player_count > 0`, all player pointers must be non-null,
/// aligned, and valid for `player_count` elements.
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
    if xs.is_null()
        || ys.is_null()
        || zs.is_null()
        || entity_flags.is_null()
        || out_actions.is_null()
        || out_neighbor_counts.is_null()
        || (player_count > 0
            && (player_xs.is_null() || player_ys.is_null() || player_zs.is_null()))
    {
        return -1;
    }

    catch_unwind(AssertUnwindSafe(|| {
        let xs_s = slice::from_raw_parts(xs, entity_count);
        let ys_s = slice::from_raw_parts(ys, entity_count);
        let zs_s = slice::from_raw_parts(zs, entity_count);
        let flags_s = slice::from_raw_parts(entity_flags, entity_count);

        let (pxs_s, pys_s, pzs_s): (&[f32], &[f32], &[f32]) = if player_count > 0 {
            (
                slice::from_raw_parts(player_xs, player_count),
                slice::from_raw_parts(player_ys, player_count),
                slice::from_raw_parts(player_zs, player_count),
            )
        } else {
            (&[], &[], &[])
        };

        let oa = slice::from_raw_parts_mut(out_actions, entity_count);
        let on = slice::from_raw_parts_mut(out_neighbor_counts, entity_count);

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
