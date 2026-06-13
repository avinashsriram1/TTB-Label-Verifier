use crate::matching::{
    extract_abv_candidates, extract_net_contents_candidates, extract_proof_candidates,
    is_proper_token_subset, normalize_text, normalized_contains, parse_abv_value,
    parse_net_contents_ml, similarity, token_overlap,
};
use crate::models::{
    CheckStatus, ExpectedFields, FieldCheck, OcrOutput, ProductInput, TextSpan, Verdict,
    VerificationResult,
};
use crate::ocr::OcrEngine;
use crate::warning::check_government_warning;
use std::collections::{BTreeMap, BTreeSet};
use std::time::Instant;

pub async fn verify_product(engine: &dyn OcrEngine, product: ProductInput) -> VerificationResult {
    let started = Instant::now();
    let mut outputs = Vec::with_capacity(product.images.len());
    let mut notes = Vec::new();

    for image in &product.images {
        match engine.read(image).await {
            Ok(output) => outputs.push(output),
            Err(err) => {
                notes.push(format!(
                    "OCR failed for {} using {}: {}",
                    image.filename,
                    engine.name(),
                    err
                ));
                outputs.push(OcrOutput {
                    image_id: image.image_id.clone(),
                    filename: image.filename.clone(),
                    engine: engine.name().to_string(),
                    raw_text: String::new(),
                    spans: Vec::new(),
                    warnings: vec![err.to_string()],
                    elapsed_ms: 0,
                });
            }
        }
    }

    let raw_text = outputs
        .iter()
        .map(|output| output.raw_text.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    let spans = outputs
        .iter()
        .flat_map(|output| output.spans.clone())
        .collect::<Vec<_>>();
    let engines = outputs
        .iter()
        .map(|output| output.engine.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();

    let fields = verify_fields(&product.expected, &raw_text, &spans);
    let government_warning = check_government_warning(&raw_text, &spans);
    let verdict = aggregate_verdict(&fields, &government_warning.status);

    VerificationResult {
        product_id: product.product_id,
        label: product.label,
        verdict,
        fields,
        government_warning,
        raw_text,
        spans,
        engines,
        image_count: product.images.len(),
        latency_ms: started.elapsed().as_millis(),
        notes,
    }
}

fn verify_fields(
    expected: &ExpectedFields,
    raw_text: &str,
    spans: &[TextSpan],
) -> BTreeMap<String, FieldCheck> {
    let mut fields = BTreeMap::new();

    fields.insert(
        "brand_name".to_string(),
        check_text_field(
            "brand_name",
            "Brand name",
            expected.brand_name.as_deref(),
            raw_text,
            spans,
            true,
        ),
    );
    fields.insert(
        "class_type".to_string(),
        check_class_type_field(expected.class_type.as_deref(), raw_text, spans),
    );
    fields.insert(
        "alcohol_content".to_string(),
        check_abv_field(expected.alcohol_content.as_deref(), raw_text, spans),
    );
    fields.insert(
        "net_contents".to_string(),
        check_net_contents_field(expected.net_contents.as_deref(), raw_text, spans),
    );

    if expected
        .bottler
        .as_deref()
        .is_some_and(|value| !value.trim().is_empty())
    {
        fields.insert(
            "bottler".to_string(),
            check_text_field(
                "bottler",
                "Bottler/producer",
                expected.bottler.as_deref(),
                raw_text,
                spans,
                false,
            ),
        );
    }

    if expected
        .country
        .as_deref()
        .is_some_and(|value| !value.trim().is_empty())
    {
        fields.insert(
            "country".to_string(),
            check_country_field(expected.country.as_deref(), raw_text, spans),
        );
    }

    fields
}

fn check_text_field(
    field_key: &str,
    label: &str,
    expected: Option<&str>,
    raw_text: &str,
    spans: &[TextSpan],
    required: bool,
) -> FieldCheck {
    let expected_value = expected.map(str::trim).filter(|value| !value.is_empty());
    let Some(expected_value) = expected_value else {
        return FieldCheck {
            field: field_key.to_string(),
            expected: None,
            observed: None,
            status: if required {
                CheckStatus::Review
            } else {
                CheckStatus::Missing
            },
            confidence: 0.0,
            detail: format!("{label} was not provided in the application data."),
            evidence: Vec::new(),
        };
    };

    let evidence = best_span_evidence(expected_value, spans);

    if normalized_contains(raw_text, expected_value) {
        return FieldCheck {
            field: field_key.to_string(),
            expected: Some(expected_value.to_string()),
            observed: Some(expected_value.to_string()),
            status: CheckStatus::Pass,
            confidence: 0.99,
            detail: format!("{label} appears on the label."),
            evidence,
        };
    }

    let overlap = token_overlap(expected_value, raw_text);
    let sim = evidence
        .first()
        .map(|span| similarity(expected_value, &span.text))
        .unwrap_or_else(|| similarity(expected_value, raw_text));
    let confidence = overlap.max(sim);

    if confidence >= 0.88 {
        FieldCheck {
            field: field_key.to_string(),
            expected: Some(expected_value.to_string()),
            observed: Some(expected_value.to_string()),
            status: CheckStatus::Pass,
            confidence,
            detail: format!("{label} matches with minor OCR or formatting noise."),
            evidence,
        }
    } else if confidence >= 0.62 || is_proper_token_subset(expected_value, raw_text) {
        FieldCheck {
            field: field_key.to_string(),
            expected: Some(expected_value.to_string()),
            observed: Some("Possible partial match".to_string()),
            status: CheckStatus::Review,
            confidence,
            detail: format!("{label} may match, but an agent should confirm it."),
            evidence,
        }
    } else {
        FieldCheck {
            field: field_key.to_string(),
            expected: Some(expected_value.to_string()),
            observed: None,
            status: CheckStatus::Fail,
            confidence,
            detail: format!("{label} was not found with enough confidence."),
            evidence,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CountryAlias {
    canonical: &'static str,
    aliases: &'static [&'static str],
}

const COUNTRY_ALIASES: &[CountryAlias] = &[
    CountryAlias {
        canonical: "United States",
        aliases: &[
            "united states",
            "united states of america",
            "usa",
            "u s a",
            "u.s.a.",
            "us",
            "u s",
            "america",
        ],
    },
    CountryAlias {
        canonical: "Austria",
        aliases: &["austria", "osterreich", "österreich", "product of austria"],
    },
    CountryAlias {
        canonical: "Mexico",
        aliases: &["mexico", "product of mexico"],
    },
    CountryAlias {
        canonical: "France",
        aliases: &["france", "product of france"],
    },
    CountryAlias {
        canonical: "Italy",
        aliases: &["italy", "product of italy"],
    },
    CountryAlias {
        canonical: "Spain",
        aliases: &["spain", "product of spain"],
    },
    CountryAlias {
        canonical: "Canada",
        aliases: &["canada", "product of canada"],
    },
    CountryAlias {
        canonical: "Germany",
        aliases: &["germany", "deutschland", "product of germany"],
    },
    CountryAlias {
        canonical: "Chile",
        aliases: &["chile", "product of chile"],
    },
    CountryAlias {
        canonical: "Argentina",
        aliases: &["argentina", "product of argentina"],
    },
    CountryAlias {
        canonical: "Australia",
        aliases: &["australia", "product of australia"],
    },
    CountryAlias {
        canonical: "New Zealand",
        aliases: &["new zealand", "product of new zealand"],
    },
];

const US_STATE_CODES: &[&str] = &[
    "AL", "AK", "AZ", "AR", "CA", "CO", "CT", "DE", "FL", "GA", "HI", "IA", "ID", "IL", "IN", "KS",
    "KY", "LA", "MA", "MD", "ME", "MI", "MN", "MO", "MS", "MT", "NC", "ND", "NE", "NH", "NJ", "NM",
    "NV", "NY", "OH", "OK", "OR", "PA", "RI", "SC", "SD", "TN", "TX", "UT", "VA", "VT", "WA", "WI",
    "WV", "WY", "DC",
];

fn check_country_field(expected: Option<&str>, raw_text: &str, spans: &[TextSpan]) -> FieldCheck {
    let expected_value = expected.map(str::trim).filter(|value| !value.is_empty());
    let Some(expected_value) = expected_value else {
        return FieldCheck {
            field: "country".to_string(),
            expected: None,
            observed: None,
            status: CheckStatus::Missing,
            confidence: 0.0,
            detail: "Country of origin was not provided in the application data.".to_string(),
            evidence: Vec::new(),
        };
    };

    let expected_country = normalize_country(expected_value);
    let observed_country = detect_country(raw_text);
    let evidence = country_evidence(observed_country.as_deref(), spans);

    match (expected_country, observed_country) {
        (Some(expected), Some(observed)) if expected == observed => FieldCheck {
            field: "country".to_string(),
            expected: Some(expected_value.to_string()),
            observed: Some(observed),
            status: CheckStatus::Pass,
            confidence: 0.95,
            detail: "Country of origin was inferred from country text or location evidence.".to_string(),
            evidence,
        },
        (Some(_expected), Some(observed)) => FieldCheck {
            field: "country".to_string(),
            expected: Some(expected_value.to_string()),
            observed: Some(observed),
            status: CheckStatus::Fail,
            confidence: 0.1,
            detail: "Country of origin conflicts with the application data.".to_string(),
            evidence,
        },
        (Some(_expected), None) => FieldCheck {
            field: "country".to_string(),
            expected: Some(expected_value.to_string()),
            observed: None,
            status: CheckStatus::Fail,
            confidence: 0.0,
            detail: "Country of origin was not found with enough confidence.".to_string(),
            evidence,
        },
        (None, Some(observed)) => FieldCheck {
            field: "country".to_string(),
            expected: Some(expected_value.to_string()),
            observed: Some(observed),
            status: CheckStatus::Review,
            confidence: 0.65,
            detail: "A country was inferred, but the expected country is outside the built-in country aliases.".to_string(),
            evidence,
        },
        (None, None) => check_text_field(
            "country",
            "Country of origin",
            Some(expected_value),
            raw_text,
            spans,
            false,
        ),
    }
}

fn normalize_country(input: &str) -> Option<String> {
    let norm = normalize_text(input);
    COUNTRY_ALIASES
        .iter()
        .find(|country| {
            country.aliases.iter().any(|alias| {
                let alias_norm = normalize_text(alias);
                norm == alias_norm || contains_word_phrase(&norm, &alias_norm)
            })
        })
        .map(|country| country.canonical.to_string())
}

fn detect_country(raw_text: &str) -> Option<String> {
    let norm = normalize_text(raw_text);
    let explicit = COUNTRY_ALIASES
        .iter()
        .find(|country| {
            country
                .aliases
                .iter()
                .any(|alias| contains_word_phrase(&norm, &normalize_text(alias)))
        })
        .map(|country| country.canonical.to_string());

    explicit.or_else(|| {
        if contains_us_state_location(raw_text) {
            Some("United States".to_string())
        } else {
            None
        }
    })
}

fn contains_word_phrase(raw_norm: &str, phrase_norm: &str) -> bool {
    if phrase_norm.is_empty() {
        return false;
    }
    let pattern = format!(r"(?:^|\s){}(?:\s|$)", regex::escape(phrase_norm));
    regex::Regex::new(&pattern)
        .expect("valid word phrase regex")
        .is_match(raw_norm)
}

fn contains_us_state_location(raw_text: &str) -> bool {
    US_STATE_CODES.iter().any(|state| {
        let pattern = format!(r"(?i)\b[A-Z][A-Za-z .'-]+,\s*{}\b", regex::escape(state));
        regex::Regex::new(&pattern)
            .expect("valid US location regex")
            .is_match(raw_text)
    })
}

fn country_evidence(observed: Option<&str>, spans: &[TextSpan]) -> Vec<TextSpan> {
    let Some(observed) = observed else {
        return Vec::new();
    };
    let observed_norm = normalize_text(observed);
    spans
        .iter()
        .filter(|span| {
            let norm = normalize_text(&span.text);
            observed_norm
                .split_whitespace()
                .any(|token| token.len() > 2 && norm.contains(token))
                || US_STATE_CODES
                    .iter()
                    .any(|state| norm == state.to_lowercase())
        })
        .take(12)
        .cloned()
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ClassAlias {
    canonical: &'static str,
    family: &'static str,
    aliases: &'static [&'static str],
}

const CLASS_ALIASES: &[ClassAlias] = &[
    ClassAlias {
        canonical: "White Wine",
        family: "wine",
        aliases: &["white wine", "dry white wine", "still white wine"],
    },
    ClassAlias {
        canonical: "Red Wine",
        family: "wine",
        aliases: &["red wine"],
    },
    ClassAlias {
        canonical: "Rose Wine",
        family: "wine",
        aliases: &["rose wine", "rosé wine"],
    },
    ClassAlias {
        canonical: "Sparkling Wine",
        family: "wine",
        aliases: &["sparkling wine", "champagne", "prosecco"],
    },
    ClassAlias {
        canonical: "Dessert Wine",
        family: "wine",
        aliases: &["dessert wine"],
    },
    ClassAlias {
        canonical: "Fortified Wine",
        family: "wine",
        aliases: &["fortified wine", "port wine", "sherry"],
    },
    ClassAlias {
        canonical: "Wine",
        family: "wine",
        aliases: &["wine"],
    },
    ClassAlias {
        canonical: "Beer",
        family: "beer",
        aliases: &[
            "beer",
            "lager",
            "ale",
            "ipa",
            "stout",
            "porter",
            "malt beverage",
        ],
    },
    ClassAlias {
        canonical: "Cider",
        family: "beer",
        aliases: &["cider", "hard cider"],
    },
    ClassAlias {
        canonical: "Whiskey",
        family: "spirits",
        aliases: &[
            "whiskey",
            "whisky",
            "bourbon",
            "rye whiskey",
            "straight bourbon",
        ],
    },
    ClassAlias {
        canonical: "Vodka",
        family: "spirits",
        aliases: &["vodka"],
    },
    ClassAlias {
        canonical: "Gin",
        family: "spirits",
        aliases: &["gin"],
    },
    ClassAlias {
        canonical: "Rum",
        family: "spirits",
        aliases: &["rum"],
    },
    ClassAlias {
        canonical: "Tequila",
        family: "spirits",
        aliases: &["tequila", "mezcal"],
    },
    ClassAlias {
        canonical: "Brandy",
        family: "spirits",
        aliases: &["brandy", "cognac"],
    },
    ClassAlias {
        canonical: "Liqueur",
        family: "spirits",
        aliases: &["liqueur", "cordial"],
    },
    ClassAlias {
        canonical: "Spirits",
        family: "spirits",
        aliases: &["spirits", "distilled spirits"],
    },
];

fn check_class_type_field(
    expected: Option<&str>,
    raw_text: &str,
    spans: &[TextSpan],
) -> FieldCheck {
    let expected_value = expected.map(str::trim).filter(|value| !value.is_empty());
    let Some(expected_value) = expected_value else {
        return FieldCheck {
            field: "class_type".to_string(),
            expected: None,
            observed: None,
            status: CheckStatus::Review,
            confidence: 0.0,
            detail: "Class/type was not provided in the application data.".to_string(),
            evidence: Vec::new(),
        };
    };

    let expected_class = detect_expected_class(expected_value);
    let observed_class = detect_observed_class(raw_text);
    let evidence = observed_class
        .map(|class| class_evidence(class, spans))
        .unwrap_or_default();

    match (expected_class, observed_class) {
        (Some(expected), Some(observed)) if expected.canonical == observed.canonical => {
            FieldCheck {
                field: "class_type".to_string(),
                expected: Some(expected_value.to_string()),
                observed: Some(observed.canonical.to_string()),
                status: CheckStatus::Pass,
                confidence: 0.99,
                detail: "Class/type matches a known alcohol class.".to_string(),
                evidence,
            }
        }
        (Some(expected), Some(observed)) if expected.family == observed.family => FieldCheck {
            field: "class_type".to_string(),
            expected: Some(expected_value.to_string()),
            observed: Some(observed.canonical.to_string()),
            status: CheckStatus::Pass,
            confidence: 0.9,
            detail: "Class/type is compatible with the expected alcohol family.".to_string(),
            evidence,
        },
        (Some(_expected), Some(observed)) => FieldCheck {
            field: "class_type".to_string(),
            expected: Some(expected_value.to_string()),
            observed: Some(observed.canonical.to_string()),
            status: CheckStatus::Fail,
            confidence: 0.1,
            detail: "Class/type conflicts with the expected alcohol category.".to_string(),
            evidence,
        },
        (Some(_expected), None) => {
            let fallback = check_text_field(
                "class_type",
                "Class/type",
                Some(expected_value),
                raw_text,
                spans,
                true,
            );
            FieldCheck {
                observed: fallback.observed,
                detail:
                    "No known class/type phrase was detected; falling back to fuzzy text matching."
                        .to_string(),
                ..fallback
            }
        }
        (None, Some(observed)) => {
            let fallback = check_text_field(
                "class_type",
                "Class/type",
                Some(expected_value),
                raw_text,
                spans,
                true,
            );
            FieldCheck {
                observed: Some(observed.canonical.to_string()),
                status: if matches!(fallback.status, CheckStatus::Pass) {
                    CheckStatus::Pass
                } else {
                    CheckStatus::Review
                },
                confidence: fallback.confidence.max(0.65),
                detail: "A known class/type was found, but the expected value is outside the curated taxonomy.".to_string(),
                evidence,
                ..fallback
            }
        }
        (None, None) => check_text_field(
            "class_type",
            "Class/type",
            Some(expected_value),
            raw_text,
            spans,
            true,
        ),
    }
}

fn detect_expected_class(expected: &str) -> Option<ClassAlias> {
    let expected_norm = normalize_text(expected);
    CLASS_ALIASES.iter().copied().find(|class| {
        class
            .aliases
            .iter()
            .any(|alias| expected_norm.contains(&normalize_text(alias)))
    })
}

fn detect_observed_class(raw_text: &str) -> Option<ClassAlias> {
    let raw_norm = normalize_text(raw_text);
    CLASS_ALIASES
        .iter()
        .copied()
        .filter_map(|class| {
            class
                .aliases
                .iter()
                .filter(|alias| contains_class_phrase(&raw_norm, alias))
                .map(|alias| (class, normalize_text(alias).len()))
                .max_by_key(|(_, score)| *score)
        })
        .max_by_key(|(_, score)| *score)
        .map(|(class, _)| class)
}

fn contains_class_phrase(raw_norm: &str, alias: &str) -> bool {
    let alias_norm = normalize_text(alias);
    if alias_norm.is_empty() {
        return false;
    }

    let pattern = format!(r"(?:^|\s){}(?:\s|$)", regex::escape(&alias_norm));
    regex::Regex::new(&pattern)
        .expect("valid class phrase regex")
        .is_match(raw_norm)
}

fn class_evidence(class: ClassAlias, spans: &[TextSpan]) -> Vec<TextSpan> {
    spans
        .iter()
        .filter(|span| {
            let norm = normalize_text(&span.text);
            class.aliases.iter().any(|alias| {
                normalize_text(alias)
                    .split_whitespace()
                    .any(|token| norm == token)
            }) || class
                .aliases
                .iter()
                .any(|alias| norm.contains(&normalize_text(alias)))
        })
        .take(12)
        .cloned()
        .collect()
}

fn check_abv_field(expected: Option<&str>, raw_text: &str, spans: &[TextSpan]) -> FieldCheck {
    let expected_value = expected.map(str::trim).filter(|value| !value.is_empty());
    let Some(expected_value) = expected_value else {
        return FieldCheck {
            field: "alcohol_content".to_string(),
            expected: None,
            observed: None,
            status: CheckStatus::Review,
            confidence: 0.0,
            detail: "Alcohol content was not provided in the application data.".to_string(),
            evidence: Vec::new(),
        };
    };

    let Some(expected_abv) = parse_abv_value(expected_value) else {
        return FieldCheck {
            field: "alcohol_content".to_string(),
            expected: Some(expected_value.to_string()),
            observed: None,
            status: CheckStatus::Review,
            confidence: 0.0,
            detail: "Expected alcohol content could not be parsed.".to_string(),
            evidence: Vec::new(),
        };
    };

    let percent_candidates = extract_abv_candidates(raw_text);
    let proofs = extract_proof_candidates(raw_text);
    let mut candidates = percent_candidates.clone();
    candidates.extend(proofs.iter().map(|proof| proof / 2.0));
    let evidence = numeric_evidence(spans, &["%", "abv", "alc", "proof"]);

    if candidates.is_empty() {
        return FieldCheck {
            field: "alcohol_content".to_string(),
            expected: Some(expected_value.to_string()),
            observed: None,
            status: CheckStatus::Fail,
            confidence: 0.0,
            detail: "No ABV value was found on the label.".to_string(),
            evidence,
        };
    }

    let best = candidates
        .iter()
        .copied()
        .min_by(|a, b| {
            (a - expected_abv)
                .abs()
                .partial_cmp(&(b - expected_abv).abs())
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .unwrap();
    let delta = (best - expected_abv).abs();
    let matching_proof = proofs
        .iter()
        .copied()
        .find(|proof| (proof / 2.0 - best).abs() <= 0.25);
    let observed = format_abv_observed(best, matching_proof);

    if delta <= 0.15 {
        FieldCheck {
            field: "alcohol_content".to_string(),
            expected: Some(expected_value.to_string()),
            observed: Some(observed),
            status: CheckStatus::Pass,
            confidence: 0.99,
            detail: "Alcohol content matches.".to_string(),
            evidence,
        }
    } else if delta <= 0.5 {
        FieldCheck {
            field: "alcohol_content".to_string(),
            expected: Some(expected_value.to_string()),
            observed: Some(observed),
            status: CheckStatus::Review,
            confidence: 0.75,
            detail: "Alcohol content is close but should be confirmed by an agent.".to_string(),
            evidence,
        }
    } else {
        FieldCheck {
            field: "alcohol_content".to_string(),
            expected: Some(expected_value.to_string()),
            observed: Some(observed),
            status: CheckStatus::Fail,
            confidence: 0.0,
            detail: "Alcohol content does not match the application data.".to_string(),
            evidence,
        }
    }
}

fn format_abv_observed(abv: f32, proof: Option<f32>) -> String {
    match proof {
        Some(proof) => format!("{abv:.2}% / {proof:.0} Proof"),
        None => format!("{abv:.2}%"),
    }
}

fn check_net_contents_field(
    expected: Option<&str>,
    raw_text: &str,
    spans: &[TextSpan],
) -> FieldCheck {
    let expected_value = expected.map(str::trim).filter(|value| !value.is_empty());
    let Some(expected_value) = expected_value else {
        return FieldCheck {
            field: "net_contents".to_string(),
            expected: None,
            observed: None,
            status: CheckStatus::Review,
            confidence: 0.0,
            detail: "Net contents were not provided in the application data.".to_string(),
            evidence: Vec::new(),
        };
    };

    let Some(expected_ml) = parse_net_contents_ml(expected_value) else {
        return FieldCheck {
            field: "net_contents".to_string(),
            expected: Some(expected_value.to_string()),
            observed: None,
            status: CheckStatus::Review,
            confidence: 0.0,
            detail: "Expected net contents could not be parsed.".to_string(),
            evidence: Vec::new(),
        };
    };

    let candidates = extract_net_contents_candidates(raw_text);
    let evidence = numeric_evidence(spans, &["ml", "cl", "liter", "litre", "oz"]);

    if candidates.is_empty() {
        return FieldCheck {
            field: "net_contents".to_string(),
            expected: Some(expected_value.to_string()),
            observed: None,
            status: CheckStatus::Fail,
            confidence: 0.0,
            detail: "No net contents value was found on the label.".to_string(),
            evidence,
        };
    }

    let best = candidates
        .iter()
        .copied()
        .min_by(|a, b| {
            (a - expected_ml)
                .abs()
                .partial_cmp(&(b - expected_ml).abs())
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .unwrap();
    let delta = (best - expected_ml).abs();
    let tolerance = expected_ml.max(1.0) * 0.01;

    if delta <= tolerance.max(2.0) {
        FieldCheck {
            field: "net_contents".to_string(),
            expected: Some(expected_value.to_string()),
            observed: Some(format!("{best:.0} mL")),
            status: CheckStatus::Pass,
            confidence: 0.99,
            detail: "Net contents match after unit normalization.".to_string(),
            evidence,
        }
    } else if delta <= tolerance.max(10.0) {
        FieldCheck {
            field: "net_contents".to_string(),
            expected: Some(expected_value.to_string()),
            observed: Some(format!("{best:.0} mL")),
            status: CheckStatus::Review,
            confidence: 0.72,
            detail: "Net contents are close but should be confirmed by an agent.".to_string(),
            evidence,
        }
    } else {
        FieldCheck {
            field: "net_contents".to_string(),
            expected: Some(expected_value.to_string()),
            observed: Some(format!("{best:.0} mL")),
            status: CheckStatus::Fail,
            confidence: 0.0,
            detail: "Net contents do not match the application data.".to_string(),
            evidence,
        }
    }
}

fn best_span_evidence(expected: &str, spans: &[TextSpan]) -> Vec<TextSpan> {
    let mut scored = spans
        .iter()
        .map(|span| {
            let score = similarity(expected, &span.text).max(token_overlap(expected, &span.text));
            (score, span)
        })
        .filter(|(score, _)| *score >= 0.45)
        .collect::<Vec<_>>();

    scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    scored
        .into_iter()
        .take(8)
        .map(|(_, span)| span.clone())
        .collect()
}

fn numeric_evidence(spans: &[TextSpan], needles: &[&str]) -> Vec<TextSpan> {
    spans
        .iter()
        .filter(|span| {
            let text = span.text.to_lowercase();
            text.chars().any(|ch| ch.is_ascii_digit())
                || needles.iter().any(|needle| text.contains(needle))
        })
        .take(12)
        .cloned()
        .collect()
}

fn aggregate_verdict(
    fields: &BTreeMap<String, FieldCheck>,
    warning_status: &CheckStatus,
) -> Verdict {
    if matches!(warning_status, CheckStatus::Fail | CheckStatus::Missing)
        || fields
            .values()
            .any(|field| matches!(field.status, CheckStatus::Fail))
    {
        return Verdict::Fail;
    }

    if matches!(warning_status, CheckStatus::Review)
        || fields
            .values()
            .any(|field| matches!(field.status, CheckStatus::Review | CheckStatus::Missing))
    {
        return Verdict::Review;
    }

    Verdict::Pass
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{ExpectedFields, ImagePayload, OcrOutput, ProductInput};
    use crate::ocr::OcrEngine;
    use crate::warning::CANONICAL_WARNING;
    use anyhow::Result;

    #[test]
    fn field_matching_tolerates_minor_brand_noise() {
        let check = check_text_field(
            "brand_name",
            "Brand name",
            Some("Old Town Whisky"),
            "OLD TOWN WHIKSY Kentucky Straight Bourbon Whiskey",
            &[],
            true,
        );
        assert_eq!(check.status, CheckStatus::Pass);
    }

    #[test]
    fn abv_mismatch_fails() {
        let check = check_abv_field(Some("45% Alc./Vol."), "40% Alc./Vol.", &[]);
        assert_eq!(check.status, CheckStatus::Fail);
    }

    #[test]
    fn proof_expected_matches_percent_and_proof_ocr() {
        let check = check_abv_field(Some("80 Proof"), "40% Alc./Vol. (80 Proof)", &[]);
        assert_eq!(check.status, CheckStatus::Pass);
        assert_eq!(check.observed.as_deref(), Some("40.00% / 80 Proof"));
    }

    #[test]
    fn percent_expected_matches_proof_ocr() {
        let check = check_abv_field(Some("40%"), "80 Proof", &[]);
        assert_eq!(check.status, CheckStatus::Pass);
        assert_eq!(check.observed.as_deref(), Some("40.00% / 80 Proof"));
    }

    #[test]
    fn proof_expected_fails_wrong_percent() {
        let check = check_abv_field(Some("80 Proof"), "35% Alc./Vol.", &[]);
        assert_eq!(check.status, CheckStatus::Fail);
    }

    #[test]
    fn net_contents_normalizes_units() {
        let check = check_net_contents_field(Some("750 mL"), "Net Contents 75 cl", &[]);
        assert_eq!(check.status, CheckStatus::Pass);
    }

    #[test]
    fn class_type_detects_white_wine_from_generic_text() {
        let check = check_class_type_field(
            Some("Dry White Wine"),
            "An uncomplicated but expressively bold and fresh white wine for many occasions.",
            &[],
        );
        assert_eq!(check.status, CheckStatus::Pass);
        assert_eq!(check.observed.as_deref(), Some("White Wine"));
    }

    #[test]
    fn class_type_detects_still_white_wine() {
        let check = check_class_type_field(
            Some("Dry White Wine"),
            "CONTAINS SULFITES STILL WHITE WINE",
            &[],
        );
        assert_eq!(check.status, CheckStatus::Pass);
        assert_eq!(check.observed.as_deref(), Some("White Wine"));
    }

    #[test]
    fn class_type_accepts_parent_wine_category() {
        let check = check_class_type_field(Some("Wine"), "STILL WHITE WINE", &[]);
        assert_eq!(check.status, CheckStatus::Pass);
        assert_eq!(check.observed.as_deref(), Some("White Wine"));
    }

    #[test]
    fn class_type_accepts_spirits_alias() {
        let check = check_class_type_field(
            Some("Kentucky Straight Bourbon Whiskey"),
            "OLD TOM DISTILLERY BOURBON 90 PROOF",
            &[],
        );
        assert_eq!(check.status, CheckStatus::Pass);
        assert_eq!(check.observed.as_deref(), Some("Whiskey"));
    }

    #[test]
    fn class_type_accepts_beer_alias() {
        let check = check_class_type_field(Some("Beer"), "CRISP PILSNER LAGER 5% ALC/VOL", &[]);
        assert_eq!(check.status, CheckStatus::Pass);
        assert_eq!(check.observed.as_deref(), Some("Beer"));
    }

    #[test]
    fn class_type_fails_conflicting_category() {
        let check =
            check_class_type_field(Some("White Wine"), "KENTUCKY STRAIGHT BOURBON WHISKEY", &[]);
        assert_eq!(check.status, CheckStatus::Fail);
        assert_eq!(check.observed.as_deref(), Some("Whiskey"));
    }

    #[test]
    fn country_infers_united_states_from_city_state() {
        let check = check_country_field(
            Some("United States"),
            "Imported by Blue Heron Imports, San Diego, CA",
            &[],
        );
        assert_eq!(check.status, CheckStatus::Pass);
        assert_eq!(check.observed.as_deref(), Some("United States"));
    }

    #[test]
    fn country_infers_united_states_from_michigan() {
        let check = check_country_field(Some("United States"), "Grand Rapids, MI 49546", &[]);
        assert_eq!(check.status, CheckStatus::Pass);
        assert_eq!(check.observed.as_deref(), Some("United States"));
    }

    #[test]
    fn country_accepts_usa_alias() {
        let check = check_country_field(Some("USA"), "Imported by Cedar Knolls, NJ", &[]);
        assert_eq!(check.status, CheckStatus::Pass);
        assert_eq!(check.observed.as_deref(), Some("United States"));
    }

    #[test]
    fn country_accepts_austria() {
        let check = check_country_field(Some("Austria"), "Product of Austria", &[]);
        assert_eq!(check.status, CheckStatus::Pass);
        assert_eq!(check.observed.as_deref(), Some("Austria"));
    }

    #[test]
    fn country_fails_conflicting_country() {
        let check = check_country_field(Some("United States"), "Product of Austria", &[]);
        assert_eq!(check.status, CheckStatus::Fail);
        assert_eq!(check.observed.as_deref(), Some("Austria"));
    }

    #[test]
    fn failed_text_fields_do_not_expose_raw_ocr_summary() {
        let check = check_text_field(
            "brand_name",
            "Brand name",
            Some("Blue Heron"),
            "A very long unrelated OCR paragraph that should not be copied into the observed column",
            &[],
            true,
        );
        assert_eq!(check.status, CheckStatus::Fail);
        assert!(check.observed.is_none());
    }

    #[test]
    fn fuzzy_pass_text_fields_show_expected_not_ocr_blob() {
        let raw = "Ke BR AYN D 4 y . y ] ] Ling|RO) UE UDI 35% ALC/VOL (70 PROOF) 750 ML ecstas y THE PREMIUM POMEGRANATE LIQUEUR";
        let check = check_text_field("brand_name", "Brand name", Some("Ecstasy"), raw, &[], true);
        assert_eq!(check.status, CheckStatus::Pass);
        assert_eq!(check.observed.as_deref(), Some("Ecstasy"));
    }

    #[test]
    fn review_text_fields_show_neutral_observed_value() {
        let check = check_text_field(
            "brand_name",
            "Brand name",
            Some("Old Town Whisky"),
            "OLD TOWN Kentucky Bourbon",
            &[],
            true,
        );
        assert_eq!(check.status, CheckStatus::Review);
        assert_eq!(check.observed.as_deref(), Some("Possible partial match"));
    }

    #[test]
    fn aggregate_warns_when_expected_data_missing() {
        let fields = verify_fields(&ExpectedFields::default(), "", &[]);
        assert_eq!(fields["brand_name"].status, CheckStatus::Review);
    }

    #[derive(Debug)]
    struct MockEngine;

    #[async_trait::async_trait]
    impl OcrEngine for MockEngine {
        fn name(&self) -> &'static str {
            "mock"
        }

        fn is_available(&self) -> bool {
            true
        }

        async fn read(&self, image: &ImagePayload) -> Result<OcrOutput> {
            let raw_text = String::from_utf8_lossy(&image.bytes).to_string();
            Ok(OcrOutput {
                image_id: image.image_id.clone(),
                filename: image.filename.clone(),
                engine: self.name().to_string(),
                raw_text,
                spans: Vec::new(),
                warnings: Vec::new(),
                elapsed_ms: 1,
            })
        }
    }

    #[derive(Debug)]
    struct FailingEngine;

    #[async_trait::async_trait]
    impl OcrEngine for FailingEngine {
        fn name(&self) -> &'static str {
            "failing"
        }

        fn is_available(&self) -> bool {
            false
        }

        async fn read(&self, _image: &ImagePayload) -> Result<OcrOutput> {
            anyhow::bail!("simulated OCR failure")
        }
    }

    #[tokio::test]
    async fn multi_image_product_merges_front_and_back_text() {
        let product = ProductInput {
            product_id: "old-tom".to_string(),
            label: Some("Old Tom".to_string()),
            expected: ExpectedFields {
                brand_name: Some("Old Tom Distillery".to_string()),
                class_type: Some("Kentucky Straight Bourbon Whiskey".to_string()),
                alcohol_content: Some("45% Alc./Vol.".to_string()),
                net_contents: Some("750 mL".to_string()),
                bottler: None,
                country: None,
            },
            images: vec![
                ImagePayload {
                    image_id: "front".to_string(),
                    filename: "front.txt".to_string(),
                    content_type: Some("text/plain".to_string()),
                    bytes:
                        b"OLD TOM DISTILLERY Kentucky Straight Bourbon Whiskey 45% Alc./Vol. 750 mL"
                            .to_vec(),
                },
                ImagePayload {
                    image_id: "back".to_string(),
                    filename: "back.txt".to_string(),
                    content_type: Some("text/plain".to_string()),
                    bytes: CANONICAL_WARNING.as_bytes().to_vec(),
                },
            ],
        };

        let result = verify_product(&MockEngine, product).await;
        assert_eq!(result.fields["brand_name"].status, CheckStatus::Pass);
        assert_eq!(result.fields["alcohol_content"].status, CheckStatus::Pass);
        assert!(result.government_warning.present);
        assert_eq!(result.verdict, Verdict::Pass);
    }

    #[tokio::test]
    async fn lenz_moser_sample_ocr_text_passes_core_fields() {
        let ocr_text = r#"y Lenz Moser’
A4 D
1 co E R
GRUNER VELTLINER N
PRODUCT OF AUSTRIA: NIEDEROSTERREICH
‘1.0L 12%ALC/VOL.
fresh white wine for many occasions.
1Le Qualititswein aus Osterreich 12% ALC./VOL.
GOVERNMENT WARNING: (1) ACCORDING TO THE SURGEON
GENERAL, WOMEN SHOULD NOT DRINK ALCOHOLIC BEVERAGES
DURING PREGNANCY BECAUSE OF THE RISK OF BIRTH DEFECTS.
CONTAINS SULFITES
CEDAR KNOLLS, NJ STILL WHITE WINE"#;
        let product = ProductInput {
            product_id: "lenz-moser".to_string(),
            label: Some("Lenz Moser".to_string()),
            expected: ExpectedFields {
                brand_name: Some("Lenz Moser".to_string()),
                class_type: Some("Dry White Wine".to_string()),
                alcohol_content: Some("12%".to_string()),
                net_contents: Some("1.0L".to_string()),
                bottler: None,
                country: Some("Austria".to_string()),
            },
            images: vec![ImagePayload {
                image_id: "label".to_string(),
                filename: "label.txt".to_string(),
                content_type: Some("text/plain".to_string()),
                bytes: ocr_text.as_bytes().to_vec(),
            }],
        };

        let result = verify_product(&MockEngine, product).await;
        assert_eq!(result.fields["brand_name"].status, CheckStatus::Pass);
        assert_eq!(result.fields["class_type"].status, CheckStatus::Pass);
        assert_eq!(
            result.fields["class_type"].observed.as_deref(),
            Some("White Wine")
        );
        assert_eq!(result.fields["alcohol_content"].status, CheckStatus::Pass);
        assert_eq!(result.fields["net_contents"].status, CheckStatus::Pass);
        assert_eq!(result.fields["country"].status, CheckStatus::Pass);
        assert_eq!(result.government_warning.status, CheckStatus::Pass);
        assert_eq!(result.verdict, Verdict::Pass);
    }

    #[tokio::test]
    async fn ocr_engine_failure_returns_fail_result() {
        let product = ProductInput {
            product_id: "bad-image".to_string(),
            label: None,
            expected: ExpectedFields {
                brand_name: Some("Old Tom".to_string()),
                class_type: Some("Bourbon".to_string()),
                alcohol_content: Some("45%".to_string()),
                net_contents: Some("750 mL".to_string()),
                bottler: None,
                country: None,
            },
            images: vec![ImagePayload {
                image_id: "front".to_string(),
                filename: "front.png".to_string(),
                content_type: Some("image/png".to_string()),
                bytes: Vec::new(),
            }],
        };

        let result = verify_product(&FailingEngine, product).await;
        assert_eq!(result.verdict, Verdict::Fail);
        assert!(!result.notes.is_empty());
    }
}
