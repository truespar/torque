mod validation;

pub use validation::validate_schema;

use rustc_hash::FxHashMap;
use serde::{Deserialize, Serialize};
use std::fmt;

// -- Identifiers --
pub type DocId = u32;
pub type FieldId = u16;

// -- Field Type --

#[derive(Debug, Clone, PartialEq)]
pub enum FieldType {
    Text,
    String,
    Int32,
    Int64,
    Float64,
    Bool,
    StringArray,
    Int32Array,
    Int64Array,
    GeoPoint,
    FloatArray { num_dim: u16 },
    /// String or string[] — scalar values coerce to a one-element array at
    /// parse time; indexed and stored exactly like StringArray.
    StringStar,
    BoolArray,
    GeoPointArray,
    /// Marker types (Typesense v30.1): they hold no data themselves.
    /// Concrete dotted sub-fields are discovered from documents at import
    /// time and appended to the schema (see ingest::discover_dynamic_fields).
    Object,
    ObjectArray,
    Auto,
}

impl Serialize for FieldType {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for FieldType {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = std::string::String::deserialize(deserializer)?;
        Self::from_str(&s).map_err(serde::de::Error::custom)
    }
}

impl FieldType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Text => "string",
            Self::String => "string",
            Self::Int32 => "int32",
            Self::Int64 => "int64",
            Self::Float64 => "float",
            Self::Bool => "bool",
            Self::StringArray => "string[]",
            Self::Int32Array => "int32[]",
            Self::Int64Array => "int64[]",
            Self::GeoPoint => "geopoint",
            Self::FloatArray { .. } => "float[]",
            Self::StringStar => "string*",
            Self::BoolArray => "bool[]",
            Self::GeoPointArray => "geopoint[]",
            Self::Object => "object",
            Self::ObjectArray => "object[]",
            Self::Auto => "auto",
        }
    }

    pub fn from_str(s: &str) -> Result<Self, std::string::String> {
        match s {
            "string" => Ok(Self::String),
            "int32" => Ok(Self::Int32),
            "int64" => Ok(Self::Int64),
            "float" => Ok(Self::Float64),
            "bool" => Ok(Self::Bool),
            "string[]" => Ok(Self::StringArray),
            "int32[]" => Ok(Self::Int32Array),
            "int64[]" => Ok(Self::Int64Array),
            "geopoint" => Ok(Self::GeoPoint),
            "float[]" => Ok(Self::FloatArray { num_dim: 0 }),
            "string*" => Ok(Self::StringStar),
            "bool[]" => Ok(Self::BoolArray),
            "geopoint[]" => Ok(Self::GeoPointArray),
            "object" => Ok(Self::Object),
            "object[]" => Ok(Self::ObjectArray),
            "auto" => Ok(Self::Auto),
            other => Err(format!("Unknown field type: `{other}`")),
        }
    }

    /// Marker types hold no data; their sub-fields are discovered at import.
    pub fn is_dynamic_marker(&self) -> bool {
        matches!(self, Self::Object | Self::ObjectArray | Self::Auto)
    }

    pub fn is_searchable(&self) -> bool {
        matches!(
            self,
            Self::Text | Self::String | Self::StringArray | Self::StringStar
        )
    }

    pub fn is_sortable(&self) -> bool {
        matches!(
            self,
            Self::Int32 | Self::Int64 | Self::Float64 | Self::String
        )
    }
}

impl fmt::Display for FieldType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

// -- Index Mode --

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum IndexMode {
    Batch,
    Realtime,
}

impl Default for IndexMode {
    fn default() -> Self {
        Self::Batch
    }
}

// -- Vector Distance Metric --

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum VecDist {
    Cosine,
    #[serde(rename = "ip")]
    DotProduct,
    #[serde(rename = "l2")]
    Euclidean,
}

impl Default for VecDist {
    fn default() -> Self {
        Self::Cosine
    }
}

// -- Reference Definition (JOINs) --

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Reference {
    /// Target collection name.
    pub collection: std::string::String,
    /// Target field name (usually "id").
    pub field: std::string::String,
    /// Whether to cascade-delete referencing docs when the target doc is deleted.
    #[serde(default)]
    pub cascade_delete: bool,
    /// Allow indexing documents before the referenced document exists.
    #[serde(default)]
    pub async_reference: bool,
}

// -- Auto-embedding (Typesense v30.1 `embed`) --

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbedConfig {
    /// Source fields whose text is concatenated and embedded.
    pub from: Vec<std::string::String>,
    pub model_config: EmbedModelConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbedModelConfig {
    /// `openai/<model>` defaults to the OpenAI endpoint; anything else
    /// needs `url` (an OpenAI-compatible embeddings endpoint).
    pub model_name: std::string::String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<std::string::String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<std::string::String>,
    /// Put before a document's text when it is embedded (e5's `passage: `, say).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub indexing_prefix: Option<std::string::String>,
    /// Put before a query when it is embedded (e5's `query: `).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query_prefix: Option<std::string::String>,
}

// -- Field Definition --

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FieldDef {
    pub name: std::string::String,

    #[serde(rename = "type")]
    pub field_type: FieldType,

    #[serde(default)]
    pub facet: bool,

    #[serde(default = "default_true")]
    pub index: bool,

    #[serde(default)]
    pub optional: bool,

    #[serde(default)]
    pub sort: bool,

    #[serde(default)]
    pub stem: bool,

    /// Infix search on the field (Typesense `infix`): searches with `infix=always|fallback` need it.
    #[serde(default)]
    pub infix: bool,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub locale: Option<std::string::String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub num_dim: Option<u16>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vec_dist: Option<VecDist>,

    /// Reference to another collection for JOINs.
    /// Format in JSON: `"reference": "other_collection.field_name"`
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference: Option<std::string::String>,

    /// Runtime-assigned field ID (not part of the JSON API)
    #[serde(skip)]
    pub id: FieldId,

    /// Used in PATCH to drop a field
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub drop: bool,

    /// Custom stemming dictionary id (Typesense v30.1 `stem_dictionary`).
    /// Words found in the dictionary map to their root instead of being
    /// Snowball-stemmed. See `crate::stemming`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stem_dictionary: Option<std::string::String>,

    /// Auto-embedding: generate this float[] field from other fields via a
    /// remote model at index time (Typesense v30.1).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub embed: Option<EmbedConfig>,

    /// Torque extension, not in Typesense: the field holds names (people, companies, vessels). Its
    /// words, and a query's, are lower-cased in every script and split at any Unicode space, and a
    /// query without words finds nothing in it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub name_matching: bool,
}

impl Default for FieldDef {
    fn default() -> Self {
        Self {
            name: String::new(),
            field_type: FieldType::String,
            facet: false,
            index: true,
            optional: false,
            sort: false,
            stem: false,
            infix: false,
            locale: None,
            num_dim: None,
            vec_dist: None,
            reference: None,
            id: 0,
            drop: false,
            stem_dictionary: None,
            embed: None,
            name_matching: false,
        }
    }
}

impl Default for Schema {
    fn default() -> Self {
        Self {
            name: String::new(),
            fields: Vec::new(),
            default_sorting_field: None,
            enable_nested_fields: false,
            symbols_to_index: Vec::new(),
            token_separators: Vec::new(),
            default_language: Language::default(),
            persistence_enabled: true,
            index_mode: IndexMode::default(),
            metadata: None,
            synonym_sets: None,
            curation_sets: None,
            name_to_id: FxHashMap::default(),
            geopoint_field_names: Vec::new(),
            nested_layout: None,
            registry: Default::default(),
        }
    }
}

impl FieldDef {
    /// Parse the `reference` string "collection.field" into components.
    /// Returns None if no reference is set.
    pub fn parsed_reference(&self) -> Option<Reference> {
        let ref_str = self.reference.as_deref()?;
        let dot_pos = ref_str.find('.')?;
        Some(Reference {
            collection: ref_str[..dot_pos].to_string(),
            field: ref_str[dot_pos + 1..].to_string(),
            cascade_delete: false,
            async_reference: false,
        })
    }

    /// Returns true if this field is a reference (JOIN) field.
    pub fn is_reference(&self) -> bool {
        self.reference.is_some()
    }
}

fn default_true() -> bool {
    true
}

fn is_default_language(lang: &Language) -> bool {
    matches!(lang, Language::English)
}

/// Typesense echoes `"default_sorting_field": ""` for "none" and accepts it back on create.
fn empty_string_as_none<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<std::string::String>, D::Error> {
    let s: Option<std::string::String> = Option::deserialize(d)?;
    Ok(s.filter(|s| !s.is_empty()))
}

/// Typesense ignores a field named `id` in a schema it is given (with a warning: the id is implicit) and
/// does not echo it back; so does this.
fn fields_without_id<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<FieldDef>, D::Error> {
    let mut fields: Vec<FieldDef> = Vec::deserialize(d)?;
    fields.retain(|f| {
        if f.name == "id" {
            tracing::warn!("Collection schema cannot contain a field with name `id`. Ignoring field.");
            return false;
        }
        true
    });
    Ok(fields)
}

// -- Schema --

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Schema {
    pub name: std::string::String,
    #[serde(deserialize_with = "fields_without_id")]
    pub fields: Vec<FieldDef>,

    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "empty_string_as_none")]
    pub default_sorting_field: Option<std::string::String>,

    #[serde(default)]
    pub enable_nested_fields: bool,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub symbols_to_index: Vec<char>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub token_separators: Vec<char>,

    #[serde(default, skip_serializing_if = "is_default_language")]
    pub default_language: Language,

    #[serde(default = "default_true")]
    pub persistence_enabled: bool,

    #[serde(default)]
    pub index_mode: IndexMode,

    /// Arbitrary user metadata, persisted and echoed back (Typesense v30.1).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,

    /// Collection-bound synonym sets, applied to every search on this
    /// collection unless the query passes its own `synonym_sets`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub synonym_sets: Option<Vec<std::string::String>>,
    /// Curation sets bound to this collection — their items are evaluated
    /// on every search unless the query passes `enable_curations=false`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub curation_sets: Option<Vec<std::string::String>>,

    /// Runtime lookup: field name → field ID (not serialized)
    #[serde(skip)]
    pub name_to_id: FxHashMap<std::string::String, FieldId>,

    /// Cached set of geopoint field names for unflatten_json (not serialized)
    #[serde(skip)]
    pub geopoint_field_names: Vec<std::string::String>,

    /// Precomputed nested field layout for fast document_to_json (not serialized)
    #[serde(skip)]
    pub nested_layout: Option<NestedFieldLayout>,

    /// Ids of every document path (TQBF FIELDS), shared by all clones of this schema.
    #[serde(skip)]
    pub registry: crate::tqbf::registry::SharedRegistry,
}

impl Schema {
    /// Assign field IDs and build the name_to_id map.
    /// Call this after deserialization.
    pub fn assign_field_ids(&mut self) {
        self.name_to_id.clear();
        for (i, field) in self.fields.iter_mut().enumerate() {
            field.id = i as FieldId;
            self.name_to_id.insert(field.name.clone(), field.id);

            // For FloatArray, copy num_dim into the FieldType if provided
            if let Some(dim) = field.num_dim {
                if matches!(field.field_type, FieldType::FloatArray { .. }) {
                    field.field_type = FieldType::FloatArray { num_dim: dim };
                }
            }
        }

        self.geopoint_field_names = self
            .fields
            .iter()
            .filter(|f| matches!(f.field_type, FieldType::GeoPoint))
            .map(|f| f.name.clone())
            .collect();

        // Build nested field layout for fast document_to_json
        if self.enable_nested_fields {
            self.nested_layout = Some(self.build_nested_layout());
        } else {
            self.nested_layout = None;
        }

        self.registry.add_schema(self);
    }

    /// Build the precomputed nested field layout from field definitions.
    fn build_nested_layout(&self) -> NestedFieldLayout {
        let mut flat_fields = Vec::new();
        // prefix -> Vec<(schema_index, suffix, is_geopoint, is_array_type)>
        let mut groups: FxHashMap<String, Vec<(usize, String, bool, bool)>> = FxHashMap::default();

        for (i, field) in self.fields.iter().enumerate() {
            if let Some(dot) = field.name.find('.') {
                let prefix = field.name[..dot].to_string();
                let suffix = field.name[dot + 1..].to_string();
                let is_geo = matches!(field.field_type, FieldType::GeoPoint);
                let is_array = matches!(
                    field.field_type,
                    FieldType::StringArray | FieldType::Int32Array | FieldType::Int64Array | FieldType::FloatArray { .. }
                );
                groups.entry(prefix).or_default().push((i, suffix, is_geo, is_array));
            } else {
                flat_fields.push(i);
            }
        }

        let mut nested_groups: Vec<NestedGroup> = groups
            .into_iter()
            .map(|(prefix, entries)| {
                let mut children = Vec::with_capacity(entries.len());
                let mut geopoint_child_indices = Vec::new();
                let mut all_non_geo_are_arrays = true;
                let mut has_non_geo = false;

                for (child_pos, (idx, suffix, is_geo, is_array)) in entries.iter().enumerate() {
                    children.push((*idx, suffix.clone()));
                    if *is_geo {
                        geopoint_child_indices.push(child_pos);
                    } else {
                        has_non_geo = true;
                        if !is_array {
                            all_non_geo_are_arrays = false;
                        }
                    }
                }

                let is_array_group = has_non_geo && all_non_geo_are_arrays;

                NestedGroup {
                    prefix,
                    children,
                    is_array_group,
                    geopoint_child_indices,
                }
            })
            .collect();

        // Sort groups by prefix for deterministic output
        nested_groups.sort_by(|a, b| a.prefix.cmp(&b.prefix));

        NestedFieldLayout {
            flat_fields,
            groups: nested_groups,
        }
    }
}

// -- Field Value --

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum FieldValue {
    Null,
    Bool(bool),
    Int32(i32),
    Int64(i64),
    Float64(f64),
    Text(std::string::String),
    StringArray(Vec<Option<std::string::String>>),
    Int32Array(Vec<Option<i32>>),
    Int64Array(Vec<Option<i64>>),
    FloatArray(Vec<f32>),
    GeoPoint { lat: f64, lng: f64 },
    BoolArray(Vec<Option<bool>>),
    GeoPointArray(Vec<(f64, f64)>),
}

impl FieldValue {
    /// Returns the length of an array field value, or None if not an array type.
    pub fn array_len(&self) -> Option<usize> {
        match self {
            Self::StringArray(arr) => Some(arr.len()),
            Self::Int32Array(arr) => Some(arr.len()),
            Self::Int64Array(arr) => Some(arr.len()),
            Self::FloatArray(arr) => Some(arr.len()),
            Self::BoolArray(arr) => Some(arr.len()),
            Self::GeoPointArray(arr) => Some(arr.len()),
            _ => None,
        }
    }

    /// Get a single element from an array field as a JSON value.
    /// Returns None if out of bounds or the element is null.
    pub fn get_array_element(&self, i: usize) -> Option<serde_json::Value> {
        match self {
            Self::StringArray(arr) => arr.get(i)?.as_ref().map(|s| serde_json::Value::String(s.clone())),
            Self::Int32Array(arr) => arr.get(i)?.as_ref().map(|&n| serde_json::json!(n)),
            Self::Int64Array(arr) => arr.get(i)?.as_ref().map(|&n| serde_json::json!(n)),
            Self::FloatArray(arr) => arr.get(i).map(|&f| serde_json::json!(f)),
            Self::BoolArray(arr) => arr.get(i)?.as_ref().map(|&b| serde_json::json!(b)),
            Self::GeoPointArray(arr) => arr.get(i).map(|&(lat, lng)| serde_json::json!([lat, lng])),
            _ => None,
        }
    }

    /// Estimate heap bytes allocated by this value (excludes the enum discriminant itself).
    pub fn heap_size(&self) -> usize {
        match self {
            Self::Null | Self::Bool(_) | Self::Int32(_) | Self::Int64(_) | Self::Float64(_)
            | Self::GeoPoint { .. } => 0,
            Self::Text(s) => s.capacity(),
            Self::StringArray(arr) => {
                std::mem::size_of::<Option<String>>() * arr.capacity()
                    + arr.iter().filter_map(|s| s.as_ref()).map(|s| s.capacity()).sum::<usize>()
            }
            Self::Int32Array(arr) => std::mem::size_of::<Option<i32>>() * arr.capacity(),
            Self::Int64Array(arr) => std::mem::size_of::<Option<i64>>() * arr.capacity(),
            Self::FloatArray(arr) => std::mem::size_of::<f32>() * arr.capacity(),
            Self::BoolArray(arr) => std::mem::size_of::<Option<bool>>() * arr.capacity(),
            Self::GeoPointArray(arr) => std::mem::size_of::<(f64, f64)>() * arr.capacity(),
        }
    }

    /// Shrink excess capacity on heap-allocated variants.
    pub fn shrink_to_fit(&mut self) {
        match self {
            Self::Text(s) => s.shrink_to_fit(),
            Self::StringArray(arr) => {
                for s in arr.iter_mut().flatten() {
                    s.shrink_to_fit();
                }
                arr.shrink_to_fit();
            }
            Self::Int32Array(arr) => arr.shrink_to_fit(),
            Self::Int64Array(arr) => arr.shrink_to_fit(),
            Self::FloatArray(arr) => arr.shrink_to_fit(),
            Self::BoolArray(arr) => arr.shrink_to_fit(),
            Self::GeoPointArray(arr) => arr.shrink_to_fit(),
            _ => {}
        }
    }
}

// -- Language --

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Language {
    English,
    Swedish,
    Norwegian,
    Danish,
    Finnish,
    German,
    French,
    Spanish,
    Portuguese,
    Italian,
    Dutch,
    Russian,
    Arabic,
    Greek,
    Hungarian,
    Romanian,
    Turkish,
}

impl Serialize for Language {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(match self {
            Self::English => "en",
            Self::Swedish => "sv",
            Self::Norwegian => "no",
            Self::Danish => "da",
            Self::Finnish => "fi",
            Self::German => "de",
            Self::French => "fr",
            Self::Spanish => "es",
            Self::Portuguese => "pt",
            Self::Italian => "it",
            Self::Dutch => "nl",
            Self::Russian => "ru",
            Self::Arabic => "ar",
            Self::Greek => "el",
            Self::Hungarian => "hu",
            Self::Romanian => "ro",
            Self::Turkish => "tr",
        })
    }
}

impl<'de> Deserialize<'de> for Language {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        Self::from_locale(&s)
            .ok_or_else(|| serde::de::Error::custom(format!("unknown language: {s}")))
    }
}

impl Default for Language {
    fn default() -> Self {
        Self::English
    }
}

impl Language {
    pub const ALL: [Language; 17] = [
        Self::English, Self::Swedish, Self::Norwegian, Self::Danish, Self::Finnish, Self::German, Self::French,
        Self::Spanish, Self::Portuguese, Self::Italian, Self::Dutch, Self::Russian, Self::Arabic, Self::Greek,
        Self::Hungarian, Self::Romanian, Self::Turkish,
    ];

    pub fn from_locale(locale: &str) -> Option<Self> {
        match locale {
            "en" | "english" => Some(Self::English),
            "sv" | "swedish" => Some(Self::Swedish),
            "no" | "norwegian" => Some(Self::Norwegian),
            "da" | "danish" => Some(Self::Danish),
            "fi" | "finnish" => Some(Self::Finnish),
            "de" | "german" => Some(Self::German),
            "fr" | "french" => Some(Self::French),
            "es" | "spanish" => Some(Self::Spanish),
            "pt" | "portuguese" => Some(Self::Portuguese),
            "it" | "italian" => Some(Self::Italian),
            "nl" | "dutch" => Some(Self::Dutch),
            "ru" | "russian" => Some(Self::Russian),
            "ar" | "arabic" => Some(Self::Arabic),
            "el" | "greek" => Some(Self::Greek),
            "hu" | "hungarian" => Some(Self::Hungarian),
            "ro" | "romanian" => Some(Self::Romanian),
            "tr" | "turkish" => Some(Self::Turkish),
            _ => None,
        }
    }

    pub fn to_u8(self) -> u8 {
        match self {
            Self::English => 0,
            Self::Swedish => 1,
            Self::Norwegian => 2,
            Self::Danish => 3,
            Self::Finnish => 4,
            Self::German => 5,
            Self::French => 6,
            Self::Spanish => 7,
            Self::Portuguese => 8,
            Self::Italian => 9,
            Self::Dutch => 10,
            Self::Russian => 11,
            Self::Arabic => 12,
            Self::Greek => 13,
            Self::Hungarian => 14,
            Self::Romanian => 15,
            Self::Turkish => 16,
        }
    }

    pub fn from_u8(v: u8) -> Self {
        match v {
            1 => Self::Swedish,
            2 => Self::Norwegian,
            3 => Self::Danish,
            4 => Self::Finnish,
            5 => Self::German,
            6 => Self::French,
            7 => Self::Spanish,
            8 => Self::Portuguese,
            9 => Self::Italian,
            10 => Self::Dutch,
            11 => Self::Russian,
            12 => Self::Arabic,
            13 => Self::Greek,
            14 => Self::Hungarian,
            15 => Self::Romanian,
            16 => Self::Turkish,
            _ => Self::English,
        }
    }
}

// -- Nested Field Layout --

/// Precomputed layout for reconstructing nested JSON from flat document fields.
/// Built once per schema when `enable_nested_fields` is true.
pub struct NestedFieldLayout {
    /// Schema field indices for fields without dots — inserted into root map.
    pub flat_fields: Vec<usize>,
    /// Nested groups keyed by prefix (e.g. "sniCodes", "mostRecentRegisteredAddress").
    pub groups: Vec<NestedGroup>,
}

pub struct NestedGroup {
    pub prefix: String,
    /// (field_index_in_schema, suffix_after_dot)
    pub children: Vec<(usize, String)>,
    /// True = all non-geopoint children are array types → reconstruct as array-of-objects.
    pub is_array_group: bool,
    /// Indices into `children` that are geopoint fields (exempt from array grouping).
    pub geopoint_child_indices: Vec<usize>,
}

impl std::fmt::Debug for NestedFieldLayout {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NestedFieldLayout")
            .field("flat_fields", &self.flat_fields.len())
            .field("groups", &self.groups.len())
            .finish()
    }
}

impl Clone for NestedFieldLayout {
    fn clone(&self) -> Self {
        Self {
            flat_fields: self.flat_fields.clone(),
            groups: self.groups.iter().map(|g| NestedGroup {
                prefix: g.prefix.clone(),
                children: g.children.clone(),
                is_array_group: g.is_array_group,
                geopoint_child_indices: g.geopoint_child_indices.clone(),
            }).collect(),
        }
    }
}

// -- Document --

/// Sparse field storage: parallel vecs of field IDs and values, sorted by ID.
/// Null values are not stored, saving ~340 MB for schemas with many optional fields.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SparseFields {
    ids: Vec<FieldId>,
    values: Vec<FieldValue>,
}

impl SparseFields {
    pub fn new() -> Self {
        Self { ids: Vec::new(), values: Vec::new() }
    }

    pub fn with_capacity(cap: usize) -> Self {
        Self { ids: Vec::with_capacity(cap), values: Vec::with_capacity(cap) }
    }

    /// Push a field value at the given field index. Null values are skipped.
    pub fn push(&mut self, field_id: FieldId, value: FieldValue) {
        if !matches!(value, FieldValue::Null) {
            self.ids.push(field_id);
            self.values.push(value);
        }
    }

    /// Insert a value keeping ids sorted — for fields filled after parse
    /// time (auto-embedding). Replaces an existing value for the id.
    pub fn insert_sorted(&mut self, field_id: FieldId, value: FieldValue) {
        if matches!(value, FieldValue::Null) {
            return;
        }
        match self.ids.binary_search(&field_id) {
            Ok(pos) => self.values[pos] = value,
            Err(pos) => {
                self.ids.insert(pos, field_id);
                self.values.insert(pos, value);
            }
        }
    }

    /// Look up a field by its schema field ID (binary search).
    #[inline]
    pub fn get(&self, field_id: usize) -> Option<&FieldValue> {
        let id = field_id as FieldId;
        self.ids.binary_search(&id)
            .ok()
            .map(|pos| &self.values[pos])
    }

    /// Mutable lookup by field ID.
    pub fn get_mut(&mut self, field_id: usize) -> Option<&mut FieldValue> {
        let id = field_id as FieldId;
        self.ids.binary_search(&id)
            .ok()
            .map(|pos| &mut self.values[pos])
    }

    /// Set a field value (replace existing or insert). Nulls remove the entry.
    pub fn set(&mut self, field_id: FieldId, value: FieldValue) {
        if matches!(value, FieldValue::Null) {
            // Remove if exists
            if let Ok(pos) = self.ids.binary_search(&field_id) {
                self.ids.remove(pos);
                self.values.remove(pos);
            }
        } else {
            match self.ids.binary_search(&field_id) {
                Ok(pos) => self.values[pos] = value,
                Err(pos) => {
                    self.ids.insert(pos, field_id);
                    self.values.insert(pos, value);
                }
            }
        }
    }

    /// Iterate over (field_id, &FieldValue) pairs.
    pub fn iter(&self) -> impl Iterator<Item = (usize, &FieldValue)> {
        self.ids.iter().map(|&id| id as usize).zip(self.values.iter())
    }

    /// Iterate over mutable field values.
    pub fn iter_mut(&mut self) -> impl Iterator<Item = (usize, &mut FieldValue)> {
        self.ids.iter().map(|&id| id as usize).zip(self.values.iter_mut())
    }

    pub fn len(&self) -> usize {
        self.values.len()
    }

    pub fn shrink_to_fit(&mut self) {
        self.ids.shrink_to_fit();
        self.values.shrink_to_fit();
        for fv in &mut self.values {
            fv.shrink_to_fit();
        }
    }

    /// Estimate total heap bytes.
    pub fn heap_size(&self) -> usize {
        self.ids.capacity() * std::mem::size_of::<FieldId>()
            + self.values.capacity() * std::mem::size_of::<FieldValue>()
            + self.values.iter().map(|fv| fv.heap_size()).sum::<usize>()
    }
}

/// Construct SparseFields from a dense Vec<FieldValue>, filtering out Nulls.
impl From<Vec<FieldValue>> for SparseFields {
    fn from(fields: Vec<FieldValue>) -> Self {
        let mut ids = Vec::new();
        let mut values = Vec::new();
        for (i, fv) in fields.into_iter().enumerate() {
            if !matches!(fv, FieldValue::Null) {
                ids.push(i as FieldId);
                values.push(fv);
            }
        }
        SparseFields { ids, values }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Document {
    pub id: DocId,
    pub external_id: std::string::String,
    pub language: Language,
    /// Sparse field values: only non-null fields stored, sorted by field ID.
    pub fields: SparseFields,
    /// The whole document as a TQBF record (every path, indexed or not). None for documents
    /// that arrived without JSON (typed TCP ingest).
    #[serde(skip)]
    pub record: Option<std::sync::Arc<[u8]>>,
}
