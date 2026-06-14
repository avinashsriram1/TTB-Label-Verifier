use anyhow::{Context, Result};
use axum::body::Body;
use axum::extract::{Multipart, Path, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::io::AsyncWriteExt;
use tokio::sync::{RwLock, Semaphore};
use tokio::task::JoinSet;
use tower_http::cors::CorsLayer;
use tower_http::services::{ServeDir, ServeFile};
use tower_http::trace::TraceLayer;
use tracing::{error, info};
use ttb_core::{
    CheckStatus, ExpectedFields, ImagePayload, ManifestProduct, OcrEngine, ProductInput,
    TesseractCliEngine, Verdict, VerificationResult, parse_manifest, verify_product,
};
use uuid::Uuid;

const MAX_IMAGES_PER_PRODUCT: usize = 4;
const DEFAULT_BATCH_JOB_TTL_SECONDS: u64 = 3600;

#[derive(Clone)]
struct AppState {
    ocr: Arc<dyn OcrEngine>,
    jobs: Arc<RwLock<HashMap<Uuid, BatchJob>>>,
    corrections: Arc<RwLock<Vec<CorrectionRecord>>>,
    config: AppConfig,
}

#[derive(Debug, Clone)]
struct AppConfig {
    show_raw_ocr: bool,
    correction_store_path: Option<PathBuf>,
}

impl AppConfig {
    fn from_env() -> Self {
        Self {
            show_raw_ocr: env_flag("TTB_SHOW_RAW_OCR", false),
            correction_store_path: std::env::var("TTB_CORRECTIONS_PATH")
                .ok()
                .map(PathBuf::from),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
enum JobStatus {
    Queued,
    Running,
    Complete,
    Failed,
}

#[derive(Debug, Clone, Serialize)]
struct BatchCounts {
    pass: usize,
    review: usize,
    fail: usize,
}

#[derive(Debug, Clone, Default, Serialize)]
struct BatchTelemetry {
    parallelism: usize,
    total_latency_ms: u128,
    average_latency_ms: Option<u128>,
    slowest_latency_ms: Option<u128>,
    timeout_count: usize,
    fast_pass: usize,
    cheap_repair: usize,
    enhanced_retry: usize,
    timeout_review: usize,
}

#[derive(Debug, Clone, Serialize)]
struct BatchJob {
    job_id: Uuid,
    status: JobStatus,
    total: usize,
    completed: usize,
    counts: BatchCounts,
    created_unix_ms: u128,
    completed_unix_ms: Option<u128>,
    telemetry: BatchTelemetry,
    results: Vec<VerificationResult>,
    errors: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct CorrectionInput {
    product_id: String,
    label: Option<String>,
    field: String,
    expected: Option<String>,
    corrected_value: String,
    verifier_note: Option<String>,
    verdict: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct CorrectionRecord {
    correction_id: Uuid,
    product_id: String,
    label: Option<String>,
    field: String,
    expected: Option<String>,
    corrected_value: String,
    verifier_note: Option<String>,
    verdict: Option<String>,
    created_unix_ms: u128,
}

#[derive(Debug)]
struct AppError(anyhow::Error);

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        error!("{:?}", self.0);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({
                "error": self.0.to_string()
            })),
        )
            .into_response()
    }
}

impl<E> From<E> for AppError
where
    E: Into<anyhow::Error>,
{
    fn from(err: E) -> Self {
        Self(err.into())
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "ttb_api=info,tower_http=info".into()),
        )
        .init();

    let ocr: Arc<dyn OcrEngine> = Arc::new(TesseractCliEngine::new());
    let config = AppConfig::from_env();
    let state = Arc::new(AppState {
        ocr,
        jobs: Arc::new(RwLock::new(HashMap::new())),
        corrections: Arc::new(RwLock::new(Vec::new())),
        config,
    });

    let app = build_router(state);
    let port = std::env::var("PORT")
        .ok()
        .and_then(|value| value.parse::<u16>().ok())
        .unwrap_or(8080);
    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    let listener = tokio::net::TcpListener::bind(addr).await?;

    info!("TTB Label Verifier listening on http://{addr}");
    axum::serve(listener, app).await?;
    Ok(())
}

fn build_router(state: Arc<AppState>) -> Router {
    let ui_dir = std::env::var("TTB_UI_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("crates/ttb-ui/dist"));
    let fallback = ServeFile::new(ui_dir.join("index.html"));

    Router::new()
        .route("/api/health", get(health))
        .route("/api/openapi.json", get(openapi))
        .route("/api/verify", post(verify))
        .route("/api/batch/jobs", post(create_batch_job))
        .route("/api/batch/jobs/{job_id}", get(get_batch_job))
        .route("/api/batch/jobs/{job_id}/export.csv", get(export_batch_job))
        .route("/api/corrections", post(create_correction))
        .route("/api/corrections/export.csv", get(export_corrections))
        .fallback_service(ServeDir::new(ui_dir).fallback(fallback))
        .layer(TraceLayer::new_for_http())
        .layer(CorsLayer::permissive())
        .with_state(state)
}

async fn health(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "status": "ok",
        "offline": true,
        "ocr": {
            "engine": state.ocr.name(),
            "available": state.ocr.is_available()
        },
        "security": {
            "raw_ocr_visible": state.config.show_raw_ocr
        },
        "corrections": {
            "enabled": true,
            "durable": state.config.correction_store_path.is_some()
        },
        "v2": {
            "ocr_pass_reports": true,
            "span_labeling": "heuristic_baseline",
            "processing_profile": processing_profile(),
            "ocr_retry_mode": processing_profile(),
            "image_time_budget_ms": image_time_budget_ms(),
            "batch_parallelism": batch_parallelism(),
            "batch_job_ttl_seconds": batch_job_ttl_seconds(),
            "max_image_long_edge": max_image_long_edge(),
            "span_label_mode": span_label_mode(),
            "candidate_engines": [
                "tesseract-tsv-local",
                "rapidocr-onnx-candidate",
                "paddleocr-onnx-candidate",
                "litert-js-candidate"
            ]
        }
    }))
}

async fn openapi() -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "openapi": "3.1.0",
        "info": {
            "title": "Offline TTB Label Verifier API",
            "version": env!("CARGO_PKG_VERSION")
        },
        "paths": {
            "/api/verify": {
                "post": {
                    "summary": "Verify one product from 1-4 label images",
                    "requestBody": {"content": {"multipart/form-data": {}}},
                    "responses": {"200": {"description": "VerificationResult"}}
                }
            },
            "/api/batch/jobs": {
                "post": {
                    "summary": "Create a batch verification job",
                    "requestBody": {"content": {"multipart/form-data": {}}},
                    "responses": {"200": {"description": "Job id"}}
                }
            },
            "/api/batch/jobs/{job_id}": {
                "get": {
                    "summary": "Read batch job progress and results",
                    "responses": {"200": {"description": "BatchJob"}}
                }
            },
            "/api/batch/jobs/{job_id}/export.csv": {
                "get": {
                    "summary": "Export batch summary as CSV",
                    "responses": {"200": {"description": "CSV export"}}
                }
            },
            "/api/health": {
                "get": {
                    "summary": "Health and OCR engine availability",
                    "responses": {"200": {"description": "Health"}}
                }
            },
            "/api/corrections": {
                "post": {
                    "summary": "Store a structured human correction without raw images or raw OCR",
                    "responses": {"200": {"description": "CorrectionRecord"}}
                }
            },
            "/api/corrections/export.csv": {
                "get": {
                    "summary": "Export structured human corrections as CSV",
                    "responses": {"200": {"description": "CSV export"}}
                }
            }
        }
    }))
}

async fn verify(
    State(state): State<Arc<AppState>>,
    multipart: Multipart,
) -> Result<Json<VerificationResult>, AppError> {
    let form = read_multipart(multipart).await?;
    let expected = expected_from_fields(&form.fields);
    let product_id = form
        .fields
        .get("product_id")
        .cloned()
        .unwrap_or_else(|| Uuid::new_v4().to_string());
    let label = form.fields.get("label").cloned();

    if form.images.is_empty() {
        return Err(AppError(anyhow::anyhow!("upload at least one image")));
    }
    if form.images.len() > MAX_IMAGES_PER_PRODUCT {
        return Err(AppError(anyhow::anyhow!(
            "upload at most {MAX_IMAGES_PER_PRODUCT} images per product"
        )));
    }

    let product = ProductInput {
        product_id,
        label,
        expected,
        images: form.images,
    };
    let result = sanitize_result_for_response(
        verify_product(state.ocr.as_ref(), product).await,
        &state.config,
    );
    Ok(Json(result))
}

async fn create_batch_job(
    State(state): State<Arc<AppState>>,
    multipart: Multipart,
) -> Result<Json<serde_json::Value>, AppError> {
    cleanup_expired_jobs(&state).await;

    let form = read_multipart(multipart).await?;
    if form.images.is_empty() {
        return Err(AppError(anyhow::anyhow!("upload at least one image")));
    }

    let products = build_batch_products(form)?;
    let job_id = Uuid::new_v4();
    let parallelism = batch_parallelism();
    let job = BatchJob {
        job_id,
        status: JobStatus::Queued,
        total: products.len(),
        completed: 0,
        counts: BatchCounts {
            pass: 0,
            review: 0,
            fail: 0,
        },
        created_unix_ms: unix_ms(),
        completed_unix_ms: None,
        telemetry: BatchTelemetry {
            parallelism,
            ..BatchTelemetry::default()
        },
        results: Vec::new(),
        errors: Vec::new(),
    };

    state.jobs.write().await.insert(job_id, job);
    let state_for_task = state.clone();
    tokio::spawn(async move {
        process_batch_job(state_for_task, job_id, products).await;
    });

    Ok(Json(serde_json::json!({ "job_id": job_id })))
}

async fn get_batch_job(
    State(state): State<Arc<AppState>>,
    Path(job_id): Path<Uuid>,
) -> Result<Json<BatchJob>, AppError> {
    cleanup_expired_jobs(&state).await;

    let jobs = state.jobs.read().await;
    let job = jobs
        .get(&job_id)
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("batch job not found"))?;
    Ok(Json(job))
}

async fn export_batch_job(
    State(state): State<Arc<AppState>>,
    Path(job_id): Path<Uuid>,
) -> Result<Response, AppError> {
    cleanup_expired_jobs(&state).await;

    let jobs = state.jobs.read().await;
    let job = jobs
        .get(&job_id)
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("batch job not found"))?;

    let mut writer = csv::Writer::from_writer(Vec::new());
    writer.write_record([
        "product_id",
        "label",
        "verdict",
        "brand_name",
        "class_type",
        "alcohol_content",
        "net_contents",
        "government_warning",
        "latency_ms",
    ])?;

    for result in job.results {
        writer.write_record([
            result.product_id,
            result.label.unwrap_or_default(),
            format!("{:?}", result.verdict).to_lowercase(),
            result
                .fields
                .get("brand_name")
                .map(|field| format!("{:?}", field.status).to_lowercase())
                .unwrap_or_default(),
            result
                .fields
                .get("class_type")
                .map(|field| format!("{:?}", field.status).to_lowercase())
                .unwrap_or_default(),
            result
                .fields
                .get("alcohol_content")
                .map(|field| format!("{:?}", field.status).to_lowercase())
                .unwrap_or_default(),
            result
                .fields
                .get("net_contents")
                .map(|field| format!("{:?}", field.status).to_lowercase())
                .unwrap_or_default(),
            format!("{:?}", result.government_warning.status).to_lowercase(),
            result.latency_ms.to_string(),
        ])?;
    }

    let body = writer.into_inner().context("finalize CSV export")?;
    let mut headers = HeaderMap::new();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("text/csv"));
    headers.insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_static("attachment; filename=\"ttb-batch-results.csv\""),
    );
    Ok((headers, Body::from(body)).into_response())
}

async fn create_correction(
    State(state): State<Arc<AppState>>,
    Json(input): Json<CorrectionInput>,
) -> Result<Json<CorrectionRecord>, AppError> {
    let product_id = input.product_id.trim();
    let field = input.field.trim();
    let corrected_value = input.corrected_value.trim();

    if product_id.is_empty() {
        return Err(AppError(anyhow::anyhow!("product_id is required")));
    }
    if field.is_empty() {
        return Err(AppError(anyhow::anyhow!("field is required")));
    }
    if corrected_value.is_empty() {
        return Err(AppError(anyhow::anyhow!("corrected_value is required")));
    }

    let record = CorrectionRecord {
        correction_id: Uuid::new_v4(),
        product_id: product_id.to_string(),
        label: input.label.filter(|value| !value.trim().is_empty()),
        field: field.to_string(),
        expected: input.expected.filter(|value| !value.trim().is_empty()),
        corrected_value: corrected_value.to_string(),
        verifier_note: input.verifier_note.filter(|value| !value.trim().is_empty()),
        verdict: input.verdict.filter(|value| !value.trim().is_empty()),
        created_unix_ms: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_millis())
            .unwrap_or_default(),
    };

    if let Some(path) = &state.config.correction_store_path {
        append_correction_record(path, &record).await?;
    }

    state.corrections.write().await.push(record.clone());
    Ok(Json(record))
}

async fn export_corrections(State(state): State<Arc<AppState>>) -> Result<Response, AppError> {
    let corrections = state.corrections.read().await;
    let mut writer = csv::Writer::from_writer(Vec::new());
    writer.write_record([
        "correction_id",
        "product_id",
        "label",
        "field",
        "expected",
        "corrected_value",
        "verifier_note",
        "verdict",
        "created_unix_ms",
    ])?;

    for record in corrections.iter() {
        writer.write_record([
            record.correction_id.to_string(),
            record.product_id.clone(),
            record.label.clone().unwrap_or_default(),
            record.field.clone(),
            record.expected.clone().unwrap_or_default(),
            record.corrected_value.clone(),
            record.verifier_note.clone().unwrap_or_default(),
            record.verdict.clone().unwrap_or_default(),
            record.created_unix_ms.to_string(),
        ])?;
    }

    let body = writer
        .into_inner()
        .context("finalize corrections CSV export")?;
    let mut headers = HeaderMap::new();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("text/csv"));
    headers.insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_static("attachment; filename=\"ttb-corrections.csv\""),
    );
    Ok((headers, Body::from(body)).into_response())
}

async fn process_batch_job(state: Arc<AppState>, job_id: Uuid, products: Vec<ProductInput>) {
    {
        let mut jobs = state.jobs.write().await;
        if let Some(job) = jobs.get_mut(&job_id) {
            job.status = JobStatus::Running;
        }
    }

    let semaphore = Arc::new(Semaphore::new(batch_parallelism()));
    let mut tasks = JoinSet::new();

    for product in products {
        let ocr = state.ocr.clone();
        let semaphore = semaphore.clone();
        tasks.spawn(async move {
            let _permit = semaphore.acquire_owned().await.expect("semaphore open");
            verify_product(ocr.as_ref(), product).await
        });
    }

    while let Some(joined) = tasks.join_next().await {
        match joined {
            Ok(result) => {
                let result = apply_batch_verdict_policy(result);
                let result = sanitize_result_for_response(result, &state.config);
                record_batch_result(&state, job_id, result).await;
            }
            Err(err) => {
                let mut jobs = state.jobs.write().await;
                if let Some(job) = jobs.get_mut(&job_id) {
                    job.completed += 1;
                    job.errors.push(err.to_string());
                }
            }
        }
    }

    let mut jobs = state.jobs.write().await;
    if let Some(job) = jobs.get_mut(&job_id) {
        job.status = if job.errors.is_empty() {
            JobStatus::Complete
        } else {
            JobStatus::Failed
        };
        job.completed_unix_ms = Some(unix_ms());
    }
}

async fn record_batch_result(state: &Arc<AppState>, job_id: Uuid, result: VerificationResult) {
    let mut jobs = state.jobs.write().await;
    let Some(job) = jobs.get_mut(&job_id) else {
        return;
    };

    job.completed += 1;
    match result.verdict {
        Verdict::Pass => job.counts.pass += 1,
        Verdict::Review => job.counts.review += 1,
        Verdict::Fail => job.counts.fail += 1,
    }

    job.telemetry.total_latency_ms += result.latency_ms;
    job.telemetry.average_latency_ms = Some(job.telemetry.total_latency_ms / job.completed as u128);
    job.telemetry.slowest_latency_ms = Some(
        job.telemetry
            .slowest_latency_ms
            .unwrap_or(0)
            .max(result.latency_ms),
    );
    if result.budget_exhausted {
        job.telemetry.timeout_count += 1;
    }
    match result.processing_path {
        ttb_core::ProcessingPath::FastPass => job.telemetry.fast_pass += 1,
        ttb_core::ProcessingPath::CheapRepair => job.telemetry.cheap_repair += 1,
        ttb_core::ProcessingPath::EnhancedRetry => job.telemetry.enhanced_retry += 1,
        ttb_core::ProcessingPath::TimeoutReview => job.telemetry.timeout_review += 1,
    }

    job.results.push(result);
}

fn apply_batch_verdict_policy(mut result: VerificationResult) -> VerificationResult {
    let mut demoted = Vec::new();

    for field_name in ["country", "bottler"] {
        let Some(field) = result.fields.get_mut(field_name) else {
            continue;
        };
        let expected_present = field
            .expected
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty());
        let could_not_observe = field.observed.is_none()
            && matches!(field.status, CheckStatus::Fail | CheckStatus::Missing);

        if expected_present && could_not_observe {
            field.status = CheckStatus::Review;
            field.detail = format!(
                "{} Batch review required because OCR could not confidently observe this optional field.",
                field.detail
            );
            demoted.push(field_name);
        }
    }

    if !demoted.is_empty() {
        result.notes.push(format!(
            "Batch policy routed missing optional field evidence to review: {}.",
            demoted.join(", ")
        ));
        result.verdict = aggregate_response_verdict(&result);
    }

    result
}

fn aggregate_response_verdict(result: &VerificationResult) -> Verdict {
    if matches!(
        result.government_warning.status,
        CheckStatus::Fail | CheckStatus::Missing
    ) || result
        .fields
        .values()
        .any(|field| matches!(field.status, CheckStatus::Fail))
    {
        return Verdict::Fail;
    }

    if matches!(result.government_warning.status, CheckStatus::Review)
        || result
            .fields
            .values()
            .any(|field| matches!(field.status, CheckStatus::Review | CheckStatus::Missing))
    {
        return Verdict::Review;
    }

    Verdict::Pass
}

async fn append_correction_record(path: &PathBuf, record: &CorrectionRecord) -> Result<()> {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .with_context(|| format!("create correction store directory {}", parent.display()))?;
    }

    let mut file = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .await
        .with_context(|| format!("open correction store {}", path.display()))?;
    let line = serde_json::to_string(record).context("serialize correction record")?;
    file.write_all(line.as_bytes()).await?;
    file.write_all(b"\n").await?;
    Ok(())
}

fn sanitize_result_for_response(
    mut result: VerificationResult,
    config: &AppConfig,
) -> VerificationResult {
    if config.show_raw_ocr {
        return result;
    }

    result.raw_text.clear();
    for span in &mut result.spans {
        span.text.clear();
    }
    for field in result.fields.values_mut() {
        for span in &mut field.evidence {
            span.text.clear();
        }
    }
    for span in &mut result.government_warning.evidence {
        span.text.clear();
    }
    result.government_warning.found_text = None;
    result
        .notes
        .push("Raw OCR text is hidden by deployment configuration.".to_string());
    result
}

fn env_flag(name: &str, default: bool) -> bool {
    std::env::var(name)
        .ok()
        .map(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(default)
}

fn processing_profile() -> String {
    let configured = std::env::var("TTB_PROCESSING_PROFILE")
        .or_else(|_| std::env::var("TTB_OCR_RETRY_MODE"))
        .unwrap_or_else(|_| "adaptive".to_string());

    match configured.trim().to_ascii_lowercase().as_str() {
        "fast" => "fast".to_string(),
        "balanced" | "adaptive" => "adaptive".to_string(),
        "enhanced" => "enhanced".to_string(),
        _ => "adaptive".to_string(),
    }
}

fn image_time_budget_ms() -> u128 {
    std::env::var("TTB_IMAGE_TIME_BUDGET_MS")
        .ok()
        .and_then(|value| value.parse::<u128>().ok())
        .filter(|value| *value >= 500)
        .unwrap_or(4500)
}

fn batch_parallelism() -> usize {
    std::env::var("TTB_BATCH_PARALLELISM")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| (1..=8).contains(value))
        .unwrap_or(2)
}

fn batch_job_ttl_seconds() -> u64 {
    std::env::var("TTB_BATCH_JOB_TTL_SECONDS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| (60..=86_400).contains(value))
        .unwrap_or(DEFAULT_BATCH_JOB_TTL_SECONDS)
}

fn unix_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or_default()
}

async fn cleanup_expired_jobs(state: &Arc<AppState>) {
    let cutoff = unix_ms().saturating_sub(batch_job_ttl_seconds() as u128 * 1000);
    let mut jobs = state.jobs.write().await;
    jobs.retain(|_, job| match job.completed_unix_ms {
        Some(completed_at) => completed_at >= cutoff,
        None => true,
    });
}

fn max_image_long_edge() -> u32 {
    std::env::var("TTB_MAX_IMAGE_LONG_EDGE")
        .ok()
        .and_then(|value| value.parse::<u32>().ok())
        .filter(|value| *value >= 600)
        .unwrap_or(1800)
}

fn span_label_mode() -> String {
    match std::env::var("TTB_SPAN_LABEL_MODE")
        .unwrap_or_else(|_| "candidate".to_string())
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "off" => "off".to_string(),
        "full" => "full".to_string(),
        _ => "candidate".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ttb_core::{
        BoundingBox, CheckStatus, FieldCheck, ProcessingPath, TextSpan, Verdict, WarningCheck,
    };

    fn raw_span() -> TextSpan {
        TextSpan {
            image_id: "img".to_string(),
            source_engine: "test".to_string(),
            text: "RAW OCR TEXT".to_string(),
            confidence: Some(95.0),
            bbox: Some(BoundingBox {
                x: 1,
                y: 2,
                width: 3,
                height: 4,
            }),
        }
    }

    fn result_with_raw_ocr() -> VerificationResult {
        let span = raw_span();
        let mut fields = BTreeMap::new();
        fields.insert(
            "brand_name".to_string(),
            FieldCheck {
                field: "brand_name".to_string(),
                expected: Some("Brand".to_string()),
                observed: Some("Brand".to_string()),
                status: CheckStatus::Pass,
                confidence: 0.99,
                detail: "Brand appears.".to_string(),
                evidence: vec![span.clone()],
            },
        );

        VerificationResult {
            product_id: "product".to_string(),
            label: None,
            verdict: Verdict::Pass,
            fields,
            government_warning: WarningCheck {
                present: true,
                status: CheckStatus::Pass,
                found_text: Some("GOVERNMENT WARNING raw body".to_string()),
                heading_all_caps: Some(true),
                bold_confirmed: None,
                wording_similarity: 1.0,
                detail: "Warning found.".to_string(),
                issues: Vec::new(),
                evidence: vec![span.clone()],
            },
            raw_text: "FULL RAW OCR".to_string(),
            spans: vec![span],
            span_labels: Vec::new(),
            ocr_passes: Vec::new(),
            processing_path: ProcessingPath::FastPass,
            stage_timings: Vec::new(),
            budget_ms: 4500,
            budget_exhausted: false,
            escalation_reason: None,
            engines: vec!["test".to_string()],
            image_count: 1,
            latency_ms: 1,
            notes: Vec::new(),
        }
    }

    #[test]
    fn response_sanitizer_hides_raw_ocr_by_default() {
        let config = AppConfig {
            show_raw_ocr: false,
            correction_store_path: None,
        };
        let result = sanitize_result_for_response(result_with_raw_ocr(), &config);

        assert!(result.raw_text.is_empty());
        assert_eq!(result.spans[0].text, "");
        assert_eq!(result.fields["brand_name"].evidence[0].text, "");
        assert_eq!(result.government_warning.evidence[0].text, "");
        assert!(result.government_warning.found_text.is_none());
    }

    #[test]
    fn response_sanitizer_can_keep_raw_ocr_for_local_debug() {
        let config = AppConfig {
            show_raw_ocr: true,
            correction_store_path: None,
        };
        let result = sanitize_result_for_response(result_with_raw_ocr(), &config);

        assert_eq!(result.raw_text, "FULL RAW OCR");
        assert_eq!(result.spans[0].text, "RAW OCR TEXT");
        assert_eq!(
            result.government_warning.found_text.as_deref(),
            Some("GOVERNMENT WARNING raw body")
        );
    }

    #[test]
    fn batch_policy_demotes_missing_optional_country_to_review() {
        let mut result = result_with_raw_ocr();
        result.verdict = Verdict::Fail;
        result.fields.insert(
            "country".to_string(),
            FieldCheck {
                field: "country".to_string(),
                expected: Some("United States".to_string()),
                observed: None,
                status: CheckStatus::Fail,
                confidence: 0.0,
                detail: "Country of origin was not found with enough confidence.".to_string(),
                evidence: Vec::new(),
            },
        );

        let result = apply_batch_verdict_policy(result);
        assert_eq!(result.fields["country"].status, CheckStatus::Review);
        assert_eq!(result.verdict, Verdict::Review);
    }

    #[test]
    fn batch_policy_keeps_conflicting_country_as_fail() {
        let mut result = result_with_raw_ocr();
        result.verdict = Verdict::Fail;
        result.fields.insert(
            "country".to_string(),
            FieldCheck {
                field: "country".to_string(),
                expected: Some("United States".to_string()),
                observed: Some("Austria".to_string()),
                status: CheckStatus::Fail,
                confidence: 0.1,
                detail: "Country of origin conflicts with the application data.".to_string(),
                evidence: Vec::new(),
            },
        );

        let result = apply_batch_verdict_policy(result);
        assert_eq!(result.fields["country"].status, CheckStatus::Fail);
        assert_eq!(result.verdict, Verdict::Fail);
    }

    #[test]
    fn batch_policy_keeps_government_warning_fail() {
        let mut result = result_with_raw_ocr();
        result.verdict = Verdict::Fail;
        result.government_warning.status = CheckStatus::Fail;
        result.fields.insert(
            "bottler".to_string(),
            FieldCheck {
                field: "bottler".to_string(),
                expected: Some("Blue Heron Imports".to_string()),
                observed: None,
                status: CheckStatus::Fail,
                confidence: 0.0,
                detail: "Bottler/producer was not found with enough confidence.".to_string(),
                evidence: Vec::new(),
            },
        );

        let result = apply_batch_verdict_policy(result);
        assert_eq!(result.fields["bottler"].status, CheckStatus::Review);
        assert_eq!(result.verdict, Verdict::Fail);
    }

    fn test_state_with_job(job: BatchJob) -> Arc<AppState> {
        let mut jobs = HashMap::new();
        jobs.insert(job.job_id, job);
        Arc::new(AppState {
            ocr: Arc::new(TesseractCliEngine::new()),
            jobs: Arc::new(RwLock::new(jobs)),
            corrections: Arc::new(RwLock::new(Vec::new())),
            config: AppConfig {
                show_raw_ocr: false,
                correction_store_path: None,
            },
        })
    }

    fn empty_job(job_id: Uuid) -> BatchJob {
        BatchJob {
            job_id,
            status: JobStatus::Running,
            total: 2,
            completed: 0,
            counts: BatchCounts {
                pass: 0,
                review: 0,
                fail: 0,
            },
            created_unix_ms: unix_ms(),
            completed_unix_ms: None,
            telemetry: BatchTelemetry {
                parallelism: 2,
                ..BatchTelemetry::default()
            },
            results: Vec::new(),
            errors: Vec::new(),
        }
    }

    #[tokio::test]
    async fn batch_result_updates_telemetry_as_products_finish() {
        let job_id = Uuid::new_v4();
        let state = test_state_with_job(empty_job(job_id));
        let mut result = result_with_raw_ocr();
        result.latency_ms = 4100;
        result.processing_path = ProcessingPath::EnhancedRetry;

        record_batch_result(&state, job_id, result).await;

        let jobs = state.jobs.read().await;
        let job = jobs.get(&job_id).unwrap();
        assert_eq!(job.completed, 1);
        assert_eq!(job.counts.pass, 1);
        assert_eq!(job.telemetry.average_latency_ms, Some(4100));
        assert_eq!(job.telemetry.slowest_latency_ms, Some(4100));
        assert_eq!(job.telemetry.enhanced_retry, 1);
    }

    #[tokio::test]
    async fn cleanup_expired_jobs_keeps_running_jobs() {
        let job_id = Uuid::new_v4();
        let mut job = empty_job(job_id);
        job.created_unix_ms = 1;
        job.completed_unix_ms = None;
        let state = test_state_with_job(job);

        cleanup_expired_jobs(&state).await;

        assert!(state.jobs.read().await.contains_key(&job_id));
    }

    #[tokio::test]
    async fn cleanup_expired_jobs_removes_completed_jobs() {
        let job_id = Uuid::new_v4();
        let mut job = empty_job(job_id);
        job.status = JobStatus::Complete;
        job.completed_unix_ms = Some(1);
        let state = test_state_with_job(job);

        cleanup_expired_jobs(&state).await;

        assert!(!state.jobs.read().await.contains_key(&job_id));
    }

    #[test]
    fn batch_manifest_matches_uploaded_basename_from_json_path() {
        let form = MultipartForm {
            fields: BTreeMap::new(),
            images: vec![ImagePayload {
                image_id: "img".to_string(),
                filename: "front.png".to_string(),
                content_type: None,
                bytes: Vec::new(),
            }],
            manifest: Some((
                Some("manifest.json".to_string()),
                br#"{"products":[{"id":"p1","image":"folder/front.png","brand":"Brand"}]}"#
                    .to_vec(),
            )),
        };

        let products = build_batch_products(form).unwrap();
        assert_eq!(products.len(), 1);
        assert_eq!(products[0].product_id, "p1");
        assert_eq!(products[0].images[0].filename, "front.png");
    }

    #[test]
    fn batch_manifest_reports_ambiguous_basename() {
        let form = MultipartForm {
            fields: BTreeMap::new(),
            images: vec![
                ImagePayload {
                    image_id: "one".to_string(),
                    filename: "front.png".to_string(),
                    content_type: None,
                    bytes: Vec::new(),
                },
                ImagePayload {
                    image_id: "two".to_string(),
                    filename: "nested/front.png".to_string(),
                    content_type: None,
                    bytes: Vec::new(),
                },
            ],
            manifest: Some((
                Some("manifest.json".to_string()),
                br#"{"products":[{"id":"p1","image":"folder/front.png"}]}"#.to_vec(),
            )),
        };

        let err = build_batch_products(form).unwrap_err().to_string();
        assert!(err.contains("ambiguous"));
    }
}

struct MultipartForm {
    fields: BTreeMap<String, String>,
    images: Vec<ImagePayload>,
    manifest: Option<(Option<String>, Vec<u8>)>,
}

async fn read_multipart(mut multipart: Multipart) -> Result<MultipartForm> {
    let mut fields = BTreeMap::new();
    let mut images = Vec::new();
    let mut manifest = None;

    while let Some(field) = multipart.next_field().await? {
        let name = field.name().unwrap_or_default().to_string();
        let filename = field.file_name().map(ToOwned::to_owned);
        let content_type = field.content_type().map(ToOwned::to_owned);
        let bytes = field.bytes().await?.to_vec();

        if name == "images" || name == "images[]" || name == "image" {
            let filename = filename.unwrap_or_else(|| format!("image-{}.upload", images.len() + 1));
            images.push(ImagePayload {
                image_id: Uuid::new_v4().to_string(),
                filename,
                content_type,
                bytes,
            });
        } else if name == "manifest" {
            manifest = Some((filename, bytes));
        } else if !bytes.is_empty() {
            fields.insert(name, String::from_utf8_lossy(&bytes).trim().to_string());
        }
    }

    Ok(MultipartForm {
        fields,
        images,
        manifest,
    })
}

fn expected_from_fields(fields: &BTreeMap<String, String>) -> ExpectedFields {
    ExpectedFields {
        brand_name: fields
            .get("brand_name")
            .or_else(|| fields.get("brand"))
            .cloned(),
        class_type: fields
            .get("class_type")
            .or_else(|| fields.get("class"))
            .or_else(|| fields.get("type"))
            .cloned(),
        alcohol_content: fields
            .get("alcohol_content")
            .or_else(|| fields.get("abv"))
            .cloned(),
        net_contents: fields
            .get("net_contents")
            .or_else(|| fields.get("volume"))
            .cloned(),
        bottler: fields
            .get("bottler")
            .or_else(|| fields.get("producer"))
            .cloned(),
        country: fields
            .get("country")
            .or_else(|| fields.get("country_of_origin"))
            .cloned(),
    }
}

fn build_batch_products(form: MultipartForm) -> Result<Vec<ProductInput>> {
    let image_lookup = UploadedImages::new(form.images)?;

    let Some((manifest_name, manifest_bytes)) = form.manifest else {
        return Ok(image_lookup
            .into_remaining()
            .map(|image| ProductInput {
                product_id: image.filename.clone(),
                label: Some(image.filename.clone()),
                expected: ExpectedFields::default(),
                images: vec![image],
            })
            .collect());
    };

    let manifest = parse_manifest(manifest_name.as_deref(), &manifest_bytes)?;
    products_from_manifest(manifest, image_lookup)
}

fn products_from_manifest(
    manifest: Vec<ManifestProduct>,
    mut image_lookup: UploadedImages,
) -> Result<Vec<ProductInput>> {
    let mut products = Vec::new();

    for item in manifest {
        let mut images = Vec::new();
        for image_name in item.image_names {
            images.push(image_lookup.take(&image_name)?);
        }

        if images.len() > MAX_IMAGES_PER_PRODUCT {
            anyhow::bail!(
                "product {} has {} images; max is {}",
                item.product_id,
                images.len(),
                MAX_IMAGES_PER_PRODUCT
            );
        }

        products.push(ProductInput {
            product_id: item.product_id,
            label: item.label,
            expected: item.expected,
            images,
        });
    }

    for image in image_lookup.into_remaining() {
        products.push(ProductInput {
            product_id: image.filename.clone(),
            label: Some(image.filename.clone()),
            expected: ExpectedFields::default(),
            images: vec![image],
        });
    }

    Ok(products)
}

struct UploadedImages {
    images: Vec<Option<ImagePayload>>,
    keys: HashMap<String, Vec<usize>>,
}

impl UploadedImages {
    fn new(images: Vec<ImagePayload>) -> Result<Self> {
        let mut lookup = Self {
            images: images.into_iter().map(Some).collect(),
            keys: HashMap::new(),
        };

        for index in 0..lookup.images.len() {
            let filename = lookup.images[index]
                .as_ref()
                .map(|image| image.filename.clone())
                .unwrap_or_default();
            for key in image_lookup_keys(&filename) {
                lookup.keys.entry(key).or_default().push(index);
            }
        }

        Ok(lookup)
    }

    fn take(&mut self, requested: &str) -> Result<ImagePayload> {
        let requested = requested.trim();
        for key in image_lookup_keys(requested) {
            let Some(indices) = self.keys.get(&key) else {
                continue;
            };
            if indices.len() > 1 {
                anyhow::bail!(
                    "manifest image reference {requested} is ambiguous; upload unique filenames or use exact paths"
                );
            }
            let index = indices[0];
            return self.images[index].take().ok_or_else(|| {
                anyhow::anyhow!("manifest references image {requested} more than once")
            });
        }

        anyhow::bail!("manifest references missing image {requested}")
    }

    fn into_remaining(self) -> impl Iterator<Item = ImagePayload> {
        self.images.into_iter().flatten()
    }
}

fn image_lookup_keys(filename: &str) -> Vec<String> {
    let trimmed = filename.trim();
    let basename = image_basename(trimmed);
    let basename_lower = basename.to_ascii_lowercase();
    let mut keys = vec![
        format!("exact:{trimmed}"),
        format!("basename:{basename}"),
        format!("basename-lower:{basename_lower}"),
    ];
    keys.sort();
    keys.dedup();
    keys
}

fn image_basename(filename: &str) -> &str {
    filename
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(filename)
        .trim()
}
