//! Global per-step feature ordering

use std::collections::{BTreeMap, BTreeSet, HashMap};

use tracing::warn;

use crate::generation::feature::placed_features::PlacedFeature;

fn ptr_of(pf: &'static PlacedFeature) -> usize {
    std::ptr::from_ref::<PlacedFeature>(pf) as usize
}

/// A vertex in the feature-ordering graph
#[derive(Clone, Copy)]
struct FeatureData {
    feature_index: usize,
    step: usize,
    feature: &'static PlacedFeature,
}

impl PartialEq for FeatureData {
    fn eq(&self, other: &Self) -> bool {
        self.step == other.step && self.feature_index == other.feature_index
    }
}
impl Eq for FeatureData {}
impl Ord for FeatureData {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.step
            .cmp(&other.step)
            .then(self.feature_index.cmp(&other.feature_index))
    }
}
impl PartialOrd for FeatureData {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

/// Global, per-step feature ordering for a biome source
pub struct IndexedFeatures {
    /// per step → global ordered list of placed features
    steps: Vec<Vec<&'static PlacedFeature>>,
    /// per step → (feature pointer → index within `steps[step]`)
    index_of: Vec<HashMap<usize, usize>>,
    /// biome id → its own step-indexed feature lists
    biome_features: HashMap<u8, Vec<Vec<&'static PlacedFeature>>>,
}

impl IndexedFeatures {
    /// Number of generation steps
    #[must_use]
    pub const fn num_steps(&self) -> usize {
        self.steps.len()
    }

    /// Placed feature at `index` within `step`'s global ordered list
    #[must_use]
    pub fn feature_at(&self, step: usize, index: usize) -> Option<&'static PlacedFeature> {
        self.steps.get(step).and_then(|s| s.get(index)).copied()
    }

    /// Global index of a feature within `step`'s ordered list
    #[must_use]
    pub fn index_of(&self, step: usize, feature_ptr: usize) -> Option<usize> {
        self.index_of.get(step).and_then(|m| m.get(&feature_ptr).copied())
    }

    /// Global indices of features to run at `step` for a chunk containing `biome_ids`
    #[must_use]
    pub fn step_feature_indices(&self, biome_ids: &[u8], step: usize) -> Vec<usize> {
        let mut indices: BTreeSet<usize> = BTreeSet::new();
        for &biome_id in biome_ids {
            if let Some(steps) = self.biome_features.get(&biome_id)
                && let Some(list) = steps.get(step)
            {
                for &pf in list {
                    if let Some(global_index) = self.index_of(step, ptr_of(pf)) {
                        indices.insert(global_index);
                    }
                }
            }
        }
        indices.into_iter().collect()
    }

    /// Port of `FeatureSorter.buildFeaturesPerStep`
    #[must_use]
    pub fn build(sources: &[(u8, Vec<Vec<&'static PlacedFeature>>)]) -> Self {
        // feature pointer → global encounter-order index
        let mut feature_index: HashMap<usize, usize> = HashMap::new();
        let mut next_index: usize = 0;
        let mut max_step = 0usize;

        // Adjacency built from consecutive features within each biome
        let mut edges: BTreeMap<FeatureData, BTreeSet<FeatureData>> = BTreeMap::new();

        for (_biome_id, source) in sources {
            max_step = max_step.max(source.len());

            let mut feature_list: Vec<FeatureData> = Vec::new();
            for (step, feats) in source.iter().enumerate() {
                for &pf in feats {
                    let ptr = ptr_of(pf);
                    let idx = *feature_index.entry(ptr).or_insert_with(|| {
                        let i = next_index;
                        next_index += 1;
                        i
                    });
                    feature_list.push(FeatureData {
                        feature_index: idx,
                        step,
                        feature: pf,
                    });
                }
            }

            for i in 0..feature_list.len() {
                let node = feature_list[i];
                let entry = edges.entry(node).or_default();
                if i + 1 < feature_list.len() {
                    entry.insert(feature_list[i + 1]);
                }
            }
        }

        let sorted = Self::topological_sort(&edges);

        let mut steps: Vec<Vec<&'static PlacedFeature>> = vec![Vec::new(); max_step];
        for fd in &sorted {
            steps[fd.step].push(fd.feature);
        }
        let index_of: Vec<HashMap<usize, usize>> = steps
            .iter()
            .map(|list| {
                list.iter()
                    .enumerate()
                    .map(|(i, &pf)| (ptr_of(pf), i))
                    .collect()
            })
            .collect();

        let biome_features: HashMap<u8, Vec<Vec<&'static PlacedFeature>>> =
            sources.iter().cloned().collect();

        Self {
            steps,
            index_of,
            biome_features,
        }
    }

    /// Reverse post-order DFS matching vanilla `Graph.depthFirstSearch`
    fn topological_sort(edges: &BTreeMap<FeatureData, BTreeSet<FeatureData>>) -> Vec<FeatureData> {
        let mut discovered: BTreeSet<FeatureData> = BTreeSet::new();
        let mut visiting: BTreeSet<FeatureData> = BTreeSet::new();
        let mut reverse_topo: Vec<FeatureData> = Vec::new();
        let mut warned_cycle = false;

        for &start in edges.keys() {
            if discovered.contains(&start) {
                continue;
            }
            // Explicit DFS stack
            let children_of = |node: &FeatureData| -> Vec<FeatureData> {
                edges
                    .get(node)
                    .map(|set| set.iter().copied().collect())
                    .unwrap_or_default()
            };
            visiting.insert(start);
            let mut stack: Vec<(FeatureData, Vec<FeatureData>, usize)> =
                vec![(start, children_of(&start), 0)];

            while let Some((node, children, idx)) = stack.last_mut() {
                if *idx < children.len() {
                    let next = children[*idx];
                    *idx += 1;
                    if discovered.contains(&next) {
                        continue;
                    }
                    if visiting.contains(&next) {
                        if !warned_cycle {
                            warn!(
                                "Feature order cycle detected; feature placement order may \
                                 differ from vanilla for this pack"
                            );
                            warned_cycle = true;
                        }
                        continue; // skip the back edge to break the cycle
                    }
                    visiting.insert(next);
                    let next_children = children_of(&next);
                    stack.push((next, next_children, 0));
                } else {
                    let node = *node;
                    visiting.remove(&node);
                    discovered.insert(node);
                    reverse_topo.push(node);
                    stack.pop();
                }
            }
        }

        reverse_topo.reverse();
        reverse_topo
    }
}
