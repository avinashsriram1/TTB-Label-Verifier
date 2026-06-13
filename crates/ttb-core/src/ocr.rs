use crate::models::{BoundingBox, ImagePayload, OcrOutput, TextSpan};
use anyhow::{Context, Result, anyhow};
use async_trait::async_trait;
use image::ImageFormat;
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

        let primary = run_tesseract_bytes(&image.bytes, &image.image_id, self.name()).await?;
        raw_parts.push(primary.raw_text);
        all_spans.extend(primary.spans);

        let merged_primary = raw_parts.join(" ");
        if !merged_primary.contains("GOVERNMENT WARNING") {
            for angle in [90, 180, 270] {
                let rotated = match rotate_image_bytes(&image.bytes, angle)
                    .context("rotate image for warning OCR retry")
                {
                    Ok(rotated) => rotated,
                    Err(err) => {
                        warnings.push(format!("rotated OCR retry {angle} failed: {err}"));
                        continue;
                    }
                };

                match run_tesseract_bytes(
                    &rotated,
                    &format!("{}-rot{angle}", image.image_id),
                    self.name(),
                )
                .await
                {
                    Ok(output) if !output.raw_text.trim().is_empty() => {
                        raw_parts.push(output.raw_text);
                        all_spans.extend(output.spans);
                    }
                    Ok(_) => {}
                    Err(err) => warnings.push(format!("rotated OCR retry {angle} failed: {err}")),
                }
            }
        }

        Ok(OcrOutput {
            image_id: image.image_id.clone(),
            filename: image.filename.clone(),
            engine: self.name().to_string(),
            raw_text: raw_parts.join(" "),
            spans: all_spans,
            warnings,
            elapsed_ms: started.elapsed().as_millis(),
        })
    }
}

struct TesseractRun {
    raw_text: String,
    spans: Vec<TextSpan>,
}

async fn run_tesseract_bytes(bytes: &[u8], image_id: &str, engine: &str) -> Result<TesseractRun> {
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

    Ok(TesseractRun { spans, raw_text })
}

fn rotate_image_bytes(bytes: &[u8], angle: u16) -> Result<Vec<u8>> {
    let image = image::load_from_memory(bytes).context("decode image for OCR rotation")?;
    let rotated = match angle {
        90 => image.rotate90(),
        180 => image.rotate180(),
        270 => image.rotate270(),
        _ => image,
    };

    let mut cursor = std::io::Cursor::new(Vec::new());
    rotated
        .write_to(&mut cursor, ImageFormat::Png)
        .context("encode rotated OCR image")?;
    Ok(cursor.into_inner())
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
    fn rotates_image_bytes_for_warning_retry() {
        let mut bytes = Vec::new();
        let image = image::RgbImage::from_pixel(2, 4, image::Rgb([255, 255, 255]));
        image
            .write_to(&mut std::io::Cursor::new(&mut bytes), ImageFormat::Png)
            .unwrap();

        let rotated = rotate_image_bytes(&bytes, 90).unwrap();
        let decoded = image::load_from_memory(&rotated).unwrap();
        assert_eq!(decoded.width(), 4);
        assert_eq!(decoded.height(), 2);
    }
}
