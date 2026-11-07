use crate::schema::DocId;
use std::cmp::Ordering;
use std::collections::BinaryHeap;

#[derive(Debug, Clone)]
pub struct ScoredDoc {
    pub doc_id: DocId,
    pub score: f32,
}

/// Min-heap entry: lowest score sits at the top so we can evict it.
struct MinEntry(ScoredDoc);

impl PartialEq for MinEntry {
    fn eq(&self, other: &Self) -> bool {
        self.0.doc_id == other.0.doc_id
    }
}
impl Eq for MinEntry {}

impl PartialOrd for MinEntry {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for MinEntry {
    fn cmp(&self, other: &Self) -> Ordering {
        // Reversed: lowest score is "greatest" → sits at top of max-heap
        other
            .0
            .score
            .partial_cmp(&self.0.score)
            .unwrap_or(Ordering::Equal)
            .then_with(|| other.0.doc_id.cmp(&self.0.doc_id))
    }
}

/// Collects top-K scored documents using a min-heap.
pub struct TopKCollector {
    heap: BinaryHeap<MinEntry>,
    capacity: usize,
}

impl TopKCollector {
    pub fn new(capacity: usize) -> Self {
        Self {
            heap: BinaryHeap::with_capacity(capacity + 1),
            capacity,
        }
    }

    pub fn push(&mut self, doc_id: DocId, score: f32) {
        if self.heap.len() < self.capacity {
            self.heap.push(MinEntry(ScoredDoc { doc_id, score }));
        } else if let Some(min) = self.heap.peek() {
            if score > min.0.score || (score == min.0.score && doc_id > min.0.doc_id) {
                self.heap.pop();
                self.heap.push(MinEntry(ScoredDoc { doc_id, score }));
            }
        }
    }

    /// Extract results sorted by score descending.
    pub fn into_sorted(self) -> Vec<ScoredDoc> {
        let mut docs: Vec<_> = self.heap.into_iter().map(|e| e.0).collect();
        docs.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(Ordering::Equal)
                .then_with(|| b.doc_id.cmp(&a.doc_id))
        });
        docs
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn top_k_basic() {
        let mut c = TopKCollector::new(3);
        c.push(0, 1.0);
        c.push(1, 5.0);
        c.push(2, 3.0);
        c.push(3, 2.0);
        c.push(4, 4.0);

        let result = c.into_sorted();
        assert_eq!(result.len(), 3);
        assert_eq!(result[0].doc_id, 1); // 5.0
        assert_eq!(result[1].doc_id, 4); // 4.0
        assert_eq!(result[2].doc_id, 2); // 3.0
    }

    #[test]
    fn top_k_fewer_than_capacity() {
        let mut c = TopKCollector::new(10);
        c.push(0, 1.0);
        c.push(1, 2.0);

        let result = c.into_sorted();
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].doc_id, 1);
        assert_eq!(result[1].doc_id, 0);
    }

    #[test]
    fn top_k_100_items() {
        let mut c = TopKCollector::new(10);
        for i in 0..100u32 {
            c.push(i, i as f32);
        }

        let result = c.into_sorted();
        assert_eq!(result.len(), 10);
        // Should be 99, 98, 97, ..., 90
        for (i, doc) in result.iter().enumerate() {
            assert_eq!(doc.doc_id, 99 - i as u32);
        }
    }

    #[test]
    fn top_k_empty() {
        let c = TopKCollector::new(5);
        let result = c.into_sorted();
        assert!(result.is_empty());
    }

    #[test]
    fn top_k_tiebreak_highest_doc_id_first() {
        let mut c = TopKCollector::new(3);
        c.push(10, 5.0);
        c.push(20, 5.0);
        c.push(30, 5.0);
        c.push(5, 5.0);  // lower ID, same score — should not displace higher IDs

        let result = c.into_sorted();
        assert_eq!(result.len(), 3);
        // Highest doc IDs first when scores are equal
        assert_eq!(result[0].doc_id, 30);
        assert_eq!(result[1].doc_id, 20);
        assert_eq!(result[2].doc_id, 10);
    }
}
