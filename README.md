# Offline TTB Label Verifier

Rust prototype for verifying alcohol label images against application data without
calling cloud OCR or ML endpoints. The app is designed for the TTB take-home prompt:
fast label review, simple UI, batch processing, multi-image products, and strict
government-warning handling.

## What V1 Ships

- Single-product verification from 1-4 uploaded label images.
- Batch jobs with optional CSV or JSON manifest.
- Multi-image product grouping, so a front image can supply brand/ABV while a back
  image supplies the mandatory warning.
- Offline OCR through a pluggable Rust `OcrEngine` trait.
- Local Tesseract TSV adapter for OCR text, confidence, and bounding boxes.
- Strict native government-warning check. Agents do not type the full warning.
- Fuzzy matching for brand/class fields, deterministic parsing for ABV/proof and
  net contents.
- Review queue for every `review` or `fail` result.
- Docker deployment with Tesseract bundled into the image.
- OpenAPI JSON route for future COLA/.NET integration.

## Why This Design

The prompt calls out three constraints that drive the architecture:

1. Results need to return in roughly 5 seconds, or agents will ignore the tool.
2. TTB networks may block outbound ML endpoints.
3. COLA is .NET, but this prototype should not create framework lock-in.

The solution is a small Rust service with a framework-neutral core crate and an HTTP
contract. The verification engine can later be called by COLA through HTTP, gRPC, a
sidecar, or a C ABI wrapper without rewriting the matching logic.

## Architecture

```text
Browser UI
  -> Axum API
    -> ttb-core verification pipeline
      -> OcrEngine trait
        -> local Tesseract TSV adapter
      -> multi-image OCR merge
      -> strict Government Warning check
      -> field matching and verdict scoring
```

Workspace layout:

```text
crates/ttb-core   Framework-neutral OCR, matching, warning, manifest, and verification logic
crates/ttb-api    Axum API, batch job state, static UI hosting, CSV export
crates/ttb-ui     Vite/TypeScript UI
samples/          Example manifests and sample guidance
Dockerfile        Offline deployment image with Tesseract installed
```

## API

### `POST /api/verify`

Multipart fields:

- `images[]`: 1-4 label image files
- `brand_name`
- `class_type`
- `alcohol_content`
- `net_contents`
- optional `bottler`
- optional `country`
- optional `product_id`

Returns a product-level verdict, per-field results, warning result, OCR spans, raw OCR,
engine metadata, and latency.

### `POST /api/batch/jobs`

Multipart fields:

- `images[]`: label image files
- optional `manifest`: CSV or JSON

CSV is one row per image. JSON can group multiple images under one product:

```json
[
  {
    "product": "Old Tom Bourbon 750",
    "images": ["old-tom-front.png", "old-tom-back.png"],
    "brand_name": "Old Tom Distillery",
    "class_type": "Kentucky Straight Bourbon Whiskey",
    "alcohol_content": "45% Alc./Vol.",
    "net_contents": "750 mL"
  }
]
```

### Other Routes

- `GET /api/batch/jobs/{job_id}`
- `GET /api/batch/jobs/{job_id}/export.csv`
- `GET /api/health`
- `GET /api/openapi.json`

## Government Warning Policy

V1 always checks the warning. The UI does not ask agents to type the full statement.

Hard fail:

- no government warning found
- heading is not `GOVERNMENT WARNING`

Pass:

- OCR finds the all-caps `GOVERNMENT WARNING` heading

V1 intentionally does not require word-for-word body matching because OCR can distort
long, small warning text even when the mandatory heading is visible. Full OCR text is
kept only inside the explicit debug disclosure, and field-level observed values are
sanitized so they do not expose raw OCR snippets.

Bold is intentionally not an automatic pass condition because OCR cannot reliably prove
font weight from arbitrary label photos.

## Run Locally

Install Rust, Node.js, and Tesseract.

```powershell
cd crates/ttb-ui
npm install
npm run build
cd ../..
cargo run -p ttb-api
```

Open <http://localhost:8080>.

On this Windows machine the default MSVC Rust target could not link because Visual
Studio Build Tools are not installed. The already-configured GNU toolchain works:

```powershell
cargo +stable-x86_64-pc-windows-gnu run -p ttb-api
```

## Docker

```bash
docker build -t ttb-label-verifier .
docker run --rm -p 8080:8080 ttb-label-verifier
```

The container includes the API, built UI, Tesseract, and sample manifests. It does not
need outbound network access at runtime.

## Tests

```powershell
cargo test
```

The test suite covers:

- government warning present/missing/title-case behavior
- fuzzy text matching
- ABV/proof parsing
- net contents normalization
- country aliases and US city/state inference
- sanitized observed values that do not leak raw OCR
- CSV and JSON manifest parsing
- Tesseract TSV parsing

## V1 vs V2

| Area | V1 | V2 |
|---|---|---|
| Runtime | Rust service plus local OCR | Same core with additional model runtimes |
| Cloud APIs | None | Still offline-first; optional only if firewall-safe |
| OCR | Tesseract TSV adapter behind `OcrEngine` | Add benchmarked OCR engines such as ONNX/Paddle/RapidOCR |
| Field extraction | Deterministic parsers and fuzzy matching | Learned text-span labeling from human corrections |
| LiteRT.js | Not included | Candidate for browser-side acceleration or span labeling |
| LLM fallback | Not included | Local lightweight fallback for low-confidence cases |
| Review queue | Shows review/fail results | Human corrections become training data |
| Storage | Ephemeral job state | Optional durable correction and audit store |

LiteRT/LiteRT.js is intentionally kept out of V1. It is promising for client-side WebGPU
or WASM inference, but V1's priority is a reliable offline verifier with a stable API,
not model-conversion and browser-driver risk.

## V2 Implementation Roadmap

V2 should build on the V1 edge-case testing rather than replace the V1 guardrails. The
known test labels exposed four priorities:

- Lenz Moser: class/type extraction must prefer clean taxonomy matches such as
  `White Wine` over arbitrary OCR chunks.
- Blue Heron: proof must normalize to ABV, missing warnings must still hard fail, and
  US origin can be inferred from city/state text such as `San Diego, CA`.
- Cascade Winery: sideways warning text needs stronger image preprocessing and OCR
  retry logic.
- Ecstasy: observed values must never expose raw OCR text, even when a fuzzy match
  succeeds.

This branch implements the first V2 increment:

- OCR pass telemetry for preprocessing and rotation retries.
- Local contrast and threshold preprocessing before selected OCR retries.
- A heuristic span-labeling baseline for future learned extraction.
- Review queue filters by verdict and fail/review reason.
- Structured correction capture with no raw image or raw OCR storage.
- API-level redaction for raw OCR, span text, and warning found text.

### Image Robustness

- Add local preprocessing profiles before OCR: contrast normalization, grayscale,
  thresholding, denoise, resize, border crop, and label-region detection.
- Expand the current rotation retry into a bounded strategy that tries selected
  preprocessing plus `0`, `90`, `180`, and `270` degree rotations only when OCR
  confidence is low or the warning heading is missing.
- Add perspective correction and region slicing for labels where warning text is
  sideways, low resolution, or separated from the main panel.
- Record per-pass timing, OCR confidence, warning detection, and chosen preprocessing
  profile so the app can prove it remains under the 5 second per-image target.

### OCR Engine Benchmarking

- Keep the current `OcrEngine` trait as the integration boundary.
- Add local-only candidate engines behind feature flags, including enhanced Tesseract
  configs, RapidOCR/PaddleOCR through ONNX, and a Rust-native OCR path if quality is
  acceptable.
- Benchmark each engine against the V1 edge-case corpus for latency, warning detection,
  extracted fields, and final verdict.
- Choose the default engine from measured quality and speed, while keeping Tesseract as
  the baseline fallback.

### Learned Field Extraction

- Add a span-labeling layer after OCR that classifies spans as `brand_name`,
  `class_type`, `alcohol_content`, `net_contents`, `government_warning`, `bottler`,
  `country`, or `other`.
- Use OCR text, normalized box position, box size, line grouping, neighboring text,
  confidence, and taxonomy hints as model features.
- Start with a lightweight local model such as gradient boosting or a compact ONNX
  classifier before considering larger neural models.
- Keep deterministic ABV/proof parsing, net contents parsing, government-warning
  heading checks, country inference, and sanitized observed values as non-negotiable
  guardrails.

### LiteRT and LiteRT.js

- Evaluate LiteRT.js for small browser-side models such as image quality scoring,
  warning-region detection, or span labeling.
- Keep all inference local through WebGPU, WASM, or an offline server-side runtime.
- Do not make LiteRT a required dependency until model conversion, browser coverage,
  and performance are proven on the edge-case corpus.
- Keep the Rust API as the source of truth for final verdicts so COLA/.NET clients can
  continue integrating through the OpenAPI contract.

### Review, Corrections, and Security

- Add an optional durable correction store with explicit retention settings and no
  raw-image storage by default.
- Store human corrections as structured labels rather than full OCR dumps.
- Use correction data to train and evaluate the span-labeling model.
- Keep the V1 in-memory review queue available for no-retention deployments.
- Add a deployment flag to hide raw OCR debug output entirely in production.
- Expand batch review filters by fail reason, warning status, class mismatch, country
  mismatch, and low confidence.

Runtime flags:

- `TTB_SHOW_RAW_OCR=true` exposes raw OCR in API/UI debug output for local debugging.
  The default is `false`.
- `TTB_OCR_RETRY_MODE=fast|balanced|enhanced` controls local OCR retries. The
  default is `fast`, which runs the primary OCR pass only for V1-like speed.
  Use `balanced` for contrast plus side-rotation retries, or `enhanced` for the
  full preprocessing and rotation set on difficult labels.
- `TTB_CORRECTIONS_PATH=./tmp/corrections.ndjson` appends structured correction
  records to newline-delimited JSON. If unset, corrections stay in memory only.

## Assumptions

- This is a prototype, not a system of record.
- Uploaded files and batch results are kept only in process memory for the current run.
- Tesseract is the bundled OCR engine in the Docker image.
- Future COLA integration should use the HTTP/OpenAPI contract first, then add gRPC,
  sidecar, or C ABI adapters only if needed.
