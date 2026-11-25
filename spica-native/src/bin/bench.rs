//! Standalone Rust benchmark CLI (`cargo run --release --bin bench`).
//! Tests Spatial Hash Collisions, AI LOD Targeting, and Boids Steering at Minecraft scale.

use std::time::Instant;

use spica_rust_native::{
    farm_governor, governor, heatmap, init_thread_pool, kernels, run_in_pool,
};

fn lcg_next(state: &mut u64) -> f32 {
    *state = state
        .wrapping_mul(6364136223846793005)
        .wrapping_add(1442695040888963407);
    ((*state >> 33) as f32) / ((u32::MAX >> 1) as f32)
}

fn main() {
    let threads = init_thread_pool(0);
    let entities = 5_000usize;
    let players = 100usize;
    let iterations = 100usize;

    println!("=== Spica Native Rust Engine Standalone Benchmark ===");
    println!(
        "OS: {}-{} | Rayon Workers: {} | Entities: {} | Players: {} | Iterations: {}\n",
        std::env::consts::OS,
        std::env::consts::ARCH,
        threads,
        entities,
        players,
        iterations
    );

    let mut seed = 0x51CA_2026u64;
    let mut xs = Vec::with_capacity(entities);
    let mut ys = Vec::with_capacity(entities);
    let mut zs = Vec::with_capacity(entities);
    let mut vxs = Vec::with_capacity(entities);
    let mut vys = Vec::with_capacity(entities);
    let mut vzs = Vec::with_capacity(entities);
    let mut radii = Vec::with_capacity(entities);

    // Clustered entities (simulates mob farms + loaded chunks across 256x256 area)
    for i in 0..entities {
        let cluster = (i % 16) as f32;
        let cx = (cluster % 4.0) * 64.0;
        let cz = (cluster / 4.0).floor() * 64.0;
        xs.push(cx + lcg_next(&mut seed) * 18.0);
        ys.push(64.0 + lcg_next(&mut seed) * 6.0);
        zs.push(cz + lcg_next(&mut seed) * 18.0);
        vxs.push((lcg_next(&mut seed) - 0.5) * 0.4);
        vys.push(0.0);
        vzs.push((lcg_next(&mut seed) - 0.5) * 0.4);
        radii.push(0.45);
    }

    let mut pxs = Vec::with_capacity(players);
    let mut pys = Vec::with_capacity(players);
    let mut pzs = Vec::with_capacity(players);
    for _ in 0..players {
        pxs.push(lcg_next(&mut seed) * 256.0);
        pys.push(64.0);
        pzs.push(lcg_next(&mut seed) * 256.0);
    }

    let mut opx = vec![0.0f32; entities];
    let mut opy = vec![0.0f32; entities];
    let mut opz = vec![0.0f32; entities];
    let mut ocounts = vec![0i32; entities];

    // Warmup
    run_in_pool(|| {
        kernels::compute_entity_collisions(
            &xs, &ys, &zs, &radii, 1.5, 0.5, 0.4, &mut opx, &mut opy, &mut opz, &mut ocounts,
        );
    });

    // 1) Collisions
    let t0 = Instant::now();
    let mut total_pairs = 0i64;
    for _ in 0..iterations {
        total_pairs = run_in_pool(|| {
            kernels::compute_entity_collisions(
                &xs, &ys, &zs, &radii, 1.5, 0.5, 0.4, &mut opx, &mut opy, &mut opz, &mut ocounts,
            )
        });
    }
    let dt_coll = t0.elapsed().as_secs_f64() * 1000.0 / (iterations as f64);
    println!(
        "[1] Spatial Hash Collisions : {:.3} ms/tick (colliding pairs: {})",
        dt_coll, total_pairs
    );

    // 2) AI Targets + LOD
    let mut oidx = vec![0i32; entities];
    let mut od2 = vec![0.0f32; entities];
    let mut odx = vec![0.0f32; entities];
    let mut ody = vec![0.0f32; entities];
    let mut odz = vec![0.0f32; entities];
    let mut olod = vec![0i32; entities];

    let t1 = Instant::now();
    let mut full_active = 0i32;
    for _ in 0..iterations {
        full_active = run_in_pool(|| {
            kernels::compute_ai_targets(
                &xs, &ys, &zs, &pxs, &pys, &pzs, 32.0, 64.0, &mut oidx, &mut od2, &mut odx,
                &mut ody, &mut odz, &mut olod,
            )
        });
    }
    let dt_ai = t1.elapsed().as_secs_f64() * 1000.0 / (iterations as f64);
    println!(
        "[2] AI Nearest Target + LOD : {:.3} ms/tick (LOD_FULL mobs: {})",
        dt_ai, full_active
    );

    // 3) Boids Steering
    let mut oax = vec![0.0f32; entities];
    let mut oay = vec![0.0f32; entities];
    let mut oaz = vec![0.0f32; entities];

    let t2 = Instant::now();
    for _ in 0..iterations {
        run_in_pool(|| {
            kernels::compute_boids_steering(
                &xs, &ys, &zs, &vxs, &vys, &vzs, 128.0, 64.0, 128.0, 5.0, 1.2, 0.6, 0.4, 1.0,
                0.5, &mut oax, &mut oay, &mut oaz,
            );
        });
    }
    let dt_boids = t2.elapsed().as_secs_f64() * 1000.0 / (iterations as f64);
    println!(
        "[3] Custom Boids Steering   : {:.3} ms/tick",
        dt_boids
    );

    // 4) Chunk Density & Crowding Heatmap
    let max_hot = 16usize;
    let mut hot_cx = vec![0i32; max_hot];
    let mut hot_cz = vec![0i32; max_hot];
    let mut hot_ec = vec![0i32; max_hot];
    let mut hot_cc = vec![0i32; max_hot];

    let t3 = Instant::now();
    let mut hot_found = 0i32;
    for _ in 0..iterations {
        hot_found = heatmap::analyze_chunks(
            &xs,
            &zs,
            &ocounts,
            max_hot,
            &mut hot_cx,
            &mut hot_cz,
            &mut hot_ec,
            &mut hot_cc,
        );
    }
    let dt_heatmap = t3.elapsed().as_secs_f64() * 1000.0 / (iterations as f64);
    println!(
        "[4] Chunk Density Heatmap   : {:.3} ms/tick (top chunks: {}, hottest: ({},{}) ec={} crowded={})",
        dt_heatmap, hot_found, hot_cx[0], hot_cz[0], hot_ec[0], hot_cc[0]
    );

    // 5) Dropped-Item Stack Merge Planner
    let items = 2_000usize;
    let mut ixs = Vec::with_capacity(items);
    let mut iys = Vec::with_capacity(items);
    let mut izs = Vec::with_capacity(items);
    let mut itypes = Vec::with_capacity(items);
    let mut iamounts = Vec::with_capacity(items);
    for i in 0..items {
        let cluster = (i % 16) as f32;
        let cx = (cluster % 4.0) * 64.0;
        let cz = (cluster / 4.0).floor() * 64.0;
        ixs.push(cx + lcg_next(&mut seed) * 10.0);
        iys.push(64.0 + lcg_next(&mut seed) * 2.0);
        izs.push(cz + lcg_next(&mut seed) * 10.0);
        itypes.push((i % 24) as i32);
        iamounts.push(1 + (i % 16) as i32);
    }
    let mut merge_targets = vec![0i32; items];
    let mut merge_sources = vec![0i32; items];

    let t4 = Instant::now();
    let mut merge_pairs = 0i32;
    for _ in 0..iterations {
        merge_pairs = kernels::plan_item_merges(
            &ixs,
            &iys,
            &izs,
            &itypes,
            &iamounts,
            64,
            3.5,
            &mut merge_targets,
            &mut merge_sources,
        );
    }
    let dt_merge = t4.elapsed().as_secs_f64() * 1000.0 / (iterations as f64);
    println!(
        "[5] Item Merge Planner (2k) : {:.3} ms/tick (merge pairs: {})",
        dt_merge, merge_pairs
    );

    // 6) Adaptive MSPT/TPS Governor
    let gov_iters = 100_000usize;
    let t5 = Instant::now();
    let mut ewma = 0.0f32;
    let mut last_state = 0i32;
    for i in 0..gov_iters {
        let sample_mspt = 22.0 + ((i % 40) as f32);
        let out = governor::evaluate_governor(
            sample_mspt,
            ewma,
            entities as i32,
            players as i32,
            32.0,
            64.0,
            4,
            10,
            3,
            8,
        );
        ewma = out.ewma_mspt;
        last_state = out.state_code;
    }
    let dt_gov_ms = t5.elapsed().as_secs_f64() * 1000.0 / (gov_iters as f64);
    println!(
        "[6] Adaptive MSPT Governor  : {:.6} ms/eval ({:.1} ns, final state={}, ewma={:.2}ms)",
        dt_gov_ms,
        dt_gov_ms * 1_000_000.0,
        last_state,
        ewma
    );

    // 7) Farm & Villager Governor
    let mut entity_flags = Vec::with_capacity(entities);
    for i in 0..entities {
        let flag = if i % 20 == 0 {
            farm_governor::FLAG_PROTECTED
        } else if i % 10 == 1 {
            farm_governor::FLAG_VILLAGER | farm_governor::FLAG_TRAPPED_BOX
        } else if i % 10 == 2 {
            farm_governor::FLAG_VILLAGER
        } else if i % 50 == 3 {
            farm_governor::FLAG_RECENT_INTERACT
        } else {
            0
        };
        entity_flags.push(flag);
    }
    let mut farm_actions = vec![0i32; entities];
    let mut farm_neighbors = vec![0i32; entities];

    let t6 = Instant::now();
    let mut throttled_count = 0i32;
    for _ in 0..iterations {
        throttled_count = run_in_pool(|| {
            farm_governor::evaluate_farm_throttling(
                &xs,
                &ys,
                &zs,
                &entity_flags,
                &pxs,
                &pys,
                &pzs,
                3.0,
                6,
                14,
                48,
                12.0,
                4,
                last_state,
                &mut farm_actions,
                &mut farm_neighbors,
            )
        });
    }
    let dt_farm = t6.elapsed().as_secs_f64() * 1000.0 / (iterations as f64);
    println!(
        "[7] Farm & Villager Governor: {:.3} ms/tick (throttled: {}/{})",
        dt_farm, throttled_count, entities
    );

    let total_ms = dt_coll + dt_ai + dt_boids + dt_heatmap + dt_merge + dt_gov_ms + dt_farm;
    println!(
        "\nTotal combined budget: {:.3} ms / 50.000 ms tick ({:.2}% of 1 tick)",
        total_ms,
        total_ms / 50.0 * 100.0
    );
}
