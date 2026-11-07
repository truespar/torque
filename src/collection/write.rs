//! A realtime collection's segments under writes. A write (or the records a write-ahead log
//! replays) becomes a segment of its own after the collection's, every copy of an id it writes or
//! deletes hidden in its segment (`apply`): nothing is indexed twice, and a reader holds a bundle
//! that never changes. Segments merge in the background (`merge`), and a merge carries every
//! deletion made in the segments it read while it ran over to the merged segment (`publish`), so
//! merging never blocks a write nor undoes one. Every document keeps its `_seq_id`.

use crate::bitmap::Bitmap;
use crate::index::{IndexBundle, Segment, SeqIds};
use crate::schema::{DocId, Document};
use rustc_hash::{FxHashMap, FxHashSet};
use std::ops::Range;
use std::sync::Arc;

/// Writes applied to a bundle as one, upserts and deletes in the order made (one write's upserts
/// then its deletes, or every record a log replays): the last version of each document written,
/// in the order first written; one deleted after it was written gone; one deleted and written
/// again new (it does not keep the place of the copy it replaces, as Typesense gives a re-created
/// document a new sequence id).
#[derive(Default)]
pub struct Writes {
    docs: Vec<Option<Document>>,
    at: FxHashMap<String, usize>,
    deleted: FxHashSet<String>,
    fresh: FxHashSet<String>,
}

impl Writes {
    pub fn upsert(&mut self, doc: Document) {
        match self.at.get(&doc.external_id) {
            Some(&i) => self.docs[i] = Some(doc),
            None => {
                if self.deleted.remove(&doc.external_id) {
                    self.fresh.insert(doc.external_id.clone());
                }
                self.at.insert(doc.external_id.clone(), self.docs.len());
                self.docs.push(Some(doc));
            }
        }
    }

    pub fn delete(&mut self, id: &str) {
        if let Some(i) = self.at.remove(id) {
            self.docs[i] = None;
            self.fresh.remove(id);
        }
        self.deleted.insert(id.to_string());
    }

    pub fn is_empty(&self) -> bool {
        self.at.is_empty() && self.deleted.is_empty()
    }
}

/// What `writes` make of `bundle`: their documents as one more segment, in memory (the last of
/// `IndexBundle::memory`), in `_seq_id` order: those written again keep the id of the copy they
/// replace, the new ones get the next ones (`IndexBundle::next_seq`). Every copy of an id written
/// or deleted is hidden in its segment; a segment's deletions are copied only when they change.
pub fn apply(bundle: &IndexBundle, writes: Writes) -> IndexBundle {
    let Writes { docs, deleted, fresh, .. } = writes;
    let mut masks: FxHashMap<usize, Bitmap> = FxHashMap::default();
    let mut hidden = 0u32;
    // Hide every copy of the id; the `_seq_id` of the live one.
    let mut hide = |id: &str| -> Option<u64> {
        let mut live = None;
        for (i, seg) in bundle.segments.iter().enumerate() {
            let Some(doc) = seg.ext_id_lookup(id) else { continue };
            let mask = masks.entry(i).or_insert_with(|| bundle.deleted_for(i).cloned().unwrap_or_default());
            if !mask.contains(doc) {
                live.get_or_insert(seg.seq_id(doc));
                mask.add(doc);
                hidden += 1;
            }
        }
        live
    };
    let mut rewrites: Vec<(u64, Document)> = Vec::new();
    let mut new_docs: Vec<Document> = Vec::new();
    for doc in docs.into_iter().flatten() {
        match hide(&doc.external_id) {
            Some(seq) if !fresh.contains(&doc.external_id) => rewrites.push((seq, doc)),
            _ => new_docs.push(doc),
        }
    }
    for id in &deleted {
        hide(id);
    }
    rewrites.sort_unstable_by_key(|r| r.0);

    let mut next_seq = bundle.next_seq();
    let mut seqs: Vec<u64> = rewrites.iter().map(|r| r.0).collect();
    let mut seg_docs: Vec<Document> = rewrites.into_iter().map(|r| r.1).collect();
    for doc in new_docs {
        seqs.push(next_seq);
        next_seq += 1;
        seg_docs.push(doc);
    }

    let mut segments = bundle.segments.clone();
    let mut deleted_per_segment = bundle.deleted_per_segment.clone();
    for (i, mask) in masks {
        deleted_per_segment[i] = Arc::new(mask);
    }
    let mut total_docs = bundle.total_docs - hidden;
    let mut memory = bundle.memory;
    if !seg_docs.is_empty() {
        // no dictionary trained for a segment merged soon
        let mut segment = crate::index::docstore::untrained(|| crate::ingest::build_index(&bundle.schema, seg_docs));
        segment.base_doc_id = bundle.next_base();
        segment.seq = SeqIds::from_ids(seqs);
        total_docs += segment.total_docs();
        if segments.len() == 1 && segments[0].total_docs() == 0 {
            // an empty collection's placeholder segment goes
            segments[0] = Arc::new(segment);
            deleted_per_segment[0] = Arc::new(Bitmap::new());
            memory = 1;
        } else {
            segments.push(Arc::new(segment));
            deleted_per_segment.push(Arc::new(Bitmap::new()));
            memory += 1;
        }
    }
    IndexBundle {
        schema: Arc::clone(&bundle.schema),
        segments,
        deleted_per_segment,
        total_docs,
        lsn: bundle.lsn,
        next_seq: next_seq.max(bundle.next_seq),
        memory,
    }
}

/// A merge's result: the merged segment, and for each of its documents where it came from (the
/// segment's place in the merged range, and the document there).
pub struct Merged {
    pub segment: Segment,
    sources: Vec<(u32, DocId)>,
}

impl Merged {
    pub fn docs(&self) -> usize {
        self.sources.len()
    }
}

/// Merge `snapshot`'s segments `range` into one of their live documents in `_seq_id` order, made
/// from their sections (`index::merge`: no document parsed or indexed again): each keeps its id,
/// the segment the first one's base. Deleted documents go.
pub fn merge(snapshot: &IndexBundle, range: Range<usize>) -> std::io::Result<Merged> {
    let mut live: Vec<(u64, u32, DocId)> = Vec::new();
    for (o, i) in range.clone().enumerate() {
        let seg = &snapshot.segments[i];
        let deleted = snapshot.deleted_for(i).filter(|d| !d.is_empty());
        for doc in 0..seg.total_docs() {
            if !deleted.is_some_and(|d| d.contains(doc)) {
                live.push((seg.seq_id(doc), o as u32, doc));
            }
        }
    }
    live.sort_unstable_by_key(|l| l.0);
    let sources: Vec<&Segment> = snapshot.segments[range.clone()].iter().map(|s| &**s).collect();
    let order: Vec<(u32, DocId)> = live.iter().map(|l| (l.1, l.2)).collect();
    let mut segment = crate::index::merge::merge(&snapshot.schema, &sources, &order, dictionary(snapshot))?;
    segment.base_doc_id = snapshot.segments[range.start].base_doc_id;
    segment.seq = SeqIds::from_ids(live.iter().map(|l| l.0));
    Ok(Merged { segment, sources: order })
}

/// The dictionary the collection's stored documents are compressed with: its largest segment's.
/// Every merge compresses with it, so a block of a merged segment can be copied as it is into
/// the next merge (`index::DocStore::merge`).
pub(crate) fn dictionary(bundle: &IndexBundle) -> Option<&[u8]> {
    bundle.segments.iter().filter(|s| !s.doc_store.dictionary().is_empty()).max_by_key(|s| s.total_docs()).map(|s| s.doc_store.dictionary())
}

/// `current` with `snapshot`'s segments `range` replaced by `merged` (in memory, or for the
/// disk): None when they are no longer `current`'s (a publisher replaced the segments since; the
/// merge is dropped). Segments are known by id: a save puts a segment's file, mapped, in place of
/// the same built in memory. A write appends segments and hides documents in the ones it finds; a
/// document of the range hidden since the snapshot is hidden in the merged segment too, so no
/// write made during the merge is undone. A merge of no live document leaves no segment, unless
/// the range was every segment (a bundle keeps one).
pub fn publish(current: &IndexBundle, snapshot: &IndexBundle, range: Range<usize>, merged: Merged, in_memory: bool) -> Option<IndexBundle> {
    if range.end > current.segments.len() || !range.clone().all(|i| current.segments[i].id == snapshot.segments[i].id) {
        return None;
    }
    let mut mask = Bitmap::new();
    for (k, &(o, doc)) in merged.sources.iter().enumerate() {
        if current.deleted_for(range.start + o as usize).is_some_and(|d| d.contains(doc)) {
            mask.add(k as DocId);
        }
    }
    let keep = merged.segment.total_docs() > 0 || range.len() == current.segments.len();
    let tail_start = current.segments.len() - current.memory;
    let in_tail = range.end.saturating_sub(tail_start.max(range.start));
    let mut segments = Vec::with_capacity(current.segments.len() - range.len() + 1);
    segments.extend_from_slice(&current.segments[..range.start]);
    let mut deleted_per_segment = Vec::with_capacity(segments.capacity());
    deleted_per_segment.extend_from_slice(&current.deleted_per_segment[..range.start]);
    if keep {
        segments.push(Arc::new(merged.segment));
        deleted_per_segment.push(Arc::new(mask));
    }
    segments.extend_from_slice(&current.segments[range.end..]);
    deleted_per_segment.extend_from_slice(&current.deleted_per_segment[range.end..]);
    Some(IndexBundle {
        schema: Arc::clone(&current.schema),
        segments,
        deleted_per_segment,
        // the merged segment's live documents are the range's live ones
        total_docs: current.total_docs,
        lsn: current.lsn,
        next_seq: current.next_seq(),
        memory: current.memory - in_tail + usize::from(in_memory && keep),
    })
}

/// A segment's size tier: how many digits its live document count has.
fn tier(bundle: &IndexBundle, i: usize) -> u32 {
    let live = bundle.segments[i].total_docs() - bundle.deleted_for(i).map_or(0, |d| d.cardinality() as u32);
    live.max(1).ilog10()
}

/// Segments of one tier merge once `MERGE_FACTOR` of them are side by side: each document is
/// rebuilt about once per tier (log10 of the collection's size times), and a search fans out over
/// at most `MERGE_FACTOR - 1` segments per tier.
pub const MERGE_FACTOR: usize = 10;

/// A flushed segment whose deleted copies are this share of its documents is merged on its own
/// (`reclaim`), which drops them: a segment whose documents are written again or deleted would
/// otherwise keep them until a run of its tier forms, and big segments rarely form one. Each
/// document deleted costs at most 1 / RECLAIM_SHARE documents merged again, and a segment holds at
/// most 1 / (1 - RECLAIM_SHARE) times its live documents.
pub const RECLAIM_SHARE: f64 = 0.2;

/// The segment among `candidates` with the largest share of deleted copies, when that is
/// `RECLAIM_SHARE` or more.
pub fn reclaim(bundle: &IndexBundle, candidates: Range<usize>) -> Option<Range<usize>> {
    candidates
        .map(|i| (i, bundle.deleted_for(i).map_or(0, |d| d.cardinality()) as f64 / f64::from(bundle.segments[i].total_docs().max(1))))
        .filter(|&(_, share)| share >= RECLAIM_SHARE)
        .max_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(i, _)| i..i + 1)
}

/// The newest run of `MERGE_FACTOR` or more neighbouring segments of one tier among `candidates`
/// (a range of `bundle`'s segments), if any.
pub fn merge_run(bundle: &IndexBundle, candidates: Range<usize>) -> Option<Range<usize>> {
    let mut end = candidates.end;
    while end > candidates.start {
        let t = tier(bundle, end - 1);
        let mut start = end - 1;
        while start > candidates.start && tier(bundle, start - 1) == t {
            start -= 1;
        }
        if end - start >= MERGE_FACTOR {
            return Some(start..end);
        }
        end = start;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::{FieldValue, Language, Schema};

    fn schema() -> Arc<Schema> {
        let mut s: Schema = serde_json::from_value(serde_json::json!({"name": "t", "fields": [{"name": "title", "type": "string"}]})).unwrap();
        s.assign_field_ids();
        Arc::new(s)
    }

    fn doc(id: &str, title: &str) -> Document {
        Document { id: 0, external_id: id.into(), language: Language::English, fields: vec![FieldValue::Text(title.into())].into(), record: None }
    }

    fn write(bundle: &IndexBundle, upserts: &[(&str, &str)], deletes: &[&str]) -> IndexBundle {
        let mut w = Writes::default();
        for (id, title) in upserts {
            w.upsert(doc(id, title));
        }
        for id in deletes {
            w.delete(id);
        }
        apply(bundle, w)
    }

    /// The live documents, in `_seq_id` order, with their ids and titles.
    fn live(bundle: &IndexBundle) -> Vec<(u64, String, String)> {
        let mut out = Vec::new();
        for (i, seg) in bundle.segments.iter().enumerate() {
            for d in 0..seg.total_docs() {
                if bundle.deleted_for(i).is_some_and(|m| m.contains(d)) {
                    continue;
                }
                let doc = seg.get_doc(d, &bundle.schema).unwrap();
                let title = match doc.fields.get(0) {
                    Some(FieldValue::Text(t)) => t.clone(),
                    _ => String::new(),
                };
                out.push((seg.seq_id(d), doc.external_id, title));
            }
        }
        out.sort();
        assert_eq!(out.len() as u32, bundle.total_docs, "total_docs counts the live documents");
        out
    }

    fn ids(bundle: &IndexBundle) -> Vec<String> {
        live(bundle).into_iter().map(|l| l.1).collect()
    }

    #[test]
    fn a_write_keeps_the_place_of_what_it_writes_again() {
        let b = IndexBundle::empty(schema());
        let b = write(&b, &[("a", "1"), ("b", "1"), ("c", "1")], &[]);
        assert_eq!(b.segments.len(), 1, "the empty placeholder goes");
        let b = write(&b, &[("d", "1"), ("b", "2")], &["c"]);
        let b = write(&b, &[("c", "2")], &[]);
        assert_eq!(ids(&b), ["a", "b", "d", "c"], "b keeps its place, c deleted and written again is new");
        assert_eq!(live(&b)[1], (1, "b".into(), "2".into()));
        assert_eq!(b.memory, 3);
        assert_eq!(b.next_seq(), 5);
    }

    #[test]
    fn writes_replayed_as_one_order_documents_as_written_one_by_one() {
        let base = write(&IndexBundle::empty(schema()), &[("a", "1"), ("b", "1"), ("c", "1")], &[]);
        let one_by_one = write(&write(&write(&base, &[("d", "1")], &["b"]), &[("b", "2"), ("a", "2")], &[]), &[("e", "1")], &["d"]);
        let mut w = Writes::default();
        w.upsert(doc("d", "1"));
        w.delete("b");
        w.upsert(doc("b", "2"));
        w.upsert(doc("a", "2"));
        w.upsert(doc("e", "1"));
        w.delete("d");
        let replayed = apply(&base, w);
        let order = |b: &IndexBundle| live(b).into_iter().map(|l| (l.1, l.2)).collect::<Vec<_>>();
        assert_eq!(order(&replayed), order(&one_by_one));
        assert_eq!(order(&replayed), [("a".to_string(), "2".to_string()), ("c".into(), "1".into()), ("b".into(), "2".into()), ("e".into(), "1".into())]);
    }

    #[test]
    fn a_merge_keeps_every_id_and_the_deletions_made_while_it_ran() {
        let mut b = IndexBundle::empty(schema());
        for n in 0..12 {
            b = write(&b, &[(&format!("d{n}"), "1")], &[]);
        }
        b = write(&b, &[("d3", "2")], &["d5"]);
        assert_eq!(b.memory, 13);
        let before = live(&b);
        let snapshot = b.clone();
        let range = 0..10;
        let merged = merge(&snapshot, range.clone()).unwrap();
        // a write while the merge built: it rewrites d1 and deletes d7, both in the merged range
        let current = write(&snapshot, &[("d1", "3")], &["d7"]);
        let published = publish(&current, &snapshot, range, merged, true).unwrap();
        assert_eq!(published.segments.len(), current.segments.len() - 9);
        assert_eq!(published.memory, current.memory - 9);
        let after = live(&published);
        assert_eq!(after, live(&current), "the merge changes no document");
        assert_eq!(after.iter().find(|l| l.1 == "d1").unwrap(), &(1, "d1".into(), "3".into()));
        assert!(!after.iter().any(|l| l.1 == "d7"));
        assert_eq!(before.len() - 1, after.len());
        assert_eq!(published.segments[0].seq.ids.len(), 3, "ids 0-9 less d3, d5 and d7, which are later or gone: three runs");
    }

    #[test]
    fn a_merge_of_segments_a_publisher_replaced_is_dropped() {
        let mut b = IndexBundle::empty(schema());
        for n in 0..3 {
            b = write(&b, &[(&format!("d{n}"), "1")], &[]);
        }
        let snapshot = b.clone();
        let merged = merge(&snapshot, 0..2).unwrap();
        let other = merge(&snapshot, 0..3).unwrap();
        let current = publish(&snapshot, &snapshot, 0..3, other, true).unwrap();
        assert!(publish(&current, &snapshot, 0..2, merged, true).is_none());
    }

    #[test]
    fn runs_merge_by_tier_newest_first() {
        let mut b = IndexBundle::empty(schema());
        for n in 0..9 {
            b = write(&b, &[(&format!("d{n}"), "1")], &[]);
        }
        let all = 0..b.segments.len();
        assert_eq!(merge_run(&b, all), None, "nine of one tier");
        b = write(&b, &[("d9", "1")], &[]);
        assert_eq!(merge_run(&b, 0..b.segments.len()), Some(0..10));
        let many: Vec<(String, &str)> = (10..30).map(|n| (format!("d{n}"), "1")).collect();
        let refs: Vec<(&str, &str)> = many.iter().map(|(a, t)| (a.as_str(), *t)).collect();
        b = write(&b, &refs, &[]);
        assert_eq!(merge_run(&b, 0..b.segments.len()), Some(0..10), "the tier-1 segment after them is a run of its own");
    }

    #[test]
    fn a_segment_with_many_deleted_copies_merges_alone() {
        let names: Vec<String> = (0..10).map(|n| format!("d{n}")).collect();
        let ten: Vec<(&str, &str)> = names.iter().map(|n| (n.as_str(), "1")).collect();
        let mut b = write(&IndexBundle::empty(schema()), &ten, &[]);
        b = write(&b, &[("x", "1"), ("d1", "2")], &[]);
        assert_eq!(reclaim(&b, 0..2), None, "one of ten copies hidden");
        b = write(&b, &[], &["d2"]);
        assert_eq!(reclaim(&b, 0..2), Some(0..1), "two of ten");
        let merged = merge(&b, 0..1).unwrap();
        let after = publish(&b, &b, 0..1, merged, true).unwrap();
        assert_eq!(live(&after), live(&b));
        assert_eq!((after.segments[0].total_docs(), after.memory), (8, b.memory));
        assert_eq!(reclaim(&after, 0..2), None);
    }

    #[test]
    fn a_merge_of_no_live_document_leaves_no_segment() {
        let mut b = IndexBundle::empty(schema());
        b = write(&b, &[("a", "1"), ("b", "1")], &[]);
        b = write(&b, &[("c", "1")], &[]);
        b = write(&b, &[("d", "1")], &["a", "b"]);
        let merged = merge(&b, 0..1).unwrap();
        let after = publish(&b, &b, 0..1, merged, true).unwrap();
        assert_eq!((after.segments.len(), after.memory), (2, 2));
        assert_eq!(ids(&after), ["c", "d"]);
        // a merge of every segment keeps one
        let none = write(&after, &[], &["c", "d"]);
        let merged = merge(&none, 0..2).unwrap();
        let empty = publish(&none, &none, 0..2, merged, true).unwrap();
        assert_eq!((empty.segments.len(), empty.total_docs, empty.memory), (1, 0, 1));
        assert_eq!(empty.next_seq(), 4, "ids go on after every one given out");
    }

    #[test]
    fn seq_ids_take_runs() {
        let s = SeqIds::from_ids([4, 5, 6, 10, 11, 20]);
        assert_eq!((s.docs.clone(), s.ids.clone()), (vec![0, 3, 5], vec![4, 10, 20]));
        assert_eq!((0..6).map(|d| s.get(d).unwrap()).collect::<Vec<_>>(), [4, 5, 6, 10, 11, 20]);
    }
}
