//! Candidate query for the shadow tracking pilot. The JVM builds the same sparse chunk index
//! for both indexed backends. No visibility or server state is changed by this query.

const RANGE: f64 = 48.0;

pub(crate) fn table_slots(mobs: usize) -> usize {
    (2 * mobs).next_power_of_two()
}

fn chunk(x: f64) -> i32 {
    (x.floor() as i32) >> 4
}

fn key(x: i32, z: i32) -> u64 {
    (u64::from(x as u32) << 32) | u64::from(z as u32)
}

fn slot(key: u64, mask: usize) -> usize {
    let mixed = (key ^ (key >> 33)).wrapping_mul(0xff51afd7ed558ccd);
    (mixed ^ (mixed >> 33)) as usize & mask
}

pub(crate) fn query(
    players_xz: &[f64],
    mobs_xz: &[f64],
    table: &[i64],
    next_mob: &[i32],
    pairs: &mut [u64],
) -> Option<i32> {
    let mob_count = next_mob.len();
    let slots = table_slots(mob_count);
    let mask = slots - 1;
    let mut count = 0;
    pairs.fill(0);
    for (player, xz) in players_xz.chunks_exact(2).enumerate() {
        let (x, z) = (xz[0], xz[1]);
        for cx in chunk(x - RANGE)..=chunk(x + RANGE) {
            for cz in chunk(z - RANGE)..=chunk(z + RANGE) {
                let key = key(cx, cz);
                let mut bucket = slot(key, mask);
                for _ in 0..slots {
                    let mut mob = table[2 * bucket + 1];
                    if mob == -1 {
                        break;
                    }
                    if table[2 * bucket] as u64 != key {
                        bucket = (bucket + 1) & mask;
                        continue;
                    }
                    while mob != -1 {
                        let index = usize::try_from(mob).ok()?;
                        let next = i64::from(*next_mob.get(index)?);
                        if next < -1 || next >= mob {
                            return None;
                        }
                        let dx = x - mobs_xz[2 * index];
                        let dz = z - mobs_xz[2 * index + 1];
                        if dx * dx + dz * dz <= RANGE * RANGE {
                            let bit = player * mob_count + index;
                            pairs[bit / 64] |= 1 << (bit % 64);
                            count += 1;
                        }
                        mob = next;
                    }
                    break;
                }
            }
        }
    }
    Some(count)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn includes_exact_horizontal_boundary_across_negative_chunks() {
        let mobs = [-48.0, -0.25, -48.01, -0.25, 0.0, 48.0];
        let slots = table_slots(3);
        let mut table = vec![-1; 2 * slots];
        let mut next = [-1; 3];
        for (i, xz) in mobs.chunks_exact(2).enumerate() {
            let key = key(chunk(xz[0]), chunk(xz[1]));
            let mut bucket = slot(key, slots - 1);
            while table[2 * bucket + 1] != -1 && table[2 * bucket] as u64 != key {
                bucket = (bucket + 1) & (slots - 1);
            }
            next[i] = table[2 * bucket + 1] as i32;
            table[2 * bucket] = key as i64;
            table[2 * bucket + 1] = i as i64;
        }
        let mut pairs = [u64::MAX];
        assert_eq!(
            query(&[0.0, -0.25], &mobs, &table, &next, &mut pairs),
            Some(1)
        );
        assert_eq!(pairs, [0b001]);
        next[0] = 0;
        assert_eq!(query(&[0.0, -0.25], &mobs, &table, &next, &mut pairs), None);
    }
}
