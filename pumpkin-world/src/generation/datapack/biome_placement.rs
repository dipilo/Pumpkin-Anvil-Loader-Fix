//! Runtime datapack biome placement
use pumpkin_data::chunk::Biome;
use tracing::debug;

use super::codec::biome_source::BiomeEntry;
use crate::chunk::dynamic_biome::DYNAMIC_BIOMES;

/// A datapack-driven multi-noise biome placement
pub struct DatapackBiomeSupplier {
    entries: Vec<PlacementEntry>,
}

struct PlacementEntry {
    /// Quantized inclusive ranges per axis, in the order
    /// `NoiseValuePoint::convert_to_list` produces
    ranges: [(i64, i64); 7],
    biome_id: u8,
}

impl DatapackBiomeSupplier {
    /// Build from a parsed inline `biome_source.biomes` list
    #[must_use]
    pub fn from_entries(entries: &[BiomeEntry]) -> Option<Self> {
        let registry = DYNAMIC_BIOMES.read().unwrap();
        let mut out = Vec::with_capacity(entries.len());
        let mut unresolved = 0usize;
        for entry in entries {
            let Some(biome_id) = resolve_biome_id(&entry.biome, |name| registry.lookup(name)) else {
                unresolved += 1;
                continue;
            };
            let mut ranges = [(0i64, 0i64); 7];
            for (axis, param) in entry.parameters.axes().iter().enumerate() {
                ranges[axis] = param.quantized();
            }
            out.push(PlacementEntry { ranges, biome_id });
        }
        if unresolved > 0 {
            debug!("Datapack biome placement: skipped {unresolved} entry/entries with unresolvable biomes");
        }
        if out.is_empty() {
            None
        } else {
            Some(Self { entries: out })
        }
    }

    /// Nearest biome id for a climate point (`[i64;7]` from `NoiseValuePoint::convert_to_list`), 
    /// using vanilla's sum-of-squared per-axis distance
    #[must_use]
    pub fn biome_id(&self, point: &[i64; 7]) -> u8 {
        let mut best_id = self.entries[0].biome_id;
        let mut best_dist = i64::MAX;
        for entry in &self.entries {
            let mut dist = 0i64;
            for axis in 0..7 {
                let (lo, hi) = entry.ranges[axis];
                let p = point[axis];
                let d = if p > hi {
                    p - hi
                } else if p < lo {
                    lo - p
                } else {
                    0
                };
                dist += d * d;
                if dist >= best_dist {
                    break;
                }
            }
            if dist < best_dist {
                best_dist = dist;
                best_id = entry.biome_id;
            }
        }
        best_id
    }

    /// Diagnostics for parity analysis: exact (no early-break) `(best_id,
    /// best_dist, min_dist_to `target`)` for a climate point. Used by the
    /// golden-chunk harness to tell tie-breaking apart from a genuine climate
    /// difference.
    #[must_use]
    pub fn debug_nearest(&self, point: &[i64; 7], target: u8) -> (u8, i64, i64) {
        let mut best_id = self.entries[0].biome_id;
        let mut best_dist = i64::MAX;
        let mut target_dist = i64::MAX;
        for entry in &self.entries {
            let mut dist = 0i64;
            for axis in 0..7 {
                let (lo, hi) = entry.ranges[axis];
                let p = point[axis];
                let d = if p > hi {
                    p - hi
                } else if p < lo {
                    lo - p
                } else {
                    0
                };
                dist += d * d;
            }
            if dist < best_dist {
                best_dist = dist;
                best_id = entry.biome_id;
            }
            if entry.biome_id == target && dist < target_dist {
                target_dist = dist;
            }
        }
        (best_id, best_dist, target_dist)
    }

    /// Diagnostics: per-axis `dist²` gap between the best `target`-biome entry and
    /// the overall best entry for `point`. Sums to `plains_dist - best_dist`; the
    /// axis with the largest value is what makes `target` lose.
    #[must_use]
    pub fn debug_axis_gap(&self, point: &[i64; 7], target: u8) -> [i64; 7] {
        let axis_dist = |entry: &PlacementEntry| -> ([i64; 7], i64) {
            let mut a = [0i64; 7];
            for axis in 0..7 {
                let (lo, hi) = entry.ranges[axis];
                let p = point[axis];
                let d = if p > hi {
                    p - hi
                } else if p < lo {
                    lo - p
                } else {
                    0
                };
                a[axis] = d * d;
            }
            let total = a.iter().sum();
            (a, total)
        };
        let mut best_total = i64::MAX;
        let mut best_axes = [0i64; 7];
        let mut tbest_total = i64::MAX;
        let mut tbest_axes = [0i64; 7];
        for entry in &self.entries {
            let (a, total) = axis_dist(entry);
            if total < best_total {
                best_total = total;
                best_axes = a;
            }
            if entry.biome_id == target && total < tbest_total {
                tbest_total = total;
                tbest_axes = a;
            }
        }
        let mut gap = [0i64; 7];
        for i in 0..7 {
            gap[i] = tbest_axes[i] - best_axes[i];
        }
        gap
    }

    /// Diagnostics: the quantized ranges of the nearest entry for `biome` at
    /// `point` (or `None` if that biome has no entry).
    #[must_use]
    pub fn debug_nearest_ranges(&self, point: &[i64; 7], biome: u8) -> Option<[(i64, i64); 7]> {
        let mut best: Option<([(i64, i64); 7], i64)> = None;
        for entry in &self.entries {
            if entry.biome_id != biome {
                continue;
            }
            let mut dist = 0i64;
            for axis in 0..7 {
                let (lo, hi) = entry.ranges[axis];
                let p = point[axis];
                let d = if p > hi {
                    p - hi
                } else if p < lo {
                    lo - p
                } else {
                    0
                };
                dist += d * d;
            }
            if best.is_none_or(|(_, bd)| dist < bd) {
                best = Some((entry.ranges, dist));
            }
        }
        best.map(|(r, _)| r)
    }

    /// Number of placement entries that resolved to a biome id
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// Resolve a biome resource location to a `u8` id
/// which is keyed by the fully-qualified name (`terralith:...`)
fn resolve_biome_id(name: &str, dynamic: impl Fn(&str) -> Option<u8>) -> Option<u8> {
    let stripped = name.strip_prefix("minecraft:").unwrap_or(name);
    if let Some(biome) = Biome::from_name(stripped) {
        return Some(biome.id);
    }
    dynamic(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_vanilla_biome_by_name() {
        // Vanilla resolves regardless of the dynamic registry
        let id = resolve_biome_id("minecraft:plains", |_| None);
        assert_eq!(id, Some(Biome::PLAINS.id));
        assert_eq!(resolve_biome_id("plains", |_| None), Some(Biome::PLAINS.id));
    }

    #[test]
    fn falls_back_to_dynamic_registry_for_modded() {
        // Modded name comes from the dynamic lookup
        assert_eq!(resolve_biome_id("terralith:mirage_isles", |_| None), None);
        assert_eq!(
            resolve_biome_id("terralith:mirage_isles", |n| (n == "terralith:mirage_isles")
                .then_some(138)),
            Some(138)
        );
    }

    #[test]
    fn nearest_uses_squared_distance() {
        // Two entries; a point inside entry B's box must pick B (distance 0)
        let supplier = DatapackBiomeSupplier {
            entries: vec![
                PlacementEntry {
                    ranges: [(-100, 100); 7],
                    biome_id: 1,
                },
                PlacementEntry {
                    ranges: [(1000, 2000); 7],
                    biome_id: 2,
                },
            ],
        };
        // point at 1500 on every axis is inside entry B
        assert_eq!(supplier.biome_id(&[1500; 7]), 2);
        // point at 0 is inside entry A
        assert_eq!(supplier.biome_id(&[0; 7]), 1);
        // point at 500 is closer to A's box (dist 400/axis) than B's (500/axis)
        assert_eq!(supplier.biome_id(&[500; 7]), 1);
    }
}
