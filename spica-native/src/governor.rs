//! Adaptive MSPT/TPS Governor ("The Brain") for Purpur/Paper 1.21.4.
//!
//! Computes an exponentially weighted moving average (EWMA) of server tick time (MSPT),
//! factors in loaded-entity pressure, and outputs smooth, clamped distance, AI LOD,
//! and dropped-item merging parameters across five health states.

pub const STATE_GREEN: i32 = 0;
pub const STATE_YELLOW: i32 = 1;
pub const STATE_ORANGE: i32 = 2;
pub const STATE_RED: i32 = 3;
pub const STATE_EMERGENCY: i32 = 4;

const EWMA_ALPHA: f32 = 0.25;
const ENTITY_PRESSURE_THRESHOLD: i32 = 4500;

/// Governor decision bundle returned each evaluation cycle.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GovernorOutput {
    pub state_code: i32,
    pub view_dist: i32,
    pub sim_dist: i32,
    pub enable_item_merge: i32,
    pub full_ai_range: f32,
    pub hibernate_range: f32,
    pub merge_radius: f32,
    pub ewma_mspt: f32,
}

/// Evaluate server health and compute adaptive tuning parameters.
///
/// - Updates `ewma_mspt` with `alpha = 0.25` (initializing to `current_mspt` when `ewma_mspt <= 0.0`).
/// - Classifies `ewma_mspt` into `GREEN`/`YELLOW`/`ORANGE`/`RED`/`EMERGENCY`, with a `+1`
///   entity-pressure boost (capped at `EMERGENCY`) when `loaded_entities > 4500`.
/// - Clamps view/simulation distances to `[min, max]` and AI ranges to `full_ai_range >= 10.0`,
///   `hibernate_range >= full_ai_range + 6.0`.
#[allow(clippy::too_many_arguments)]
pub fn evaluate_governor(
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
) -> GovernorOutput {
    let _ = online_players;

    let safe_current = if current_mspt.is_finite() {
        current_mspt.max(0.0)
    } else {
        0.0
    };
    let safe_prev_ewma = if ewma_mspt.is_finite() {
        ewma_mspt
    } else {
        0.0
    };

    let updated_ewma = if safe_prev_ewma <= 0.0 {
        safe_current
    } else {
        EWMA_ALPHA * safe_current + (1.0 - EWMA_ALPHA) * safe_prev_ewma
    };

    let mut state_code = if updated_ewma < 28.0 {
        STATE_GREEN
    } else if updated_ewma < 38.0 {
        STATE_YELLOW
    } else if updated_ewma < 45.0 {
        STATE_ORANGE
    } else if updated_ewma < 52.0 {
        STATE_RED
    } else {
        STATE_EMERGENCY
    };

    if loaded_entities > ENTITY_PRESSURE_THRESHOLD {
        state_code = (state_code + 1).min(STATE_EMERGENCY);
    }

    let lo_view = min_view_dist.min(max_view_dist);
    let hi_view = min_view_dist.max(max_view_dist);
    let lo_sim = min_sim_dist.min(max_sim_dist);
    let hi_sim = min_sim_dist.max(max_sim_dist);

    let (view_dist, sim_dist, ai_scale, enable_item_merge, merge_radius) = match state_code {
        STATE_GREEN => (hi_view, hi_sim, 1.0f32, 0, 2.5f32),
        STATE_YELLOW => (
            hi_view.saturating_sub(1).clamp(lo_view, hi_view),
            hi_sim.saturating_sub(1).clamp(lo_sim, hi_sim),
            0.85f32,
            1,
            3.0f32,
        ),
        STATE_ORANGE => (
            hi_view.saturating_sub(2).clamp(lo_view, hi_view),
            hi_sim.saturating_sub(2).clamp(lo_sim, hi_sim),
            0.70f32,
            1,
            3.5f32,
        ),
        STATE_RED => (
            lo_view.saturating_add(1).clamp(lo_view, hi_view),
            lo_sim,
            0.55f32,
            1,
            4.5f32,
        ),
        _ => (lo_view, lo_sim, 0.40f32, 1, 5.5f32),
    };

    let raw_full = if base_full_range.is_finite() {
        base_full_range.max(0.0)
    } else {
        32.0
    };
    let raw_hib = if base_hibernate_range.is_finite() {
        base_hibernate_range.max(0.0)
    } else {
        64.0
    };

    let full_ai_range = (raw_full * ai_scale).max(10.0);
    let hibernate_range = (raw_hib * ai_scale).max(full_ai_range + 6.0);

    GovernorOutput {
        state_code,
        view_dist,
        sim_dist,
        enable_item_merge,
        full_ai_range,
        hibernate_range,
        merge_radius,
        ewma_mspt: updated_ewma,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ewma_initialization_and_update() {
        let first = evaluate_governor(20.0, 0.0, 1000, 10, 32.0, 64.0, 4, 10, 3, 8);
        assert!((first.ewma_mspt - 20.0).abs() < 1e-5);
        assert_eq!(first.state_code, STATE_GREEN);
        assert_eq!(first.view_dist, 10);
        assert_eq!(first.sim_dist, 8);
        assert_eq!(first.enable_item_merge, 0);
        assert!((first.merge_radius - 2.5).abs() < 1e-5);

        let second = evaluate_governor(40.0, first.ewma_mspt, 1000, 10, 32.0, 64.0, 4, 10, 3, 8);
        // 0.25 * 40 + 0.75 * 20 = 25.0
        assert!((second.ewma_mspt - 25.0).abs() < 1e-5);
        assert_eq!(second.state_code, STATE_GREEN);
    }

    #[test]
    fn test_all_state_thresholds() {
        let yellow = evaluate_governor(30.0, 30.0, 500, 5, 32.0, 64.0, 4, 10, 3, 8);
        assert_eq!(yellow.state_code, STATE_YELLOW);
        assert_eq!(yellow.view_dist, 9);
        assert_eq!(yellow.sim_dist, 7);
        assert_eq!(yellow.enable_item_merge, 1);
        assert!((yellow.merge_radius - 3.0).abs() < 1e-5);
        assert!((yellow.full_ai_range - 27.2).abs() < 1e-4);

        let orange = evaluate_governor(40.0, 40.0, 500, 5, 32.0, 64.0, 4, 10, 3, 8);
        assert_eq!(orange.state_code, STATE_ORANGE);
        assert_eq!(orange.view_dist, 8);
        assert_eq!(orange.sim_dist, 6);
        assert_eq!(orange.enable_item_merge, 1);
        assert!((orange.merge_radius - 3.5).abs() < 1e-5);

        let red = evaluate_governor(48.0, 48.0, 500, 5, 32.0, 64.0, 4, 10, 3, 8);
        assert_eq!(red.state_code, STATE_RED);
        assert_eq!(red.view_dist, 5);
        assert_eq!(red.sim_dist, 3);
        assert_eq!(red.enable_item_merge, 1);
        assert!((red.merge_radius - 4.5).abs() < 1e-5);

        let emergency = evaluate_governor(55.0, 55.0, 500, 5, 32.0, 64.0, 4, 10, 3, 8);
        assert_eq!(emergency.state_code, STATE_EMERGENCY);
        assert_eq!(emergency.view_dist, 4);
        assert_eq!(emergency.sim_dist, 3);
        assert_eq!(emergency.enable_item_merge, 1);
        assert!((emergency.merge_radius - 5.5).abs() < 1e-5);
    }

    #[test]
    fn test_entity_pressure_boost_and_clamping() {
        // Low MSPT but > 4500 entities boosts GREEN -> YELLOW.
        let boosted = evaluate_governor(18.0, 18.0, 5000, 20, 16.0, 20.0, 5, 5, 4, 4);
        assert_eq!(boosted.state_code, STATE_YELLOW);
        assert_eq!(boosted.view_dist, 5);
        assert_eq!(boosted.sim_dist, 4);
        assert_eq!(boosted.enable_item_merge, 1);

        // EMERGENCY with high entity count stays capped at EMERGENCY, and AI ranges clamp properly.
        let capped = evaluate_governor(60.0, 60.0, 9000, 20, 16.0, 20.0, 4, 4, 3, 3);
        assert_eq!(capped.state_code, STATE_EMERGENCY);
        assert_eq!(capped.view_dist, 4);
        assert_eq!(capped.sim_dist, 3);
        assert!((capped.full_ai_range - 10.0).abs() < 1e-5);
        assert!((capped.hibernate_range - 16.0).abs() < 1e-5);
    }
}
