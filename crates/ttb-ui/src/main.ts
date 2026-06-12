import "./styles.css";

type Verdict = "pass" | "review" | "fail";
type CheckStatus = "pass" | "review" | "fail" | "missing";

type FieldCheck = {
  field: string;
  expected?: string | null;
  observed?: string | null;
  status: CheckStatus;
  confidence: number;
  detail: string;
};

type WarningCheck = {
  present: boolean;
  status: CheckStatus;
  found_text?: string | null;
  detail: string;
  issues: string[];
};

type OcrPassReport = {
  image_id: string;
  profile: string;
  rotation_degrees: number;
  elapsed_ms: number;
  span_count: number;
  mean_confidence?: number | null;
  warning_heading_detected: boolean;
  error?: string | null;
};

type SpanLabel = {
  image_id: string;
  label: string;
  confidence: number;
  reason: string;
};

type VerificationResult = {
  product_id: string;
  label?: string | null;
  verdict: Verdict;
  fields: Record<string, FieldCheck>;
  government_warning: WarningCheck;
  raw_text: string;
  span_labels: SpanLabel[];
  ocr_passes: OcrPassReport[];
  processing_path: "fast_pass" | "cheap_repair" | "enhanced_retry" | "timeout_review";
  stage_timings: { stage: string; elapsed_ms: number }[];
  budget_ms: number;
  budget_exhausted: boolean;
  escalation_reason?: string | null;
  engines: string[];
  image_count: number;
  latency_ms: number;
  notes: string[];
};

type BatchJob = {
  job_id: string;
  status: "queued" | "running" | "complete" | "failed";
  total: number;
  completed: number;
  counts: { pass: number; review: number; fail: number };
  results: VerificationResult[];
  errors: string[];
};

type HealthResponse = {
  security?: { raw_ocr_visible?: boolean };
  corrections?: { enabled?: boolean; durable?: boolean };
  v2?: {
    processing_profile?: string;
    image_time_budget_ms?: number;
    batch_parallelism?: number;
    span_label_mode?: string;
  };
};

const appElement = document.querySelector<HTMLDivElement>("#app");
if (!appElement) throw new Error("missing app root");
const app = appElement;

let activeTab: "single" | "batch" | "review" = "single";
let singleResult: VerificationResult | null = null;
let currentJob: BatchJob | null = null;
let reviewQueue: VerificationResult[] = [];
let singleFormMessage = "";
let reviewStatusFilter: "all" | "review" | "fail" = "all";
let reviewReasonFilter: "all" | "warning" | "class_type" | "country" | "low_confidence" = "all";
let correctionMessage = "";
let serverConfig = {
  rawOcrVisible: false,
  correctionsEnabled: true,
  correctionsDurable: false,
  processingProfile: "adaptive",
  imageTimeBudgetMs: 4500,
  batchParallelism: 2,
  spanLabelMode: "candidate",
};
let pollTimer: number | undefined;

function render() {
  app.innerHTML = `
    <header class="topbar">
      <div>
        <h1>TTB Label Verifier</h1>
        <p>Offline label checks with V2 OCR telemetry, structured corrections, and raw OCR hidden by default.</p>
      </div>
      <nav class="tabs" aria-label="Views">
        ${tabButton("single", "Single")}
        ${tabButton("batch", "Batch")}
        ${tabButton("review", "Review")}
      </nav>
    </header>
    <main>
      ${activeTab === "single" ? renderSingle() : ""}
      ${activeTab === "batch" ? renderBatch() : ""}
      ${activeTab === "review" ? renderReview() : ""}
    </main>
  `;

  bindTabs();
  if (activeTab === "single") bindSingle();
  if (activeTab === "batch") bindBatch();
  if (activeTab === "review") bindReview();
}

function tabButton(tab: typeof activeTab, label: string) {
  return `<button class="tab ${activeTab === tab ? "active" : ""}" data-tab="${tab}">${label}</button>`;
}

function bindTabs() {
  document.querySelectorAll<HTMLButtonElement>("[data-tab]").forEach((button) => {
    button.addEventListener("click", () => {
      activeTab = button.dataset.tab as typeof activeTab;
      render();
    });
  });
}

function renderSingle() {
  return `
    <section class="grid two">
      <form id="single-form" class="panel">
        <div class="panel-title">
          <h2>Single Product</h2>
          <span class="chip">1-4 images</span>
        </div>
        <div class="auto-check">
          <strong>Government warning is checked automatically.</strong>
          <span>The image only needs to show the all-caps GOVERNMENT WARNING heading.</span>
        </div>
        ${singleFormMessage ? `<p class="form-error">${escapeHtml(singleFormMessage)}</p>` : ""}
        ${field("brand_name", "Brand name", "Lenz Moser", "The exact brand from the application, such as Lenz Moser. Case does not matter.", true)}
        ${field("class_type", "Class / type", "Dry White Wine", "Use a practical class/type like Dry White Wine, Beer, Bourbon Whiskey, Vodka, or Cider.", true)}
        ${field("alcohol_content", "Alcohol content", "12%", "Enter the application ABV, such as 12%, 45% Alc./Vol., or 90 Proof if that is what you have.", true)}
        ${field("net_contents", "Net contents", "1.0L", "Enter the package size, such as 750 mL, 1.0L, 12 fl oz, or 75 cl.", true)}
        ${field("bottler", "Bottler / producer", "optional", "Optional producer or bottler text from the application. Leave blank if it is not part of this check.")}
        ${field("country", "Country of origin", "Austria", "Use this for imported products, such as Austria, Mexico, France, or Product of Austria.")}
        <label class="drop">
          <span>Label images ${help("Upload one to four images for the same product. Use multiple images when the front and back labels carry different required text.")}</span>
          <input name="images" type="file" accept="image/*,.tif,.tiff" multiple required />
        </label>
        <button class="primary" type="submit">Verify</button>
      </form>
      <section class="panel result-panel">
        <div class="panel-title">
          <h2>Result</h2>
          ${singleResult ? badge(singleResult.verdict) : ""}
        </div>
        ${singleResult ? renderResult(singleResult) : `<div class="empty">No result yet.</div>`}
      </section>
    </section>
  `;
}

function renderBatch() {
  return `
    <section class="grid two">
      <form id="batch-form" class="panel">
        <div class="panel-title">
          <h2>Batch Upload</h2>
          <span class="chip">CSV or JSON manifest</span>
        </div>
        <div class="auto-check">
          <strong>Batch jobs run in parallel.</strong>
          <span>Each result shows its own processing time for debugging.</span>
        </div>
        <label class="drop">
          <span>Label images ${help("Upload all label images for the batch. A JSON manifest can group front and back images under one product.")}</span>
          <input name="images" type="file" accept="image/*,.tif,.tiff" multiple required />
        </label>
        <label class="drop">
          <span>Manifest ${help("Optional CSV or JSON file with expected application values. Without it, each image becomes a separate review item.")}</span>
          <input name="manifest" type="file" accept=".csv,.json" />
        </label>
        <button class="primary" type="submit">Start Batch</button>
      </form>
      <section class="panel">
        <div class="panel-title">
          <h2>Progress</h2>
          ${currentJob ? `<span class="chip">${currentJob.completed}/${currentJob.total}</span>` : ""}
        </div>
        ${currentJob ? renderBatchJob(currentJob) : `<div class="empty">No batch running.</div>`}
      </section>
    </section>
  `;
}

function renderReview() {
  const filtered = filteredReviewQueue();
  return `
    <section class="panel">
      <div class="panel-title">
        <h2>Review Queue</h2>
        <span class="chip">${filtered.length}/${reviewQueue.length} items</span>
      </div>
      <div class="auto-check">
        <strong>Temporary review list.</strong>
        <span>Review and fail results stay here only until this browser tab is refreshed or closed.</span>
      </div>
      <div class="review-tools">
        <label>
          <span>Status</span>
          <select id="review-status-filter">
            ${option("all", "All", reviewStatusFilter)}
            ${option("review", "Review", reviewStatusFilter)}
            ${option("fail", "Fail", reviewStatusFilter)}
          </select>
        </label>
        <label>
          <span>Reason</span>
          <select id="review-reason-filter">
            ${option("all", "All reasons", reviewReasonFilter)}
            ${option("warning", "Government warning", reviewReasonFilter)}
            ${option("class_type", "Class/type", reviewReasonFilter)}
            ${option("country", "Country", reviewReasonFilter)}
            ${option("low_confidence", "Low confidence", reviewReasonFilter)}
          </select>
        </label>
        <a class="export" href="/api/corrections/export.csv">Export Corrections</a>
      </div>
      ${correctionMessage ? `<p class="note">${escapeHtml(correctionMessage)}</p>` : ""}
      ${filtered.length ? filtered.map(renderResult).join("") : `<div class="empty">No review or fail results match the current filters.</div>`}
    </section>
  `;
}

function field(name: string, label: string, placeholder: string, helpText: string, required = false) {
  return `
    <label>
      <span>${label} ${help(helpText)}</span>
      <input name="${name}" placeholder="${placeholder}" ${required ? "required aria-required=\"true\"" : ""} autocomplete="off" />
    </label>
  `;
}

function help(text: string) {
  return `<button class="help" type="button" title="${escapeHtml(text)}" aria-label="${escapeHtml(text)}">?</button>`;
}

function option(value: string, label: string, selectedValue: string) {
  return `<option value="${value}" ${selectedValue === value ? "selected" : ""}>${label}</option>`;
}

function renderBatchJob(job: BatchJob) {
  const pct = job.total ? Math.round((job.completed / job.total) * 100) : 0;
  return `
    <div class="progress"><span style="width:${pct}%"></span></div>
    <div class="counts">
      <span class="status pass">Pass ${job.counts.pass}</span>
      <span class="status review">Review ${job.counts.review}</span>
      <span class="status fail">Fail ${job.counts.fail}</span>
    </div>
    ${job.job_id ? `<a class="export" href="/api/batch/jobs/${job.job_id}/export.csv">Export CSV</a>` : ""}
    <div class="result-list">${job.results.map(renderResult).join("")}</div>
    ${job.errors.map((error) => `<p class="error">${escapeHtml(error)}</p>`).join("")}
  `;
}

function renderResult(result: VerificationResult) {
  const fieldRows = Object.values(result.fields)
    .map(
      (field) => `
      <tr>
        <td>${labelFor(field.field)}</td>
        <td>${escapeHtml(field.expected ?? "")}</td>
        <td>${escapeHtml(field.observed ?? "")}</td>
        <td>${status(field.status)}</td>
        <td>${escapeHtml(field.detail)}</td>
      </tr>
    `,
    )
    .join("");

  return `
    <article class="result ${result.verdict}">
      <div class="result-head">
        <div>
          <h3>${escapeHtml(result.label || result.product_id)}</h3>
          <p>${result.image_count} image(s)</p>
        </div>
        <div class="result-actions">
          <span class="timer">${formatDuration(result.latency_ms)}</span>
          ${badge(result.verdict)}
        </div>
      </div>
      <table>
        <thead><tr><th>Field</th><th>Expected</th><th>Observed</th><th>Status</th><th>Detail</th></tr></thead>
        <tbody>${fieldRows}</tbody>
      </table>
      <div class="warning ${result.government_warning.status}">
        <strong>Government Warning</strong>
        ${status(result.government_warning.status)}
        <p>${escapeHtml(result.government_warning.detail)}</p>
        ${result.government_warning.issues.map((issue) => `<p>${escapeHtml(issue)}</p>`).join("")}
      </div>
      <details class="debug">
        <summary>OCR/debug details</summary>
        <div class="debug-grid">
          <span>Processing time</span><strong>${formatDuration(result.latency_ms)}</strong>
          <span>Processing path</span><strong>${labelFor(result.processing_path || "fast_pass")}</strong>
          <span>Budget</span><strong>${formatDuration(result.budget_ms || serverConfig.imageTimeBudgetMs)}</strong>
          <span>Budget status</span><strong>${result.budget_exhausted ? "exhausted" : "within budget"}</strong>
          ${result.escalation_reason ? `<span>Escalation</span><strong>${escapeHtml(result.escalation_reason)}</strong>` : ""}
          <span>OCR engine</span><strong>${escapeHtml(result.engines?.join(", ") || "unknown")}</strong>
          <span>Images processed</span><strong>${result.image_count}</strong>
          <span>OCR passes</span><strong>${result.ocr_passes?.length ?? 0}</strong>
          <span>Span labels</span><strong>${summarizeSpanLabels(result.span_labels ?? [])}</strong>
        </div>
        ${renderStageTimings(result.stage_timings ?? [])}
        ${renderOcrPasses(result.ocr_passes ?? [])}
        ${
          serverConfig.rawOcrVisible
            ? `<pre>${escapeHtml(result.raw_text || "No OCR text returned.")}</pre>`
            : `<p class="redacted">Raw OCR text is hidden by deployment configuration. Set TTB_SHOW_RAW_OCR=true only for local debugging.</p>`
        }
      </details>
      ${activeTab === "review" && serverConfig.correctionsEnabled ? renderCorrectionForm(result) : ""}
      ${result.notes.map((note) => `<p class="note">${escapeHtml(note)}</p>`).join("")}
    </article>
  `;
}

function renderOcrPasses(passes: OcrPassReport[]) {
  if (!passes.length) return `<p class="empty">No OCR pass telemetry returned.</p>`;
  return `
    <table class="pass-table">
      <thead><tr><th>Profile</th><th>Rotation</th><th>Time</th><th>Confidence</th><th>Warning</th></tr></thead>
      <tbody>
        ${passes
          .map(
            (pass) => `
              <tr>
                <td>${escapeHtml(pass.profile)}</td>
                <td>${pass.rotation_degrees} deg</td>
                <td>${formatDuration(pass.elapsed_ms)}</td>
                <td>${pass.mean_confidence == null ? "" : `${pass.mean_confidence.toFixed(1)}%`}</td>
                <td>${pass.warning_heading_detected ? "Found" : pass.error ? "Error" : "Not found"}</td>
              </tr>
            `,
          )
          .join("")}
      </tbody>
    </table>
  `;
}

function renderStageTimings(timings: { stage: string; elapsed_ms: number }[]) {
  if (!timings.length) return "";
  return `
    <table class="pass-table">
      <thead><tr><th>Stage</th><th>Time</th></tr></thead>
      <tbody>
        ${timings
          .map((timing) => `<tr><td>${escapeHtml(timing.stage)}</td><td>${formatDuration(timing.elapsed_ms)}</td></tr>`)
          .join("")}
      </tbody>
    </table>
  `;
}

function summarizeSpanLabels(labels: SpanLabel[]) {
  if (!labels.length) return "none";
  const counts = labels.reduce<Record<string, number>>((acc, label) => {
    acc[label.label] = (acc[label.label] ?? 0) + 1;
    return acc;
  }, {});
  return Object.entries(counts)
    .map(([label, count]) => `${label}: ${count}`)
    .join(", ");
}

function renderCorrectionForm(result: VerificationResult) {
  const fieldOptions = [
    ...Object.keys(result.fields).map((field) => ({ value: field, label: labelFor(field) })),
    { value: "government_warning", label: "Government Warning" },
  ];

  return `
    <form class="correction-form" data-product-id="${escapeHtml(result.product_id)}">
      <strong>Structured correction</strong>
      <select name="field">
        ${fieldOptions.map((item) => `<option value="${item.value}">${item.label}</option>`).join("")}
      </select>
      <input name="corrected_value" placeholder="Correct value or concise review note" required />
      <button type="submit">Save Correction</button>
    </form>
  `;
}

function labelFor(field: string) {
  return field
    .split("_")
    .map((part) => part.slice(0, 1).toUpperCase() + part.slice(1))
    .join(" ");
}

function badge(verdict: Verdict) {
  return `<span class="verdict ${verdict}">${verdict.toUpperCase()}</span>`;
}

function status(value: CheckStatus) {
  return `<span class="status ${value}">${value}</span>`;
}

function formatDuration(ms: number) {
  return ms >= 1000 ? `${(ms / 1000).toFixed(2)} sec` : `${ms} ms`;
}

function bindSingle() {
  document.querySelector<HTMLFormElement>("#single-form")?.addEventListener("submit", async (event) => {
    event.preventDefault();
    const form = event.currentTarget as HTMLFormElement;
    const validationError = validateSingleForm(form);
    if (validationError) {
      singleFormMessage = validationError;
      render();
      return;
    }
    singleFormMessage = "";
    const data = new FormData(form);
    normalizeImageField(data);
    singleResult = await postJson<VerificationResult>("/api/verify", data);
    mergeReviewResults([singleResult]);
    render();
  });
}

function validateSingleForm(form: HTMLFormElement) {
  const data = new FormData(form);
  const requiredFields = [
    ["brand_name", "Brand name"],
    ["class_type", "Class / type"],
    ["alcohol_content", "Alcohol content"],
    ["net_contents", "Net contents"],
  ] as const;

  for (const [name, label] of requiredFields) {
    const value = String(data.get(name) || "").trim();
    if (!value) {
      return `${label} is required before OCR runs. Placeholder examples are not submitted as values.`;
    }
  }

  const files = data.getAll("images").filter((file) => file instanceof File && file.size > 0);
  if (!files.length) return "Upload at least one label image before running verification.";
  return "";
}

function bindBatch() {
  document.querySelector<HTMLFormElement>("#batch-form")?.addEventListener("submit", async (event) => {
    event.preventDefault();
    const form = event.currentTarget as HTMLFormElement;
    const data = new FormData(form);
    normalizeImageField(data);
    const response = await postJson<{ job_id: string }>("/api/batch/jobs", data);
    await pollJob(response.job_id);
  });
}

function bindReview() {
  document.querySelector<HTMLSelectElement>("#review-status-filter")?.addEventListener("change", (event) => {
    reviewStatusFilter = (event.currentTarget as HTMLSelectElement).value as typeof reviewStatusFilter;
    render();
  });
  document.querySelector<HTMLSelectElement>("#review-reason-filter")?.addEventListener("change", (event) => {
    reviewReasonFilter = (event.currentTarget as HTMLSelectElement).value as typeof reviewReasonFilter;
    render();
  });
  document.querySelectorAll<HTMLFormElement>(".correction-form").forEach((form) => {
    form.addEventListener("submit", async (event) => {
      event.preventDefault();
      const form = event.currentTarget as HTMLFormElement;
      const productId = form.dataset.productId || "";
      const result = reviewQueue.find((item) => item.product_id === productId);
      const data = new FormData(form);
      const field = String(data.get("field") || "");
      const correctedValue = String(data.get("corrected_value") || "").trim();
      if (!result || !field || !correctedValue) return;

      await postJsonBody("/api/corrections", {
        product_id: result.product_id,
        label: result.label,
        field,
        expected: field === "government_warning" ? null : result.fields[field]?.expected ?? null,
        corrected_value: correctedValue,
        verdict: result.verdict,
      });
      correctionMessage = `Correction saved for ${result.label || result.product_id}.`;
      render();
    });
  });
}

function normalizeImageField(data: FormData) {
  const files = data.getAll("images");
  data.delete("images");
  files.forEach((file) => {
    if (file instanceof File && file.size > 0) data.append("images[]", file);
  });
}

async function pollJob(jobId: string) {
  if (pollTimer) window.clearInterval(pollTimer);
  const tick = async () => {
    currentJob = await getJson<BatchJob>(`/api/batch/jobs/${jobId}`);
    mergeReviewResults(currentJob.results);
    render();
    if (currentJob.status === "complete" || currentJob.status === "failed") {
      if (pollTimer) window.clearInterval(pollTimer);
    }
  };
  await tick();
  pollTimer = window.setInterval(tick, 1000);
}

function mergeReviewResults(results: VerificationResult[]) {
  const incoming = results.filter((result) => result.verdict !== "pass");
  for (const result of incoming) {
    const existingIndex = reviewQueue.findIndex((item) => item.product_id === result.product_id);
    if (existingIndex >= 0) {
      reviewQueue[existingIndex] = result;
    } else {
      reviewQueue = [result, ...reviewQueue];
    }
  }
}

function filteredReviewQueue() {
  return reviewQueue.filter((result) => {
    if (reviewStatusFilter !== "all" && result.verdict !== reviewStatusFilter) return false;
    if (reviewReasonFilter === "all") return true;
    if (reviewReasonFilter === "warning") return result.government_warning.status !== "pass";
    if (reviewReasonFilter === "class_type") return result.fields.class_type?.status !== "pass";
    if (reviewReasonFilter === "country") return result.fields.country?.status !== "pass";
    if (reviewReasonFilter === "low_confidence") {
      return Object.values(result.fields).some((field) => field.confidence < 0.7);
    }
    return true;
  });
}

async function postJson<T>(url: string, body: FormData): Promise<T> {
  const response = await fetch(url, { method: "POST", body });
  if (!response.ok) throw new Error(await response.text());
  return response.json() as Promise<T>;
}

async function postJsonBody<T>(url: string, body: unknown): Promise<T> {
  const response = await fetch(url, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(body),
  });
  if (!response.ok) throw new Error(await response.text());
  return response.json() as Promise<T>;
}

async function getJson<T>(url: string): Promise<T> {
  const response = await fetch(url);
  if (!response.ok) throw new Error(await response.text());
  return response.json() as Promise<T>;
}

function escapeHtml(value: string) {
  return value.replace(/[&<>"']/g, (char) => {
    const escapes: Record<string, string> = {
      "&": "&amp;",
      "<": "&lt;",
      ">": "&gt;",
      '"': "&quot;",
      "'": "&#039;",
    };
    return escapes[char];
  });
}

async function loadServerConfig() {
  const health = await getJson<HealthResponse>("/api/health");
  serverConfig = {
    rawOcrVisible: Boolean(health.security?.raw_ocr_visible),
    correctionsEnabled: health.corrections?.enabled !== false,
    correctionsDurable: Boolean(health.corrections?.durable),
    processingProfile: health.v2?.processing_profile || "adaptive",
    imageTimeBudgetMs: health.v2?.image_time_budget_ms || 4500,
    batchParallelism: health.v2?.batch_parallelism || 2,
    spanLabelMode: health.v2?.span_label_mode || "candidate",
  };
  render();
}

render();
void loadServerConfig().catch(() => {
  render();
});
