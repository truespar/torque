use super::{FieldId, FieldType, Schema};
use std::collections::HashSet;

#[derive(Debug)]
pub struct SchemaError {
    pub message: String,
}

impl std::fmt::Display for SchemaError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for SchemaError {}

// Typesense has no field cap; ours is the FieldId range.
const MAX_FIELDS: usize = FieldId::MAX as usize;
const MAX_FIELD_NAME_LEN: usize = 256;

/// Validate a collection schema. Returns Ok(()) or a descriptive error.
pub fn validate_schema(schema: &Schema) -> Result<(), SchemaError> {
    // Collection name must be non-empty and valid
    if schema.name.is_empty() {
        return Err(SchemaError {
            message: "Collection name cannot be empty.".into(),
        });
    }
    if !is_valid_name(&schema.name) {
        return Err(SchemaError {
            message: format!(
                "Collection name `{}` is invalid. Only alphanumeric characters, underscores, and hyphens are allowed.",
                schema.name
            ),
        });
    }

    // At least one field required
    if schema.fields.is_empty() {
        return Err(SchemaError {
            message: "A collection must have at least one field.".into(),
        });
    }

    // Too many fields
    if schema.fields.len() > MAX_FIELDS {
        return Err(SchemaError {
            message: format!(
                "Schema has {} fields, exceeding the maximum of {MAX_FIELDS}.",
                schema.fields.len()
            ),
        });
    }

    let mut seen_names = HashSet::new();

    for field in &schema.fields {
        // Field name must be non-empty
        if field.name.is_empty() {
            return Err(SchemaError {
                message: "Field name cannot be empty.".into(),
            });
        }

        // object/object[] fields flatten into dotted sub-fields, which
        // requires nested-field support (Typesense v30.1 errors identically).
        if matches!(
            field.field_type,
            FieldType::Object | FieldType::ObjectArray
        ) && !schema.enable_nested_fields
        {
            return Err(SchemaError {
                message: format!(
                    "Field `{}` has type `{}`, which requires `enable_nested_fields: true`.",
                    field.name, field.field_type
                ),
            });
        }

        // Auto fields may use wildcard name patterns (".*", "prefix.*") —
        // skip the identifier check for them.
        if field.field_type == FieldType::Auto && field.name.contains('*') {
            continue;
        }

        // Field name length limit
        if field.name.len() > MAX_FIELD_NAME_LEN {
            return Err(SchemaError {
                message: format!(
                    "Field name `{}...` is {} characters, exceeding the maximum of {MAX_FIELD_NAME_LEN}.",
                    &field.name[..32],
                    field.name.len()
                ),
            });
        }

        // Field names must be valid identifiers
        if !is_valid_field_name(&field.name, schema.enable_nested_fields) {
            return Err(SchemaError {
                message: if schema.enable_nested_fields {
                    format!(
                        "Field name `{}` is invalid. Only alphanumeric characters, underscores, and dots are allowed.",
                        field.name
                    )
                } else {
                    format!(
                        "Field name `{}` is invalid. Only alphanumeric characters and underscores are allowed.",
                        field.name
                    )
                },
            });
        }

        if field.name_matching {
            if !matches!(field.field_type, FieldType::String | FieldType::StringArray | FieldType::StringStar) {
                return Err(SchemaError {
                    message: format!("Field `{}`: `name_matching` needs a `string` or `string[]` field.", field.name),
                });
            }
            if field.stem || field.stem_dictionary.is_some() {
                return Err(SchemaError {
                    message: format!("Field `{}`: `name_matching` cannot be combined with `stem` or `stem_dictionary`.", field.name),
                });
            }
        }

        // `id` is reserved
        if field.name == "id" {
            return Err(SchemaError {
                message: "`id` is a reserved field name and cannot be used.".into(),
            });
        }

        // No duplicate field names
        if !seen_names.insert(&field.name) {
            return Err(SchemaError {
                message: format!("Duplicate field name `{}`.", field.name),
            });
        }

        // num_dim required for FloatArray only when used as a vector field
        // float[] without num_dim is a regular numeric array (like Typesense)

        // num_dim > 0 if provided
        if let Some(dim) = field.num_dim {
            if dim == 0 {
                return Err(SchemaError {
                    message: format!(
                        "Field `{}` has `num_dim` set to 0. Must be > 0.",
                        field.name
                    ),
                });
            }
        }

        // reference field validation
        if let Some(ref ref_str) = field.reference {
            // Must contain exactly one dot
            let dot_count = ref_str.chars().filter(|&c| c == '.').count();
            if dot_count != 1 {
                return Err(SchemaError {
                    message: format!(
                        "Field `{}` has invalid `reference` format `{ref_str}`. Expected `collection_name.field_name`.",
                        field.name
                    ),
                });
            }
            // Reference field type must be string, int32, int64, or array variants
            match field.field_type {
                FieldType::String | FieldType::Int32 | FieldType::Int64
                | FieldType::StringArray | FieldType::Int32Array | FieldType::Int64Array => {}
                _ => {
                    return Err(SchemaError {
                        message: format!(
                            "Field `{}` has `reference` set but type `{}` is not a valid reference type. \
                             Use string, int32, int64, or their array variants.",
                            field.name, field.field_type
                        ),
                    });
                }
            }
        }

        // vec_dist only valid with FloatArray
        if field.vec_dist.is_some() && !matches!(field.field_type, FieldType::FloatArray { .. }) {
            return Err(SchemaError {
                message: format!(
                    "Field `{}` has `vec_dist` set but is not a `float[]` type.",
                    field.name
                ),
            });
        }

        // num_dim only valid with FloatArray
        if field.num_dim.is_some() && !matches!(field.field_type, FieldType::FloatArray { .. }) {
            return Err(SchemaError {
                message: format!(
                    "Field `{}` has `num_dim` set but is not a `float[]` type.",
                    field.name
                ),
            });
        }

        // Auto-embedding: float[] with num_dim, sourced from string fields.
        if let Some(ref embed) = field.embed {
            if !matches!(field.field_type, FieldType::FloatArray { .. }) {
                return Err(SchemaError {
                    message: format!(
                        "Field `{}` has `embed` set but is not a `float[]` type.",
                        field.name
                    ),
                });
            }
            if field.num_dim.is_none() {
                return Err(SchemaError {
                    message: format!(
                        "Field `{}` has `embed` set but no `num_dim`. Set it to the model's embedding dimension.",
                        field.name
                    ),
                });
            }
            if embed.from.is_empty() {
                return Err(SchemaError {
                    message: format!(
                        "Field `{}`: `embed.from` must list at least one source field.",
                        field.name
                    ),
                });
            }
            for src in &embed.from {
                let Some(src_field) = schema.fields.iter().find(|f| &f.name == src) else {
                    return Err(SchemaError {
                        message: format!(
                            "Field `{}`: `embed.from` references unknown field `{src}`.",
                            field.name
                        ),
                    });
                };
                if !matches!(
                    src_field.field_type,
                    FieldType::String
                        | FieldType::Text
                        | FieldType::StringArray
                        | FieldType::StringStar
                ) {
                    return Err(SchemaError {
                        message: format!(
                            "Field `{}`: `embed.from` field `{src}` must be a string type.",
                            field.name
                        ),
                    });
                }
            }
        }
    }

    // default_sorting_field must reference a sortable field
    if let Some(ref sort_field) = schema.default_sorting_field {
        let field = schema.fields.iter().find(|f| &f.name == sort_field);
        match field {
            None => {
                return Err(SchemaError {
                    message: format!(
                        "`default_sorting_field` references `{sort_field}` which is not a field in the schema."
                    ),
                });
            }
            Some(f) if !f.sort => {
                return Err(SchemaError {
                    message: format!(
                        "`default_sorting_field` references `{sort_field}` but that field does not have `sort: true`."
                    ),
                });
            }
            Some(f) if !f.field_type.is_sortable() => {
                return Err(SchemaError {
                    message: format!(
                        "`default_sorting_field` references `{sort_field}` but type `{}` is not sortable.",
                        f.field_type
                    ),
                });
            }
            _ => {}
        }
    }

    Ok(())
}

fn is_valid_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == '-')
}

fn is_valid_field_name(name: &str, allow_dots: bool) -> bool {
    if allow_dots {
        !name.is_empty()
            && !name.starts_with('.')
            && !name.ends_with('.')
            && !name.contains("..")
            && name.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '.')
    } else {
        !name.is_empty() && name.chars().all(|c| c.is_alphanumeric() || c == '_')
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::{FieldDef, FieldType, Language, VecDist};

    fn make_field(name: &str, field_type: FieldType) -> FieldDef {
        FieldDef {
            name: name.into(),
            field_type,
            facet: false,
            index: true,
            optional: false,
            sort: false,
            stem: true,
            infix: false,
            locale: None,
            num_dim: None,
            vec_dist: None,
            id: 0,
            reference: None,
            drop: false,
            stem_dictionary: None,
            embed: None,
            name_matching: false,
        }
    }

    fn make_schema(name: &str, fields: Vec<FieldDef>) -> Schema {
        Schema {
            name: name.into(),
            fields,
            default_sorting_field: None,
            enable_nested_fields: false,
            symbols_to_index: vec![],
            token_separators: vec![],
            default_language: Language::default(),
            persistence_enabled: true,
            index_mode: Default::default(),
            name_to_id: Default::default(),
            geopoint_field_names: vec![],
            nested_layout: None,
            registry: Default::default(),
            metadata: None,
            synonym_sets: None,
            curation_sets: None,
        }
    }

    #[test]
    fn valid_basic_schema() {
        let schema = make_schema("products", vec![make_field("title", FieldType::String)]);
        assert!(validate_schema(&schema).is_ok());
    }

    #[test]
    fn valid_multi_field_schema() {
        let mut price = make_field("price", FieldType::Float64);
        price.sort = true;
        price.facet = true;

        let schema = Schema {
            default_sorting_field: Some("price".into()),
            ..make_schema(
                "products",
                vec![
                    make_field("title", FieldType::Text),
                    price,
                    make_field("tags", FieldType::StringArray),
                ],
            )
        };
        assert!(validate_schema(&schema).is_ok());
    }

    #[test]
    fn valid_vector_field() {
        let mut emb = make_field("embedding", FieldType::FloatArray { num_dim: 768 });
        emb.num_dim = Some(768);
        emb.vec_dist = Some(VecDist::Cosine);

        let schema = make_schema("docs", vec![make_field("title", FieldType::String), emb]);
        assert!(validate_schema(&schema).is_ok());
    }

    #[test]
    fn reject_empty_name() {
        let schema = make_schema("", vec![make_field("title", FieldType::String)]);
        let err = validate_schema(&schema).unwrap_err();
        assert!(err.message.contains("name cannot be empty"));
    }

    #[test]
    fn reject_no_fields() {
        let schema = make_schema("products", vec![]);
        let err = validate_schema(&schema).unwrap_err();
        assert!(err.message.contains("at least one field"));
    }

    #[test]
    fn reject_duplicate_field_names() {
        let schema = make_schema(
            "products",
            vec![
                make_field("title", FieldType::String),
                make_field("title", FieldType::Int32),
            ],
        );
        let err = validate_schema(&schema).unwrap_err();
        assert!(err.message.contains("Duplicate field name"));
    }

    #[test]
    fn reject_reserved_id_field() {
        let schema = make_schema("products", vec![make_field("id", FieldType::String)]);
        let err = validate_schema(&schema).unwrap_err();
        assert!(err.message.contains("reserved"));
    }

    #[test]
    fn an_id_field_in_schema_json_is_ignored() {
        // As Typesense: declaring `id` is accepted, the field is dropped (the id is implicit).
        let schema: Schema = serde_json::from_str(
            r#"{"name":"products","fields":[{"name":"id","type":"string","facet":false},{"name":"title","type":"string"}]}"#,
        )
        .unwrap();
        assert_eq!(schema.fields.iter().map(|f| f.name.as_str()).collect::<Vec<_>>(), ["title"]);
        assert!(validate_schema(&schema).is_ok());
    }

    #[test]
    fn accept_float_array_without_num_dim() {
        // float[] without num_dim is a regular numeric array (like Typesense)
        let schema = make_schema(
            "docs",
            vec![make_field("scores", FieldType::FloatArray { num_dim: 0 })],
        );
        assert!(validate_schema(&schema).is_ok());
    }

    #[test]
    fn reject_vec_dist_on_non_vector() {
        let mut f = make_field("title", FieldType::String);
        f.vec_dist = Some(VecDist::Cosine);
        let schema = make_schema("docs", vec![f]);
        let err = validate_schema(&schema).unwrap_err();
        assert!(err.message.contains("vec_dist"));
    }

    #[test]
    fn reject_num_dim_on_non_vector() {
        let mut f = make_field("title", FieldType::String);
        f.num_dim = Some(128);
        let schema = make_schema("docs", vec![f]);
        let err = validate_schema(&schema).unwrap_err();
        assert!(err.message.contains("num_dim"));
    }

    #[test]
    fn reject_default_sort_field_not_in_schema() {
        let schema = Schema {
            default_sorting_field: Some("missing".into()),
            ..make_schema("products", vec![make_field("title", FieldType::String)])
        };
        let err = validate_schema(&schema).unwrap_err();
        assert!(err.message.contains("not a field in the schema"));
    }

    #[test]
    fn reject_default_sort_field_not_sortable() {
        let schema = Schema {
            default_sorting_field: Some("title".into()),
            ..make_schema("products", vec![make_field("title", FieldType::String)])
        };
        let err = validate_schema(&schema).unwrap_err();
        assert!(err.message.contains("sort: true"));
    }

    #[test]
    fn reject_invalid_field_name() {
        let schema = make_schema(
            "products",
            vec![make_field("field with spaces", FieldType::String)],
        );
        let err = validate_schema(&schema).unwrap_err();
        assert!(err.message.contains("invalid"));
    }

    #[test]
    fn reject_invalid_collection_name() {
        let schema = make_schema(
            "my collection!",
            vec![make_field("title", FieldType::String)],
        );
        let err = validate_schema(&schema).unwrap_err();
        assert!(err.message.contains("invalid"));
    }

    fn make_nested_schema(name: &str, fields: Vec<FieldDef>) -> Schema {
        Schema {
            enable_nested_fields: true,
            ..make_schema(name, fields)
        }
    }

    #[test]
    fn valid_dotted_field_name_with_nested_enabled() {
        let schema = make_nested_schema(
            "products",
            vec![
                make_field("title", FieldType::String),
                make_field("metadata.brand", FieldType::String),
                make_field("metadata.weight_grams", FieldType::Int32),
            ],
        );
        assert!(validate_schema(&schema).is_ok());
    }

    #[test]
    fn reject_dotted_field_name_without_nested() {
        let schema = make_schema(
            "products",
            vec![make_field("metadata.brand", FieldType::String)],
        );
        let err = validate_schema(&schema).unwrap_err();
        assert!(err.message.contains("invalid"));
    }

    #[test]
    fn reject_leading_dot_in_nested_field() {
        let schema = make_nested_schema(
            "products",
            vec![make_field(".brand", FieldType::String)],
        );
        let err = validate_schema(&schema).unwrap_err();
        assert!(err.message.contains("invalid"));
    }

    #[test]
    fn reject_trailing_dot_in_nested_field() {
        let schema = make_nested_schema(
            "products",
            vec![make_field("brand.", FieldType::String)],
        );
        let err = validate_schema(&schema).unwrap_err();
        assert!(err.message.contains("invalid"));
    }

    #[test]
    fn reject_double_dot_in_nested_field() {
        let schema = make_nested_schema(
            "products",
            vec![make_field("meta..brand", FieldType::String)],
        );
        let err = validate_schema(&schema).unwrap_err();
        assert!(err.message.contains("invalid"));
    }

    #[test]
    fn reject_too_many_fields() {
        let fields: Vec<FieldDef> = (0..=MAX_FIELDS)
            .map(|i| make_field(&format!("field_{i}"), FieldType::String))
            .collect();
        let schema = make_schema("products", fields);
        let err = validate_schema(&schema).unwrap_err();
        assert!(err.message.contains("65536"), "{}", err.message);
        assert!(err.message.contains("maximum"), "{}", err.message);
    }

    #[test]
    fn reject_long_field_name() {
        let long_name = "a".repeat(257);
        let schema = make_schema("products", vec![make_field(&long_name, FieldType::String)]);
        let err = validate_schema(&schema).unwrap_err();
        assert!(err.message.contains("257"), "{}", err.message);
        assert!(err.message.contains("maximum"), "{}", err.message);
    }

    #[test]
    fn accept_max_fields() {
        let fields: Vec<FieldDef> = (0..300)
            .map(|i| make_field(&format!("field_{i}"), FieldType::String))
            .collect();
        let schema = make_schema("products", fields);
        assert!(validate_schema(&schema).is_ok());
    }
}
