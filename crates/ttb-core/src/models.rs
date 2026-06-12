use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    Pass,
    Review,
    Fail,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CheckStatus {
    Pass,
    Review,
    Fail,
    Missing,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExpectedFields {
    pub brand_name: Option<String>,
    pub class_type: Option<String>,
    pub alcohol_content: Option<String>,
    pub net_contents: Option<String>,
    pub bottler: Option<String>,
    pub country: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ImagePayload {
    pub image_id: String,
    pub filename: String,
    pub content_type: Option<String>,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct ProductInput {
    pub product_id: String,
    pub label: Option<String>,
    pub expected: ExpectedFields,
    pub images: Vec<ImagePayload>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BoundingBox {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TextSpan {
    pub image_id: String,
    pub source_engine: String,
    pub text: String,
    pub confidence: Option<f32>,
    pub bbox: Option<BoundingBox>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OcrPassReport {
    pub image_id: String,
    pub profile: String,
    pub rotation_degrees: u16,
    pub elapsed_ms: u128,
    pub span_count: usize,
    pub mean_confidence: Option<f32>,
    pub warning_heading_detected: bool,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OcrOutput {
    pub image_id: String,
    pub filename: String,
    pub engine: String,
    pub raw_text: String,
    pub spans: Vec<TextSpan>,
    pub passes: Vec<OcrPassReport>,
    pub warnings: Vec<String>,
    pub elapsed_ms: u128,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SpanLabelKind {
    BrandName,
    ClassType,
    AlcoholContent,
    NetContents,
    GovernmentWarning,
    Bottler,
    Country,
    Other,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpanLabel {
    pub image_id: String,
    pub bbox: Option<BoundingBox>,
    pub label: SpanLabelKind,
    pub confidence: f32,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FieldCheck {
    pub field: String,
    pub expected: Option<String>,
    pub observed: Option<String>,
    pub status: CheckStatus,
    pub confidence: f32,
    pub detail: String,
    pub evidence: Vec<TextSpan>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WarningCheck {
    pub present: bool,
    pub status: CheckStatus,
    pub found_text: Option<String>,
    pub heading_all_caps: Option<bool>,
    pub bold_confirmed: Option<bool>,
    pub wording_similarity: f32,
    pub detail: String,
    pub issues: Vec<String>,
    pub evidence: Vec<TextSpan>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerificationResult {
    pub product_id: String,
    pub label: Option<String>,
    pub verdict: Verdict,
    pub fields: BTreeMap<String, FieldCheck>,
    pub government_warning: WarningCheck,
    pub raw_text: String,
    pub spans: Vec<TextSpan>,
    pub span_labels: Vec<SpanLabel>,
    pub ocr_passes: Vec<OcrPassReport>,
    pub engines: Vec<String>,
    pub image_count: usize,
    pub latency_ms: u128,
    pub notes: Vec<String>,
}
