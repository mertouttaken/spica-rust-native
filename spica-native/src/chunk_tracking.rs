//! Candidate query for the shadow tracking pilot. The JVM builds the same sparse chunk index
//! for both indexed backends. No visibility or server state is changed by this query.

use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard};

const RANGE: f64 = 48.0;
pub(crate) const MAX_MOBS: usize = 16_384;
pub(crate) const MAX_PLAYERS: usize = 64;
const MAX_OPEN_INDEXES: usize = 64;

static INDEXES: Mutex<Indexes> = Mutex::new(Indexes {
    next_handle: 1,
    worlds: Vec::new(),
});

struct Indexes {
    next_handle: i64,
    worlds: Vec<(i64, TrackingIndex)>,
}

fn indexes() -> MutexGuard<'static, Indexes> {
    // Each batch validates its input before changing an index. A panicking batch is fatal to
    // that probe, but its cleanup and unrelated indexes must remain available.
    INDEXES.lock().unwrap_or_else(|error| error.into_inner())
}

pub(crate) fn open_index() -> Option<i64> {
    let mut indexes = indexes();
    if indexes.worlds.len() >= MAX_OPEN_INDEXES {
        return None;
    }
    let handle = indexes.next_handle;
    indexes.next_handle = handle.checked_add(1)?;
    indexes.worlds.push((handle, TrackingIndex::default()));
    Some(handle)
}

pub(crate) fn close_index(handle: i64) {
    let mut indexes = indexes();
    if let Some(index) = indexes.worlds.iter().position(|(id, _)| *id == handle) {
        indexes.worlds.swap_remove(index);
    }
}

pub(crate) fn update_index(
    handle: i64,
    slot_count: usize,
    deltas: &[u64],
    players_xz: &[f64],
    pairs: &mut [u64],
) -> Option<i32> {
    let mut indexes = indexes();
    let (_, index) = indexes.worlds.iter_mut().find(|(id, _)| *id == handle)?;
    index.update_and_query(slot_count, deltas, players_xz, pairs)
}

#[derive(Clone, Copy)]
struct Resident {
    key: u64,
    offset: usize,
    x: f64,
    z: f64,
}

#[derive(Default)]
struct TrackingIndex {
    residents: Vec<Option<Resident>>,
    chunks: HashMap<u64, Vec<usize>>,
}

impl TrackingIndex {
    fn update_and_query(
        &mut self,
        slot_count: usize,
        deltas: &[u64],
        players_xz: &[f64],
        pairs: &mut [u64],
    ) -> Option<i32> {
        if slot_count < self.residents.len()
            || !deltas.chunks_exact(3).all(|row| {
                let token = row[0] as i64;
                let slot = if token < 0 { !token } else { token };
                slot < slot_count as i64
                    && (token < 0
                        || (f64::from_bits(row[1]).is_finite()
                            && f64::from_bits(row[2]).is_finite()))
            })
        {
            return None;
        }
        self.residents.resize(slot_count, None);
        for row in deltas.chunks_exact(3) {
            let token = row[0] as i64;
            if token < 0 {
                self.remove((!token) as usize);
            } else {
                self.upsert(
                    token as usize,
                    f64::from_bits(row[1]),
                    f64::from_bits(row[2]),
                );
            }
        }
        Some(self.query(players_xz, pairs))
    }

    fn remove(&mut self, slot: usize) {
        if let Some(old) = self.residents[slot].take() {
            let bucket = self.chunks.get_mut(&old.key).expect("resident bucket");
            bucket.swap_remove(old.offset);
            if let Some(&swapped) = bucket.get(old.offset) {
                self.residents[swapped]
                    .as_mut()
                    .expect("bucket resident")
                    .offset = old.offset;
            }
            if bucket.is_empty() {
                self.chunks.remove(&old.key);
            }
        }
    }

    fn upsert(&mut self, slot: usize, x: f64, z: f64) {
        let key = key(chunk(x), chunk(z));
        if let Some(resident) = self.residents[slot].as_mut() {
            if resident.key == key {
                resident.x = x;
                resident.z = z;
                return;
            }
            self.remove(slot);
        }
        let bucket = self.chunks.entry(key).or_default();
        self.residents[slot] = Some(Resident {
            key,
            offset: bucket.len(),
            x,
            z,
        });
        bucket.push(slot);
    }

    fn query(&self, players_xz: &[f64], pairs: &mut [u64]) -> i32 {
        let mut count = 0;
        pairs.fill(0);
        for (player, xz) in players_xz.chunks_exact(2).enumerate() {
            let (x, z) = (xz[0], xz[1]);
            for cx in chunk(x - RANGE)..=chunk(x + RANGE) {
                for cz in chunk(z - RANGE)..=chunk(z + RANGE) {
                    if let Some(bucket) = self.chunks.get(&key(cx, cz)) {
                        for &slot in bucket {
                            let resident = self.residents[slot].expect("bucket resident");
                            let dx = x - resident.x;
                            let dz = z - resident.z;
                            if dx * dx + dz * dz <= RANGE * RANGE {
                                let bit = player * self.residents.len() + slot;
                                pairs[bit / 64] |= 1 << (bit % 64);
                                count += 1;
                            }
                        }
                    }
                }
            }
        }
        count
    }
}

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
    fn closed_tracking_handle_cannot_read_a_reopened_index() {
        let first = open_index().unwrap();
        assert_eq!(update_index(first, 0, &[], &[], &mut []), Some(0));
        close_index(first);
        close_index(first);

        let second = open_index().unwrap();
        assert_ne!(first, second);
        assert_eq!(update_index(first, 0, &[], &[], &mut []), None);
        assert_eq!(update_index(second, 0, &[], &[], &mut []), Some(0));
        close_index(second);
    }

    #[test]
    fn incremental_moves_removes_reuses_and_rejects_a_whole_invalid_batch() {
        fn row(slot: i64, x: f64, z: f64) -> [u64; 3] {
            [slot as u64, x.to_bits(), z.to_bits()]
        }

        let mut index = TrackingIndex::default();
        let players = [0.0, 0.0, -48.0, 0.0];
        let mut pairs = [0u64];
        let seed = [row(0, 0.0, 0.0), row(1, 1.0, 0.0), row(2, 2.0, 0.0)].concat();
        assert_eq!(
            index.update_and_query(3, &seed, &players, &mut pairs),
            Some(4)
        );
        assert_eq!(pairs, [0b001_111]);

        // Move one entry across a chunk boundary, then remove the entry swapped into its slot.
        let changes = [row(0, -48.0, 0.0), row(!2, 0.0, 0.0), row(1, 2.0, 0.0)].concat();
        assert_eq!(
            index.update_and_query(3, &changes, &players, &mut pairs),
            Some(3)
        );
        assert_eq!(pairs, [0b001_011]);

        let invalid = [row(!0, 0.0, 0.0), row(3, 0.0, 0.0)].concat();
        assert_eq!(
            index.update_and_query(3, &invalid, &players, &mut pairs),
            None
        );
        assert_eq!(
            index.update_and_query(3, &[], &players, &mut pairs),
            Some(3)
        );
        assert_eq!(pairs, [0b001_011]);

        let reuse = [row(2, -48.01, 0.0), row(!0, 0.0, 0.0)].concat();
        assert_eq!(
            index.update_and_query(3, &reuse, &players, &mut pairs),
            Some(2)
        );
        assert_eq!(pairs, [0b100_010]);
    }

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
