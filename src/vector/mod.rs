pub mod distance;
#[cfg(feature = "cuda")]
pub mod gpu;
pub mod hnsw;
pub mod hybrid;
pub mod quantize;

use crate::index::merge::DocMap;
use crate::schema::{DocId, FieldId, VecDist};
use crate::tqbf::bytes::{Bytes, Pods};
use crate::tqbf::container::{kind, SegmentFile, SegmentWriter};
use distance::{normalize_vector, normalize_vectors};
use hnsw::{HnswGraph, HnswView};
use quantize::RaBitQuantizer;
use crate::bitmap::Bitmap;
use std::io;

/// Vector index of a float[] field with num_dim: flat vectors + HNSW graph, read in place from the
/// field's VECTORS section (TQBF spec §7.7):
///
/// ```text
/// u32 doc_count, u16 num_dim, u8 metric, u8 effective_metric (0 cosine, 1 ip, 2 l2),
/// u32 entry_point (u32::MAX = none), u32 max_level, u32 n_layers, u8 quantized, 3 x 0; to 64
/// f32 vectors[doc_count x num_dim] (unit length for cosine), padded to 8
/// graph layers (`HnswView`)
/// quantized: f32 rotation[num_dim x num_dim], padded to 8; u8 codes[doc_count x
///            ceil(num_dim / 8)], padded to 8; f32 norms[doc_count], padded to 8
/// ```
pub struct VectorIndex {
    bytes: Bytes,
    /// Flat vector storage: contiguous, indexed by DocId.
    /// Layout: [doc0_dim0, doc0_dim1, ..., doc1_dim0, ...]
    /// If metric is Cosine, these are pre-normalized to unit length.
    pub vectors: Pods<f32>,
    pub num_dim: u16,
    pub metric: VecDist,
    /// Effective metric used internally by HNSW.
    /// Cosine → DotProduct (on pre-normalized vectors), others unchanged.
    effective_metric: VecDist,
    pub hnsw: HnswView,
    /// RaBitQ quantizer (if num_dim >= 64).
    pub quantizer: Option<RaBitQuantizer>,
    /// Binary codes from RaBitQ quantization (concatenated).
    pub codes: Option<Pods<u8>>,
    /// Per-document norms from RaBitQ quantization.
    pub norms: Option<Pods<f32>>,
    /// GPU-resident flat vectors (f32).
    #[cfg(feature = "cuda")]
    pub gpu_vectors: Option<cudarc::driver::CudaSlice<f32>>,
    /// GPU-resident RaBitQ codes (u32 words).
    #[cfg(feature = "cuda")]
    pub gpu_codes: Option<cudarc::driver::CudaSlice<u32>>,
    /// GPU-resident RaBitQ norms (f32).
    #[cfg(feature = "cuda")]
    pub gpu_norms: Option<cudarc::driver::CudaSlice<f32>>,
}

/// HNSW neighbours per node (twice as many on layer 0) and the beam of an insertion.
const M: usize = 16;
const EF_CONSTRUCTION: usize = 200;

fn metric_code(m: VecDist) -> u8 {
    match m {
        VecDist::Cosine => 0,
        VecDist::DotProduct => 1,
        VecDist::Euclidean => 2,
    }
}

fn metric_of(code: u8) -> io::Result<VecDist> {
    match code {
        0 => Ok(VecDist::Cosine),
        1 => Ok(VecDist::DotProduct),
        2 => Ok(VecDist::Euclidean),
        _ => Err(invalid("VECTORS metric")),
    }
}

fn invalid(msg: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.to_string())
}

fn pad8(out: &mut Vec<u8>) {
    out.resize(out.len().next_multiple_of(8), 0);
}

impl VectorIndex {
    /// Build a VectorIndex from flat vectors and an HNSW graph.
    /// If num_dim >= 64, also builds RaBitQ codes for GPU dispatch.
    pub fn build(mut vectors: Vec<f32>, num_docs: u32, num_dim: u16, metric: VecDist) -> Self {
        let dim = num_dim as usize;

        // Pre-normalize for cosine: cosine(a,b) = dot(norm(a), norm(b))
        let effective_metric = if metric == VecDist::Cosine && num_docs > 0 {
            normalize_vectors(&mut vectors, num_docs as usize, dim);
            VecDist::DotProduct
        } else {
            metric
        };

        let hnsw = HnswGraph::build(&vectors, num_docs, num_dim, &effective_metric, M, EF_CONSTRUCTION);
        Self::assemble(vectors, num_docs, num_dim, metric, effective_metric, &hnsw, None)
    }

    /// A vector field's index merged from the sources' (`index::merge`; every source with documents
    /// carried over has one): the vectors, RaBitQ codes and norms of the documents carried over moved
    /// over in merged order;
    /// the HNSW graph of the largest source without deletions kept, its nodes renumbered and their
    /// edges as they are, and every other vector inserted into it as a build inserts it (Lucene's
    /// merge from the largest segment's graph). The graph is built afresh when every source has
    /// deletions (a graph without some of its nodes loses the paths through them).
    pub(crate) fn merge(srcs: &[Option<&VectorIndex>], docs: &DocMap, num_dim: u16, metric: VecDist) -> io::Result<Self> {
        let dim = num_dim as usize;
        let n = docs.len();
        let src = |s: u32| srcs[s as usize].ok_or_else(|| invalid("segment merge: a source with documents has no vector index"));
        if srcs.iter().flatten().any(|v| v.num_dim != num_dim || v.metric != metric) {
            return Err(invalid("segment merge: vector indexes of another dimension or metric"));
        }
        let effective_metric = if metric == VecDist::Cosine && n > 0 { VecDist::DotProduct } else { metric };
        // stored normalized already for cosine
        let mut vectors = vec![0f32; n as usize * dim];
        for (k, &(s, d)) in docs.order().iter().enumerate() {
            vectors[k * dim..(k + 1) * dim].copy_from_slice(src(s)?.get_vector(d));
        }
        let nodes = |v: &VectorIndex| (v.vectors.len() / dim.max(1)) as u32;
        let base = (0..srcs.len())
            .filter_map(|s| srcs[s].filter(|v| docs.whole(s) && v.hnsw.entry_point.is_some()).map(|v| (s, v)))
            .max_by_key(|&(_, v)| nodes(v));
        let hnsw = match base {
            Some((s, v)) => HnswGraph::build_from(&v.hnsw, |d| docs.get(s, d), nodes(v), &vectors, n, num_dim, &effective_metric, M, EF_CONSTRUCTION),
            None => HnswGraph::build(&vectors, n, num_dim, &effective_metric, M, EF_CONSTRUCTION),
        };
        let with_docs: Vec<&VectorIndex> = srcs.iter().flatten().copied().filter(|v| !v.vectors.is_empty()).collect();
        let quantized = match (num_dim >= 64 && n > 0, with_docs.first()) {
            (true, Some(first)) if with_docs.iter().all(|v| v.quantizer.is_some() && v.codes.is_some() && v.norms.is_some()) => {
                let width = dim.div_ceil(8);
                let (mut codes, mut norms) = (vec![0u8; n as usize * width], vec![0f32; n as usize]);
                for (k, &(s, d)) in docs.order().iter().enumerate() {
                    let (v, d) = (src(s)?, d as usize);
                    codes[k * width..(k + 1) * width].copy_from_slice(&v.codes.as_ref().expect("codes")[d * width..(d + 1) * width]);
                    norms[k] = v.norms.as_ref().expect("norms")[d];
                }
                // every source's rotation is the one of the same seed
                Some((first.quantizer.as_ref().expect("a quantizer").rotation.clone(), codes, norms))
            }
            _ => None,
        };
        Ok(Self::assemble(vectors, n, num_dim, metric, effective_metric, &hnsw, quantized))
    }

    /// The index of `vectors` (normalized already for cosine) and their graph: its VECTORS section
    /// written and read. RaBitQ (dimensions >= 64) from `quantized` (rotation, codes, norms), or
    /// encoded.
    fn assemble(vectors: Vec<f32>, num_docs: u32, num_dim: u16, metric: VecDist, effective_metric: VecDist, hnsw: &HnswGraph, quantized: Option<(Pods<f32>, Vec<u8>, Vec<f32>)>) -> Self {
        // RaBitQ quantization for dimensions >= 64
        let (quantizer, codes, norms) = match quantized {
            _ if num_dim < 64 || num_docs == 0 => (None, None, None),
            Some((rotation, c, n)) => (Some(RaBitQuantizer::from_rotation(rotation, num_dim)), Some(c), Some(n)),
            None => {
                let q = RaBitQuantizer::new(num_dim, 42);
                let (c, n) = q.encode_batch(&vectors);
                (Some(q), Some(c), Some(n))
            }
        };

        let mut out = Vec::with_capacity(64 + vectors.len() * 4);
        out.extend_from_slice(&num_docs.to_le_bytes());
        out.extend_from_slice(&num_dim.to_le_bytes());
        out.extend_from_slice(&[metric_code(metric), metric_code(effective_metric)]);
        out.extend_from_slice(&hnsw.entry_point.unwrap_or(u32::MAX).to_le_bytes());
        out.extend_from_slice(&(hnsw.max_level as u32).to_le_bytes());
        out.extend_from_slice(&(hnsw.layers.len() as u32).to_le_bytes());
        out.extend_from_slice(&[u8::from(quantizer.is_some()), 0, 0, 0]);
        out.resize(64, 0);
        out.extend_from_slice(bytemuck::cast_slice::<f32, u8>(&vectors[..]));
        pad8(&mut out);
        HnswView::write(hnsw, num_docs, &mut out).expect("HNSW graph within the VECTORS limits");
        if let (Some(q), Some(c), Some(n)) = (&quantizer, &codes, &norms) {
            out.extend_from_slice(bytemuck::cast_slice::<f32, u8>(&q.rotation[..]));
            pad8(&mut out);
            out.extend_from_slice(c);
            pad8(&mut out);
            out.extend_from_slice(bytemuck::cast_slice::<f32, u8>(&n[..]));
            pad8(&mut out);
        }
        Self::parse(Bytes::from_vec(out), num_docs).expect("VECTORS section just written")
    }

    /// The field's VECTORS section in `file`, if it has one.
    pub fn open(file: &SegmentFile, field: FieldId) -> io::Result<Option<Self>> {
        file.section(kind::VECTORS, field).map(|b| Self::parse(b, file.doc_count)).transpose()
    }

    fn parse(bytes: Bytes, segment_docs: u32) -> io::Result<Self> {
        if bytes.len() < 64 {
            return Err(invalid("VECTORS truncated"));
        }
        let doc_count = bytes.u32_at(0);
        let num_dim = u16::from_le_bytes([bytes[4], bytes[5]]);
        let (metric, effective_metric) = (metric_of(bytes[6])?, metric_of(bytes[7])?);
        let ep = bytes.u32_at(8);
        let (max_level, n_layers, quantized) = (bytes.u32_at(12) as usize, bytes.u32_at(16) as usize, bytes[20] != 0);
        if doc_count != segment_docs || num_dim == 0 || n_layers > 64 {
            return Err(invalid("VECTORS header"));
        }
        let dim = num_dim as usize;
        let mut at = 64usize;
        let take = |at: &mut usize, len: usize| -> io::Result<Bytes> {
            if at.checked_add(len).is_none_or(|e| e > bytes.len()) {
                return Err(invalid("VECTORS truncated"));
            }
            let b = bytes.slice(*at, len);
            *at = (*at + len).next_multiple_of(8);
            Ok(b)
        };
        let vectors = Pods::new(take(&mut at, 4 * doc_count as usize * dim)?)?;
        let entry_point = (ep != u32::MAX).then_some(ep);
        let hnsw = HnswView::parse(&bytes, &mut at, n_layers, doc_count, entry_point, max_level)?;
        let (quantizer, codes, norms) = if quantized {
            let rotation = Pods::new(take(&mut at, 4 * dim * dim)?)?;
            let codes = Pods::new(take(&mut at, doc_count as usize * dim.div_ceil(8))?)?;
            let norms = Pods::new(take(&mut at, 4 * doc_count as usize)?)?;
            (Some(RaBitQuantizer::from_rotation(rotation, num_dim)), Some(codes), Some(norms))
        } else {
            (None, None, None)
        };
        Ok(Self {
            bytes,
            vectors,
            num_dim,
            metric,
            effective_metric,
            hnsw,
            quantizer,
            codes,
            norms,
            #[cfg(feature = "cuda")]
            gpu_vectors: None,
            #[cfg(feature = "cuda")]
            gpu_codes: None,
            #[cfg(feature = "cuda")]
            gpu_norms: None,
        })
    }

    pub fn add_sections(&self, field: FieldId, w: &mut SegmentWriter) {
        w.add_bytes(kind::VECTORS, field, self.bytes.clone());
    }

    /// Bytes of the section (heap before the segment is persisted, mapped after).
    pub fn section_bytes(&self) -> usize {
        self.bytes.len()
    }

    /// Return the effective (internal) metric used by HNSW.
    pub fn effective_metric(&self) -> VecDist {
        self.effective_metric
    }

    /// Get the vector for a given doc ID.
    pub fn get_vector(&self, doc_id: DocId) -> &[f32] {
        let dim = self.num_dim as usize;
        let start = doc_id as usize * dim;
        &self.vectors[start..start + dim]
    }

    /// Search for the k nearest neighbors of `query`.
    pub fn search(
        &self,
        query: &[f32],
        k: usize,
        ef_search: usize,
        filter: Option<&Bitmap>,
    ) -> Vec<(DocId, f32)> {
        let dim = self.num_dim as usize;

        // If cosine, normalize the query vector copy to match pre-normalized data
        let query_normalized;
        let search_query = if self.metric == VecDist::Cosine {
            query_normalized = {
                let mut q = query.to_vec();
                normalize_vector(&mut q);
                q
            };
            &query_normalized
        } else {
            query
        };

        let mut results = self.hnsw.search(
            search_query,
            &self.vectors,
            dim,
            &self.effective_metric,
            k,
            ef_search,
            filter,
        );

        // Convert internal distance back to actual metric value
        if self.effective_metric.higher_is_better() {
            for result in &mut results {
                result.1 = -result.1; // un-negate
            }
        } else if self.metric == VecDist::Euclidean {
            // HNSW used squared L2 internally — sqrt for actual distance
            for result in &mut results {
                result.1 = result.1.sqrt();
            }
        }

        results
    }

    /// Brute-force flat scan for small filtered result sets.
    /// Computes exact distances for all candidates in the filter bitmap.
    pub fn flat_search(
        &self,
        query: &[f32],
        k: usize,
        filter: Option<&Bitmap>,
    ) -> Vec<(DocId, f32)> {
        use crate::vector::distance::{dot_product, euclidean_distance};

        let dim = self.num_dim as usize;
        let num_docs = self.vectors.len() / dim;

        // Cosine: normalize query, use dot product (vectors are pre-normalized)
        let query_normalized;
        let search_query = if self.metric == VecDist::Cosine {
            query_normalized = {
                let mut q = query.to_vec();
                normalize_vector(&mut q);
                q
            };
            &query_normalized
        } else {
            query
        };

        let candidates: Box<dyn Iterator<Item = u32>> = if let Some(bm) = filter {
            Box::new(bm.iter())
        } else {
            Box::new(0..num_docs as u32)
        };

        let mut results: Vec<(DocId, f32)> = Vec::new();
        for doc_id in candidates {
            let idx = doc_id as usize;
            if idx * dim + dim > self.vectors.len() { continue; }
            let vec = &self.vectors[idx * dim..idx * dim + dim];

            let dist = match self.metric {
                VecDist::Cosine | VecDist::DotProduct => dot_product(search_query, vec),
                VecDist::Euclidean => euclidean_distance(search_query, vec),
            };
            results.push((doc_id, dist));
        }

        // Sort: higher is better for ip, lower is better for cosine/l2
        let higher = self.metric.higher_is_better();
        results.sort_by(|a, b| {
            if higher {
                b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal)
            } else {
                a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal)
            }
        });
        results.truncate(k);
        results
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn random_vectors(n: usize, dim: usize, seed: u32) -> Vec<f32> {
        let mut state = seed;
        (0..n * dim)
            .map(|_| {
                state = state.wrapping_mul(1103515245).wrapping_add(12345);
                ((state >> 16) as f32 / 32768.0) - 1.0
            })
            .collect()
    }

    #[test]
    fn view_searches_like_the_built_graph_and_round_trips() {
        for (n, dim, metric) in [(600usize, 8u16, VecDist::Euclidean), (2500, 64, VecDist::Cosine), (300, 16, VecDist::DotProduct)] {
            let raw = random_vectors(n, dim as usize, 7 + u32::from(dim));
            let built = VectorIndex::build(raw.clone(), n as u32, dim, metric);
            let mut normed = raw.clone();
            if metric == VecDist::Cosine {
                normalize_vectors(&mut normed, n, dim as usize);
            }
            // The view must search exactly like the graph it was written from (graphs of 2000+
            // vectors are built in parallel, so a second build differs).
            let graph = HnswGraph::build(&normed, n as u32, dim, &built.effective_metric(), 16, 200);
            let mut gbytes = Vec::new();
            HnswView::write(&graph, n as u32, &mut gbytes).unwrap();
            let gbytes = Bytes::from_vec(gbytes);
            let view = HnswView::parse(&gbytes, &mut 0, graph.layers.len(), n as u32, graph.entry_point, graph.max_level).unwrap();
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("segment.tqbf");
            let mut w = SegmentWriter::new();
            built.add_sections(3, &mut w);
            w.write(&path, n as u32, 0).unwrap();
            let file = SegmentFile::open(&path).unwrap();
            let back = VectorIndex::open(&file, 3).unwrap().unwrap();
            assert_eq!(back.vectors[..], built.vectors[..]);
            assert_eq!(back.quantizer.is_some(), dim >= 64);
            if let (Some(a), Some(b)) = (&back.codes, &built.codes) {
                assert_eq!(a[..], b[..]);
                assert_eq!(back.norms.as_ref().unwrap()[..], built.norms.as_ref().unwrap()[..]);
                assert_eq!(back.quantizer.as_ref().unwrap().rotation[..], built.quantizer.as_ref().unwrap().rotation[..]);
            }
            let odd: Vec<u32> = (0..n as u32).filter(|d| d % 3 == 1).collect();
            let filter = Bitmap::of(&odd);
            for q in 0..20 {
                let query = &raw[q * dim as usize..(q + 1) * dim as usize];
                for f in [None, Some(&filter)] {
                    let want = graph.search(
                        &if metric == VecDist::Cosine { normed[q * dim as usize..(q + 1) * dim as usize].to_vec() } else { query.to_vec() },
                        &normed, dim as usize, &built.effective_metric(), 10, 64, f,
                    );
                    let got = view.search(
                        &if metric == VecDist::Cosine { normed[q * dim as usize..(q + 1) * dim as usize].to_vec() } else { query.to_vec() },
                        &normed, dim as usize, &built.effective_metric(), 10, 64, f,
                    );
                    assert_eq!(got, want, "query {q}");
                    assert_eq!(back.search(query, 10, 64, f), built.search(query, 10, 64, f));
                }
            }
        }
    }
}
