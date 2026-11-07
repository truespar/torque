use crate::schema::DocId;
use rustc_hash::FxHashMap;

/// Reciprocal Rank Fusion: merge lexical and vector result lists.
/// Each result is (DocId, score). The k parameter (default 60.0) controls
/// how much rank position matters vs. absolute position.
pub fn reciprocal_rank_fusion(
    lexical_results: &[(DocId, f32)],
    vector_results: &[(DocId, f32)],
    k: f32,
) -> Vec<(DocId, f32)> {
    let mut rrf_scores: FxHashMap<DocId, f32> = FxHashMap::default();

    for (rank, &(doc_id, _)) in lexical_results.iter().enumerate() {
        *rrf_scores.entry(doc_id).or_default() += 1.0 / (k + rank as f32 + 1.0);
    }
    for (rank, &(doc_id, _)) in vector_results.iter().enumerate() {
        *rrf_scores.entry(doc_id).or_default() += 1.0 / (k + rank as f32 + 1.0);
    }

    let mut fused: Vec<(DocId, f32)> = rrf_scores.into_iter().collect();
    fused.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    fused
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rrf_overlapping_results() {
        let lexical = vec![(0, 10.0), (1, 8.0), (2, 6.0)];
        let vector = vec![(1, 0.9), (0, 0.8), (3, 0.7)];

        let fused = reciprocal_rank_fusion(&lexical, &vector, 60.0);

        // Doc 0: rank 0 in lexical (1/61) + rank 1 in vector (1/62)
        // Doc 1: rank 1 in lexical (1/62) + rank 0 in vector (1/61)
        // Both should have same RRF score (1/61 + 1/62)
        assert!(fused.len() == 4); // 0, 1, 2, 3
        // Doc 0 and 1 should be top 2
        let top_ids: Vec<DocId> = fused.iter().take(2).map(|&(id, _)| id).collect();
        assert!(top_ids.contains(&0));
        assert!(top_ids.contains(&1));
    }

    #[test]
    fn rrf_disjoint_results() {
        let lexical = vec![(0, 10.0), (1, 8.0)];
        let vector = vec![(2, 0.9), (3, 0.8)];

        let fused = reciprocal_rank_fusion(&lexical, &vector, 60.0);

        assert_eq!(fused.len(), 4);
        // All should have single-source scores: 1/(60+rank+1)
        for &(_, score) in &fused {
            assert!(score > 0.0);
        }
    }

    #[test]
    fn rrf_empty_lists() {
        let fused = reciprocal_rank_fusion(&[], &[], 60.0);
        assert!(fused.is_empty());
    }

    #[test]
    fn rrf_one_empty() {
        let lexical = vec![(0, 10.0), (1, 8.0)];
        let fused = reciprocal_rank_fusion(&lexical, &[], 60.0);

        assert_eq!(fused.len(), 2);
        // Doc 0 should rank higher (rank 0 vs rank 1)
        assert_eq!(fused[0].0, 0);
    }

    #[test]
    fn rrf_doc_in_both_ranks_higher() {
        let lexical = vec![(0, 10.0), (1, 8.0)];
        let vector = vec![(0, 0.9), (2, 0.8)];

        let fused = reciprocal_rank_fusion(&lexical, &vector, 60.0);

        // Doc 0 appears in both lists → should have highest RRF score
        assert_eq!(fused[0].0, 0);
    }
}
