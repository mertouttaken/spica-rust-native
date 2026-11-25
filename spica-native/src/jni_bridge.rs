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

use crate::{farm_governor, governor, heatmap, kernels};

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
