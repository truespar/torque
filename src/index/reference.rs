use crate::bitmap::Bitmap;
use crate::index::values::{DocSet, ValueIndex};
use crate::schema::{DocId, FieldId, Schema};
use rustc_hash::FxHashMap;

/// A reference field's JOIN index: the field's value sections (VALUES or NUM_VALUES, VALUE_DOCS,
/// DOC_ORDS), read in place. The target collection is swapped on its own schedule, so references
/// are the target ids themselves, resolved against the target's IDS at query time; no target doc
/// ids are stored.
#[derive(Clone)]
pub struct ReferenceIndex {
    /// Target collection name.
    pub target_collection: String,
    /// Target field name (usually "id").
    pub target_field: String,
    values: ValueIndex,
}

impl ReferenceIndex {
    /// The reference indexes of a segment, over its value indexes.
    pub fn for_segment(schema: &Schema, values: &FxHashMap<FieldId, ValueIndex>) -> FxHashMap<FieldId, Self> {
        schema
            .fields
            .iter()
            .filter_map(|f| {
                let r = f.parsed_reference()?;
                let values = values.get(&f.id)?.clone();
                Some((f.id, Self { target_collection: r.collection, target_field: r.field, values }))
            })
            .collect()
    }

    fn ord_of(&self, target_id: &str) -> Option<u64> {
        if self.values.is_float() {
            None
        } else if let Some(o) = self.values.str_ord(target_id) {
            Some(o)
        } else {
            target_id.parse::<i64>().ok().and_then(|k| self.values.key_ord(k))
        }
    }

    /// The local documents that reference `target_id`.
    pub fn reverse_lookup(&self, target_id: &str) -> Option<Bitmap> {
        self.ord_of(target_id).map(|o| self.values.docs(o))
    }

    /// The local documents that reference any of the target ids.
    pub fn reverse_lookup_many<'a>(&self, target_ids: impl Iterator<Item = &'a str>) -> Bitmap {
        let mut set = DocSet::new(self.values.doc_count());
        for id in target_ids {
            if let Some(o) = self.ord_of(id) {
                self.values.add_to(o, &mut set);
            }
        }
        set.finish()
    }

    /// The target ids a local document references, in array order.
    pub fn forward_lookup(&self, doc_id: DocId) -> Vec<Box<str>> {
        self.ords(doc_id).filter_map(|o| self.target_id(o)).collect()
    }

    /// The ordinals of the target ids a local document references, in array order.
    pub fn ords(&self, doc_id: DocId) -> impl Iterator<Item = u64> + '_ {
        self.values.doc_ords().into_iter().flat_map(move |o| o.elems(doc_id).flatten())
    }

    /// The target id of an ordinal.
    pub fn target_id(&self, ord: u64) -> Option<Box<str>> {
        match self.values.str_at(ord) {
            Some(s) => Some(s.into_boxed_str()),
            None => self.values.key_at(ord).map(|k| k.to_string().into_boxed_str()),
        }
    }
}
