use crate::matching::normalize_text;
use crate::models::{CheckStatus, TextSpan, WarningCheck};
use regex::Regex;

pub const CANONICAL_WARNING: &str = "GOVERNMENT WARNING: (1) According to the Surgeon General, women should not drink alcoholic beverages during pregnancy because of the risk of birth defects. (2) Consumption of alcoholic beverages impairs your ability to drive a car or operate machinery, and may cause health problems.";

pub fn check_government_warning(raw_text: &str, spans: &[TextSpan]) -> WarningCheck {
    let heading_ci = Regex::new(r"(?i)government\s+warning").expect("valid heading regex");
    let heading_caps = Regex::new(r"GOVERNMENT\s+WARNING").expect("valid caps heading regex");
    let matched_heading = heading_ci.find(raw_text);

    let evidence = spans
        .iter()
        .filter(|span| {
            let norm = normalize_text(&span.text);
            norm.contains("government")
                || norm.contains("warning")
                || norm.contains("surgeon")
                || norm.contains("pregnan")
                || norm.contains("machinery")
                || norm.contains("health")
        })
        .cloned()
        .collect::<Vec<_>>();

    let Some(heading_match) = matched_heading else {
        return WarningCheck {
            present: false,
            status: CheckStatus::Fail,
            found_text: None,
            heading_all_caps: None,
            bold_confirmed: None,
            wording_similarity: 0.0,
            detail: "Government warning statement is missing.".to_string(),
            issues: vec![
                "Government warning statement is mandatory on alcohol labels.".to_string(),
            ],
            evidence,
        };
    };

    let heading_text = &raw_text[heading_match.start()..heading_match.end()];
    let all_caps = heading_caps.is_match(heading_text);
    let found_text = raw_text[heading_match.start()..].trim().to_string();

    if !all_caps {
        return WarningCheck {
            present: true,
            status: CheckStatus::Fail,
            found_text: Some(found_text),
            heading_all_caps: Some(false),
            bold_confirmed: None,
            wording_similarity: 0.0,
            detail: "Government warning heading is not all caps.".to_string(),
            issues: vec![
                "The heading must read GOVERNMENT WARNING in all capital letters.".to_string(),
            ],
            evidence,
        };
    }

    WarningCheck {
        present: true,
        status: CheckStatus::Pass,
        found_text: Some(found_text),
        heading_all_caps: Some(true),
        bold_confirmed: None,
        wording_similarity: 1.0,
        detail: "Government warning heading is present in all capital letters.".to_string(),
        issues: Vec::new(),
        evidence,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_exact_warning() {
        let result = check_government_warning(CANONICAL_WARNING, &[]);
        assert!(result.present);
        assert_eq!(result.heading_all_caps, Some(true));
        assert_eq!(result.status, CheckStatus::Pass);
    }

    #[test]
    fn fails_missing_warning() {
        let result = check_government_warning("OLD TOM DISTILLERY 45% ALC/VOL 750 ML", &[]);
        assert_eq!(result.status, CheckStatus::Fail);
        assert!(!result.present);
    }

    #[test]
    fn fails_title_case_heading() {
        let text = CANONICAL_WARNING.replace("GOVERNMENT WARNING", "Government Warning");
        let result = check_government_warning(&text, &[]);
        assert_eq!(result.status, CheckStatus::Fail);
        assert_eq!(result.heading_all_caps, Some(false));
    }

    #[test]
    fn passes_noisy_body_when_heading_is_all_caps() {
        let text = "GOVERNMENT WARNING: Drinking may be risky.";
        let result = check_government_warning(text, &[]);
        assert_eq!(result.status, CheckStatus::Pass);
    }
}
