//! Zero-copy JNI bridge for Purpur/Paper 1.21.4 (Java 21+ with zero extra JVM flags).
//!
//! Uses `GetDirectBufferAddress` to read/write off-heap `ByteBuffer.allocateDirect(...)`
//! memory directly with zero copying and zero GC allocation per tick.
//!
//! Safety guarantees (K1):
//! - Every `JByteBuffer` is validated via `SpanValidator` BEFORE any Rust slice is formed:
//!   direct address non-null, `align_of::<T>()`, `count * size_of::<T>() <= isize::MAX`,
//!   capacity >= needed bytes, address wraparound check, `isReadOnly()` rejection on outputs,
//!   and strict rejection of read-write / write-write overlapping DirectByteBuffer views.
//! - Every exported function wraps its entire body (including JNI return allocation) in
//!   `catch_unwind`.

use std::panic::{catch_unwind, AssertUnwindSafe};

use jni::objects::{JByteBuffer, JClass, JObject};
use jni::sys::{jfloat, jint, jlong, jstring};
use jni::JNIEnv;

use crate::ffi_guard::{FfiGuardError, RawReadSpan, RawWriteSpan, SpanValidator};
use crate::{
    block_diff, farm_governor, flowfield, governor, heatmap, kernels, mob_tracker, noise,
    redstone_guard, surge_guard,
};

/// Inspects a Java DirectByteBuffer and registers it as a read-only span in `validator`.
fn register_jni_read<T>(
    env: &mut JNIEnv<'_>,
    validator: &mut SpanValidator,
    buf: &JByteBuffer<'_>,
    count: usize,
) -> Result<RawReadSpan<T>, FfiGuardError> {
    if count == 0 {
        return Ok(RawReadSpan::empty());
    }
    if buf.is_null() {
        return Err(FfiGuardError::NullPointer);
    }
    let ptr = env
        .get_direct_buffer_address(buf)
        .map_err(|_| FfiGuardError::JniBufferError)?;
    let cap = env
        .get_direct_buffer_capacity(buf)
        .map_err(|_| FfiGuardError::JniBufferError)?;

    validator.register_read(ptr as *const T, count, Some(cap))
}

/// Inspects a Java DirectByteBuffer, verifies it is NOT read-only, and registers it as an
/// exclusively writable span in `validator`.
fn register_jni_write<T>(
    env: &mut JNIEnv<'_>,
    validator: &mut SpanValidator,
    buf: &JByteBuffer<'_>,
    count: usize,
) -> Result<RawWriteSpan<T>, FfiGuardError> {
    if count == 0 {
        return Ok(RawWriteSpan::empty());
    }
    if buf.is_null() {
        return Err(FfiGuardError::NullPointer);
    }
    // Reject read-only DirectByteBuffer views (e.g. ByteBuffer#asReadOnlyBuffer())
    let obj: &JObject<'_> = buf.as_ref();
    let is_ro = env
        .call_method(obj, "isReadOnly", "()Z", &[])
        .and_then(|v| v.z());
    match is_ro {
        Ok(true) => return Err(FfiGuardError::ReadOnlyBuffer),
        Ok(false) => {}
        Err(_) => {
            let _ = env.exception_clear();
            return Err(FfiGuardError::JniBufferError);
        }
    }

    let ptr = env
        .get_direct_buffer_address(buf)
        .map_err(|_| FfiGuardError::JniBufferError)?;
    let cap = env
        .get_direct_buffer_capacity(buf)
        .map_err(|_| FfiGuardError::JniBufferError)?;

    validator.register_write(ptr as *mut T, count, Some(cap))
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

/// Explicit JNI ABI contract revision (R13): bumped whenever FFI safety or output layout changes,
/// allowing `NativeLoader` to distinguish incompatible binaries even within the same package SemVer.
const NATIVE_ABI_REV: u32 = 2;

#[no_mangle]
#[allow(unused_mut)]
pub extern "system" fn Java_net_spicanetwork_rustnative_nativebridge_RustNative_nativeGetInfo<'local>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
) -> jstring {
    catch_unwind(AssertUnwindSafe(|| {
        let info = format!(
            "spica-rust-native v{} | abi={} | os={}-{} | threads={}",
            env!("CARGO_PKG_VERSION"),
            NATIVE_ABI_REV,
            std::env::consts::OS,
            std::env::consts::ARCH,
            crate::active_threads()
        );
        env.new_string(info)
            .map(|s| s.into_raw())
            .unwrap_or(std::ptr::null_mut())
    }))
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

    catch_unwind(AssertUnwindSafe(|| {
        let mut guard = SpanValidator::new();
        let Ok(xs_r) = register_jni_read::<f32>(&mut env, &mut guard, &xs_buf, n) else {
            return -1;
        };
        let Ok(ys_r) = register_jni_read::<f32>(&mut env, &mut guard, &ys_buf, n) else {
            return -1;
        };
        let Ok(zs_r) = register_jni_read::<f32>(&mut env, &mut guard, &zs_buf, n) else {
            return -1;
        };
        let Ok(rad_r) = register_jni_read::<f32>(&mut env, &mut guard, &radii_buf, n) else {
            return -1;
        };
        let Ok(mut opx_w) = register_jni_write::<f32>(&mut env, &mut guard, &out_px_buf, n) else {
            return -1;
        };
        let Ok(mut opy_w) = register_jni_write::<f32>(&mut env, &mut guard, &out_py_buf, n) else {
            return -1;
        };
        let Ok(mut opz_w) = register_jni_write::<f32>(&mut env, &mut guard, &out_pz_buf, n) else {
            return -1;
        };
        let Ok(mut oc_w) = register_jni_write::<i32>(&mut env, &mut guard, &out_counts_buf, n) else {
            return -1;
        };

        unsafe {
            let xs = xs_r.as_slice();
            let ys = ys_r.as_slice();
            let zs = zs_r.as_slice();
            let radii = rad_r.as_slice();
            let opx = opx_w.as_mut_slice();
            let opy = opy_w.as_mut_slice();
            let opz = opz_w.as_mut_slice();
            let oc = oc_w.as_mut_slice();

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
        }
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

    catch_unwind(AssertUnwindSafe(|| {
        let mut guard = SpanValidator::new();
        let Ok(mx_r) = register_jni_read::<f32>(&mut env, &mut guard, &mob_x_buf, n) else {
            return -1;
        };
        let Ok(my_r) = register_jni_read::<f32>(&mut env, &mut guard, &mob_y_buf, n) else {
            return -1;
        };
        let Ok(mz_r) = register_jni_read::<f32>(&mut env, &mut guard, &mob_z_buf, n) else {
            return -1;
        };
        let Ok(tx_r) = register_jni_read::<f32>(&mut env, &mut guard, &target_x_buf, m) else {
            return -1;
        };
        let Ok(ty_r) = register_jni_read::<f32>(&mut env, &mut guard, &target_y_buf, m) else {
            return -1;
        };
        let Ok(tz_r) = register_jni_read::<f32>(&mut env, &mut guard, &target_z_buf, m) else {
            return -1;
        };
        let Ok(mut oi_w) = register_jni_write::<i32>(&mut env, &mut guard, &out_idx_buf, n) else {
            return -1;
        };
        let Ok(mut od_w) = register_jni_write::<f32>(&mut env, &mut guard, &out_dist_sq_buf, n) else {
            return -1;
        };
        let Ok(mut odx_w) = register_jni_write::<f32>(&mut env, &mut guard, &out_dx_buf, n) else {
            return -1;
        };
        let Ok(mut ody_w) = register_jni_write::<f32>(&mut env, &mut guard, &out_dy_buf, n) else {
            return -1;
        };
        let Ok(mut odz_w) = register_jni_write::<f32>(&mut env, &mut guard, &out_dz_buf, n) else {
            return -1;
        };
        let Ok(mut ot_w) = register_jni_write::<i32>(&mut env, &mut guard, &out_lod_buf, n) else {
            return -1;
        };

        unsafe {
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
        }
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

    catch_unwind(AssertUnwindSafe(|| {
        let mut guard = SpanValidator::new();
        let Ok(xs_r) = register_jni_read::<f32>(&mut env, &mut guard, &xs_buf, n) else {
            return -1;
        };
        let Ok(ys_r) = register_jni_read::<f32>(&mut env, &mut guard, &ys_buf, n) else {
            return -1;
        };
        let Ok(zs_r) = register_jni_read::<f32>(&mut env, &mut guard, &zs_buf, n) else {
            return -1;
        };
        let Ok(vxs_r) = register_jni_read::<f32>(&mut env, &mut guard, &vxs_buf, n) else {
            return -1;
        };
        let Ok(vys_r) = register_jni_read::<f32>(&mut env, &mut guard, &vys_buf, n) else {
            return -1;
        };
        let Ok(vzs_r) = register_jni_read::<f32>(&mut env, &mut guard, &vzs_buf, n) else {
            return -1;
        };
        let Ok(mut oax_w) = register_jni_write::<f32>(&mut env, &mut guard, &out_ax_buf, n) else {
            return -1;
        };
        let Ok(mut oay_w) = register_jni_write::<f32>(&mut env, &mut guard, &out_ay_buf, n) else {
            return -1;
        };
        let Ok(mut oaz_w) = register_jni_write::<f32>(&mut env, &mut guard, &out_az_buf, n) else {
            return -1;
        };

        unsafe {
            let xs = xs_r.as_slice();
            let ys = ys_r.as_slice();
            let zs = zs_r.as_slice();
            let vxs = vxs_r.as_slice();
            let vys = vys_r.as_slice();
            let vzs = vzs_r.as_slice();
            let oax = oax_w.as_mut_slice();
            let oay = oay_w.as_mut_slice();
            let oaz = oaz_w.as_mut_slice();

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
        }
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

    catch_unwind(AssertUnwindSafe(|| {
        let mut guard = SpanValidator::new();
        let Ok(xs_r) = register_jni_read::<f32>(&mut env, &mut guard, &xs_buf, n) else {
            return -1;
        };
        let Ok(ys_r) = register_jni_read::<f32>(&mut env, &mut guard, &ys_buf, n) else {
            return -1;
        };
        let Ok(zs_r) = register_jni_read::<f32>(&mut env, &mut guard, &zs_buf, n) else {
            return -1;
        };
        let Ok(mut out_w) = register_jni_write::<i32>(&mut env, &mut guard, &out_indices_buf, m) else {
            return -1;
        };

        unsafe {
            kernels::query_radius(
                xs_r.as_slice(),
                ys_r.as_slice(),
                zs_r.as_slice(),
                qx,
                qy,
                qz,
                radius,
                out_w.as_mut_slice(),
            )
        }
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

    catch_unwind(AssertUnwindSafe(|| {
        let mut guard = SpanValidator::new();
        let Ok(xs_r) = register_jni_read::<f32>(&mut env, &mut guard, &xs_buf, n) else {
            return -1;
        };
        let Ok(zs_r) = register_jni_read::<f32>(&mut env, &mut guard, &zs_buf, n) else {
            return -1;
        };
        let nc_r = if neighbor_counts_buf.is_null() {
            RawReadSpan::empty()
        } else {
            let Ok(nc) = register_jni_read::<i32>(&mut env, &mut guard, &neighbor_counts_buf, n)
            else {
                return -1;
            };
            nc
        };
        let Ok(mut cx_w) = register_jni_write::<i32>(&mut env, &mut guard, &out_chunk_x_buf, m) else {
            return -1;
        };
        let Ok(mut cz_w) = register_jni_write::<i32>(&mut env, &mut guard, &out_chunk_z_buf, m) else {
            return -1;
        };
        let Ok(mut ec_w) = register_jni_write::<i32>(&mut env, &mut guard, &out_entity_count_buf, m)
        else {
            return -1;
        };
        let Ok(mut cc_w) = register_jni_write::<i32>(&mut env, &mut guard, &out_crowded_count_buf, m)
        else {
            return -1;
        };

        unsafe {
            heatmap::analyze_chunks(
                xs_r.as_slice(),
                zs_r.as_slice(),
                nc_r.as_slice(),
                m,
                cx_w.as_mut_slice(),
                cz_w.as_mut_slice(),
                ec_w.as_mut_slice(),
                cc_w.as_mut_slice(),
            )
        }
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

    catch_unwind(AssertUnwindSafe(|| {
        let mut guard = SpanValidator::new();
        let Ok(xs_r) = register_jni_read::<f32>(&mut env, &mut guard, &xs_buf, n) else {
            return -1;
        };
        let Ok(ys_r) = register_jni_read::<f32>(&mut env, &mut guard, &ys_buf, n) else {
            return -1;
        };
        let Ok(zs_r) = register_jni_read::<f32>(&mut env, &mut guard, &zs_buf, n) else {
            return -1;
        };
        let Ok(ti_r) = register_jni_read::<i32>(&mut env, &mut guard, &type_ids_buf, n) else {
            return -1;
        };
        let Ok(sa_r) = register_jni_read::<i32>(&mut env, &mut guard, &stack_amounts_buf, n) else {
            return -1;
        };
        let Ok(mut ot_w) = register_jni_write::<i32>(&mut env, &mut guard, &out_target_idx_buf, m)
        else {
            return -1;
        };
        let Ok(mut os_w) = register_jni_write::<i32>(&mut env, &mut guard, &out_source_idx_buf, m)
        else {
            return -1;
        };

        unsafe {
            kernels::plan_item_merges(
                xs_r.as_slice(),
                ys_r.as_slice(),
                zs_r.as_slice(),
                ti_r.as_slice(),
                sa_r.as_slice(),
                max_stack_size,
                merge_radius,
                ot_w.as_mut_slice(),
                os_w.as_mut_slice(),
            )
        }
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
    catch_unwind(AssertUnwindSafe(|| {
        let mut guard = SpanValidator::new();
        let Ok(mut oi_w) = register_jni_write::<i32>(&mut env, &mut guard, &out_int_state_buf, 4)
        else {
            return -1;
        };
        let Ok(mut of_w) = register_jni_write::<f32>(&mut env, &mut guard, &out_float_params_buf, 4)
        else {
            return -1;
        };

        unsafe {
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
        }
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

    catch_unwind(AssertUnwindSafe(|| {
        let mut guard = SpanValidator::new();
        let Ok(xs_r) = register_jni_read::<f32>(&mut env, &mut guard, &xs_buf, n) else {
            return -1;
        };
        let Ok(ys_r) = register_jni_read::<f32>(&mut env, &mut guard, &ys_buf, n) else {
            return -1;
        };
        let Ok(zs_r) = register_jni_read::<f32>(&mut env, &mut guard, &zs_buf, n) else {
            return -1;
        };
        let Ok(fl_r) = register_jni_read::<i32>(&mut env, &mut guard, &entity_flags_buf, n) else {
            return -1;
        };
        let Ok(px_r) = register_jni_read::<f32>(&mut env, &mut guard, &player_xs_buf, p) else {
            return -1;
        };
        let Ok(py_r) = register_jni_read::<f32>(&mut env, &mut guard, &player_ys_buf, p) else {
            return -1;
        };
        let Ok(pz_r) = register_jni_read::<f32>(&mut env, &mut guard, &player_zs_buf, p) else {
            return -1;
        };
        let Ok(mut oa_w) = register_jni_write::<i32>(&mut env, &mut guard, &out_actions_buf, n) else {
            return -1;
        };
        let Ok(mut on_w) =
            register_jni_write::<i32>(&mut env, &mut guard, &out_neighbor_counts_buf, n)
        else {
            return -1;
        };

        unsafe {
            let xs = xs_r.as_slice();
            let ys = ys_r.as_slice();
            let zs = zs_r.as_slice();
            let entity_flags = fl_r.as_slice();
            let player_xs = px_r.as_slice();
            let player_ys = py_r.as_slice();
            let player_zs = pz_r.as_slice();
            let out_actions = oa_w.as_mut_slice();
            let out_neighbor_counts = on_w.as_mut_slice();

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
        }
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

    catch_unwind(AssertUnwindSafe(|| {
        let mut guard = SpanValidator::new();
        let Ok(px_r) = register_jni_read::<f32>(&mut env, &mut guard, &px_buf, n) else {
            return -1;
        };
        let Ok(py_r) = register_jni_read::<f32>(&mut env, &mut guard, &py_buf, n) else {
            return -1;
        };
        let Ok(pz_r) = register_jni_read::<f32>(&mut env, &mut guard, &pz_buf, n) else {
            return -1;
        };
        let Ok(vx_r) = register_jni_read::<f32>(&mut env, &mut guard, &vx_buf, n) else {
            return -1;
        };
        let Ok(vy_r) = register_jni_read::<f32>(&mut env, &mut guard, &vy_buf, n) else {
            return -1;
        };
        let Ok(vz_r) = register_jni_read::<f32>(&mut env, &mut guard, &vz_buf, n) else {
            return -1;
        };
        let Ok(gl_r) = register_jni_read::<i32>(&mut env, &mut guard, &is_gliding_buf, n) else {
            return -1;
        };
        let Ok(mut oa_w) = register_jni_write::<i32>(&mut env, &mut guard, &out_actions_buf, n) else {
            return -1;
        };
        let Ok(mut cvx_w) = register_jni_write::<f32>(&mut env, &mut guard, &out_clamped_vx_buf, n)
        else {
            return -1;
        };
        let Ok(mut cvy_w) = register_jni_write::<f32>(&mut env, &mut guard, &out_clamped_vy_buf, n)
        else {
            return -1;
        };
        let Ok(mut cvz_w) = register_jni_write::<f32>(&mut env, &mut guard, &out_clamped_vz_buf, n)
        else {
            return -1;
        };

        unsafe {
            surge_guard::evaluate_player_surges(
                px_r.as_slice(),
                py_r.as_slice(),
                pz_r.as_slice(),
                vx_r.as_slice(),
                vy_r.as_slice(),
                vz_r.as_slice(),
                gl_r.as_slice(),
                brain_state_code,
                base_max_elytra_speed,
                oa_w.as_mut_slice(),
                cvx_w.as_mut_slice(),
                cvy_w.as_mut_slice(),
                cvz_w.as_mut_slice(),
            )
        }
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

    catch_unwind(AssertUnwindSafe(|| {
        let mut guard = SpanValidator::new();
        let Ok(cg_r) = register_jni_read::<u8>(&mut env, &mut guard, &cost_grid_buf, len) else {
            return -1;
        };
        let Ok(mut od_w) = register_jni_write::<f32>(&mut env, &mut guard, &out_dist_buf, len) else {
            return -1;
        };
        let Ok(mut ox_w) = register_jni_write::<f32>(&mut env, &mut guard, &out_dir_x_buf, len) else {
            return -1;
        };
        let Ok(mut oz_w) = register_jni_write::<f32>(&mut env, &mut guard, &out_dir_z_buf, len) else {
            return -1;
        };

        unsafe {
            let cost_grid = cg_r.as_slice();
            let out_dist = od_w.as_mut_slice();
            let out_dir_x = ox_w.as_mut_slice();
            let out_dir_z = oz_w.as_mut_slice();

            crate::run_in_pool(|| {
                flowfield::compute_flow_field(
                    w, d, cost_grid, gx, gz, out_dist, out_dir_x, out_dir_z,
                )
            })
        }
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

    catch_unwind(AssertUnwindSafe(|| {
        let mut guard = SpanValidator::new();
        let Ok(mut oh_w) = register_jni_write::<f32>(&mut env, &mut guard, &out_heights_buf, total)
        else {
            return -1;
        };

        unsafe {
            let out_heights = oh_w.as_mut_slice();
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
        }
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
        let _ = catch_unwind(AssertUnwindSafe(|| {
            if !out_stats_buf.is_null() {
                let mut guard = SpanValidator::new();
                if let Ok(mut os_w) =
                    register_jni_write::<i32>(&mut env, &mut guard, &out_stats_buf, 4)
                {
                    unsafe {
                        let out_stats = os_w.as_mut_slice();
                        out_stats.fill(0);
                    }
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

    catch_unwind(AssertUnwindSafe(|| {
        let mut guard = SpanValidator::new();
        let Ok(mxs_r) = register_jni_read::<f32>(&mut env, &mut guard, &mob_xs_buf, m) else {
            return -1;
        };
        let Ok(mys_r) = register_jni_read::<f32>(&mut env, &mut guard, &mob_ys_buf, m) else {
            return -1;
        };
        let Ok(mzs_r) = register_jni_read::<f32>(&mut env, &mut guard, &mob_zs_buf, m) else {
            return -1;
        };
        let Ok(mfl_r) = register_jni_read::<i32>(&mut env, &mut guard, &mob_flags_buf, m) else {
            return -1;
        };
        let Ok(pxs_r) = register_jni_read::<f32>(&mut env, &mut guard, &player_xs_buf, p) else {
            return -1;
        };
        let Ok(pys_r) = register_jni_read::<f32>(&mut env, &mut guard, &player_ys_buf, p) else {
            return -1;
        };
        let Ok(pzs_r) = register_jni_read::<f32>(&mut env, &mut guard, &player_zs_buf, p) else {
            return -1;
        };
        let Ok(plx_r) = register_jni_read::<f32>(&mut env, &mut guard, &player_look_xs_buf, p)
        else {
            return -1;
        };
        let Ok(ply_r) = register_jni_read::<f32>(&mut env, &mut guard, &player_look_ys_buf, p)
        else {
            return -1;
        };
        let Ok(plz_r) = register_jni_read::<f32>(&mut env, &mut guard, &player_look_zs_buf, p)
        else {
            return -1;
        };

        let Ok(mut hb_w) =
            register_jni_write::<u8>(&mut env, &mut guard, &visibility_bitset_buf, bitset_bytes)
        else {
            return -1;
        };
        let Ok(mut dp_w) =
            register_jni_write::<i32>(&mut env, &mut guard, &out_delta_player_idx_buf, max_d)
        else {
            return -1;
        };
        let Ok(mut dm_w) =
            register_jni_write::<i32>(&mut env, &mut guard, &out_delta_mob_idx_buf, max_d)
        else {
            return -1;
        };
        let Ok(mut da_w) =
            register_jni_write::<i32>(&mut env, &mut guard, &out_delta_action_buf, max_d)
        else {
            return -1;
        };
        let Ok(mut os_w) = register_jni_write::<i32>(&mut env, &mut guard, &out_stats_buf, 4) else {
            return -1;
        };

        unsafe {
            let mob_xs = mxs_r.as_slice();
            let mob_ys = mys_r.as_slice();
            let mob_zs = mzs_r.as_slice();
            let mob_flags = mfl_r.as_slice();
            let player_xs = pxs_r.as_slice();
            let player_ys = pys_r.as_slice();
            let player_zs = pzs_r.as_slice();
            let player_look_xs = plx_r.as_slice();
            let player_look_ys = ply_r.as_slice();
            let player_look_zs = plz_r.as_slice();
            let hidden_bitset = hb_w.as_mut_slice();
            let out_delta_player_idx = dp_w.as_mut_slice();
            let out_delta_mob_idx = dm_w.as_mut_slice();
            let out_delta_action = da_w.as_mut_slice();
            let out_stats = os_w.as_mut_slice();

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
        }
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
            let _ = catch_unwind(AssertUnwindSafe(|| {
                if !out_top_indices_buf.is_null() {
                    let mut guard = SpanValidator::new();
                    if let Ok(mut ot_w) = register_jni_write::<i32>(
                        &mut env,
                        &mut guard,
                        &out_top_indices_buf,
                        max_top_chunks as usize,
                    ) {
                        unsafe {
                            ot_w.as_mut_slice().fill(-1);
                        }
                    }
                }
            }));
        }
        return 0;
    }

    let n = chunk_count as usize;
    let top_k = max_top_chunks.max(0) as usize;

    catch_unwind(AssertUnwindSafe(|| {
        let mut guard = SpanValidator::new();
        let Ok(cxs_r) = register_jni_read::<i32>(&mut env, &mut guard, &chunk_xs_buf, n) else {
            return -1;
        };
        let Ok(czs_r) = register_jni_read::<i32>(&mut env, &mut guard, &chunk_zs_buf, n) else {
            return -1;
        };
        let Ok(rs_r) = register_jni_read::<i32>(&mut env, &mut guard, &redstone_per_sec_buf, n)
        else {
            return -1;
        };
        let Ok(hop_r) = register_jni_read::<i32>(&mut env, &mut guard, &hopper_per_sec_buf, n)
        else {
            return -1;
        };
        let Ok(ph_r) = register_jni_read::<f32>(&mut env, &mut guard, &prev_heat_buf, n) else {
            return -1;
        };
        let Ok(mut oa_w) = register_jni_write::<i32>(&mut env, &mut guard, &out_actions_buf, n) else {
            return -1;
        };
        let Ok(mut nh_w) = register_jni_write::<f32>(&mut env, &mut guard, &out_new_heat_buf, n) else {
            return -1;
        };
        let Ok(mut tr_w) = register_jni_write::<f32>(&mut env, &mut guard, &out_throttle_ratio_buf, n)
        else {
            return -1;
        };
        let Ok(mut top_w) =
            register_jni_write::<i32>(&mut env, &mut guard, &out_top_indices_buf, top_k)
        else {
            return -1;
        };

        unsafe {
            let chunk_xs = cxs_r.as_slice();
            let chunk_zs = czs_r.as_slice();
            let redstone_per_sec = rs_r.as_slice();
            let hopper_per_sec = hop_r.as_slice();
            let prev_heat = ph_r.as_slice();
            let out_actions = oa_w.as_mut_slice();
            let out_new_heat = nh_w.as_mut_slice();
            let out_throttle_ratio = tr_w.as_mut_slice();
            let out_top_indices = top_w.as_mut_slice();

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
        }
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

    catch_unwind(AssertUnwindSafe(|| {
        let mut guard = SpanValidator::new();
        let Ok(cur_r) = register_jni_read::<i32>(&mut env, &mut guard, &current_blocks_buf, total)
        else {
            return -1;
        };
        let Ok(tgt_r) = register_jni_read::<i32>(&mut env, &mut guard, &target_blocks_buf, total)
        else {
            return -1;
        };
        let Ok(mut ox_w) = register_jni_write::<i32>(&mut env, &mut guard, &out_rel_x_buf, max_d)
        else {
            return -1;
        };
        let Ok(mut oy_w) = register_jni_write::<i32>(&mut env, &mut guard, &out_rel_y_buf, max_d)
        else {
            return -1;
        };
        let Ok(mut oz_w) = register_jni_write::<i32>(&mut env, &mut guard, &out_rel_z_buf, max_d)
        else {
            return -1;
        };
        let Ok(mut oi_w) =
            register_jni_write::<i32>(&mut env, &mut guard, &out_new_block_id_buf, max_d)
        else {
            return -1;
        };

        unsafe {
            let cur = cur_r.as_slice();
            let tgt = tgt_r.as_slice();
            let out_x = ox_w.as_mut_slice();
            let out_y = oy_w.as_mut_slice();
            let out_z = oz_w.as_mut_slice();
            let out_id = oi_w.as_mut_slice();

            crate::run_in_pool(|| {
                block_diff::compute_block_diff(w, h, d, cur, tgt, out_x, out_y, out_z, out_id)
            })
        }
    }))
    .unwrap_or(-1)
}
