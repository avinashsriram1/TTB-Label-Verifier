use crate::models::{BoundingBox, ImagePayload, OcrOutput, OcrPassReport, TextSpan};
use anyhow::{Context, Result, anyhow};
use async_trait::async_trait;
use image::{DynamicImage, ImageFormat};
use std::io::Write;
use std::path::PathBuf;
use std::process::Command as StdCommand;
use std::time::Instant;
use tempfile::NamedTempFile;
use tokio::process::Command;

#[async_trait]
pub trait OcrEngine: Send + Sync {
    fn name(&self) -> &'static str;
    fn is_available(&self) -> bool;
    async fn read(&self, image: &ImagePayload) -> Result<OcrOutput>;
}

#[derive(Debug, Clone, Default)]
pub struct TesseractCliEngine;

impl TesseractCliEngine {
    pub fn new() -> Self {
        Self
    }

    fn command_path() -> PathBuf {
        if let Ok(cmd) = std::env::var("TESSERACT_CMD") {
            let cmd = cmd.trim();
            if !cmd.is_empty() {
                return PathBuf::from(cmd);
            }
        }

        let windows_default = PathBuf::from(r"C:\Program Files\Tesseract-OCR\tesseract.exe");
        if windows_default.exists() {
            return windows_default;
        }

        PathBuf::from("tesseract")
    }
}

#[async_trait]
impl OcrEngine for TesseractCliEngine {
    fn name(&self) -> &'static str {
        "tesseract-tsv-local"
    }

    fn is_available(&self) -> bool {
        StdCommand::new(Self::command_path())
            .arg("--version")
            .output()
            .map(|output| output.status.success())
            .unwrap_or(false)
    }

    async fn read(&self, image: &ImagePayload) -> Result<OcrOutput> {
        let started = Instant::now();
        let mut warnings = Vec::new();
        let mut all_spans = Vec::new();
        let mut raw_parts = Vec::new();
        let mut passes = Vec::new();

        let primary =
            run_tesseract_bytes(&image.bytes, &image.image_id, self.name(), "original", 0).await?;
        passes.push(primary.report.clone());
        raw_parts.push(primary.raw_text);
        all_spans.extend(primary.spans);

        if should_retry_ocr(&passes) {
            for variant in retry_variants(&image.bytes, &image.image_id) {
                let variant = match variant {
                    Ok(variant) => variant,
                    Err(err) => {
                        warnings.push(format!("OCR preprocessing retry failed: {err}"));
                        continue;
                    }
                };

                match run_tesseract_bytes(
                    &variant.bytes,
                    &variant.image_id,
                    self.name(),
                    variant.profile,
                    variant.rotation_degrees,
                )
                .await
                {
                    Ok(output) if !output.raw_text.trim().is_empty() => {
                        passes.push(output.report.clone());
                        raw_parts.push(output.raw_text);
                        all_spans.extend(output.spans);
                    }
                    Ok(output) => passes.push(output.report.clone()),
                    Err(err) => {
                        warnings.push(format!(
                            "OCR retry {} {}deg failed: {err}",
                            variant.profile, variant.rotation_degrees
                        ));
                        passes.push(OcrPassReport {
                            image_id: variant.image_id,
                            profile: variant.profile.to_string(),
                            rotation_degrees: variant.rotation_degrees,
                            elapsed_ms: 0,
                            span_count: 0,
                            mean_confidence: None,
                            warning_heading_detected: false,
                            error: Some(err.to_string()),
                        });
                    }
                }

                if passes.iter().any(|pass| pass.warning_heading_detected) {
                    break;
                }
            }
        }

        Ok(OcrOutput {
            image_id: image.image_id.clone(),
            filename: image.filename.clone(),
            engine: self.name().to_string(),
            raw_text: raw_parts.join(" "),
            spans: all_spans,
            passes,
            warnings,
            elapsed_ms: started.elapsed().as_millis(),
        })
    }
}

struct TesseractRun {
    raw_text: String,
    spans: Vec<TextSpan>,
    report: OcrPassReport,
}

struct OcrRetryVariant {
    image_id: String,
    profile: &'static str,
    rotation_degrees: u16,
    bytes: Vec<u8>,
}

async fn run_tesseract_bytes(
    bytes: &[u8],
    image_id: &str,
    engine: &str,
    profile: &'static str,
    rotation_degrees: u16,
) -> Result<TesseractRun> {
    let started = Instant::now();
    let mut tmp = NamedTempFile::new().context("create temporary OCR image")?;
    tmp.write_all(bytes).context("write temporary OCR image")?;
    let path = tmp.path().to_path_buf();

    let output = Command::new(TesseractCliEngine::command_path())
        .arg(&path)
        .arg("stdout")
        .arg("--psm")
        .arg("6")
        .arg("tsv")
        .output()
        .await
        .context(
            "run tesseract; set TESSERACT_CMD, add Tesseract to PATH, or use the Docker image",
        )?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!("tesseract failed: {}", stderr.trim()));
    }

    let tsv = String::from_utf8_lossy(&output.stdout);
    let spans = parse_tesseract_tsv(&tsv, image_id, engine);
    let raw_text = spans
        .iter()
        .map(|span| span.text.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    let mean_confidence = mean_confidence(&spans);
    let warning_heading_detected = raw_text.contains("GOVERNMENT WARNING");
    let span_count = spans.len();
    let report = OcrPassReport {
        image_id: image_id.to_string(),
        profile: profile.to_string(),
        rotation_degrees,
        elapsed_ms: started.elapsed().as_millis(),
        span_count,
        mean_confidence,
        warning_heading_detected,
        error: None,
    };

    Ok(TesseractRun {
        spans,
        raw_text,
        report,
    })
}

fn should_retry_ocr(passes: &[OcrPassReport]) -> bool {
    let Some(primary) = passes.first() else {
        return false;
    };

    !primary.warning_heading_detected || primary.mean_confidence.unwrap_or(0.0) < 55.0
}

fn retry_variants(bytes: &[u8], image_id: &str) -> Vec<Result<OcrRetryVariant>> {
    [
        ("contrast", 0),
        ("threshold", 0),
        ("original", 90),
        ("original", 180),
        ("original", 270),
    ]
    .into_iter()
    .map(|(profile, rotation_degrees)| {
        prepare_variant(bytes, profile, rotation_degrees).map(|bytes| OcrRetryVariant {
            image_id: format!("{image_id}-{profile}-rot{rotation_degrees}"),
            profile,
            rotation_degrees,
            bytes,
        })
    })
    .collect()
}

fn prepare_variant(bytes: &[u8], profile: &'static str, rotation_degrees: u16) -> Result<Vec<u8>> {
    let image = image::load_from_memory(bytes).context("decode image for OCR rotation")?;
    let processed = match profile {
        "original" => image,
        "contrast" => image.grayscale().adjust_contrast(35.0),
        "threshold" => threshold_image(&image),
        _ => anyhow::bail!("unknown OCR preprocessing profile {profile}"),
    };
    let rotated = match rotation_degrees {
        0 => processed,
        90 => processed.rotate90(),
        180 => processed.rotate180(),
        270 => processed.rotate270(),
        _ => anyhow::bail!("unsupported OCR rotation {rotation_degrees}"),
    };

    let mut cursor = std::io::Cursor::new(Vec::new());
    rotated
        .write_to(&mut cursor, ImageFormat::Png)
        .context("encode rotated OCR image")?;
    Ok(cursor.into_inner())
}

fn threshold_image(image: &DynamicImage) -> DynamicImage {
    let mut luma = image.grayscale().to_luma8();
    for pixel in luma.pixels_mut() {
        pixel.0[0] = if pixel.0[0] >= 170 { 255 } else { 0 };
    }
    DynamicImage::ImageLuma8(luma)
}

fn mean_confidence(spans: &[TextSpan]) -> Option<f32> {
    let confidences = spans
        .iter()
        .filter_map(|span| span.confidence)
        .collect::<Vec<_>>();
    if confidences.is_empty() {
        return None;
    }

    Some(confidences.iter().sum::<f32>() / confidences.len() as f32)
}

fn parse_tesseract_tsv(tsv: &str, image_id: &str, engine: &str) -> Vec<TextSpan> {
    let mut lines = tsv.lines();
    let Some(header) = lines.next() else {
        return Vec::new();
    };

    let headers = header.split('\t').collect::<Vec<_>>();
    let index = |name: &str| headers.iter().position(|candidate| *candidate == name);

    let left_i = index("left");
    let top_i = index("top");
    let width_i = index("width");
    let height_i = index("height");
    let conf_i = index("conf");
    let text_i = index("text");

    lines
        .filter_map(|line| {
            let columns = line.split('\t').collect::<Vec<_>>();
            let text = columns.get(text_i?)?.trim();
            if text.is_empty() {
                return None;
            }

            let confidence = columns
                .get(conf_i?)
                .and_then(|value| value.parse::<f32>().ok())
                .filter(|value| *value >= 0.0);

            let bbox = match (left_i, top_i, width_i, height_i) {
                (Some(left), Some(top), Some(width), Some(height)) => Some(BoundingBox {
                    x: columns.get(left)?.parse().ok()?,
                    y: columns.get(top)?.parse().ok()?,
                    width: columns.get(width)?.parse().ok()?,
                    height: columns.get(height)?.parse().ok()?,
                }),
                _ => None,
            };

            Some(TextSpan {
                image_id: image_id.to_string(),
                source_engine: engine.to_string(),
                text: text.to_string(),
                confidence,
                bbox,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_tesseract_tsv_words() {
        let tsv = "level\tpage_num\tblock_num\tpar_num\tline_num\tword_num\tleft\ttop\twidth\theight\tconf\ttext\n5\t1\t1\t1\t1\t1\t10\t20\t30\t40\t96\tOLD\n5\t1\t1\t1\t1\t2\t50\t20\t60\t40\t91\tTOM\n";
        let spans = parse_tesseract_tsv(tsv, "img-1", "test");
        assert_eq!(spans.len(), 2);
        assert_eq!(spans[0].text, "OLD");
        assert_eq!(spans[0].bbox.as_ref().unwrap().x, 10);
    }

    #[test]
    fn prepares_rotated_image_variant_for_warning_retry() {
        let mut bytes = Vec::new();
        let image = image::RgbImage::from_pixel(2, 4, image::Rgb([255, 255, 255]));
        image
            .write_to(&mut std::io::Cursor::new(&mut bytes), ImageFormat::Png)
            .unwrap();

        let rotated = prepare_variant(&bytes, "original", 90).unwrap();
        let decoded = image::load_from_memory(&rotated).unwrap();
        assert_eq!(decoded.width(), 4);
        assert_eq!(decoded.height(), 2);
    }

    #[test]
    fn threshold_variant_keeps_image_decodable() {
        let mut bytes = Vec::new();
        let image = image::RgbImage::from_pixel(2, 2, image::Rgb([180, 180, 180]));
        image
            .write_to(&mut std::io::Cursor::new(&mut bytes), ImageFormat::Png)
            .unwrap();

        let processed = prepare_variant(&bytes, "threshold", 0).unwrap();
        let decoded = image::load_from_memory(&processed).unwrap();
        assert_eq!(decoded.width(), 2);
        assert_eq!(decoded.height(), 2);
    }

    #[test]
    fn low_confidence_or_missing_warning_triggers_retry() {
        assert!(should_retry_ocr(&[OcrPassReport {
            image_id: "img".to_string(),
            profile: "original".to_string(),
            rotation_degrees: 0,
            elapsed_ms: 1,
            span_count: 1,
            mean_confidence: Some(90.0),
            warning_heading_detected: false,
            error: None,
        }]));

        assert!(!should_retry_ocr(&[OcrPassReport {
            image_id: "img".to_string(),
            profile: "original".to_string(),
            rotation_degrees: 0,
            elapsed_ms: 1,
            span_count: 1,
            mean_confidence: Some(90.0),
            warning_heading_detected: true,
            error: None,
        }]));
    }
}
