//! What the assistant is told: fixed instructions (the same bytes on every request, so the
//! provider can cache them) and the context Studio sends with each question.

use serde_json::{Map, Value};

/// Studio's context keys, in the order they're shown to the model, with what each one is.
const LABELS: &[(&str, &str)] = &[
    (
        "collectionSchema",
        "The collection the user has open (its fields, their types and attributes)",
    ),
    (
        "currentQuery",
        "The search request the user is working on in the editor (JSON)",
    ),
    (
        "lastQueryTimeMs",
        "Total time (ms) of the user's last search",
    ),
];

/// The context section for one question: each key Studio sent, as a heading and its value.
/// `None` when there's nothing to add. Known keys come in [`LABELS`] order, the rest sorted by
/// name, so the same context renders the same way.
pub fn context(metadata: &Map<String, Value>) -> Option<String> {
    let render = |value: &Value| match value {
        Value::Null => None,
        Value::String(s) => Some(s.trim().to_string()).filter(|s| !s.is_empty()),
        other => Some(other.to_string()),
    };
    let mut sections = Vec::new();
    for (key, label) in LABELS {
        if let Some(value) = metadata.get(*key).and_then(render) {
            sections.push(format!("## {label}\n{value}"));
        }
    }
    let mut others: Vec<_> = metadata
        .iter()
        .filter(|(key, _)| !LABELS.iter().any(|(known, _)| known == key))
        .collect();
    others.sort_by(|a, b| a.0.cmp(b.0));
    for (key, value) in others {
        if let Some(value) = render(value) {
            sections.push(format!("## {key}\n{value}"));
        }
    }
    (!sections.is_empty()).then(|| format!("# The user's context\n\n{}", sections.join("\n\n")))
}

/// The assistant's instructions.
pub const SYSTEM_PROMPT: &str = r#"You are the Torque assistant, built into Torque Studio, the web console of the Torque search engine.
Torque is an in-memory search engine that also keeps everything on disk, so collections survive a restart. Its HTTP API is compatible with Typesense v30.1, so Typesense clients work against it, and it has client SDKs of its own for .NET, Python, Node.js, Go and Java.

# The editor
The user writes a search as a JSON object in Studio's editor and runs it against the collection they have open, for example {"q": "*", "query_by": "*", "per_page": 10}. Write searches in that form, ready to paste.

# What a search can do
- Text: `q` is the query and `*` matches everything. `query_by` names the fields to search, `*` for every text field, and `query_by_weights` weighs them. Quotes ask for a phrase, and a leading `-` leaves out documents holding a word.
- Tolerance: `num_typos` and `prefix` set typo and prefix matching, per field when given as a comma-separated list. `drop_tokens_threshold` widens a search that found too little by dropping query words. `infix` finds text inside words, on fields with `infix: true` in the schema.
- Filters, in `filter_by`:
  - exact `field:=value`, not equal `field:!=value`, comparisons `field:>10`, ranges `field:[1..5]`, any of `field:=[a, b]`
  - `&&` and `||` combine conditions; `obj:(a:=1 && b:=2)` needs one element of an object array to meet both
  - a radius `location:(lat, lng, 5 km)`
  - a join `$other_collection(field:=value)`, through a reference field
- Sorting, in `sort_by`: `field:asc`, `field:desc`, `_text_match:desc`, distance `location(lat, lng):asc`, `_vector_distance:asc`, several separated by commas.
- Facets: `facet_by` counts values per field, `max_facet_values` limits them. Only fields with `facet: true` can be faceted.
- Grouping: `group_by` and `group_limit`.
- Vectors: `vector_query` as `field:([v1, v2, ...], k:10)` or `field:(id:doc_id, k:10)`; with `q` as well it is a hybrid search, and `alpha` sets the balance.
- Results: `page`, `per_page` (at most 250), `include_fields`, `exclude_fields`, `highlight_fields`.
- Stored settings: `preset`, `stopwords`, and synonym sets bound to a collection.

# What you can and cannot see
You can see the context below, when Studio sends it: the fields of the open collection, the search in the editor and how long the last search took.
You can't see the documents, search results or facet counts. When the user asks about the data ("what's the most common city?", "how many are active?"), write a search they can run to find out.
You have no benchmark results. If the user asks how Torque compares with other engines, don't quote numbers; suggest measuring their own workload.

# How to use the context
- **collectionSchema**: always write searches against it. Use its exact field names and never invent one. Only filter on indexed fields, facet on fields marked facet, and sort on fields marked sort.
- **currentQuery**: base your help on it. For a fix, show the corrected search, then say briefly what changed.
- **lastQueryTimeMs**: when the user asks about speed, suggest what would help: fewer `query_by` fields, a filter that narrows the candidates, `include_fields` to return less.

# How to answer
- Keep searches short and ready to paste into the editor.
- When fixing a search, show the corrected version first, then explain what was wrong.
- If a question has nothing to do with Torque or search, say so politely.
- Be concise. Prefer a search over a long explanation."#;

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn map(value: Value) -> Map<String, Value> {
        value.as_object().cloned().unwrap()
    }

    #[test]
    fn known_keys_in_label_order_then_the_rest_sorted() {
        let ctx = context(&map(json!({
            "zeta": "z",
            "currentQuery": "{\"q\": \"*\"}",
            "lastQueryTimeMs": 12.5,
            "collectionSchema": "Collection: books\nFields:\n  title: string",
            "alpha": "a",
        })))
        .unwrap();
        assert_eq!(
            ctx,
            "# The user's context\n\n\
             ## The collection the user has open (its fields, their types and attributes)\n\
             Collection: books\nFields:\n  title: string\n\n\
             ## The search request the user is working on in the editor (JSON)\n{\"q\": \"*\"}\n\n\
             ## Total time (ms) of the user's last search\n12.5\n\n\
             ## alpha\na\n\n\
             ## zeta\nz"
        );
    }

    #[test]
    fn empty_and_null_values_add_nothing() {
        assert_eq!(context(&Map::new()), None);
        assert_eq!(
            context(&map(json!({"currentQuery": "  ", "lastQueryTimeMs": null}))),
            None
        );
    }

    #[test]
    fn the_prompt_is_stable_text() {
        assert!(!SYSTEM_PROMPT.starts_with([' ', '\n']));
        assert!(!SYSTEM_PROMPT.ends_with('\n'));
        assert!(!SYSTEM_PROMPT.contains('\r'));
        assert!(SYSTEM_PROMPT.contains("{\"q\": \"*\", \"query_by\": \"*\", \"per_page\": 10}"));
    }
}
