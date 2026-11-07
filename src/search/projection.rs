//! include_fields and exclude_fields: what of a document a hit returns,
//! and without highlight_fields, which query_by fields are highlighted.
//!
//! An entry names a field (`name`), a nested field (`address.city`), an object with everything
//! under it (`address`), a pattern (`name*`: every field whose path starts so), or a referenced
//! collection (`$brands(name, country)`: its documents, embedded under its name; a list naming only
//! referenced collections leaves the document whole). With
//! include_fields a document keeps what they name and the objects on the way to it (an array of
//! objects keeps its elements, each pruned); exclude_fields then removes what they name. A scoped
//! API key's lists restrict a search's (`restrict`): its exclusions are added, and what is kept is
//! what both include lists name.
//!
//! Typesense expands entries against its schema, so a pattern misses a stored field the schema
//! lacks and an object's stored-only keys fall out once anything is excluded, and it keeps a key
//! when an entry merely starts with its name (`addr` keeps the included fields of `address`); and a
//! scoped key's lists replace the search's. Here an entry covers a key by its path, a dot at the
//! boundary, and a key's lists only restrict.

use serde_json::{Map, Value};

#[derive(Clone, Debug, PartialEq, Eq)]
enum Entry {
    /// A field, nested field or object.
    Name(String),
    /// `prefix*`.
    Prefix(String),
}

impl Entry {
    fn parse(s: &str) -> Option<Self> {
        let s = s.trim();
        if s.is_empty() {
            return None;
        }
        // a referenced collection's documents sit under its name
        if let Some(rest) = s.strip_prefix('$') {
            return Some(Entry::Name(rest.split('(').next().unwrap_or("").trim().to_string()));
        }
        Some(match s.strip_suffix('*') {
            Some(p) => Entry::Prefix(p.to_string()),
            None => Entry::Name(s.to_string()),
        })
    }

    /// The entry names the path or an object holding it.
    fn covers(&self, path: &str) -> bool {
        match self {
            Entry::Name(n) => path == n || under(path, n),
            Entry::Prefix(p) => path.starts_with(p.as_str()),
        }
    }

    /// The path is an object holding something the entry names.
    fn below(&self, path: &str) -> bool {
        match self {
            Entry::Name(n) | Entry::Prefix(n) => under(n, path),
        }
    }

    /// What both entries name.
    fn intersect(&self, other: &Entry) -> Option<Entry> {
        use Entry::{Name, Prefix};
        match (self, other) {
            (Name(a), Name(b)) => {
                if a == b || under(b, a) {
                    Some(other.clone())
                } else if under(a, b) {
                    Some(self.clone())
                } else {
                    None
                }
            }
            (Name(n), Prefix(p)) | (Prefix(p), Name(n)) => {
                if n.starts_with(p.as_str()) {
                    Some(Name(n.clone()))
                } else if under(p, n) {
                    Some(Prefix(p.clone()))
                } else {
                    None
                }
            }
            (Prefix(a), Prefix(b)) => {
                if a.starts_with(b.as_str()) {
                    Some(self.clone())
                } else if b.starts_with(a.as_str()) {
                    Some(other.clone())
                } else {
                    None
                }
            }
        }
    }
}

/// `path` lies under the object `parent`.
fn under(path: &str, parent: &str) -> bool {
    path.strip_prefix(parent).is_some_and(|r| r.starts_with('.'))
}

fn entries(list: Option<&str>) -> Vec<Entry> {
    list.map(|l| crate::index::facet::split_top(l).into_iter().filter_map(Entry::parse).collect()).unwrap_or_default()
}

/// include_fields' entries; none when it names only referenced collections (Typesense then
/// returns the whole document, the referenced documents embedded).
fn inclusions(list: Option<&str>) -> Vec<Entry> {
    let only_references = list.is_none_or(|l| crate::index::facet::split_top(l).iter().all(|e| e.trim().is_empty() || e.trim().starts_with('$')));
    if only_references { Vec::new() } else { entries(list) }
}

/// exclude_fields' entries for documents: `out_of` and `conversation_history` name keys of the
/// response, which Typesense leaves out of it instead.
fn exclusions(list: Option<&str>) -> Vec<Entry> {
    entries(list).into_iter().filter(|e| !matches!(e, Entry::Name(n) if n == "out_of" || n == "conversation_history")).collect()
}

/// A search's include and exclude lists.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Projection {
    /// None: everything.
    include: Option<Vec<Entry>>,
    exclude: Vec<Entry>,
}

impl Projection {
    pub fn new(include_fields: Option<&str>, exclude_fields: Option<&str>) -> Self {
        let include = inclusions(include_fields);
        Projection { include: (!include.is_empty()).then_some(include), exclude: exclusions(exclude_fields) }
    }

    /// Restrict by a scoped API key's lists.
    pub fn restrict(&mut self, include_fields: Option<&str>, exclude_fields: Option<&str>) {
        let key = inclusions(include_fields);
        if !key.is_empty() {
            self.include = Some(match self.include.take() {
                None => key,
                Some(own) => own.iter().flat_map(|a| key.iter().filter_map(move |b| a.intersect(b))).collect(),
            });
        }
        self.exclude.extend(exclusions(exclude_fields));
    }

    /// Every document returned whole.
    pub fn is_empty(&self) -> bool {
        self.include.is_none() && self.exclude.is_empty()
    }

    /// The field at `path` is returned.
    pub fn allows(&self, path: &str) -> bool {
        self.include.as_ref().is_none_or(|inc| inc.iter().any(|e| e.covers(path))) && !self.exclude.iter().any(|e| e.covers(path))
    }

    /// Remove from `doc` what a hit does not return.
    pub fn prune(&self, doc: &mut Value) {
        if let Value::Object(m) = doc {
            if !self.is_empty() {
                self.prune_object(m, "");
            }
        }
    }

    fn prune_object(&self, obj: &mut Map<String, Value>, parent: &str) {
        obj.retain(|k, v| {
            let path = if parent.is_empty() { k.clone() } else { format!("{parent}.{k}") };
            if self.exclude.iter().any(|e| e.covers(&path)) {
                return false;
            }
            let included = self.include.as_ref().is_none_or(|inc| inc.iter().any(|e| e.covers(&path)));
            if included && !self.exclude.iter().any(|e| e.below(&path)) {
                return true;
            }
            if !included && !self.include.iter().flatten().any(|e| e.below(&path)) {
                return false;
            }
            // partly returned: what under it is
            match v {
                Value::Object(m) => {
                    let had = !m.is_empty();
                    self.prune_object(m, &path);
                    !(had && m.is_empty())
                }
                Value::Array(a) if a.iter().any(Value::is_object) => {
                    for e in a.iter_mut() {
                        if let Value::Object(m) = e {
                            self.prune_object(m, &path);
                        }
                    }
                    true
                }
                _ => included,
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn pruned(include: Option<&str>, exclude: Option<&str>, doc: Value) -> Value {
        let mut doc = doc;
        Projection::new(include, exclude).prune(&mut doc);
        doc
    }

    fn person() -> Value {
        json!({
            "id": "1", "firstName": "Anna", "lastName": "Berg",
            "spar": {"personDetaljer_Kod": "A", "adress_PostNr": "11122", "adress_Postort": "Stockholm"},
            "names": [{"name": "Anna", "type": "first"}, {"name": "Berg", "type": "last"}],
            "tags": ["a", "b"]
        })
    }

    #[test]
    fn include_top_level_nested_and_patterns() {
        assert_eq!(pruned(Some("id,firstName"), None, person()), json!({"id": "1", "firstName": "Anna"}));
        assert_eq!(
            pruned(Some("firstName, spar.adress_PostNr,spar.adress_Postort"), None, person()),
            json!({"firstName": "Anna", "spar": {"adress_PostNr": "11122", "adress_Postort": "Stockholm"}})
        );
        assert_eq!(pruned(Some("spar"), None, person())["spar"]["personDetaljer_Kod"], "A");
        assert_eq!(pruned(Some("spar.adress*"), None, person()), json!({"spar": {"adress_PostNr": "11122", "adress_Postort": "Stockholm"}}));
        assert_eq!(pruned(Some("*Name"), None, person()), json!({}), "a pattern is a prefix");
        assert_eq!(pruned(Some("first*"), None, person()), json!({"firstName": "Anna"}));
        // an array of objects keeps its elements, pruned; a name prefix is no path
        assert_eq!(pruned(Some("names.name"), None, person()), json!({"names": [{"name": "Anna"}, {"name": "Berg"}]}));
        assert_eq!(pruned(Some("spa"), None, person()), json!({}));
        assert_eq!(pruned(Some("tags"), None, person())["tags"], json!(["a", "b"]));
    }

    #[test]
    fn exclude_top_level_and_nested() {
        let doc = pruned(None, Some("spar.adress_PostNr,tags"), person());
        assert_eq!(doc["spar"], json!({"personDetaljer_Kod": "A", "adress_Postort": "Stockholm"}));
        assert!(doc.get("tags").is_none());
        assert_eq!(pruned(Some("spar"), Some("spar.adress*"), person()), json!({"spar": {"personDetaljer_Kod": "A"}}));
        // an object emptied by exclusions goes
        assert_eq!(pruned(Some("spar"), Some("spar.personDetaljer_Kod,spar.adress*"), person()), json!({}));
    }

    #[test]
    fn allows_for_highlighting() {
        let p = Projection::new(Some("names,spar.adress_Postort"), Some("names.type"));
        assert!(p.allows("names.name"));
        assert!(!p.allows("names.type"));
        assert!(p.allows("spar.adress_Postort"));
        assert!(!p.allows("spar.adress_PostNr"));
        assert!(!p.allows("firstName"));
        assert!(Projection::default().allows("anything"));
    }

    #[test]
    fn a_scoped_key_restricts() {
        let mut p = Projection::new(Some("firstName,spar"), Some("tags"));
        p.restrict(Some("spar.adress*,lastName"), Some("spar.adress_PostNr"));
        let mut doc = person();
        p.prune(&mut doc);
        assert_eq!(doc, json!({"spar": {"adress_Postort": "Stockholm"}}));
        // nothing in common: nothing returned
        let mut p = Projection::new(Some("firstName"), None);
        p.restrict(Some("lastName"), None);
        let mut doc = person();
        p.prune(&mut doc);
        assert_eq!(doc, json!({}));
        // the key's list when the search has none
        let mut p = Projection::default();
        p.restrict(Some("id"), None);
        let mut doc = person();
        p.prune(&mut doc);
        assert_eq!(doc, json!({"id": "1"}));
    }

    #[test]
    fn references_keep_their_key() {
        let doc = json!({"id": "p1", "title": "Shoe", "brands": {"name": "Nike"}});
        assert_eq!(pruned(Some("title, $brands(name, country)"), None, doc.clone()), json!({"title": "Shoe", "brands": {"name": "Nike"}}));
        // naming only referenced collections keeps the whole document (Typesense)
        assert_eq!(pruned(Some("$brands(name)"), None, doc.clone()), doc);
    }
}
