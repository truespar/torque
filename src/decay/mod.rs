use crate::schema::{FieldId, FieldValue};

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DecayFunc {
    Gauss,
    Linear,
    Exp,
    Diff,
}

pub struct DecaySort {
    pub field_id: FieldId,
    pub origin: i64,
    pub func: DecayFunc,
    pub scale: i64,
    pub decay: f64,
    pub offset: i64,
}

impl DecaySort {
    pub fn score(&self, field_value: i64) -> f64 {
        let distance = (field_value - self.origin).abs() - self.offset;
        if distance <= 0 {
            return 1.0;
        }
        let d = distance as f64;
        let s = self.scale as f64;
        match self.func {
            DecayFunc::Gauss => (-(d * d) / (2.0 * s * s)).exp(),
            DecayFunc::Linear => (1.0 - d / s).max(0.0),
            DecayFunc::Exp => self.decay.powf(d / s),
            DecayFunc::Diff => 1.0 / (1.0 + d),
        }
    }

    /// Extract the numeric field value from a document as i64.
    pub fn doc_score(&self, fields: &crate::schema::SparseFields) -> f64 {
        let val = match fields.get(self.field_id as usize) {
            Some(FieldValue::Int32(n)) => *n as i64,
            Some(FieldValue::Int64(n)) => *n,
            Some(FieldValue::Float64(f)) => *f as i64,
            _ => return 0.0,
        };
        self.score(val)
    }

    /// Monotonically equivalent score for sorting only — avoids transcendentals.
    /// Preserves sort order relative to `score()` but values differ.
    /// - Gauss exp(-d²/(2s²)): monotone decreasing in d² → return -d*d
    /// - Exp decay^(d/s): monotone decreasing in d → return -d
    /// - Diff 1/(1+d): monotone decreasing in d → return -d
    /// - Linear: already cheap, use real score
    pub fn sort_score(&self, field_value: i64) -> f64 {
        let distance = (field_value - self.origin).abs() - self.offset;
        if distance <= 0 {
            return 1.0;
        }
        let d = distance as f64;
        match self.func {
            DecayFunc::Gauss => -(d * d),
            DecayFunc::Linear => (1.0 - d / self.scale as f64).max(0.0),
            DecayFunc::Exp => -d,
            DecayFunc::Diff => -d,
        }
    }
}

/// Parse a decay sort expression from a sort_by field part.
/// Format: `field_name(origin:VALUE,func:FUNC,scale:VALUE[,decay:VALUE][,offset:VALUE])`
/// Returns `(DecaySort, remaining_str_after_parens)` or error.
pub fn parse_decay_expr(
    field_str: &str,
    schema: &crate::schema::Schema,
) -> Result<DecaySort, String> {
    let paren_start = field_str
        .find('(')
        .ok_or("Expected '(' in decay expression")?;
    let paren_end = field_str
        .rfind(')')
        .ok_or("Expected ')' in decay expression")?;

    let field_name = field_str[..paren_start].trim();
    let params_str = &field_str[paren_start + 1..paren_end];

    let field_id = schema
        .name_to_id
        .get(field_name)
        .ok_or_else(|| format!("Decay sort field `{field_name}` not found in schema."))?;

    let field_def = &schema.fields[*field_id as usize];
    if !field_def.sort {
        return Err(format!(
            "Field `{field_name}` is not a sortable field (required for decay sort)."
        ));
    }

    let mut origin: Option<i64> = None;
    let mut func: Option<DecayFunc> = None;
    let mut scale: Option<i64> = None;
    let mut decay = 0.5;
    let mut offset = 0i64;

    for param in params_str.split(',') {
        let param = param.trim();
        let (key, value) = param
            .split_once(':')
            .ok_or_else(|| format!("Invalid decay param: `{param}`. Expected `key:value`."))?;
        match key.trim() {
            "origin" => {
                origin = Some(value.trim().parse::<i64>().map_err(|_| {
                    format!("Invalid origin value: `{}`", value.trim())
                })?);
            }
            "func" => {
                func = Some(match value.trim() {
                    "gauss" => DecayFunc::Gauss,
                    "linear" => DecayFunc::Linear,
                    "exp" => DecayFunc::Exp,
                    "diff" => DecayFunc::Diff,
                    other => return Err(format!("Unknown decay function: `{other}`")),
                });
            }
            "scale" => {
                scale = Some(value.trim().parse::<i64>().map_err(|_| {
                    format!("Invalid scale value: `{}`", value.trim())
                })?);
            }
            "decay" => {
                decay = value.trim().parse::<f64>().map_err(|_| {
                    format!("Invalid decay value: `{}`", value.trim())
                })?;
                if !(0.0..=1.0).contains(&decay) {
                    return Err(format!("Decay must be between 0.0 and 1.0, got {decay}"));
                }
            }
            "offset" => {
                offset = value.trim().parse::<i64>().map_err(|_| {
                    format!("Invalid offset value: `{}`", value.trim())
                })?;
            }
            other => return Err(format!("Unknown decay param: `{other}`")),
        }
    }

    let origin = origin.ok_or("Missing required param `origin` in decay expression")?;
    let func = func.ok_or("Missing required param `func` in decay expression")?;
    let scale = scale.ok_or("Missing required param `scale` in decay expression")?;
    if scale == 0 {
        return Err("Decay scale must be non-zero".to_string());
    }

    Ok(DecaySort {
        field_id: *field_id,
        origin,
        func,
        scale,
        decay,
        offset,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_decay(func: DecayFunc, origin: i64, scale: i64, decay: f64, offset: i64) -> DecaySort {
        DecaySort {
            field_id: 0,
            origin,
            func,
            scale,
            decay,
            offset,
        }
    }

    #[test]
    fn gauss_at_origin_is_one() {
        let d = make_decay(DecayFunc::Gauss, 0, 100, 0.5, 0);
        assert!((d.score(0) - 1.0).abs() < 1e-10);
    }

    #[test]
    fn gauss_decays_with_distance() {
        let d = make_decay(DecayFunc::Gauss, 0, 100, 0.5, 0);
        let at_50 = d.score(50);
        let at_100 = d.score(100);
        assert!(at_50 > at_100);
        assert!(at_50 < 1.0);
        assert!(at_100 > 0.0);
    }

    #[test]
    fn linear_at_origin_is_one() {
        let d = make_decay(DecayFunc::Linear, 0, 100, 0.5, 0);
        assert!((d.score(0) - 1.0).abs() < 1e-10);
    }

    #[test]
    fn linear_at_scale_is_zero() {
        let d = make_decay(DecayFunc::Linear, 0, 100, 0.5, 0);
        assert!((d.score(100) - 0.0).abs() < 1e-10);
    }

    #[test]
    fn linear_at_half_scale() {
        let d = make_decay(DecayFunc::Linear, 0, 100, 0.5, 0);
        assert!((d.score(50) - 0.5).abs() < 1e-10);
    }

    #[test]
    fn exp_at_origin_is_one() {
        let d = make_decay(DecayFunc::Exp, 0, 100, 0.5, 0);
        assert!((d.score(0) - 1.0).abs() < 1e-10);
    }

    #[test]
    fn exp_at_scale_equals_decay() {
        let d = make_decay(DecayFunc::Exp, 0, 100, 0.5, 0);
        assert!((d.score(100) - 0.5).abs() < 1e-10);
    }

    #[test]
    fn diff_at_origin_is_one() {
        let d = make_decay(DecayFunc::Diff, 0, 100, 0.5, 0);
        assert!((d.score(0) - 1.0).abs() < 1e-10);
    }

    #[test]
    fn diff_decreases_with_distance() {
        let d = make_decay(DecayFunc::Diff, 0, 100, 0.5, 0);
        let at_1 = d.score(1);
        let at_10 = d.score(10);
        assert!((at_1 - 0.5).abs() < 1e-10); // 1/(1+1) = 0.5
        assert!(at_10 < at_1);
    }

    #[test]
    fn offset_keeps_score_at_one() {
        let d = make_decay(DecayFunc::Linear, 100, 50, 0.5, 20);
        // distance = |90-100| - 20 = 10 - 20 = -10 → score = 1.0
        assert!((d.score(90) - 1.0).abs() < 1e-10);
        // distance = |70-100| - 20 = 30 - 20 = 10 → score = 1.0 - 10/50 = 0.8
        assert!((d.score(70) - 0.8).abs() < 1e-10);
    }

    #[test]
    fn sort_score_preserves_gauss_order() {
        let d = make_decay(DecayFunc::Gauss, 0, 100, 0.5, 0);
        // sort_score must preserve the same ordering as score
        let s50 = d.sort_score(50);
        let s100 = d.sort_score(100);
        assert!(s50 > s100);
        assert!((d.sort_score(0) - 1.0).abs() < 1e-10);
    }

    #[test]
    fn sort_score_preserves_exp_order() {
        let d = make_decay(DecayFunc::Exp, 0, 100, 0.5, 0);
        let s50 = d.sort_score(50);
        let s100 = d.sort_score(100);
        assert!(s50 > s100);
        assert!((d.sort_score(0) - 1.0).abs() < 1e-10);
    }

    #[test]
    fn sort_score_preserves_diff_order() {
        let d = make_decay(DecayFunc::Diff, 0, 100, 0.5, 0);
        let s1 = d.sort_score(1);
        let s10 = d.sort_score(10);
        assert!(s1 > s10);
        assert!((d.sort_score(0) - 1.0).abs() < 1e-10);
    }

    #[test]
    fn sort_score_linear_matches_score() {
        let d = make_decay(DecayFunc::Linear, 0, 100, 0.5, 0);
        // Linear sort_score is the real score
        assert!((d.sort_score(50) - d.score(50)).abs() < 1e-10);
    }

    #[test]
    fn negative_distance_symmetric() {
        let d = make_decay(DecayFunc::Gauss, 100, 50, 0.5, 0);
        let left = d.score(50);
        let right = d.score(150);
        assert!((left - right).abs() < 1e-10);
    }

    #[test]
    fn parse_decay_expression() {
        use crate::schema::*;
        let mut schema = Schema {
            name: "test".into(),
            fields: vec![FieldDef {
                name: "ts".into(),
                field_type: FieldType::Int64,
                facet: false,
                index: false,
                optional: false,
                sort: true,
                stem: false,
                infix: false,
                locale: None,
                num_dim: None,
                vec_dist: None,
                id: 0,
                reference: None, drop: false,
                stem_dictionary: None,
                embed: None,
                name_matching: false,
            }],
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
        };
        schema.assign_field_ids();

        let ds = parse_decay_expr("ts(origin:1000,func:gauss,scale:100)", &schema).unwrap();
        assert_eq!(ds.origin, 1000);
        assert_eq!(ds.func, DecayFunc::Gauss);
        assert_eq!(ds.scale, 100);
        assert!((ds.decay - 0.5).abs() < 1e-10);
        assert_eq!(ds.offset, 0);
    }

    #[test]
    fn parse_decay_with_all_params() {
        use crate::schema::*;
        let mut schema = Schema {
            name: "test".into(),
            fields: vec![FieldDef {
                name: "ts".into(),
                field_type: FieldType::Int64,
                facet: false,
                index: false,
                optional: false,
                sort: true,
                stem: false,
                infix: false,
                locale: None,
                num_dim: None,
                vec_dist: None,
                id: 0,
                reference: None, drop: false,
                stem_dictionary: None,
                embed: None,
                name_matching: false,
            }],
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
        };
        schema.assign_field_ids();

        let ds = parse_decay_expr(
            "ts(origin:500,func:exp,scale:200,decay:0.3,offset:10)",
            &schema,
        )
        .unwrap();
        assert_eq!(ds.origin, 500);
        assert_eq!(ds.func, DecayFunc::Exp);
        assert_eq!(ds.scale, 200);
        assert!((ds.decay - 0.3).abs() < 1e-10);
        assert_eq!(ds.offset, 10);
    }
}
