//! Elytra / High-Speed Chunk-Load Surge Guard for Purpur/Paper 1.21.4.
//!
//! Dynamically scales permissible Elytra flight velocity according to the current
//! Adaptive Governor health state (`GREEN`..`EMERGENCY`) to prevent chunk-generation
//! and chunk-loading I/O spikes during high-load periods, while preserving flight
//! direction and never increasing fall speed.

pub const SURGE_OK: i32 = 0;
pub const SURGE_WARN: i32 = 1;
pub const SURGE_CLAMP_VELOCITY: i32 = 2;

const DEFAULT_MAX_ELYTRA_SPEED: f32 = 1.8;
const MIN_EFFECTIVE_MAX_SPEED: f32 = 0.45;
const WARN_SPEED_RATIO: f32 = 0.85;
const MAX_3D_SPEED_RATIO: f32 = 1.2;
const MAX_UPWARD_VY_RATIO: f32 = 0.65;

/// Evaluate high-speed Elytra surges for a batch of online players.
///
/// Writes per-player action codes (`SURGE_OK`, `SURGE_WARN`, `SURGE_CLAMP_VELOCITY`)
/// and safe (or clamped) velocity vectors into the output slices.
///
/// Returns the number of players whose velocity was clamped (`SURGE_CLAMP_VELOCITY`).
#[allow(clippy::too_many_arguments)]
pub fn evaluate_player_surges(
    px: &[f32],
    py: &[f32],
    pz: &[f32],
    vx: &[f32],
    vy: &[f32],
    vz: &[f32],
    is_gliding: &[i32],
    brain_state_code: i32,
    base_max_elytra_speed: f32,
    out_actions: &mut [i32],
    out_clamped_vx: &mut [f32],
    out_clamped_vy: &mut [f32],
    out_clamped_vz: &mut [f32],
) -> i32 {
    let n = px
        .len()
        .min(py.len())
        .min(pz.len())
        .min(vx.len())
        .min(vy.len())
        .min(vz.len())
        .min(is_gliding.len())
        .min(out_actions.len())
        .min(out_clamped_vx.len())
        .min(out_clamped_vy.len())
        .min(out_clamped_vz.len());

    if n == 0 {
        return 0;
    }

    let safe_base = if base_max_elytra_speed.is_finite() && base_max_elytra_speed > 0.0 {
        base_max_elytra_speed
    } else {
        DEFAULT_MAX_ELYTRA_SPEED
    };

    let scale = match brain_state_code {
        s if s <= 0 => 1.15f32,
        1 => 0.85f32,
        2 => 0.70f32,
        3 => 0.55f32,
        _ => 0.42f32,
    };

    let max_speed = (safe_base * scale).max(MIN_EFFECTIVE_MAX_SPEED);
    let warn_speed = max_speed * WARN_SPEED_RATIO;
    let max_speed_3d = max_speed * MAX_3D_SPEED_RATIO;
    let max_upward_vy = max_speed * MAX_UPWARD_VY_RATIO;

    let mut clamped_count = 0i32;

    for i in 0..n {
        let cur_vx = if vx[i].is_finite() { vx[i] } else { 0.0 };
        let cur_vy = if vy[i].is_finite() { vy[i] } else { 0.0 };
        let cur_vz = if vz[i].is_finite() { vz[i] } else { 0.0 };

        out_actions[i] = SURGE_OK;
        out_clamped_vx[i] = cur_vx;
        out_clamped_vy[i] = cur_vy;
        out_clamped_vz[i] = cur_vz;

        if is_gliding[i] == 0 {
            continue;
        }

        // Compute norms in f64 to avoid f32 intermediate squaring overflow on extreme inputs.
        let vx64 = cur_vx as f64;
        let vy64 = cur_vy as f64;
        let vz64 = cur_vz as f64;
        let horiz_sq = vx64 * vx64 + vz64 * vz64;
        let horiz = horiz_sq.sqrt() as f32;
        let speed = (horiz_sq + vy64 * vy64).sqrt() as f32;

        if horiz > max_speed || speed > max_speed_3d {
            let horiz_scale = if horiz > max_speed {
                max_speed / horiz.max(1e-5)
            } else {
                1.0
            };

            out_clamped_vx[i] = cur_vx * horiz_scale;
            // Clamp upward vy gently; keep downward vy safe (never increase fall speed).
            out_clamped_vy[i] = cur_vy.clamp(-max_speed_3d, max_upward_vy);
            out_clamped_vz[i] = cur_vz * horiz_scale;
            out_actions[i] = SURGE_CLAMP_VELOCITY;
            clamped_count += 1;
        } else if horiz > warn_speed {
            out_actions[i] = SURGE_WARN;
        }
    }

    clamped_count
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_green_pass_through_and_warn() {
        // GREEN (0): base 1.8 * 1.15 = 2.07 max_speed, warn_speed = 1.7595
        let px = [0.0f32, 10.0, 20.0];
        let py = [100.0f32, 100.0, 100.0];
        let pz = [0.0f32, 10.0, 20.0];
        let vx = [1.5f32, 1.9, 2.6];
        let vy = [0.1f32, 0.0, 0.2];
        let vz = [0.0f32, 0.0, 0.0];
        let is_gliding = [1i32, 1, 1];

        let mut actions = [0i32; 3];
        let mut out_vx = [0.0f32; 3];
        let mut out_vy = [0.0f32; 3];
        let mut out_vz = [0.0f32; 3];

        let clamped = evaluate_player_surges(
            &px,
            &py,
            &pz,
            &vx,
            &vy,
            &vz,
            &is_gliding,
            0,
            1.8,
            &mut actions,
            &mut out_vx,
            &mut out_vy,
            &mut out_vz,
        );

        assert_eq!(clamped, 1);
        assert_eq!(actions[0], SURGE_OK);
        assert!((out_vx[0] - 1.5).abs() < 1e-5);
        assert!((out_vy[0] - 0.1).abs() < 1e-5);

        assert_eq!(actions[1], SURGE_WARN);
        assert!((out_vx[1] - 1.9).abs() < 1e-5);

        assert_eq!(actions[2], SURGE_CLAMP_VELOCITY);
        assert!((out_vx[2] - 2.07).abs() < 1e-4);
        assert!((out_vy[2] - 0.2).abs() < 1e-5);
    }

    #[test]
    fn test_red_clamping_preserves_flight_direction_and_safe_vy() {
        // RED (3): base 1.8 * 0.55 = 0.99 max_speed, max_upward_vy = 0.6435
        let px = [0.0f32, 5.0];
        let py = [80.0f32, 80.0];
        let pz = [0.0f32, 5.0];
        let vx = [3.0f32, -0.6];
        let vy = [1.8f32, -0.4];
        let vz = [4.0f32, 1.2];
        let is_gliding = [1i32, 1];

        let mut actions = [0i32; 2];
        let mut out_vx = [0.0f32; 2];
        let mut out_vy = [0.0f32; 2];
        let mut out_vz = [0.0f32; 2];

        let clamped = evaluate_player_surges(
            &px,
            &py,
            &pz,
            &vx,
            &vy,
            &vz,
            &is_gliding,
            3,
            1.8,
            &mut actions,
            &mut out_vx,
            &mut out_vy,
            &mut out_vz,
        );

        assert_eq!(clamped, 2);
        assert_eq!(actions[0], SURGE_CLAMP_VELOCITY);
        let new_horiz = (out_vx[0] * out_vx[0] + out_vz[0] * out_vz[0]).sqrt();
        assert!((new_horiz - 0.99).abs() < 1e-4);
        // Original 3:4 horizontal direction preserved.
        assert!((out_vx[0] / out_vz[0] - 0.75).abs() < 1e-4);
        assert!((out_vy[0] - 0.99 * 0.65).abs() < 1e-4);

        // Downward velocity -0.4 is safe and must not become more negative.
        assert_eq!(actions[1], SURGE_CLAMP_VELOCITY);
        assert!((out_vy[1] - (-0.4)).abs() < 1e-5);
        assert!(out_vy[1] >= vy[1]);
    }

    #[test]
    fn test_non_gliding_untouched_and_edge_cases() {
        let px = [0.0f32, 1.0];
        let py = [64.0f32, 64.0];
        let pz = [0.0f32, 1.0];
        let vx = [8.0f32, f32::NAN];
        let vy = [-2.0f32, f32::INFINITY];
        let vz = [6.0f32, 0.0];
        let is_gliding = [0i32, 1];

        let mut actions = [0i32; 2];
        let mut out_vx = [0.0f32; 2];
        let mut out_vy = [0.0f32; 2];
        let mut out_vz = [0.0f32; 2];

        let clamped = evaluate_player_surges(
            &px,
            &py,
            &pz,
            &vx,
            &vy,
            &vz,
            &is_gliding,
            4,
            f32::NAN,
            &mut actions,
            &mut out_vx,
            &mut out_vy,
            &mut out_vz,
        );

        assert_eq!(clamped, 0);
        // Non-gliding player untouched even under EMERGENCY state.
        assert_eq!(actions[0], SURGE_OK);
        assert!((out_vx[0] - 8.0).abs() < 1e-5);
        assert!((out_vy[0] - (-2.0)).abs() < 1e-5);
        assert!((out_vz[0] - 6.0).abs() < 1e-5);

        // Non-finite inputs safely sanitized.
        assert_eq!(actions[1], SURGE_OK);
        assert_eq!(out_vx[1], 0.0);
        assert_eq!(out_vy[1], 0.0);
        assert_eq!(out_vz[1], 0.0);

        // Empty batch returns 0.
        assert_eq!(
            evaluate_player_surges(
                &[],
                &[],
                &[],
                &[],
                &[],
                &[],
                &[],
                0,
                1.8,
                &mut [],
                &mut [],
                &mut [],
                &mut [],
            ),
            0
        );
    }
}
