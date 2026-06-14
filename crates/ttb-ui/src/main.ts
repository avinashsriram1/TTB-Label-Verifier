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
  telemetry?: {
    parallelism: number;
    total_latency_ms: number;
    average_latency_ms?: number | null;
    slowest_latency_ms?: number | null;
    timeout_count: number;
    fast_pass: number;
    cheap_repair: number;
    enhanced_retry: number;
    timeout_review: number;
  };
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

type BatchResultFilter = "issues" | "all" | "review" | "fail";
type HelpDialog = { title: string; body: string } | null;
type ReviewDrafts = Record<string, Record<string, string>>;

const appElement = document.querySelector<HTMLDivElement>("#app");
if (!appElement) throw new Error("missing app root");
const app = appElement;

let activeTab: "single" | "batch" | "review" = "single";
let singleResult: VerificationResult | null = null;
let currentJob: BatchJob | null = null;
let reviewQueue: VerificationResult[] = [];
let singleFormMessage = "";
let batchFormMessage = "";
let batchResultFilter: BatchResultFilter = "issues";
let reviewStatusFilter: "all" | "review" | "fail" = "all";
let reviewReasonFilter: "all" | "warning" | "class_type" | "country" | "low_confidence" = "all";
let correctionMessage = "";
let helpDialog: HelpDialog = null;
let reviewDrafts: ReviewDrafts = {};
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
    ${helpDialog ? renderHelpDialog(helpDialog) : ""}
  `;

  bindTabs();
  if (activeTab === "single") bindSingle();
  if (activeTab === "batch") bindBatch();
  if (activeTab === "review") bindReview();
  bindHelp();
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

function renderHelpDialog(dialog: Exclude<HelpDialog, null>) {
  return `
    <div class="modal-backdrop" data-help-close="true" role="presentation">
      <section class="help-dialog" role="dialog" aria-modal="true" aria-labelledby="help-title">
        <div class="panel-title">
          <h2 id="help-title">${escapeHtml(dialog.title)}</h2>
          <button class="secondary small" type="button" data-help-close="true">Close</button>
        </div>
        <p>${escapeHtml(dialog.body)}</p>
      </section>
    </div>
  `;
}

function bindHelp() {
  document.querySelectorAll<HTMLButtonElement>(".help").forEach((button) => {
    button.addEventListener("click", () => {
      helpDialog = {
        title: button.dataset.helpTitle || "Help",
        body: button.dataset.helpBody || button.title,
      };
      render();
    });
  });

  document.querySelectorAll<HTMLElement>("[data-help-close]").forEach((element) => {
    element.addEventListener("click", (event) => {
      if (event.target !== element && element.classList.contains("modal-backdrop")) return;
      helpDialog = null;
      render();
    });
  });

  document.onkeydown = (event) => {
    if (event.key === "Escape" && helpDialog) {
      helpDialog = null;
      render();
    }
  };
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
          <span>Label images ${help("Label images", "Upload one to four images for the same product. Use multiple images when the front and back labels carry different required text.")}</span>
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
        ${batchFormMessage ? `<p class="form-error">${escapeHtml(batchFormMessage)}</p>` : ""}
        <label class="drop">
          <span>Label images ${help("Batch label images", "Upload all label images for the batch. A JSON manifest can group front and back images under one product.")}</span>
          <input name="images" type="file" accept="image/*,.tif,.tiff" multiple required />
        </label>
        <label class="drop">
          <span>
            Manifest
            ${help("Batch manifest", "Optional CSV or JSON file with expected application values. Without it, each image becomes a separate review item. JSON may be a top-level array or an object with a products array. Image references can use image, images, image_names, front_image, or back_image. Extra expected_verdict, expected_path, and notes fields are ignored by the verifier.")}
          </span>
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
      <span>${label} ${help(label, helpText)}</span>
      <input name="${name}" placeholder="${placeholder}" ${required ? "required aria-required=\"true\"" : ""} autocomplete="off" />
    </label>
  `;
}

function help(title: string, text: string) {
  return `<button class="help" type="button" title="${escapeHtml(text)}" aria-label="${escapeHtml(title)} help" data-help-title="${escapeHtml(title)}" data-help-body="${escapeHtml(text)}">?</button>`;
}

function option(value: string, label: string, selectedValue: string) {
  return `<option value="${value}" ${selectedValue === value ? "selected" : ""}>${label}</option>`;
}

function renderBatchJob(job: BatchJob) {
  const pct = job.total ? Math.round((job.completed / job.total) * 100) : 0;
  const filteredResults = sortedBatchResults(filteredBatchResults(job.results));
  return `
    <div class="progress"><span style="width:${pct}%"></span></div>
    <div class="counts">
      <span class="status pass">Pass ${job.counts.pass}</span>
      <span class="status review">Review ${job.counts.review}</span>
      <span class="status fail">Fail ${job.counts.fail}</span>
    </div>
    ${renderBatchTelemetry(job)}
    <div class="batch-tools">
      <label>
        <span>Visible results</span>
        <select id="batch-result-filter">
          ${option("issues", "Issues only", batchResultFilter)}
          ${option("all", "All", batchResultFilter)}
          ${option("review", "Review", batchResultFilter)}
          ${option("fail", "Fail", batchResultFilter)}
        </select>
      </label>
      ${job.job_id ? `<a class="export" href="/api/batch/jobs/${job.job_id}/export.csv">Export CSV</a>` : ""}
    </div>
    <div class="result-list">${filteredResults.length ? filteredResults.map(renderBatchResult).join("") : `<div class="empty">No results match the current batch filter.</div>`}</div>
    ${job.errors.map((error) => `<p class="error">${escapeHtml(error)}</p>`).join("")}
  `;
}

function renderBatchTelemetry(job: BatchJob) {
  const telemetry = job.telemetry;
  if (!telemetry) return "";
  return `
    <div class="batch-telemetry">
      <span><strong>${job.completed}/${job.total}</strong> complete</span>
      <span><strong>${telemetry.parallelism}</strong> workers</span>
      <span><strong>${formatDuration(telemetry.average_latency_ms ?? 0)}</strong> avg</span>
      <span><strong>${formatDuration(telemetry.slowest_latency_ms ?? 0)}</strong> slowest</span>
      <span><strong>${telemetry.timeout_count}</strong> timed out</span>
      <span><strong>${telemetry.enhanced_retry}</strong> enhanced</span>
    </div>
  `;
}

function filteredBatchResults(results: VerificationResult[]) {
  if (batchResultFilter === "all") return results;
  if (batchResultFilter === "review") return results.filter((result) => result.verdict === "review");
  if (batchResultFilter === "fail") return results.filter((result) => result.verdict === "fail");
  return results.filter((result) => result.verdict !== "pass");
}

function sortedBatchResults(results: VerificationResult[]) {
  const rank: Record<Verdict, number> = { fail: 0, review: 1, pass: 2 };
  return [...results].sort((a, b) => {
    const severity = rank[a.verdict] - rank[b.verdict];
    if (severity !== 0) return severity;
    return (a.label || a.product_id).localeCompare(b.label || b.product_id);
  });
}

function renderBatchResult(result: VerificationResult) {
  return `
    <details class="result batch-result ${result.verdict}">
      <summary>
        <span>
          <strong>${escapeHtml(result.label || result.product_id)}</strong>
          <small>${result.image_count} image(s) - ${issueSummary(result)}</small>
        </span>
        <span class="result-actions">
          <span class="details-chip">Open details</span>
          <span class="timer">${formatDuration(result.latency_ms)}</span>
          ${badge(result.verdict)}
        </span>
      </summary>
      ${renderResultBody(result, false)}
    </details>
  `;
}

function renderResult(result: VerificationResult) {
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
      ${renderResultBody(result, activeTab === "review" && serverConfig.correctionsEnabled)}
    </article>
  `;
}

function renderResultBody(result: VerificationResult, includeReviewActions: boolean) {
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
      ${includeReviewActions ? renderReviewEditor(result) : ""}
      ${result.notes.map((note) => `<p class="note">${escapeHtml(note)}</p>`).join("")}
  `;
}

function issueSummary(result: VerificationResult) {
  const issues = Object.values(result.fields)
    .filter((field) => field.status !== "pass")
    .map((field) => labelFor(field.field));
  if (result.government_warning.status !== "pass") issues.unshift("Government Warning");
  if (!issues.length) return "No active issues";
  return `${issues.length} issue${issues.length === 1 ? "" : "s"}: ${issues.slice(0, 3).join(", ")}${issues.length > 3 ? ", ..." : ""}`;
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

function renderReviewEditor(result: VerificationResult) {
  const issues = editableIssues(result);
  const drafts = reviewDrafts[result.product_id] ?? {};
  const hasDrafts = Object.values(drafts).some((value) => value.trim());

  return `
    <form class="review-editor" data-product-id="${escapeHtml(result.product_id)}">
      <div class="review-editor-head">
        <strong>Agent review actions</strong>
        <span class="unsaved" ${hasDrafts ? "" : "hidden"}>Unsaved changes</span>
      </div>
      ${
        issues.length
          ? issues
              .map(
                (issue) => `
                  <label>
                    <span>${escapeHtml(issue.label)}</span>
                    <input class="review-edit" name="${escapeHtml(issue.field)}" value="${escapeHtml(drafts[issue.field] ?? "")}" placeholder="${escapeHtml(issue.placeholder)}" autocomplete="off" />
                  </label>
                `,
              )
              .join("")
          : `<p class="empty">No active review fields remain.</p>`
      }
      <div class="review-actions">
        <button class="primary compact" type="submit">Save and Resolve Entered Issues</button>
        <button class="danger" type="button" data-review-action="fail">Mark Application Failed</button>
        <button class="secondary" type="button" data-review-action="dismiss">Dismiss From Queue</button>
      </div>
    </form>
  `;
}

function editableIssues(result: VerificationResult) {
  const issues = Object.values(result.fields)
    .filter((field) => field.status !== "pass")
    .map((field) => ({
      field: field.field,
      label: labelFor(field.field),
      placeholder: field.expected
        ? `Correct value or note for ${field.expected}`
        : "Correct value or concise review note",
    }));

  if (result.government_warning.status !== "pass") {
    issues.unshift({
      field: "government_warning",
      label: "Government Warning",
      placeholder: "Confirm warning correction or enter review note",
    });
  }

  return issues;
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
    batchFormMessage = "";
    try {
      const response = await postJson<{ job_id: string }>("/api/batch/jobs", data);
      await pollJob(response.job_id);
    } catch (error) {
      batchFormMessage = `Batch upload failed: ${errorMessage(error)}`;
      render();
    }
  });

  document.querySelector<HTMLSelectElement>("#batch-result-filter")?.addEventListener("change", (event) => {
    batchResultFilter = (event.currentTarget as HTMLSelectElement).value as BatchResultFilter;
    render();
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
  document.querySelectorAll<HTMLInputElement>(".review-edit").forEach((input) => {
    input.addEventListener("input", () => {
      const form = input.closest<HTMLFormElement>(".review-editor");
      const productId = form?.dataset.productId || "";
      if (!productId) return;
      reviewDrafts[productId] = reviewDrafts[productId] ?? {};
      reviewDrafts[productId][input.name] = input.value;
      form?.querySelector<HTMLElement>(".unsaved")?.removeAttribute("hidden");
    });
  });
  document.querySelectorAll<HTMLFormElement>(".review-editor").forEach((form) => {
    form.addEventListener("submit", async (event) => {
      event.preventDefault();
      const form = event.currentTarget as HTMLFormElement;
      const productId = form.dataset.productId || "";
      await saveReviewEdits(productId, form);
      render();
    });
  });
  document.querySelectorAll<HTMLButtonElement>("[data-review-action]").forEach((button) => {
    button.addEventListener("click", async () => {
      const form = button.closest<HTMLFormElement>(".review-editor");
      const productId = form?.dataset.productId || "";
      if (!productId) return;
      const action = button.dataset.reviewAction;
      if (action === "fail") {
        await markApplicationFailed(productId);
      } else if (action === "dismiss") {
        dismissReviewItem(productId);
      }
      render();
    });
  });
}

async function saveReviewEdits(productId: string, form?: HTMLFormElement) {
  const result = reviewQueue.find((item) => item.product_id === productId);
  if (!result) return;

  const formValues = form ? reviewValuesFromForm(form) : {};
  const drafts = { ...(reviewDrafts[productId] ?? {}), ...formValues };
  const entries = Object.entries(drafts)
    .map(([field, value]) => [field, value.trim()] as const)
    .filter(([, value]) => value);

  if (!entries.length) {
    correctionMessage = `Enter at least one correction for ${result.label || result.product_id}.`;
    return;
  }

  for (const [field, correctedValue] of entries) {
    await postJsonBody("/api/corrections", {
      product_id: result.product_id,
      label: result.label,
      field,
      expected: field === "government_warning" ? null : result.fields[field]?.expected ?? null,
      corrected_value: correctedValue,
      verdict: result.verdict,
    });
    applyCorrectionToResult(result, field, correctedValue);
  }

  result.verdict = aggregateUiVerdict(result);
  delete reviewDrafts[productId];

  if (result.verdict === "pass") {
    reviewQueue = reviewQueue.filter((item) => item.product_id !== productId);
    correctionMessage = `Corrections saved for ${result.label || result.product_id}; item dismissed because no issues remain.`;
  } else {
    correctionMessage = `Corrections saved for ${result.label || result.product_id}. Remaining issues: ${issueSummary(result)}.`;
  }
}

function reviewValuesFromForm(form: HTMLFormElement) {
  const values: Record<string, string> = {};
  form.querySelectorAll<HTMLInputElement>(".review-edit").forEach((input) => {
    values[input.name] = input.value;
  });
  return values;
}

function applyCorrectionToResult(result: VerificationResult, field: string, correctedValue: string) {
  if (field === "government_warning") {
    result.government_warning.status = "pass";
    result.government_warning.present = true;
    result.government_warning.detail = "Agent saved a government warning correction.";
    result.government_warning.issues = [];
    return;
  }

  const check = result.fields[field];
  if (!check) return;
  check.observed = correctedValue;
  check.status = "pass";
  check.confidence = 1;
  check.detail = "Agent corrected and saved this field.";
}

async function markApplicationFailed(productId: string) {
  const result = reviewQueue.find((item) => item.product_id === productId);
  if (!result) return;

  await postJsonBody("/api/corrections", {
    product_id: result.product_id,
    label: result.label,
    field: "final_verdict",
    expected: null,
    corrected_value: "fail",
    verifier_note: "Agent marked the application failed from the review queue.",
    verdict: "fail",
  });

  delete reviewDrafts[productId];
  reviewQueue = reviewQueue.filter((item) => item.product_id !== productId);
  correctionMessage = `${result.label || result.product_id} was marked failed and removed from the review queue.`;
}

function dismissReviewItem(productId: string) {
  const result = reviewQueue.find((item) => item.product_id === productId);
  delete reviewDrafts[productId];
  reviewQueue = reviewQueue.filter((item) => item.product_id !== productId);
  correctionMessage = `${result?.label || productId} was dismissed from this browser queue.`;
}

function aggregateUiVerdict(result: VerificationResult): Verdict {
  if (
    result.government_warning.status === "fail" ||
    result.government_warning.status === "missing" ||
    Object.values(result.fields).some((field) => field.status === "fail")
  ) {
    return "fail";
  }
  if (
    result.government_warning.status === "review" ||
    Object.values(result.fields).some((field) => field.status === "review" || field.status === "missing")
  ) {
    return "review";
  }
  return "pass";
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
  if (!response.ok) throw new Error(await responseErrorText(response));
  return response.json() as Promise<T>;
}

async function postJsonBody<T>(url: string, body: unknown): Promise<T> {
  const response = await fetch(url, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(body),
  });
  if (!response.ok) throw new Error(await responseErrorText(response));
  return response.json() as Promise<T>;
}

async function getJson<T>(url: string): Promise<T> {
  const response = await fetch(url);
  if (!response.ok) throw new Error(await responseErrorText(response));
  return response.json() as Promise<T>;
}

async function responseErrorText(response: Response) {
  const text = await response.text();
  try {
    const parsed = JSON.parse(text) as { error?: string };
    return parsed.error || text;
  } catch {
    return text || `${response.status} ${response.statusText}`;
  }
}

function errorMessage(error: unknown) {
  return error instanceof Error ? error.message : String(error);
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
