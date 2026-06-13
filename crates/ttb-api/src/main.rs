use anyhow::{Context, Result};
use axum::body::Body;
use axum::extract::{Multipart, Path, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Serialize;
use std::collections::{BTreeMap, HashMap};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::{RwLock, Semaphore};
use tower_http::cors::CorsLayer;
use tower_http::services::{ServeDir, ServeFile};
use tower_http::trace::TraceLayer;
use tracing::{error, info};
use ttb_core::{
    ExpectedFields, ImagePayload, ManifestProduct, OcrEngine, ProductInput, TesseractCliEngine,
    Verdict, VerificationResult, parse_manifest, verify_product,
};
use uuid::Uuid;

const MAX_IMAGES_PER_PRODUCT: usize = 4;
const BATCH_PARALLELISM: usize = 4;

#[derive(Clone)]
struct AppState {
    ocr: Arc<dyn OcrEngine>,
    jobs: Arc<RwLock<HashMap<Uuid, BatchJob>>>,
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

#[derive(Debug, Clone, Serialize)]
struct BatchJob {
    job_id: Uuid,
    status: JobStatus,
    total: usize,
    completed: usize,
    counts: BatchCounts,
    results: Vec<VerificationResult>,
    errors: Vec<String>,
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
    let state = Arc::new(AppState {
        ocr,
        jobs: Arc::new(RwLock::new(HashMap::new())),
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
    let result = verify_product(state.ocr.as_ref(), product).await;
    Ok(Json(result))
}

async fn create_batch_job(
    State(state): State<Arc<AppState>>,
    multipart: Multipart,
) -> Result<Json<serde_json::Value>, AppError> {
    let form = read_multipart(multipart).await?;
    if form.images.is_empty() {
        return Err(AppError(anyhow::anyhow!("upload at least one image")));
    }

    let products = build_batch_products(form)?;
    let job_id = Uuid::new_v4();
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

async fn process_batch_job(state: Arc<AppState>, job_id: Uuid, products: Vec<ProductInput>) {
    {
        let mut jobs = state.jobs.write().await;
        if let Some(job) = jobs.get_mut(&job_id) {
            job.status = JobStatus::Running;
        }
    }

    let semaphore = Arc::new(Semaphore::new(BATCH_PARALLELISM));
    let mut handles = Vec::with_capacity(products.len());

    for product in products {
        let ocr = state.ocr.clone();
        let semaphore = semaphore.clone();
        handles.push(tokio::spawn(async move {
            let _permit = semaphore.acquire_owned().await.expect("semaphore open");
            verify_product(ocr.as_ref(), product).await
        }));
    }

    for handle in handles {
        match handle.await {
            Ok(result) => {
                let mut jobs = state.jobs.write().await;
                if let Some(job) = jobs.get_mut(&job_id) {
                    job.completed += 1;
                    match result.verdict {
                        Verdict::Pass => job.counts.pass += 1,
                        Verdict::Review => job.counts.review += 1,
                        Verdict::Fail => job.counts.fail += 1,
                    }
                    job.results.push(result);
                }
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
    let image_map = form
        .images
        .into_iter()
        .map(|image| (image.filename.clone(), image))
        .collect::<HashMap<_, _>>();

    let Some((manifest_name, manifest_bytes)) = form.manifest else {
        return Ok(image_map
            .into_values()
            .map(|image| ProductInput {
                product_id: image.filename.clone(),
                label: Some(image.filename.clone()),
                expected: ExpectedFields::default(),
                images: vec![image],
            })
            .collect());
    };

    let manifest = parse_manifest(manifest_name.as_deref(), &manifest_bytes)?;
    products_from_manifest(manifest, image_map)
}

fn products_from_manifest(
    manifest: Vec<ManifestProduct>,
    mut image_map: HashMap<String, ImagePayload>,
) -> Result<Vec<ProductInput>> {
    let mut products = Vec::new();

    for item in manifest {
        let mut images = Vec::new();
        for image_name in item.image_names {
            let image = image_map
                .remove(&image_name)
                .ok_or_else(|| anyhow::anyhow!("manifest references missing image {image_name}"))?;
            images.push(image);
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

    for image in image_map.into_values() {
        products.push(ProductInput {
            product_id: image.filename.clone(),
            label: Some(image.filename.clone()),
            expected: ExpectedFields::default(),
            images: vec![image],
        });
    }

    Ok(products)
}
