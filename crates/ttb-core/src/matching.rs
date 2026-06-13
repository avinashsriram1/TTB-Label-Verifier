use regex::Regex;
use std::sync::OnceLock;
use strsim::normalized_levenshtein;

pub fn normalize_text(input: &str) -> String {
    let replaced = input
        .trim()
        .to_lowercase()
        .replace(['\u{2018}', '\u{2019}'], "'")
        .replace(['\u{201c}', '\u{201d}'], "\"")
        .replace(['\u{2013}', '\u{2014}'], "-");

    let mut normalized = String::with_capacity(replaced.len());
    let mut last_space = true;

    for ch in replaced.chars() {
        if ch.is_alphanumeric() || ch == '%' {
            normalized.push(ch);
            last_space = false;
        } else if !last_space {
            normalized.push(' ');
            last_space = true;
        }
    }

    normalized.trim().to_string()
}

pub fn similarity(a: &str, b: &str) -> f32 {
    let a = normalize_text(a);
    let b = normalize_text(b);

    if a.is_empty() && b.is_empty() {
        return 1.0;
    }
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    if a == b {
        return 1.0;
    }

    normalized_levenshtein(&a, &b) as f32
}

pub fn token_overlap(expected: &str, observed: &str) -> f32 {
    let expected_tokens: Vec<String> = normalize_text(expected)
        .split_whitespace()
        .filter(|token| token.len() > 1)
        .map(ToOwned::to_owned)
        .collect();
    let observed_norm = normalize_text(observed);
    let observed_tokens: std::collections::HashSet<&str> =
        observed_norm.split_whitespace().collect();

    if expected_tokens.is_empty() || observed_tokens.is_empty() {
        return 0.0;
    }

    let observed_tokens = observed_tokens.into_iter().collect::<Vec<_>>();
    let hits = expected_tokens
        .iter()
        .filter(|token| {
            observed_tokens.iter().any(|observed| {
                *observed == token.as_str()
                    || (token.len() >= 4
                        && observed.len() >= 4
                        && normalized_levenshtein(token, observed) >= 0.78)
            })
        })
        .count();

    hits as f32 / expected_tokens.len() as f32
}

pub fn normalized_contains(haystack: &str, needle: &str) -> bool {
    let haystack = normalize_text(haystack);
    let needle = normalize_text(needle);
    !needle.is_empty() && haystack.contains(&needle)
}

pub fn is_proper_token_subset(shorter: &str, longer: &str) -> bool {
    let shorter_tokens: Vec<String> = normalize_text(shorter)
        .split_whitespace()
        .map(ToOwned::to_owned)
        .collect();
    let longer_norm = normalize_text(longer);
    let longer_tokens: std::collections::HashSet<&str> = longer_norm.split_whitespace().collect();

    if shorter_tokens.is_empty() || longer_tokens.len() <= shorter_tokens.len() {
        return false;
    }
    if !shorter_tokens.iter().any(|token| token.len() >= 3) {
        return false;
    }

    shorter_tokens
        .iter()
        .all(|token| longer_tokens.contains(token.as_str()))
}

pub fn parse_abv_value(input: &str) -> Option<f32> {
    if let Some(caps) = percent_regex().captures(input) {
        return caps.get(1)?.as_str().parse::<f32>().ok();
    }

    if let Some(caps) = proof_regex().captures(input) {
        return caps
            .get(1)?
            .as_str()
            .parse::<f32>()
            .ok()
            .map(|value| value / 2.0);
    }

    number_regex()
        .captures(input)
        .and_then(|caps| caps.get(1)?.as_str().parse::<f32>().ok())
}

pub fn extract_abv_candidates(input: &str) -> Vec<f32> {
    abv_regex()
        .captures_iter(input)
        .filter_map(|caps| caps.get(1)?.as_str().parse::<f32>().ok())
        .collect()
}

pub fn extract_proof_candidates(input: &str) -> Vec<f32> {
    proof_regex()
        .captures_iter(input)
        .filter_map(|caps| caps.get(1)?.as_str().parse::<f32>().ok())
        .collect()
}

pub fn parse_net_contents_ml(input: &str) -> Option<f32> {
    extract_net_contents_candidates(input).into_iter().next()
}

pub fn extract_net_contents_candidates(input: &str) -> Vec<f32> {
    net_contents_regex()
        .captures_iter(input)
        .filter_map(|caps| {
            let qty = caps.get(1)?.as_str().parse::<f32>().ok()?;
            let unit = caps.get(2)?.as_str().to_lowercase();
            let ml = if unit.starts_with("ml")
                || unit.contains("milliliter")
                || unit.contains("millilitre")
            {
                qty
            } else if unit == "l" || unit.contains("liter") || unit.contains("litre") {
                qty * 1000.0
            } else if unit == "cl" {
                qty * 10.0
            } else {
                qty * 29.5735
            };
            Some(ml)
        })
        .collect()
}

fn percent_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| Regex::new(r"(?i)(\d{1,2}(?:\.\d+)?)\s*%").expect("valid percent regex"))
}

fn proof_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| Regex::new(r"(?i)(\d{2,3}(?:\.\d+)?)\s*proof").expect("valid proof regex"))
}

fn number_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| Regex::new(r"(\d{1,2}(?:\.\d+)?)").expect("valid number regex"))
}

fn abv_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| {
        Regex::new(
            r"(?i)(\d{1,2}(?:\.\d+)?)\s*%\s*(?:alc\.?\s*/?\s*vol\.?|alcohol\s+by\s+volume|abv|by\s+vol\.?)?",
        )
        .expect("valid ABV regex")
    })
}

fn net_contents_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| {
        Regex::new(
            r"(?i)(\d+(?:\.\d+)?)\s*(ml|milliliters?|milli?litres?|milli?liters?|l|liters?|litres?|cl|fl\.?\s*oz\.?|fluid\s+ounces?|oz)",
        )
        .expect("valid net contents regex")
    })
}

pub fn summarize_text(input: &str, max_chars: usize) -> String {
    let compact = input.split_whitespace().collect::<Vec<_>>().join(" ");
    if compact.len() <= max_chars {
        return compact;
    }

    let mut clipped = compact.chars().take(max_chars).collect::<String>();
    clipped.push_str("...");
    clipped
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_case_and_punctuation() {
        assert_eq!(normalize_text("STONE'S THROW"), "stone s throw");
        assert!(normalized_contains("Old Town Whisky", "old town whisky"));
    }

    #[test]
    fn tolerates_minor_spelling_noise() {
        assert!(similarity("Old Town Whisky", "OLD TOWN WHIKSY") > 0.85);
        assert_eq!(
            token_overlap("Kentucky Bourbon", "Kentucky Straight Bourbon Whiskey"),
            1.0
        );
    }

    #[test]
    fn parses_abv_and_proof() {
        assert_eq!(parse_abv_value("45% Alc./Vol."), Some(45.0));
        assert_eq!(parse_abv_value("80 Proof"), Some(40.0));
        assert_eq!(extract_proof_candidates("90 Proof"), vec![90.0]);
    }

    #[test]
    fn parses_net_contents_to_ml() {
        assert_eq!(parse_net_contents_ml("750 mL"), Some(750.0));
        assert_eq!(parse_net_contents_ml("75 cl"), Some(750.0));
        assert!((parse_net_contents_ml("25.4 fl oz").unwrap() - 751.0).abs() < 2.0);
    }
}
