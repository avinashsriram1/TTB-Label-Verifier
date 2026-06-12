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
- statutory wording does not match closely enough

Review:

- OCR finds a near-exact warning but needs human confirmation
- wording and capitalization pass, but bold cannot be confirmed automatically

Bold is intentionally not an automatic pass condition because OCR cannot reliably prove
font weight from arbitrary label photos. The app surfaces it for the agent rather than
pretending confidence it does not have.

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

- government warning present/missing/title-case/bad wording/advisory bold
- fuzzy text matching
- ABV/proof parsing
- net contents normalization
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

## Assumptions

- This is a prototype, not a system of record.
- Uploaded files and batch results are kept only in process memory for the current run.
- Tesseract is the bundled OCR engine in the Docker image.
- Future COLA integration should use the HTTP/OpenAPI contract first, then add gRPC,
  sidecar, or C ABI adapters only if needed.
