//! Zero-copy JNI bridge for Purpur/Paper 1.21.4 (Java 21+ with zero extra JVM flags).
//!
//! Uses `GetDirectBufferAddress` to read/write off-heap `ByteBuffer.allocateDirect(...)`
//! memory directly with zero copying and zero GC allocation per tick.

use std::mem::{align_of, size_of};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::slice;

use jni::objects::{JByteBuffer, JClass};
use jni::sys::{jfloat, jint, jlong, jstring};
use jni::JNIEnv;

use crate::{
    block_diff, farm_governor, flowfield, governor, heatmap, kernels, mob_tracker, noise,
    redstone_guard, surge_guard,
};

/// Safely view a Java DirectByteBuffer as an immutable slice `&[T]`.
unsafe fn buf_as_slice<'a, T>(
    env: &mut JNIEnv<'_>,
    buf: &JByteBuffer<'_>,
    count: usize,
) -> Result<&'a [T], &'static str> {
    if count == 0 {
        return Ok(&[]);
    }
    let ptr = env
        .get_direct_buffer_address(buf)
        .map_err(|_| "not a direct ByteBuffer")?;
    let cap = env
        .get_direct_buffer_capacity(buf)
        .map_err(|_| "failed to query buffer capacity")?;

    let needed_bytes = count
        .checked_mul(size_of::<T>())
        .ok_or("buffer size overflow")?;
    if (cap as usize) < needed_bytes {
        return Err("direct buffer capacity smaller than requested count");
    }
    if ptr.is_null() || (ptr as usize) % align_of::<T>() != 0 {
        return Err("direct buffer pointer null or unaligned");
    }

    Ok(slice::from_raw_parts(ptr as *const T, count))
}

/// Safely view a Java DirectByteBuffer as a mutable slice `&mut [T]`.
unsafe fn buf_as_mut_slice<'a, T>(
    env: &mut JNIEnv<'_>,
    buf: &JByteBuffer<'_>,
    count: usize,
) -> Result<&'a mut [T], &'static str> {
    if count == 0 {
        return Ok(&mut []);
    }
    let ptr = env
        .get_direct_buffer_address(buf)
        .map_err(|_| "not a direct ByteBuffer")?;
    let cap = env
        .get_direct_buffer_capacity(buf)
        .map_err(|_| "failed to query buffer capacity")?;

    let needed_bytes = count
        .checked_mul(size_of::<T>())
        .ok_or("buffer size overflow")?;
    if (cap as usize) < needed_bytes {
        return Err("direct buffer capacity smaller than requested count");
    }
    if ptr.is_null() || (ptr as usize) % align_of::<T>() != 0 {
        return Err("direct buffer pointer null or unaligned");
    }

    Ok(slice::from_raw_parts_mut(ptr as *mut T, count))
}

#[no_mangle]
#[allow(unused_mut)]
pub extern "system" fn Java_net_spicanetwork_rustnative_nativebridge_RustNative_nativeInitPool<'local>(
    mut _env: JNIEnv<'local>,
    _class: JClass<'local>,
    num_threads: jint,
) -> jint {
    catch_unwind(AssertUnwindSafe(|| {
        crate::init_thread_pool(num_threads.max(0) as usize) as jint
    }))
    .unwrap_or(-1)
}

#[no_mangle]
#[allow(unused_mut)]
pub extern "system" fn Java_net_spicanetwork_rustnative_nativebridge_RustNative_nativeGetInfo<'local>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
) -> jstring {
    let info = catch_unwind(AssertUnwindSafe(|| {
        format!(
            "spica-rust-native v{} | os={}-{} | threads={}",
            env!("CARGO_PKG_VERSION"),
            std::env::consts::OS,
            std::env::consts::ARCH,
            crate::active_threads()
        )
    }))
    .unwrap_or_else(|_| "spica-rust-native (panic)".to_string());

    env.new_string(info)
        .map(|s| s.into_raw())
        .unwrap_or(std::ptr::null_mut())
}

#[no_mangle]
#[allow(clippy::too_many_arguments, unused_mut)]
pub extern "system" fn Java_net_spicanetwork_rustnative_nativebridge_RustNative_nativeComputeCollisions<'local>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
    count: jint,
    xs_buf: JByteBuffer<'local>,
    ys_buf: JByteBuffer<'local>,
    zs_buf: JByteBuffer<'local>,
    radii_buf: JByteBuffer<'local>,
    cell_size: jfloat,
    push_strength: jfloat,
    max_push: jfloat,
    out_px_buf: JByteBuffer<'local>,
    out_py_buf: JByteBuffer<'local>,
    out_pz_buf: JByteBuffer<'local>,
    out_counts_buf: JByteBuffer<'local>,
) -> jlong {
    if count <= 0 {
        return 0;
    }
    let n = count as usize;

    catch_unwind(AssertUnwindSafe(|| unsafe {
        let Ok(xs) = buf_as_slice::<f32>(&mut env, &xs_buf, n) else { return -1; };
        let Ok(ys) = buf_as_slice::<f32>(&mut env, &ys_buf, n) else { return -1; };
        let Ok(zs) = buf_as_slice::<f32>(&mut env, &zs_buf, n) else { return -1; };
        let Ok(radii) = buf_as_slice::<f32>(&mut env, &radii_buf, n) else { return -1; };
        let Ok(opx) = buf_as_mut_slice::<f32>(&mut env, &out_px_buf, n) else { return -1; };
        let Ok(opy) = buf_as_mut_slice::<f32>(&mut env, &out_py_buf, n) else { return -1; };
        let Ok(opz) = buf_as_mut_slice::<f32>(&mut env, &out_pz_buf, n) else { return -1; };
        let Ok(oc) = buf_as_mut_slice::<i32>(&mut env, &out_counts_buf, n) else { return -1; };

        crate::run_in_pool(|| {
            kernels::compute_entity_collisions(
                xs,
                ys,
                zs,
                radii,
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

#[no_mangle]
#[allow(clippy::too_many_arguments, unused_mut)]
pub extern "system" fn Java_net_spicanetwork_rustnative_nativebridge_RustNative_nativeComputeAiTargets<'local>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
    mob_count: jint,
    mob_x_buf: JByteBuffer<'local>,
    mob_y_buf: JByteBuffer<'local>,
    mob_z_buf: JByteBuffer<'local>,
    target_count: jint,
    target_x_buf: JByteBuffer<'local>,
    target_y_buf: JByteBuffer<'local>,
    target_z_buf: JByteBuffer<'local>,
    full_range: jfloat,
    reduced_range: jfloat,
    out_idx_buf: JByteBuffer<'local>,
    out_dist_sq_buf: JByteBuffer<'local>,
    out_dx_buf: JByteBuffer<'local>,
    out_dy_buf: JByteBuffer<'local>,
    out_dz_buf: JByteBuffer<'local>,
    out_lod_buf: JByteBuffer<'local>,
) -> jint {
    if mob_count <= 0 {
        return 0;
    }
    let n = mob_count as usize;
    let m = target_count.max(0) as usize;

    catch_unwind(AssertUnwindSafe(|| unsafe {
        let Ok(mx) = buf_as_slice::<f32>(&mut env, &mob_x_buf, n) else { return -1; };
        let Ok(my) = buf_as_slice::<f32>(&mut env, &mob_y_buf, n) else { return -1; };
        let Ok(mz) = buf_as_slice::<f32>(&mut env, &mob_z_buf, n) else { return -1; };
        let Ok(tx) = buf_as_slice::<f32>(&mut env, &target_x_buf, m) else { return -1; };
        let Ok(ty) = buf_as_slice::<f32>(&mut env, &target_y_buf, m) else { return -1; };
        let Ok(tz) = buf_as_slice::<f32>(&mut env, &target_z_buf, m) else { return -1; };
        let Ok(oi) = buf_as_mut_slice::<i32>(&mut env, &out_idx_buf, n) else { return -1; };
        let Ok(od) = buf_as_mut_slice::<f32>(&mut env, &out_dist_sq_buf, n) else { return -1; };
        let Ok(odx) = buf_as_mut_slice::<f32>(&mut env, &out_dx_buf, n) else { return -1; };
        let Ok(ody) = buf_as_mut_slice::<f32>(&mut env, &out_dy_buf, n) else { return -1; };
        let Ok(odz) = buf_as_mut_slice::<f32>(&mut env, &out_dz_buf, n) else { return -1; };
        let Ok(ot) = buf_as_mut_slice::<i32>(&mut env, &out_lod_buf, n) else { return -1; };

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

#[no_mangle]
#[allow(clippy::too_many_arguments, unused_mut)]
pub extern "system" fn Java_net_spicanetwork_rustnative_nativebridge_RustNative_nativeComputeBoids<'local>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
    count: jint,
    xs_buf: JByteBuffer<'local>,
    ys_buf: JByteBuffer<'local>,
    zs_buf: JByteBuffer<'local>,
    vxs_buf: JByteBuffer<'local>,
    vys_buf: JByteBuffer<'local>,
    vzs_buf: JByteBuffer<'local>,
    goal_x: jfloat,
    goal_y: jfloat,
    goal_z: jfloat,
    perception_radius: jfloat,
    sep_weight: jfloat,
    align_weight: jfloat,
    cohesion_weight: jfloat,
    goal_weight: jfloat,
    max_force: jfloat,
    out_ax_buf: JByteBuffer<'local>,
    out_ay_buf: JByteBuffer<'local>,
    out_az_buf: JByteBuffer<'local>,
) -> jint {
    if count <= 0 {
        return 0;
    }
    let n = count as usize;

    catch_unwind(AssertUnwindSafe(|| unsafe {
        let Ok(xs) = buf_as_slice::<f32>(&mut env, &xs_buf, n) else { return -1; };
        let Ok(ys) = buf_as_slice::<f32>(&mut env, &ys_buf, n) else { return -1; };
        let Ok(zs) = buf_as_slice::<f32>(&mut env, &zs_buf, n) else { return -1; };
        let Ok(vxs) = buf_as_slice::<f32>(&mut env, &vxs_buf, n) else { return -1; };
        let Ok(vys) = buf_as_slice::<f32>(&mut env, &vys_buf, n) else { return -1; };
        let Ok(vzs) = buf_as_slice::<f32>(&mut env, &vzs_buf, n) else { return -1; };
        let Ok(oax) = buf_as_mut_slice::<f32>(&mut env, &out_ax_buf, n) else { return -1; };
        let Ok(oay) = buf_as_mut_slice::<f32>(&mut env, &out_ay_buf, n) else { return -1; };
        let Ok(oaz) = buf_as_mut_slice::<f32>(&mut env, &out_az_buf, n) else { return -1; };

        crate::run_in_pool(|| {
            kernels::compute_boids_steering(
                xs,
                ys,
                zs,
                vxs,
                vys,
                vzs,
                goal_x,
                goal_y,
                goal_z,
                perception_radius,
                sep_weight,
                align_weight,
                cohesion_weight,
                goal_weight,
                max_force,
                oax,
                oay,
                oaz,
            )
        })
    }))
    .unwrap_or(-1)
}

#[no_mangle]
#[allow(clippy::too_many_arguments, unused_mut)]
pub extern "system" fn Java_net_spicanetwork_rustnative_nativebridge_RustNative_nativeQueryRadius<'local>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
    count: jint,
    xs_buf: JByteBuffer<'local>,
    ys_buf: JByteBuffer<'local>,
    zs_buf: JByteBuffer<'local>,
    qx: jfloat,
    qy: jfloat,
    qz: jfloat,
    radius: jfloat,
    max_results: jint,
    out_indices_buf: JByteBuffer<'local>,
) -> jint {
    if count <= 0 || max_results <= 0 {
        return 0;
    }
    let n = count as usize;
    let m = max_results as usize;

    catch_unwind(AssertUnwindSafe(|| unsafe {
        let Ok(xs) = buf_as_slice::<f32>(&mut env, &xs_buf, n) else { return -1; };
        let Ok(ys) = buf_as_slice::<f32>(&mut env, &ys_buf, n) else { return -1; };
        let Ok(zs) = buf_as_slice::<f32>(&mut env, &zs_buf, n) else { return -1; };
        let Ok(out) = buf_as_mut_slice::<i32>(&mut env, &out_indices_buf, m) else { return -1; };

        kernels::query_radius(xs, ys, zs, qx, qy, qz, radius, out)
    }))
    .unwrap_or(-1)
}

#[no_mangle]
#[allow(clippy::too_many_arguments, unused_mut)]
pub extern "system" fn Java_net_spicanetwork_rustnative_nativebridge_RustNative_nativeAnalyzeChunks<'local>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
    entity_count: jint,
    xs_buf: JByteBuffer<'local>,
    zs_buf: JByteBuffer<'local>,
    neighbor_counts_buf: JByteBuffer<'local>,
    max_hot_chunks: jint,
    out_chunk_x_buf: JByteBuffer<'local>,
    out_chunk_z_buf: JByteBuffer<'local>,
    out_entity_count_buf: JByteBuffer<'local>,
    out_crowded_count_buf: JByteBuffer<'local>,
) -> jint {
    if entity_count <= 0 || max_hot_chunks <= 0 {
        return 0;
    }
    let n = entity_count as usize;
    let m = max_hot_chunks as usize;

    catch_unwind(AssertUnwindSafe(|| unsafe {
        let Ok(xs) = buf_as_slice::<f32>(&mut env, &xs_buf, n) else { return -1; };
        let Ok(zs) = buf_as_slice::<f32>(&mut env, &zs_buf, n) else { return -1; };
        let neighbor_counts: &[i32] = if neighbor_counts_buf.is_null() {
            &[]
        } else {
            let Ok(nc) = buf_as_slice::<i32>(&mut env, &neighbor_counts_buf, n) else {
                return -1;
            };
            nc
        };
        let Ok(out_cx) = buf_as_mut_slice::<i32>(&mut env, &out_chunk_x_buf, m) else {
            return -1;
        };
        let Ok(out_cz) = buf_as_mut_slice::<i32>(&mut env, &out_chunk_z_buf, m) else {
            return -1;
        };
        let Ok(out_ec) = buf_as_mut_slice::<i32>(&mut env, &out_entity_count_buf, m) else {
            return -1;
        };
        let Ok(out_cc) = buf_as_mut_slice::<i32>(&mut env, &out_crowded_count_buf, m) else {
            return -1;
        };

        heatmap::analyze_chunks(
            xs,
            zs,
            neighbor_counts,
            m,
            out_cx,
            out_cz,
            out_ec,
            out_cc,
        )
    }))
    .unwrap_or(-1)
}

#[no_mangle]
#[allow(clippy::too_many_arguments, unused_mut)]
pub extern "system" fn Java_net_spicanetwork_rustnative_nativebridge_RustNative_nativePlanItemMerges<'local>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
    item_count: jint,
    xs_buf: JByteBuffer<'local>,
    ys_buf: JByteBuffer<'local>,
    zs_buf: JByteBuffer<'local>,
    type_ids_buf: JByteBuffer<'local>,
    stack_amounts_buf: JByteBuffer<'local>,
    max_stack_size: jint,
    merge_radius: jfloat,
    max_pairs: jint,
    out_target_idx_buf: JByteBuffer<'local>,
    out_source_idx_buf: JByteBuffer<'local>,
) -> jint {
    if item_count <= 1 || max_pairs <= 0 {
        return 0;
    }
    let n = item_count as usize;
    let m = max_pairs as usize;

    catch_unwind(AssertUnwindSafe(|| unsafe {
        let Ok(xs) = buf_as_slice::<f32>(&mut env, &xs_buf, n) else { return -1; };
        let Ok(ys) = buf_as_slice::<f32>(&mut env, &ys_buf, n) else { return -1; };
        let Ok(zs) = buf_as_slice::<f32>(&mut env, &zs_buf, n) else { return -1; };
        let Ok(type_ids) = buf_as_slice::<i32>(&mut env, &type_ids_buf, n) else { return -1; };
        let Ok(stack_amounts) = buf_as_slice::<i32>(&mut env, &stack_amounts_buf, n) else {
            return -1;
        };
        let Ok(out_target) = buf_as_mut_slice::<i32>(&mut env, &out_target_idx_buf, m) else {
            return -1;
        };
        let Ok(out_source) = buf_as_mut_slice::<i32>(&mut env, &out_source_idx_buf, m) else {
            return -1;
        };

        kernels::plan_item_merges(
            xs,
            ys,
            zs,
            type_ids,
            stack_amounts,
            max_stack_size,
            merge_radius,
            out_target,
            out_source,
        )
    }))
    .unwrap_or(-1)
}

#[no_mangle]
#[allow(clippy::too_many_arguments, unused_mut)]
pub extern "system" fn Java_net_spicanetwork_rustnative_nativebridge_RustNative_nativeEvaluateGovernor<'local>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
    current_mspt: jfloat,
    ewma_mspt: jfloat,
    loaded_entities: jint,
    online_players: jint,
    base_full_range: jfloat,
    base_hibernate_range: jfloat,
    min_view_dist: jint,
    max_view_dist: jint,
    min_sim_dist: jint,
    max_sim_dist: jint,
    out_int_state_buf: JByteBuffer<'local>,
    out_float_params_buf: JByteBuffer<'local>,
) -> jint {
    catch_unwind(AssertUnwindSafe(|| unsafe {
        let Ok(out_ints) = buf_as_mut_slice::<i32>(&mut env, &out_int_state_buf, 4) else {
            return -1;
        };
        let Ok(out_floats) = buf_as_mut_slice::<f32>(&mut env, &out_float_params_buf, 4) else {
            return -1;
        };

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

#[no_mangle]
#[allow(clippy::too_many_arguments, unused_mut)]
pub extern "system" fn Java_net_spicanetwork_rustnative_nativebridge_RustNative_nativeEvaluateFarmThrottling<'local>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
    entity_count: jint,
    xs_buf: JByteBuffer<'local>,
    ys_buf: JByteBuffer<'local>,
    zs_buf: JByteBuffer<'local>,
    entity_flags_buf: JByteBuffer<'local>,
    player_count: jint,
    player_xs_buf: JByteBuffer<'local>,
    player_ys_buf: JByteBuffer<'local>,
    player_zs_buf: JByteBuffer<'local>,
    crowd_radius: jfloat,
    mild_crowd_threshold: jint,
    severe_crowd_threshold: jint,
    chunk_mob_limit: jint,
    villager_wake_range: jfloat,
    villager_crowd_threshold: jint,
    brain_state_code: jint,
    out_actions_buf: JByteBuffer<'local>,
    out_neighbor_counts_buf: JByteBuffer<'local>,
) -> jint {
    if entity_count <= 0 {
        return 0;
    }
    let n = entity_count as usize;
    let p = player_count.max(0) as usize;

    catch_unwind(AssertUnwindSafe(|| unsafe {
        let Ok(xs) = buf_as_slice::<f32>(&mut env, &xs_buf, n) else { return -1; };
        let Ok(ys) = buf_as_slice::<f32>(&mut env, &ys_buf, n) else { return -1; };
        let Ok(zs) = buf_as_slice::<f32>(&mut env, &zs_buf, n) else { return -1; };
        let Ok(entity_flags) = buf_as_slice::<i32>(&mut env, &entity_flags_buf, n) else {
            return -1;
        };

        let (player_xs, player_ys, player_zs): (&[f32], &[f32], &[f32]) = if p == 0 {
            (&[], &[], &[])
        } else {
            let Ok(pxs) = buf_as_slice::<f32>(&mut env, &player_xs_buf, p) else { return -1; };
            let Ok(pys) = buf_as_slice::<f32>(&mut env, &player_ys_buf, p) else { return -1; };
            let Ok(pzs) = buf_as_slice::<f32>(&mut env, &player_zs_buf, p) else { return -1; };
            (pxs, pys, pzs)
        };

        let Ok(out_actions) = buf_as_mut_slice::<i32>(&mut env, &out_actions_buf, n) else {
            return -1;
        };
        let Ok(out_neighbor_counts) =
            buf_as_mut_slice::<i32>(&mut env, &out_neighbor_counts_buf, n)
        else {
            return -1;
        };

        crate::run_in_pool(|| {
            farm_governor::evaluate_farm_throttling(
                xs,
                ys,
                zs,
                entity_flags,
                player_xs,
                player_ys,
                player_zs,
                crowd_radius,
                mild_crowd_threshold,
                severe_crowd_threshold,
                chunk_mob_limit,
                villager_wake_range,
                villager_crowd_threshold,
                brain_state_code,
                out_actions,
                out_neighbor_counts,
            )
        })
    }))
    .unwrap_or(-1)
}

#[no_mangle]
#[allow(clippy::too_many_arguments, unused_mut)]
pub extern "system" fn Java_net_spicanetwork_rustnative_nativebridge_RustNative_nativeEvaluatePlayerSurges<'local>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
    player_count: jint,
    px_buf: JByteBuffer<'local>,
    py_buf: JByteBuffer<'local>,
    pz_buf: JByteBuffer<'local>,
    vx_buf: JByteBuffer<'local>,
    vy_buf: JByteBuffer<'local>,
    vz_buf: JByteBuffer<'local>,
    is_gliding_buf: JByteBuffer<'local>,
    brain_state_code: jint,
    base_max_elytra_speed: jfloat,
    out_actions_buf: JByteBuffer<'local>,
    out_clamped_vx_buf: JByteBuffer<'local>,
    out_clamped_vy_buf: JByteBuffer<'local>,
    out_clamped_vz_buf: JByteBuffer<'local>,
) -> jint {
    if player_count <= 0 {
        return 0;
    }
    let n = player_count as usize;

    catch_unwind(AssertUnwindSafe(|| unsafe {
        let Ok(px) = buf_as_slice::<f32>(&mut env, &px_buf, n) else { return -1; };
        let Ok(py) = buf_as_slice::<f32>(&mut env, &py_buf, n) else { return -1; };
        let Ok(pz) = buf_as_slice::<f32>(&mut env, &pz_buf, n) else { return -1; };
        let Ok(vx) = buf_as_slice::<f32>(&mut env, &vx_buf, n) else { return -1; };
        let Ok(vy) = buf_as_slice::<f32>(&mut env, &vy_buf, n) else { return -1; };
        let Ok(vz) = buf_as_slice::<f32>(&mut env, &vz_buf, n) else { return -1; };
        let Ok(is_gliding) = buf_as_slice::<i32>(&mut env, &is_gliding_buf, n) else { return -1; };
        let Ok(out_actions) = buf_as_mut_slice::<i32>(&mut env, &out_actions_buf, n) else {
            return -1;
        };
        let Ok(out_cvx) = buf_as_mut_slice::<f32>(&mut env, &out_clamped_vx_buf, n) else {
            return -1;
        };
        let Ok(out_cvy) = buf_as_mut_slice::<f32>(&mut env, &out_clamped_vy_buf, n) else {
            return -1;
        };
        let Ok(out_cvz) = buf_as_mut_slice::<f32>(&mut env, &out_clamped_vz_buf, n) else {
            return -1;
        };

        surge_guard::evaluate_player_surges(
            px,
            py,
            pz,
            vx,
            vy,
            vz,
            is_gliding,
            brain_state_code,
            base_max_elytra_speed,
            out_actions,
            out_cvx,
            out_cvy,
            out_cvz,
        )
    }))
    .unwrap_or(-1)
}

#[no_mangle]
#[allow(clippy::too_many_arguments, unused_mut)]
pub extern "system" fn Java_net_spicanetwork_rustnative_nativebridge_RustNative_nativeComputeFlowField<'local>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
    width: jint,
    depth: jint,
    cost_grid_buf: JByteBuffer<'local>,
    goal_x: jint,
    goal_z: jint,
    out_dist_buf: JByteBuffer<'local>,
    out_dir_x_buf: JByteBuffer<'local>,
    out_dir_z_buf: JByteBuffer<'local>,
) -> jint {
    if width <= 0 || depth <= 0 || goal_x < 0 || goal_z < 0 {
        return 0;
    }
    let w = width as usize;
    let d = depth as usize;
    let gx = goal_x as usize;
    let gz = goal_z as usize;
    if gx >= w || gz >= d {
        return 0;
    }
    let Some(len) = w.checked_mul(d) else {
        return -1;
    };
    if len > flowfield::MAX_FLOW_FIELD_CELLS {
        return -1;
    }

    catch_unwind(AssertUnwindSafe(|| unsafe {
        let Ok(cost_grid) = buf_as_slice::<u8>(&mut env, &cost_grid_buf, len) else { return -1; };
        let Ok(out_dist) = buf_as_mut_slice::<f32>(&mut env, &out_dist_buf, len) else {
            return -1;
        };
        let Ok(out_dir_x) = buf_as_mut_slice::<f32>(&mut env, &out_dir_x_buf, len) else {
            return -1;
        };
        let Ok(out_dir_z) = buf_as_mut_slice::<f32>(&mut env, &out_dir_z_buf, len) else {
            return -1;
        };

        crate::run_in_pool(|| {
            flowfield::compute_flow_field(
                w, d, cost_grid, gx, gz, out_dist, out_dir_x, out_dir_z,
            )
        })
    }))
    .unwrap_or(-1)
}

#[no_mangle]
#[allow(clippy::too_many_arguments, unused_mut)]
pub extern "system" fn Java_net_spicanetwork_rustnative_nativebridge_RustNative_nativeGenerateNoise2D<'local>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
    origin_x: jfloat,
    origin_z: jfloat,
    width: jint,
    depth: jint,
    step: jfloat,
    seed: jint,
    octaves: jint,
    frequency: jfloat,
    lacunarity: jfloat,
    persistence: jfloat,
    out_heights_buf: JByteBuffer<'local>,
) -> jint {
    if width <= 0 || depth <= 0 {
        return 0;
    }
    let w = width as usize;
    let d = depth as usize;
    let Some(total) = w.checked_mul(d) else {
        return -1;
    };
    if total > i32::MAX as usize {
        return -1;
    }

    catch_unwind(AssertUnwindSafe(|| unsafe {
        let Ok(out_heights) = buf_as_mut_slice::<f32>(&mut env, &out_heights_buf, total) else {
            return -1;
        };

        crate::run_in_pool(|| {
            noise::generate_noise_2d(
                origin_x,
                origin_z,
                w,
                d,
                step,
                seed,
                octaves,
                frequency,
                lacunarity,
                persistence,
                out_heights,
            )
        })
    }))
    .unwrap_or(-1)
}

#[no_mangle]
#[allow(clippy::too_many_arguments, unused_mut)]
pub extern "system" fn Java_net_spicanetwork_rustnative_nativebridge_RustNative_nativeComputeMobTracking<'local>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
    mob_count: jint,
    mob_xs_buf: JByteBuffer<'local>,
    mob_ys_buf: JByteBuffer<'local>,
    mob_zs_buf: JByteBuffer<'local>,
    mob_flags_buf: JByteBuffer<'local>,
    player_count: jint,
    player_xs_buf: JByteBuffer<'local>,
    player_ys_buf: JByteBuffer<'local>,
    player_zs_buf: JByteBuffer<'local>,
    player_look_xs_buf: JByteBuffer<'local>,
    player_look_ys_buf: JByteBuffer<'local>,
    player_look_zs_buf: JByteBuffer<'local>,
    max_track_range: jfloat,
    safety_near_range: jfloat,
    max_vertical_delta: jfloat,
    rear_cull_range: jfloat,
    rear_cull_dot_threshold: jfloat,
    max_mobs_per_player: jint,
    brain_state_code: jint,
    visibility_bitset_buf: JByteBuffer<'local>,
    max_deltas: jint,
    out_delta_player_idx_buf: JByteBuffer<'local>,
    out_delta_mob_idx_buf: JByteBuffer<'local>,
    out_delta_action_buf: JByteBuffer<'local>,
    out_stats_buf: JByteBuffer<'local>,
) -> jint {
    if mob_count <= 0 || player_count <= 0 {
        let _ = catch_unwind(AssertUnwindSafe(|| unsafe {
            if !out_stats_buf.is_null() {
                if let Ok(out_stats) = buf_as_mut_slice::<i32>(&mut env, &out_stats_buf, 4) {
                    out_stats[0] = 0;
                    out_stats[1] = 0;
                    out_stats[2] = 0;
                    out_stats[3] = 0;
                }
            }
        }));
        return 0;
    }

    let m = mob_count as usize;
    let p = player_count as usize;
    let max_d = max_deltas.max(0) as usize;
    let Some(total_bits) = p.checked_mul(m) else {
        return -1;
    };
    let bitset_bytes = (total_bits >> 3) + usize::from((total_bits & 7) != 0);

    catch_unwind(AssertUnwindSafe(|| unsafe {
        let Ok(mob_xs) = buf_as_slice::<f32>(&mut env, &mob_xs_buf, m) else { return -1; };
        let Ok(mob_ys) = buf_as_slice::<f32>(&mut env, &mob_ys_buf, m) else { return -1; };
        let Ok(mob_zs) = buf_as_slice::<f32>(&mut env, &mob_zs_buf, m) else { return -1; };
        let Ok(mob_flags) = buf_as_slice::<i32>(&mut env, &mob_flags_buf, m) else {
            return -1;
        };

        let Ok(player_xs) = buf_as_slice::<f32>(&mut env, &player_xs_buf, p) else {
            return -1;
        };
        let Ok(player_ys) = buf_as_slice::<f32>(&mut env, &player_ys_buf, p) else {
            return -1;
        };
        let Ok(player_zs) = buf_as_slice::<f32>(&mut env, &player_zs_buf, p) else {
            return -1;
        };
        let Ok(player_look_xs) = buf_as_slice::<f32>(&mut env, &player_look_xs_buf, p) else {
            return -1;
        };
        let Ok(player_look_ys) = buf_as_slice::<f32>(&mut env, &player_look_ys_buf, p) else {
            return -1;
        };
        let Ok(player_look_zs) = buf_as_slice::<f32>(&mut env, &player_look_zs_buf, p) else {
            return -1;
        };

        let Ok(hidden_bitset) =
            buf_as_mut_slice::<u8>(&mut env, &visibility_bitset_buf, bitset_bytes)
        else {
            return -1;
        };
        let Ok(out_delta_player_idx) =
            buf_as_mut_slice::<i32>(&mut env, &out_delta_player_idx_buf, max_d)
        else {
            return -1;
        };
        let Ok(out_delta_mob_idx) =
            buf_as_mut_slice::<i32>(&mut env, &out_delta_mob_idx_buf, max_d)
        else {
            return -1;
        };
        let Ok(out_delta_action) =
            buf_as_mut_slice::<i32>(&mut env, &out_delta_action_buf, max_d)
        else {
            return -1;
        };
        let Ok(out_stats) = buf_as_mut_slice::<i32>(&mut env, &out_stats_buf, 4) else {
            return -1;
        };

        crate::run_in_pool(|| {
            mob_tracker::compute_mob_tracking(
                mob_xs,
                mob_ys,
                mob_zs,
                mob_flags,
                player_xs,
                player_ys,
                player_zs,
                player_look_xs,
                player_look_ys,
                player_look_zs,
                max_track_range,
                safety_near_range,
                max_vertical_delta,
                rear_cull_range,
                rear_cull_dot_threshold,
                max_mobs_per_player,
                brain_state_code,
                hidden_bitset,
                out_delta_player_idx,
                out_delta_mob_idx,
                out_delta_action,
                out_stats,
            )
        })
    }))
    .unwrap_or(-1)
}

#[no_mangle]
#[allow(clippy::too_many_arguments, unused_mut)]
pub extern "system" fn Java_net_spicanetwork_rustnative_nativebridge_RustNative_nativeEvaluateRedstoneChunks<'local>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
    chunk_count: jint,
    chunk_xs_buf: JByteBuffer<'local>,
    chunk_zs_buf: JByteBuffer<'local>,
    redstone_per_sec_buf: JByteBuffer<'local>,
    hopper_per_sec_buf: JByteBuffer<'local>,
    prev_heat_buf: JByteBuffer<'local>,
    redstone_limit: jint,
    hopper_limit: jint,
    brain_state_code: jint,
    out_actions_buf: JByteBuffer<'local>,
    out_new_heat_buf: JByteBuffer<'local>,
    out_throttle_ratio_buf: JByteBuffer<'local>,
    max_top_chunks: jint,
    out_top_indices_buf: JByteBuffer<'local>,
) -> jint {
    if chunk_count <= 0 {
        if max_top_chunks > 0 {
            let _ = catch_unwind(AssertUnwindSafe(|| unsafe {
                if !out_top_indices_buf.is_null() {
                    if let Ok(out_top) = buf_as_mut_slice::<i32>(
                        &mut env,
                        &out_top_indices_buf,
                        max_top_chunks as usize,
                    ) {
                        out_top.fill(-1);
                    }
                }
            }));
        }
        return 0;
    }

    let n = chunk_count as usize;
    let top_k = max_top_chunks.max(0) as usize;

    catch_unwind(AssertUnwindSafe(|| unsafe {
        let Ok(chunk_xs) = buf_as_slice::<i32>(&mut env, &chunk_xs_buf, n) else { return -1; };
        let Ok(chunk_zs) = buf_as_slice::<i32>(&mut env, &chunk_zs_buf, n) else { return -1; };
        let Ok(redstone_per_sec) = buf_as_slice::<i32>(&mut env, &redstone_per_sec_buf, n) else {
            return -1;
        };
        let Ok(hopper_per_sec) = buf_as_slice::<i32>(&mut env, &hopper_per_sec_buf, n) else {
            return -1;
        };
        let Ok(prev_heat) = buf_as_slice::<f32>(&mut env, &prev_heat_buf, n) else { return -1; };
        let Ok(out_actions) = buf_as_mut_slice::<i32>(&mut env, &out_actions_buf, n) else {
            return -1;
        };
        let Ok(out_new_heat) = buf_as_mut_slice::<f32>(&mut env, &out_new_heat_buf, n) else {
            return -1;
        };
        let Ok(out_throttle_ratio) =
            buf_as_mut_slice::<f32>(&mut env, &out_throttle_ratio_buf, n)
        else {
            return -1;
        };
        let out_top_indices: &mut [i32] = if top_k == 0 {
            &mut []
        } else {
            let Ok(top) = buf_as_mut_slice::<i32>(&mut env, &out_top_indices_buf, top_k) else {
                return -1;
            };
            top
        };

        crate::run_in_pool(|| {
            redstone_guard::evaluate_redstone_chunks(
                chunk_xs,
                chunk_zs,
                redstone_per_sec,
                hopper_per_sec,
                prev_heat,
                redstone_limit,
                hopper_limit,
                brain_state_code,
                out_actions,
                out_new_heat,
                out_throttle_ratio,
                out_top_indices,
            )
        })
    }))
    .unwrap_or(-1)
}

#[no_mangle]
#[allow(clippy::too_many_arguments, unused_mut)]
pub extern "system" fn Java_net_spicanetwork_rustnative_nativebridge_RustNative_nativeComputeBlockDiff<'local>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
    width: jint,
    height: jint,
    depth: jint,
    current_blocks_buf: JByteBuffer<'local>,
    target_blocks_buf: JByteBuffer<'local>,
    max_diffs: jint,
    out_rel_x_buf: JByteBuffer<'local>,
    out_rel_y_buf: JByteBuffer<'local>,
    out_rel_z_buf: JByteBuffer<'local>,
    out_new_block_id_buf: JByteBuffer<'local>,
) -> jint {
    if width <= 0 || height <= 0 || depth <= 0 {
        return 0;
    }
    let w = width as usize;
    let h = height as usize;
    let d = depth as usize;
    let Some(total) = w.checked_mul(d).and_then(|p| p.checked_mul(h)) else {
        return -1;
    };
    if total > block_diff::MAX_VOLUME_BLOCKS {
        return -1;
    }
    let max_d = max_diffs.max(0) as usize;

    catch_unwind(AssertUnwindSafe(|| unsafe {
        let Ok(cur) = buf_as_slice::<i32>(&mut env, &current_blocks_buf, total) else {
            return -1;
        };
        let Ok(tgt) = buf_as_slice::<i32>(&mut env, &target_blocks_buf, total) else {
            return -1;
        };

        let (out_x, out_y, out_z, out_id): (&mut [i32], &mut [i32], &mut [i32], &mut [i32]) =
            if max_d == 0 {
                (&mut [], &mut [], &mut [], &mut [])
            } else {
                let Ok(ox) = buf_as_mut_slice::<i32>(&mut env, &out_rel_x_buf, max_d) else {
                    return -1;
                };
                let Ok(oy) = buf_as_mut_slice::<i32>(&mut env, &out_rel_y_buf, max_d) else {
                    return -1;
                };
                let Ok(oz) = buf_as_mut_slice::<i32>(&mut env, &out_rel_z_buf, max_d) else {
                    return -1;
                };
                let Ok(oi) = buf_as_mut_slice::<i32>(&mut env, &out_new_block_id_buf, max_d) else {
                    return -1;
                };
                (ox, oy, oz, oi)
            };

        crate::run_in_pool(|| {
            block_diff::compute_block_diff(w, h, d, cur, tgt, out_x, out_y, out_z, out_id)
        })
    }))
    .unwrap_or(-1)
}
