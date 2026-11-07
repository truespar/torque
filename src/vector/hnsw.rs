use crate::schema::{DocId, VecDist};
use rayon::prelude::*;
use crate::bitmap::Bitmap;
use crate::tqbf::bytes::{Bytes, Pods};
use serde::{Deserialize, Serialize};
use std::collections::BinaryHeap;
use std::cmp::Reverse;
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::sync::RwLock;

/// HNSW (Hierarchical Navigable Small World) graph for approximate nearest neighbor search.
#[derive(Clone, Serialize, Deserialize)]
pub struct HnswGraph {
    pub(crate) layers: Vec<HnswLayer>,
    pub(crate) entry_point: Option<u32>,
    pub(crate) max_level: usize,
    pub(crate) m: usize,
    pub(crate) m_max0: usize,
    pub(crate) ef_construction: usize,
}

/// Flat contiguous neighbor storage — cache-friendly, no per-node heap alloc.
#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct HnswLayer {
    /// Flat neighbor IDs: [node0_nb0, node0_nb1, ..., node1_nb0, ...]
    pub(crate) data: Vec<u32>,
    /// Actual neighbor count per node.
    pub(crate) counts: Vec<u16>,
    /// Capacity per slot (layer 0 = m_max0, upper layers = m).
    pub(crate) stride: usize,
}

impl HnswLayer {
    fn new(capacity: usize, stride: usize) -> Self {
        Self {
            data: vec![0u32; capacity * stride],
            counts: vec![0u16; capacity],
            stride,
        }
    }

    #[inline]
    fn neighbors(&self, node: u32) -> &[u32] {
        let idx = node as usize;
        let start = idx * self.stride;
        let count = self.counts[idx] as usize;
        &self.data[start..start + count]
    }

    fn set_neighbors(&mut self, node: u32, neighbors: &[u32]) {
        let idx = node as usize;
        let start = idx * self.stride;
        let count = neighbors.len().min(self.stride);
        self.data[start..start + count].copy_from_slice(&neighbors[..count]);
        self.counts[idx] = count as u16;
    }

    fn push_neighbor(&mut self, node: u32, neighbor: u32) -> bool {
        let idx = node as usize;
        let count = self.counts[idx] as usize;
        if count >= self.stride {
            return false; // at capacity
        }
        let start = idx * self.stride;
        self.data[start + count] = neighbor;
        self.counts[idx] = (count + 1) as u16;
        true
    }

    fn ensure_capacity(&mut self, min_nodes: usize) {
        if self.counts.len() < min_nodes {
            let old_len = self.counts.len();
            self.counts.resize(min_nodes, 0);
            self.data.resize(min_nodes * self.stride, 0);
            let _ = old_len; // suppress unused warning
        }
    }
}

const PARALLEL_THRESHOLD: u32 = 2000;
const SEQUENTIAL_SEED: u32 = 1000;

struct ParallelHnswLayer {
    neighbors: Vec<RwLock<Vec<u32>>>,
}

impl ParallelHnswLayer {
    fn new(capacity: usize) -> Self {
        let mut neighbors = Vec::with_capacity(capacity);
        for _ in 0..capacity {
            neighbors.push(RwLock::new(Vec::new()));
        }
        Self { neighbors }
    }
}

struct ParallelHnswBuilder {
    layers: Vec<ParallelHnswLayer>,
    entry_point: AtomicU32,
    max_level: AtomicUsize,
    m: usize,
    m_max0: usize,
    ef_construction: usize,
}

impl ParallelHnswBuilder {
    fn new(
        levels: &[usize],
        num_docs: usize,
        m: usize,
        m_max0: usize,
        ef_construction: usize,
    ) -> Self {
        let max_level = levels.iter().copied().max().unwrap_or(0);
        let mut layers = Vec::with_capacity(max_level + 1);
        for _ in 0..=max_level {
            layers.push(ParallelHnswLayer::new(num_docs));
        }
        Self {
            layers,
            entry_point: AtomicU32::new(u32::MAX),
            max_level: AtomicUsize::new(0),
            m,
            m_max0,
            ef_construction,
        }
    }

    fn insert(
        &self,
        id: u32,
        vec: &[f32],
        level: usize,
        vectors: &[f32],
        dim: usize,
        metric: &VecDist,
    ) {
        // CAS loop to set entry point if we're the first
        let ep = loop {
            let current = self.entry_point.load(Ordering::Acquire);
            if current == u32::MAX {
                // Try to become the entry point
                if self.entry_point.compare_exchange(
                    u32::MAX, id, Ordering::AcqRel, Ordering::Acquire,
                ).is_ok() {
                    self.max_level.fetch_max(level, Ordering::AcqRel);
                    return;
                }
                // Someone else won, reload
                continue;
            }
            break current;
        };

        let current_max_level = self.max_level.load(Ordering::Acquire);
        let mut current_ep = ep;

        // Phase 1: Greedy descent from top to level+1
        if current_max_level > level {
            for l in (level + 1..=current_max_level).rev() {
                if l < self.layers.len() {
                    current_ep = self.greedy_closest(current_ep, vec, l, vectors, dim, metric);
                }
            }
        }

        // Phase 2: Insert at each layer from min(level, current_max_level) down to 0
        let insert_from = level.min(current_max_level);
        for l in (0..=insert_from).rev() {
            let m_max = if l == 0 { self.m_max0 } else { self.m };

            let neighbors = self.search_layer(
                current_ep, vec, self.ef_construction, l, vectors, dim, metric,
            );

            // Heuristic neighbor selection
            let selected = select_neighbors_heuristic(
                &neighbors, m_max, vec, vectors, dim, metric,
            );

            // Set this node's neighbors
            {
                let mut my_neighbors = self.layers[l].neighbors[id as usize].write().unwrap();
                *my_neighbors = selected.clone();
            }

            // Bidirectional connect — lock each neighbor individually
            for &neighbor in &selected {
                let mut nb_list = self.layers[l].neighbors[neighbor as usize].write().unwrap();
                nb_list.push(id);
                if nb_list.len() > m_max {
                    let nb_vec_start = neighbor as usize * dim;
                    let nb_vec = &vectors[nb_vec_start..nb_vec_start + dim];
                    prune_connections(&mut nb_list, nb_vec, m_max, vectors, dim, metric);
                }
            }

            if !neighbors.is_empty() {
                current_ep = neighbors[0].0;
            }
        }

        // CAS loop to update entry point if we have a higher level
        if level > current_max_level {
            let mut old_max = self.max_level.load(Ordering::Acquire);
            loop {
                if level <= old_max {
                    break;
                }
                match self.max_level.compare_exchange(
                    old_max, level, Ordering::AcqRel, Ordering::Acquire,
                ) {
                    Ok(_) => {
                        self.entry_point.store(id, Ordering::Release);
                        break;
                    }
                    Err(actual) => old_max = actual,
                }
            }
        }
    }

    fn greedy_closest(
        &self,
        start: u32,
        query: &[f32],
        layer: usize,
        vectors: &[f32],
        dim: usize,
        metric: &VecDist,
    ) -> u32 {
        let mut current = start;
        let mut current_dist = distance_to(current, query, vectors, dim, metric);

        loop {
            let mut changed = false;
            let neighbors: Vec<u32> = {
                let locked = self.layers[layer].neighbors[current as usize].read().unwrap();
                locked.clone()
            };
            for (i, &neighbor) in neighbors.iter().enumerate() {
                #[cfg(target_arch = "x86_64")]
                if i + 1 < neighbors.len() {
                    unsafe { prefetch_vector(vectors, neighbors[i + 1], dim); }
                }
                let d = distance_to(neighbor, query, vectors, dim, metric);
                if d < current_dist {
                    current_dist = d;
                    current = neighbor;
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }

        current
    }

    fn search_layer(
        &self,
        entry: u32,
        query: &[f32],
        ef: usize,
        layer: usize,
        vectors: &[f32],
        dim: usize,
        metric: &VecDist,
    ) -> Vec<(u32, f32)> {
        let entry_dist = distance_to(entry, query, vectors, dim, metric);

        let mut candidates: BinaryHeap<Reverse<OrdF32Pair>> = BinaryHeap::new();
        let mut results: BinaryHeap<OrdF32Pair> = BinaryHeap::new();

        let mut visited = Bitmap::new();
        visited.add(entry);

        candidates.push(Reverse(OrdF32Pair(entry_dist, entry)));
        results.push(OrdF32Pair(entry_dist, entry));

        while let Some(Reverse(OrdF32Pair(c_dist, c_id))) = candidates.pop() {
            let farthest_dist = results.peek().map_or(f32::MAX, |r| r.0);
            if c_dist > farthest_dist && results.len() >= ef {
                break;
            }

            // Read lock, clone neighbors, unlock — short critical section
            let neighbors: Vec<u32> = {
                let locked = self.layers[layer].neighbors[c_id as usize].read().unwrap();
                locked.clone()
            };

            for (i, &neighbor) in neighbors.iter().enumerate() {
                #[cfg(target_arch = "x86_64")]
                if i + 1 < neighbors.len() {
                    unsafe { prefetch_vector(vectors, neighbors[i + 1], dim); }
                }

                if visited.contains(neighbor) {
                    continue;
                }
                visited.add(neighbor);

                let n_dist = distance_to(neighbor, query, vectors, dim, metric);
                let farthest_dist = results.peek().map_or(f32::MAX, |r| r.0);

                if n_dist < farthest_dist || results.len() < ef {
                    candidates.push(Reverse(OrdF32Pair(n_dist, neighbor)));
                    results.push(OrdF32Pair(n_dist, neighbor));
                    if results.len() > ef {
                        results.pop();
                    }
                }
            }
        }

        let mut result_vec: Vec<(u32, f32)> = results
            .into_iter()
            .map(|OrdF32Pair(d, id)| (id, d))
            .collect();
        result_vec.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
        result_vec
    }

    fn into_graph(self) -> HnswGraph {
        let ep_raw = self.entry_point.load(Ordering::Acquire);
        let entry_point = if ep_raw == u32::MAX { None } else { Some(ep_raw) };
        let max_level = self.max_level.load(Ordering::Acquire);

        let m = self.m;
        let m_max0 = self.m_max0;

        let layers: Vec<HnswLayer> = self.layers.into_iter().enumerate().map(|(l, pl)| {
            let stride = if l == 0 { m_max0 } else { m };
            let num_nodes = pl.neighbors.len();
            let mut layer = HnswLayer::new(num_nodes, stride);

            for (i, lock) in pl.neighbors.into_iter().enumerate() {
                let nbs = lock.into_inner().unwrap();
                layer.set_neighbors(i as u32, &nbs);
            }

            layer
        }).collect();

        HnswGraph {
            layers,
            entry_point,
            max_level,
            m,
            m_max0,
            ef_construction: self.ef_construction,
        }
    }
}

impl HnswGraph {
    /// Build an HNSW index over flat vectors.
    /// `vectors` is a flat array: [doc0_d0, doc0_d1, ..., doc1_d0, ...].
    /// Uses parallel construction with per-node locking for large datasets.
    pub fn build(
        vectors: &[f32],
        num_docs: u32,
        num_dim: u16,
        metric: &VecDist,
        m: usize,
        ef_construction: usize,
    ) -> Self {
        let dim = num_dim as usize;
        let m_max0 = m * 2;
        let ml = 1.0 / (m as f64).ln();

        if num_docs == 0 {
            return HnswGraph {
                layers: Vec::new(),
                entry_point: None,
                max_level: 0,
                m,
                m_max0,
                ef_construction,
            };
        }

        if num_docs < PARALLEL_THRESHOLD {
            // Sequential path for small datasets (all existing tests hit this)
            let mut graph = HnswGraph {
                layers: Vec::new(),
                entry_point: None,
                max_level: 0,
                m,
                m_max0,
                ef_construction,
            };
            for doc_id in 0..num_docs {
                let level = assign_level(doc_id, ml);
                let vec_start = doc_id as usize * dim;
                let vec_end = vec_start + dim;
                let doc_vec = &vectors[vec_start..vec_end];
                graph.insert(doc_id, doc_vec, level, vectors, dim, metric);
            }
            return graph;
        }

        // Parallel path: pre-compute levels, seed sequentially, then parallel insert
        let levels: Vec<usize> = (0..num_docs).map(|id| assign_level(id, ml)).collect();

        let builder = ParallelHnswBuilder::new(
            &levels, num_docs as usize, m, m_max0, ef_construction,
        );

        // Sequential seed: insert first SEQUENTIAL_SEED docs to form connected core
        let seed_count = SEQUENTIAL_SEED.min(num_docs);
        for doc_id in 0..seed_count {
            let vec_start = doc_id as usize * dim;
            let doc_vec = &vectors[vec_start..vec_start + dim];
            builder.insert(doc_id, doc_vec, levels[doc_id as usize], vectors, dim, metric);
        }

        // Parallel phase: remaining docs via rayon
        (seed_count..num_docs).into_par_iter().for_each(|doc_id| {
            let vec_start = doc_id as usize * dim;
            let doc_vec = &vectors[vec_start..vec_start + dim];
            builder.insert(doc_id, doc_vec, levels[doc_id as usize], vectors, dim, metric);
        });

        builder.into_graph()
    }

    /// A graph of `num_docs` nodes over `vectors` that keeps `base`, a graph of `base_nodes` nodes
    /// whose node `b` is node `map(b)` here (a merge's largest source without deletions): its
    /// nodes keep their levels and edges, and every other node is inserted as `build` inserts it,
    /// in node order (in parallel past `PARALLEL_THRESHOLD` nodes, after a sequential seed when the
    /// base is smaller than one).
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn build_from(
        base: &HnswView,
        map: impl Fn(u32) -> u32,
        base_nodes: u32,
        vectors: &[f32],
        num_docs: u32,
        num_dim: u16,
        metric: &VecDist,
        m: usize,
        ef_construction: usize,
    ) -> Self {
        let Some(base_ep) = base.entry_point.filter(|&e| e < base_nodes) else {
            return Self::build(vectors, num_docs, num_dim, metric, m, ef_construction);
        };
        let dim = num_dim as usize;
        let m_max0 = m * 2;
        let ml = 1.0 / (m as f64).ln();
        let renum: Vec<u32> = (0..base_nodes).map(&map).collect();
        if renum.iter().any(|&k| k >= num_docs) {
            return Self::build(vectors, num_docs, num_dim, metric, m, ef_construction);
        }
        // A base node's level: the highest layer it is a member of (the entry point alone on the
        // top layer, without neighbours there, is listed on none).
        let mut levels: Vec<usize> = (0..num_docs).map(|id| assign_level(id, ml)).collect();
        let mut from_base = vec![false; num_docs as usize];
        for &k in &renum {
            from_base[k as usize] = true;
            levels[k as usize] = 0;
        }
        for (l, layer) in base.layers.iter().enumerate().skip(1) {
            for &b in layer.members.iter().flat_map(|m| m.iter()) {
                if b < base_nodes {
                    levels[renum[b as usize] as usize] = l;
                }
            }
        }
        levels[renum[base_ep as usize] as usize] = base.max_level;
        let edges = |b: u32, l: usize| -> Vec<u32> { base.neighbors(l, b).iter().filter(|&&x| x < base_nodes).map(|&x| renum[x as usize]).collect() };
        let rest: Vec<u32> = (0..num_docs).filter(|&k| !from_base[k as usize]).collect();
        let vec_of = |k: u32| &vectors[k as usize * dim..(k as usize + 1) * dim];

        if num_docs < PARALLEL_THRESHOLD {
            let top = levels.iter().copied().max().unwrap_or(0);
            let mut graph = HnswGraph {
                layers: (0..=top).map(|l| HnswLayer::new(num_docs as usize, if l == 0 { m_max0 } else { m })).collect(),
                entry_point: Some(renum[base_ep as usize]),
                max_level: base.max_level,
                m,
                m_max0,
                ef_construction,
            };
            for b in 0..base_nodes {
                for l in 0..base.layers.len().min(graph.layers.len()) {
                    let e = edges(b, l);
                    if !e.is_empty() {
                        graph.layers[l].set_neighbors(renum[b as usize], &e);
                    }
                }
            }
            for &k in &rest {
                graph.insert(k, vec_of(k), levels[k as usize], vectors, dim, metric);
            }
            return graph;
        }

        let builder = ParallelHnswBuilder::new(&levels, num_docs as usize, m, m_max0, ef_construction);
        for b in 0..base_nodes {
            for l in 0..base.layers.len().min(builder.layers.len()) {
                let e = edges(b, l);
                if !e.is_empty() {
                    *builder.layers[l].neighbors[renum[b as usize] as usize].write().unwrap() = e;
                }
            }
        }
        builder.entry_point.store(renum[base_ep as usize], Ordering::Release);
        builder.max_level.store(base.max_level, Ordering::Release);
        let seed = (SEQUENTIAL_SEED as usize).saturating_sub(base_nodes as usize).min(rest.len());
        for &k in &rest[..seed] {
            builder.insert(k, vec_of(k), levels[k as usize], vectors, dim, metric);
        }
        rest[seed..].par_iter().for_each(|&k| builder.insert(k, vec_of(k), levels[k as usize], vectors, dim, metric));
        builder.into_graph()
    }

    fn insert(
        &mut self,
        id: u32,
        vec: &[f32],
        level: usize,
        vectors: &[f32],
        dim: usize,
        metric: &VecDist,
    ) {
        // Ensure we have enough layers
        while self.layers.len() <= level {
            let cap = if self.entry_point.is_some() {
                self.layers.first().map_or(1, |l| l.counts.len())
            } else {
                1
            };
            let l = self.layers.len();
            let stride = if l == 0 { self.m_max0 } else { self.m };
            self.layers.push(HnswLayer::new(cap, stride));
        }

        // Ensure layers are large enough for the new node
        for layer in &mut self.layers {
            layer.ensure_capacity(id as usize + 1);
        }

        let Some(ep) = self.entry_point else {
            self.entry_point = Some(id);
            self.max_level = level;
            return;
        };

        let mut current_ep = ep;

        // Phase 1: Greedy descent from top to level+1
        for l in (level + 1..=self.max_level).rev() {
            current_ep = greedy_closest(&*self, current_ep, vec, l, vectors, dim, metric);
        }

        // Phase 2: Insert at each level from min(level, max_level) down to 0
        let insert_from = level.min(self.max_level);
        for l in (0..=insert_from).rev() {
            let m_max = if l == 0 { self.m_max0 } else { self.m };

            let neighbors = search_layer(&*self, current_ep, vec, self.ef_construction, l, vectors, dim, metric, None);

            // Heuristic neighbor selection
            let selected = select_neighbors_heuristic(
                &neighbors, m_max, vec, vectors, dim, metric,
            );

            // Set this node's neighbors using flat storage
            self.layers[l].set_neighbors(id, &selected);

            // Connect bidirectionally
            for &neighbor in &selected {
                let count = self.layers[l].counts[neighbor as usize] as usize;
                if count < m_max {
                    self.layers[l].push_neighbor(neighbor, id);
                } else {
                    // At capacity — include new id in candidates and prune
                    let nb_vec_start = neighbor as usize * dim;
                    let nb_vec = &vectors[nb_vec_start..nb_vec_start + dim];
                    let mut current_nbs: Vec<u32> = self.layers[l].neighbors(neighbor).to_vec();
                    current_nbs.push(id);
                    let pruned = prune_to_heuristic(&current_nbs, nb_vec, m_max, vectors, dim, metric);
                    self.layers[l].set_neighbors(neighbor, &pruned);
                }
            }

            if !neighbors.is_empty() {
                current_ep = neighbors[0].0;
            }
        }

        if level > self.max_level {
            self.entry_point = Some(id);
            self.max_level = level;
        }
    }

    /// Estimate total heap bytes used by the HNSW graph layers.
    pub fn memory_estimate(&self) -> usize {
        self.layers.iter().map(|layer| {
            layer.data.len() * std::mem::size_of::<u32>()
                + layer.counts.len() * std::mem::size_of::<u16>()
        }).sum()
    }

    /// Search for the k nearest neighbors of `query`.
    /// If `filter` is provided, only return nodes in the bitmap.
    pub fn search(
        &self,
        query: &[f32],
        vectors: &[f32],
        dim: usize,
        metric: &VecDist,
        k: usize,
        ef_search: usize,
        filter: Option<&Bitmap>,
    ) -> Vec<(DocId, f32)> {
        search_graph(self, self.entry_point, self.max_level, query, vectors, dim, metric, k, ef_search, filter)
    }
}

/// Neighbor lists by layer: the graph being built, or one read in place from a VECTORS section.
pub(crate) trait Layers {
    fn n_layers(&self) -> usize;
    fn neighbors(&self, layer: usize, node: u32) -> &[u32];
}

impl Layers for HnswGraph {
    #[inline]
    fn n_layers(&self) -> usize {
        self.layers.len()
    }

    #[inline]
    fn neighbors(&self, layer: usize, node: u32) -> &[u32] {
        self.layers[layer].neighbors(node)
    }
}

/// k nearest neighbors of `query`: greedy descent to layer 1, then a beam search of layer 0.
#[allow(clippy::too_many_arguments)]
pub(crate) fn search_graph<L: Layers + ?Sized>(
    g: &L,
    entry_point: Option<u32>,
    max_level: usize,
    query: &[f32],
    vectors: &[f32],
    dim: usize,
    metric: &VecDist,
    k: usize,
    ef_search: usize,
    filter: Option<&Bitmap>,
) -> Vec<(DocId, f32)> {
    let Some(ep) = entry_point else {
        return Vec::new();
    };

    let mut current_ep = ep;

    // Greedy descent from top to layer 1
    for l in (1..=max_level.min(g.n_layers().saturating_sub(1))).rev() {
        current_ep = greedy_closest(g, current_ep, query, l, vectors, dim, metric);
    }

    // Search layer 0 with beam
    let ef = ef_search.max(k);
    let candidates = search_layer(g, current_ep, query, ef, 0, vectors, dim, metric, filter);

    // Return top-k, applying filter if present
    candidates.into_iter()
        .filter(|&(id, _)| filter.map_or(true, |f| f.contains(id)))
        .take(k)
        .collect()
}

/// Greedy walk on a single layer to find the closest node to `query`.
fn greedy_closest<L: Layers + ?Sized>(
    g: &L,
    start: u32,
    query: &[f32],
    layer: usize,
    vectors: &[f32],
    dim: usize,
    metric: &VecDist,
) -> u32 {
    let mut current = start;
    let mut current_dist = distance_to(current, query, vectors, dim, metric);

    loop {
        let mut changed = false;
        let nbs = g.neighbors(layer, current);
        for (i, &neighbor) in nbs.iter().enumerate() {
            #[cfg(target_arch = "x86_64")]
            if i + 1 < nbs.len() {
                unsafe { prefetch_vector(vectors, nbs[i + 1], dim); }
            }
            let d = distance_to(neighbor, query, vectors, dim, metric);
            if d < current_dist {
                current_dist = d;
                current = neighbor;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }

    current
}

/// Beam search on a single layer. Returns candidates sorted by distance (ascending).
fn search_layer<L: Layers + ?Sized>(
    g: &L,
    entry: u32,
    query: &[f32],
    ef: usize,
    layer: usize,
    vectors: &[f32],
    dim: usize,
    metric: &VecDist,
    filter: Option<&Bitmap>,
) -> Vec<(u32, f32)> {
    let entry_dist = distance_to(entry, query, vectors, dim, metric);

    // Min-heap for candidates (closest first)
    let mut candidates: BinaryHeap<Reverse<OrdF32Pair>> = BinaryHeap::new();
    // Max-heap for results (farthest first, to evict)
    let mut results: BinaryHeap<OrdF32Pair> = BinaryHeap::new();

    let mut visited = Bitmap::new();
    visited.add(entry);

    candidates.push(Reverse(OrdF32Pair(entry_dist, entry)));
    results.push(OrdF32Pair(entry_dist, entry));

    while let Some(Reverse(OrdF32Pair(c_dist, c_id))) = candidates.pop() {
        let farthest_dist = results.peek().map_or(f32::MAX, |r| r.0);
        if c_dist > farthest_dist && results.len() >= ef {
            break;
        }

        if layer < g.n_layers() {
            let nbs = g.neighbors(layer, c_id);
            for (i, &neighbor) in nbs.iter().enumerate() {
                #[cfg(target_arch = "x86_64")]
                if i + 1 < nbs.len() {
                    unsafe { prefetch_vector(vectors, nbs[i + 1], dim); }
                }

                if visited.contains(neighbor) {
                    continue;
                }
                visited.add(neighbor);

                let n_dist = distance_to(neighbor, query, vectors, dim, metric);
                let farthest_dist = results.peek().map_or(f32::MAX, |r| r.0);

                if n_dist < farthest_dist || results.len() < ef {
                    candidates.push(Reverse(OrdF32Pair(n_dist, neighbor)));

                    // For filtered search, only add to results if it passes filter
                    if filter.map_or(true, |f| f.contains(neighbor)) {
                        results.push(OrdF32Pair(n_dist, neighbor));
                        if results.len() > ef {
                            results.pop(); // remove farthest
                        }
                    }
                }
            }
        }
    }

    let mut result_vec: Vec<(u32, f32)> = results
        .into_iter()
        .map(|OrdF32Pair(d, id)| (id, d))
        .collect();
    result_vec.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
    result_vec
}

/// Prefetch the vector data for the given doc ID into L1 cache.
/// Hides ~80ns DRAM latency for random vector reads at large scale (5M+ docs).
#[cfg(target_arch = "x86_64")]
#[inline(always)]
unsafe fn prefetch_vector(vectors: &[f32], id: u32, dim: usize) {
    if let Some(v) = vectors.get(id as usize * dim..) {
        unsafe {
            std::arch::x86_64::_mm_prefetch::<3>(v.as_ptr() as *const i8); // _MM_HINT_T0 = 3 (L1 cache)
        }
    }
}

/// Compute "distance" for ordering purposes (lower = more similar).
/// Uses compute_fast (squared L2, SIMD) for performance.
/// For cosine/dot product, negate so that higher similarity → lower distance.
fn distance_to(id: u32, query: &[f32], vectors: &[f32], dim: usize, metric: &VecDist) -> f32 {
    let start = id as usize * dim;
    // A neighbor id past the vectors (a corrupt graph) is never closer than anything.
    let Some(v) = vectors.get(start..start + dim) else { return f32::MAX };
    let raw = metric.compute_fast(query, v);
    if metric.higher_is_better() {
        -raw // negate: higher similarity → lower "distance"
    } else {
        raw
    }
}

/// Heuristic neighbor selection (HNSW Algorithm 4).
/// Keeps candidates that are closer to the owner than to any already-selected neighbor.
/// Fills remaining slots by distance if heuristic selects fewer than m.
fn select_neighbors_heuristic(
    candidates: &[(u32, f32)],
    m: usize,
    owner_vec: &[f32],
    vectors: &[f32],
    dim: usize,
    metric: &VecDist,
) -> Vec<u32> {
    if candidates.len() <= m {
        return candidates.iter().map(|&(id, _)| id).collect();
    }

    let higher_is_better = metric.higher_is_better();
    let mut selected: Vec<u32> = Vec::with_capacity(m);

    // Candidates are already sorted by distance ascending (lower = closer)
    for &(cand_id, _cand_dist) in candidates {
        if selected.len() >= m {
            break;
        }

        let cand_start = cand_id as usize * dim;
        let cand_vec = &vectors[cand_start..cand_start + dim];

        // Normalized distance: lower = closer (negate for higher-is-better metrics)
        let raw_owner = metric.compute_fast(owner_vec, cand_vec);
        let dist_to_owner = if higher_is_better { -raw_owner } else { raw_owner };

        // Check: is this candidate closer to owner than to any already-selected neighbor?
        let mut keep = true;
        for &sel_id in &selected {
            let sel_start = sel_id as usize * dim;
            let sel_vec = &vectors[sel_start..sel_start + dim];
            let raw_sel = metric.compute_fast(cand_vec, sel_vec);
            let dist_to_selected = if higher_is_better { -raw_sel } else { raw_sel };

            // If candidate is closer to a selected neighbor than to the owner, skip it
            if dist_to_selected < dist_to_owner {
                keep = false;
                break;
            }
        }

        if keep {
            selected.push(cand_id);
        }
    }

    // Fill: if heuristic selected fewer than m, fill by distance order
    if selected.len() < m {
        for &(cand_id, _) in candidates {
            if selected.len() >= m {
                break;
            }
            if !selected.contains(&cand_id) {
                selected.push(cand_id);
            }
        }
    }

    selected
}

/// Prune a neighbor list using heuristic selection.
fn prune_connections(
    neighbors: &mut Vec<u32>,
    owner_vec: &[f32],
    m_max: usize,
    vectors: &[f32],
    dim: usize,
    metric: &VecDist,
) {
    // Build (id, dist) pairs and sort
    let mut pairs: Vec<(u32, f32)> = neighbors.iter()
        .map(|&id| {
            let d = distance_to(id, owner_vec, vectors, dim, metric);
            (id, d)
        })
        .collect();
    pairs.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));

    let selected = select_neighbors_heuristic(&pairs, m_max, owner_vec, vectors, dim, metric);
    *neighbors = selected;
}

/// Prune and return a new vec (for flat storage path where we can't borrow mutably).
fn prune_to_heuristic(
    neighbors: &[u32],
    owner_vec: &[f32],
    m_max: usize,
    vectors: &[f32],
    dim: usize,
    metric: &VecDist,
) -> Vec<u32> {
    let mut pairs: Vec<(u32, f32)> = neighbors.iter()
        .map(|&id| {
            let d = distance_to(id, owner_vec, vectors, dim, metric);
            (id, d)
        })
        .collect();
    pairs.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
    select_neighbors_heuristic(&pairs, m_max, owner_vec, vectors, dim, metric)
}

/// Deterministic level assignment using a hash of doc_id.
fn assign_level(doc_id: u32, ml: f64) -> usize {
    // Simple hash-based pseudo-random for reproducible level assignment
    let hash = (doc_id.wrapping_mul(2654435761)) as f64 / u32::MAX as f64;
    let level = (-hash.max(1e-10).ln() * ml).floor() as usize;
    level.min(16) // cap at 16 levels
}

/// Wrapper for f32 ordering in BinaryHeap.
#[derive(PartialEq)]
struct OrdF32Pair(f32, u32);

impl Eq for OrdF32Pair {}

impl PartialOrd for OrdF32Pair {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for OrdF32Pair {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.0
            .partial_cmp(&other.0)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| self.1.cmp(&other.1))
    }
}

/// An HNSW graph read in place from a VECTORS section: per layer, CSR neighbor lists. Layer 0
/// covers every node; an upper layer lists its members, sorted, and finds a node by binary search,
/// so it costs its members only (the graph being built keeps a slot per node on every layer).
///
/// ```text
/// per layer: u32 n_members, u32 n_edges; upper layers: u32 members[n_members], padded to 8;
///            u32 offsets[n_members + 1], padded to 8; u32 neighbors[n_edges], padded to 8
/// ```
#[derive(Clone)]
pub struct HnswView {
    pub(crate) entry_point: Option<u32>,
    pub(crate) max_level: usize,
    layers: Vec<ViewLayer>,
}

#[derive(Clone)]
struct ViewLayer {
    members: Option<Pods<u32>>,
    offsets: Pods<u32>,
    neighbors: Pods<u32>,
}

impl Layers for HnswView {
    #[inline]
    fn n_layers(&self) -> usize {
        self.layers.len()
    }

    #[inline]
    fn neighbors(&self, layer: usize, node: u32) -> &[u32] {
        let l = &self.layers[layer];
        let i = match &l.members {
            None => node as usize,
            Some(m) => match m.binary_search(&node) {
                Ok(i) => i,
                Err(_) => return &[],
            },
        };
        match (l.offsets.get(i), l.offsets.get(i + 1)) {
            (Some(&a), Some(&b)) => l.neighbors.get(a as usize..b as usize).unwrap_or(&[]),
            _ => &[],
        }
    }
}

fn pad8(out: &mut Vec<u8>) {
    out.resize(out.len().next_multiple_of(8), 0);
}

fn put_u32s(out: &mut Vec<u8>, v: &[u32]) {
    out.extend_from_slice(bytemuck::cast_slice(v));
    pad8(out);
}

impl HnswView {
    /// Append the layers of a built graph over `n_nodes` nodes.
    pub(crate) fn write(g: &HnswGraph, n_nodes: u32, out: &mut Vec<u8>) -> std::io::Result<()> {
        for (l, layer) in g.layers.iter().enumerate() {
            let cap = layer.counts.len() as u32;
            let members: Vec<u32> = if l == 0 {
                (0..n_nodes).collect()
            } else {
                (0..cap.min(n_nodes)).filter(|&i| layer.counts[i as usize] > 0).collect()
            };
            let mut offsets = Vec::with_capacity(members.len() + 1);
            let mut neighbors: Vec<u32> = Vec::new();
            offsets.push(0u32);
            for &m in &members {
                if m < cap {
                    neighbors.extend(layer.neighbors(m).iter().filter(|&&n| n < n_nodes));
                }
                offsets.push(u32::try_from(neighbors.len()).map_err(|_| {
                    std::io::Error::new(std::io::ErrorKind::InvalidInput, "HNSW layer with more than 2^32 edges")
                })?);
            }
            out.extend_from_slice(&(members.len() as u32).to_le_bytes());
            out.extend_from_slice(&(neighbors.len() as u32).to_le_bytes());
            if l > 0 {
                put_u32s(out, &members);
            }
            put_u32s(out, &offsets);
            put_u32s(out, &neighbors);
        }
        Ok(())
    }

    /// Read `n_layers` layers from `bytes` at `*at`, advancing it.
    pub(crate) fn parse(bytes: &Bytes, at: &mut usize, n_layers: usize, n_nodes: u32, entry_point: Option<u32>, max_level: usize) -> std::io::Result<Self> {
        let invalid = |m: &str| std::io::Error::new(std::io::ErrorKind::InvalidData, format!("VECTORS graph: {m}"));
        if entry_point.is_some_and(|e| e >= n_nodes) || (entry_point.is_some() && max_level >= n_layers) {
            return Err(invalid("entry point"));
        }
        let take = |at: &mut usize, n: usize| -> std::io::Result<Pods<u32>> {
            let len = 4 * n;
            if at.checked_add(len).is_none_or(|e| e > bytes.len()) {
                return Err(invalid("truncated"));
            }
            let p = Pods::new(bytes.slice(*at, len))?;
            *at = (*at + len).next_multiple_of(8);
            Ok(p)
        };
        let mut layers = Vec::with_capacity(n_layers);
        for l in 0..n_layers {
            if *at + 8 > bytes.len() {
                return Err(invalid("truncated"));
            }
            let (n, e) = (bytes.u32_at(*at) as usize, bytes.u32_at(*at + 4) as usize);
            *at += 8;
            if (l == 0 && n != n_nodes as usize) || n > n_nodes as usize {
                return Err(invalid("layer size"));
            }
            let members = if l > 0 { Some(take(at, n)?) } else { None };
            let offsets = take(at, n + 1)?;
            let neighbors = take(at, e)?;
            if offsets.first() != Some(&0) || offsets.last().map(|&o| o as usize) != Some(e) {
                return Err(invalid("offsets"));
            }
            layers.push(ViewLayer { members, offsets, neighbors });
        }
        Ok(Self { entry_point, max_level, layers })
    }

    /// Search for the k nearest neighbors of `query`; with `filter`, only nodes in it.
    #[allow(clippy::too_many_arguments)]
    pub fn search(
        &self,
        query: &[f32],
        vectors: &[f32],
        dim: usize,
        metric: &VecDist,
        k: usize,
        ef_search: usize,
        filter: Option<&Bitmap>,
    ) -> Vec<(DocId, f32)> {
        search_graph(self, self.entry_point, self.max_level, query, vectors, dim, metric, k, ef_search, filter)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn random_vectors(n: usize, dim: usize, seed: u32) -> Vec<f32> {
        let mut v = Vec::with_capacity(n * dim);
        let mut state = seed;
        for _ in 0..(n * dim) {
            // Simple LCG for reproducible "random" vectors
            state = state.wrapping_mul(1103515245).wrapping_add(12345);
            let val = ((state >> 16) as f32 / 32768.0) - 1.0; // range [-1, 1]
            v.push(val);
        }
        v
    }

    #[test]
    fn build_and_search_finds_self() {
        let dim = 32;
        let n = 100;
        let vectors = random_vectors(n, dim, 42);
        let graph = HnswGraph::build(&vectors, n as u32, dim as u16, &VecDist::Cosine, 16, 200);

        // Search for vector 0 — it should be the #1 result
        let query = &vectors[0..dim];
        let results = graph.search(query, &vectors, dim, &VecDist::Cosine, 5, 100, None);

        assert!(!results.is_empty());
        assert_eq!(results[0].0, 0, "Self should be the closest match");
    }

    #[test]
    fn search_returns_k_results() {
        let dim = 16;
        let n = 50;
        let vectors = random_vectors(n, dim, 123);
        let graph = HnswGraph::build(&vectors, n as u32, dim as u16, &VecDist::Euclidean, 8, 100);

        let query = &vectors[0..dim];
        let results = graph.search(query, &vectors, dim, &VecDist::Euclidean, 10, 50, None);

        assert_eq!(results.len(), 10);
    }

    #[test]
    fn search_with_filter() {
        let dim = 8;
        let n = 20;
        let vectors = random_vectors(n, dim, 99);
        let graph = HnswGraph::build(&vectors, n as u32, dim as u16, &VecDist::Cosine, 8, 50);

        // Only allow even doc IDs
        let mut filter = Bitmap::new();
        for i in (0..n as u32).step_by(2) {
            filter.add(i);
        }

        let query = &vectors[0..dim];
        let results = graph.search(query, &vectors, dim, &VecDist::Cosine, 5, 50, Some(&filter));

        for (id, _) in &results {
            assert!(id % 2 == 0, "Filtered result should only contain even IDs, got {id}");
        }
    }

    #[test]
    fn empty_index() {
        let graph = HnswGraph::build(&[], 0, 16, &VecDist::Cosine, 16, 200);
        let query = vec![0.0; 16];
        let results = graph.search(&query, &[], 16, &VecDist::Cosine, 10, 100, None);
        assert!(results.is_empty());
    }

    #[test]
    fn dot_product_metric() {
        let dim = 4;
        let n = 10;
        let vectors = random_vectors(n, dim, 55);
        let graph = HnswGraph::build(&vectors, n as u32, dim as u16, &VecDist::DotProduct, 8, 50);

        let query = &vectors[0..dim];
        let results = graph.search(query, &vectors, dim, &VecDist::DotProduct, 3, 20, None);

        assert!(!results.is_empty());
        assert_eq!(results[0].0, 0, "Self should be closest for dot product");
    }

    #[test]
    fn recall_quality() {
        // Build index, then compare HNSW results vs brute force for recall
        let dim = 32;
        let n = 200;
        let vectors = random_vectors(n, dim, 77);
        let graph = HnswGraph::build(&vectors, n as u32, dim as u16, &VecDist::Cosine, 16, 200);

        let k = 10;
        let mut total_recall = 0.0f64;
        let num_queries = 20;

        for q_id in 0..num_queries {
            let query = &vectors[q_id * dim..(q_id + 1) * dim];

            // HNSW search
            let hnsw_results: Vec<u32> = graph
                .search(query, &vectors, dim, &VecDist::Cosine, k, 100, None)
                .into_iter()
                .map(|(id, _)| id)
                .collect();

            // Brute force
            let mut brute: Vec<(u32, f32)> = (0..n as u32)
                .map(|i| {
                    let start = i as usize * dim;
                    let sim = super::super::distance::cosine_similarity(query, &vectors[start..start + dim]);
                    (i, sim)
                })
                .collect();
            brute.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
            let brute_top_k: Vec<u32> = brute.iter().take(k).map(|(id, _)| *id).collect();

            // Count overlap
            let overlap = hnsw_results.iter().filter(|id| brute_top_k.contains(id)).count();
            total_recall += overlap as f64 / k as f64;
        }

        let avg_recall = total_recall / num_queries as f64;
        assert!(
            avg_recall > 0.80,
            "HNSW recall should be > 80%, got {:.1}%",
            avg_recall * 100.0
        );
    }

    #[test]
    fn parallel_build_recall() {
        // Above PARALLEL_THRESHOLD to exercise the parallel path
        let dim = 32;
        let n = 5000;
        let vectors = random_vectors(n, dim, 42);
        let graph = HnswGraph::build(&vectors, n as u32, dim as u16, &VecDist::Cosine, 16, 200);

        let k = 10;
        let mut total_recall = 0.0f64;
        let num_queries = 50;

        for q_id in 0..num_queries {
            let query = &vectors[q_id * dim..(q_id + 1) * dim];

            let hnsw_results: Vec<u32> = graph
                .search(query, &vectors, dim, &VecDist::Cosine, k, 100, None)
                .into_iter()
                .map(|(id, _)| id)
                .collect();

            // Brute force
            let mut brute: Vec<(u32, f32)> = (0..n as u32)
                .map(|i| {
                    let start = i as usize * dim;
                    let sim = super::super::distance::cosine_similarity(query, &vectors[start..start + dim]);
                    (i, sim)
                })
                .collect();
            brute.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
            let brute_top_k: Vec<u32> = brute.iter().take(k).map(|(id, _)| *id).collect();

            let overlap = hnsw_results.iter().filter(|id| brute_top_k.contains(id)).count();
            total_recall += overlap as f64 / k as f64;
        }

        let avg_recall = total_recall / num_queries as f64;
        assert!(
            avg_recall > 0.75,
            "Parallel HNSW recall should be > 75%, got {:.1}%",
            avg_recall * 100.0
        );
    }

    #[test]
    fn parallel_build_self_search() {
        // Above PARALLEL_THRESHOLD — every vector should find itself as nearest
        let dim = 32;
        let n = 3000;
        let vectors = random_vectors(n, dim, 99);
        let graph = HnswGraph::build(&vectors, n as u32, dim as u16, &VecDist::Cosine, 16, 200);

        let mut self_found = 0;
        for doc_id in 0..n {
            let query = &vectors[doc_id * dim..(doc_id + 1) * dim];
            let results = graph.search(query, &vectors, dim, &VecDist::Cosine, 1, 100, None);
            if !results.is_empty() && results[0].0 == doc_id as u32 {
                self_found += 1;
            }
        }

        let self_rate = self_found as f64 / n as f64;
        assert!(
            self_rate > 0.95,
            "At least 95% of vectors should find themselves, got {:.1}%",
            self_rate * 100.0
        );
    }

    #[test]
    fn parallel_build_all_metrics() {
        let dim = 32;
        let n = 3000;

        for metric in &[VecDist::Cosine, VecDist::Euclidean, VecDist::DotProduct] {
            let vectors = random_vectors(n, dim, 77);
            let graph = HnswGraph::build(&vectors, n as u32, dim as u16, metric, 16, 200);

            // Spot check: first 100 vectors should find themselves
            let mut self_found = 0;
            for doc_id in 0..100 {
                let query = &vectors[doc_id * dim..(doc_id + 1) * dim];
                let results = graph.search(query, &vectors, dim, metric, 1, 100, None);
                if !results.is_empty() && results[0].0 == doc_id as u32 {
                    self_found += 1;
                }
            }

            assert!(
                self_found > 90,
                "{metric:?}: at least 90/100 should find themselves, got {self_found}",
            );
        }
    }

    // --- Optimization 5 tests: Heuristic neighbor selection ---

    #[test]
    fn heuristic_selects_diverse_neighbors() {
        // 4 candidates: two are very close to each other, heuristic should prefer diversity
        let dim = 2;
        // owner at origin
        let owner = [0.0f32, 0.0];
        // vectors: 0=(1,0), 1=(1.01,0), 2=(0,1), 3=(-1,0)
        let vectors = vec![
            1.0, 0.0,      // 0: east
            1.01, 0.0,     // 1: almost same as 0
            0.0, 1.0,      // 2: north
            -1.0, 0.0,     // 3: west
        ];
        // Candidates sorted by distance to owner (ascending) — Euclidean squared
        let candidates = vec![
            (0, 1.0f32),    // dist 1.0
            (2, 1.0),       // dist 1.0
            (1, 1.0201),    // dist ~1.02
            (3, 1.0),       // dist 1.0
        ];

        let selected = select_neighbors_heuristic(
            &candidates, 3, &owner, &vectors, dim, &VecDist::Euclidean,
        );

        assert_eq!(selected.len(), 3);
        // Heuristic should pick 0, 2, 3 (diverse directions), skipping 1 (too close to 0)
        assert!(selected.contains(&0));
        assert!(selected.contains(&2));
        // 1 should be skipped in favor of 3 (more diverse)
        assert!(selected.contains(&3), "Should prefer diverse neighbor 3 over redundant 1");
    }

    #[test]
    fn heuristic_fallback_fills() {
        // If heuristic skips too many, fill kicks in
        let dim = 2;
        let owner = [0.0f32, 0.0];
        // All vectors are in the same direction — heuristic will only pick first
        let vectors = vec![
            1.0, 0.0,
            2.0, 0.0,
            3.0, 0.0,
        ];
        let candidates = vec![
            (0, 1.0f32),
            (1, 4.0),
            (2, 9.0),
        ];

        let selected = select_neighbors_heuristic(
            &candidates, 3, &owner, &vectors, dim, &VecDist::Euclidean,
        );

        // Heuristic picks only 0 (1 and 2 are closer to 0 than to owner),
        // but fill should add 1 and 2
        assert_eq!(selected.len(), 3);
        assert!(selected.contains(&0));
        assert!(selected.contains(&1));
        assert!(selected.contains(&2));
    }

    // --- Optimization 6 tests: Flat neighbor storage ---

    #[test]
    fn flat_layer_roundtrip() {
        let mut layer = HnswLayer::new(10, 4);

        // Set neighbors for node 3
        layer.set_neighbors(3, &[10, 20, 30]);
        assert_eq!(layer.neighbors(3), &[10, 20, 30]);

        // Set neighbors for node 0
        layer.set_neighbors(0, &[1, 2, 3, 4]);
        assert_eq!(layer.neighbors(0), &[1, 2, 3, 4]);

        // Node 5 should have no neighbors
        assert_eq!(layer.neighbors(5), &[] as &[u32]);
    }

    #[test]
    fn flat_layer_push_and_capacity() {
        let mut layer = HnswLayer::new(5, 3);

        assert!(layer.push_neighbor(0, 10));
        assert!(layer.push_neighbor(0, 20));
        assert!(layer.push_neighbor(0, 30));
        // At capacity (stride=3), next push should fail
        assert!(!layer.push_neighbor(0, 40));

        assert_eq!(layer.neighbors(0), &[10, 20, 30]);
    }
}
