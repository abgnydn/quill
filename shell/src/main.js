const { invoke } = window.__TAURI__.core;

const editor = document.getElementById("editor");
const panel = document.getElementById("suggestions");
const status = document.getElementById("status");
const caps = document.getElementById("caps");
const rewriteBtn = document.getElementById("rewrite-btn");
const rewriteHint = document.getElementById("rewrite-hint");
const rewriteOutput = document.getElementById("rewrite-output");
const rewriteText = document.getElementById("rewrite-text");
const rewriteApply = document.getElementById("rewrite-apply");
const rewriteDismiss = document.getElementById("rewrite-dismiss");

let debounceTimer = null;
let inflight = false;
let pending = false;

function debounce(fn, ms) {
  return (...args) => {
    clearTimeout(debounceTimer);
    debounceTimer = setTimeout(() => fn(...args), ms);
  };
}

async function probeCapabilities() {
  try {
    const c = await invoke("capabilities");
    let label = "harper-only";
    if (c.llm_built && c.model_loaded) {
      label = c.personal_adapter_loaded ? "harper + llm + personal" : "harper + llm";
      rewriteBtn.disabled = false;
      rewriteHint.textContent = "";
    } else if (c.llm_built && !c.model_loaded) {
      label = "harper + llm (no model)";
      rewriteHint.textContent = "set QUILL_MODEL to your .gguf and relaunch";
    } else {
      rewriteHint.textContent = "rebuild with --features llm to enable";
    }
    caps.textContent = label;
    // Pill in the personalization panel reflects the same state.
    if (personalPill) {
      if (c.personal_adapter_loaded) {
        personalPill.textContent = "personal";
        personalPill.className = "pill pill-personal";
      } else {
        personalPill.textContent = "base only";
        personalPill.className = "pill pill-base";
      }
    }
  } catch (e) {
    caps.textContent = `caps error: ${e}`;
  }
}

async function runCheck() {
  if (inflight) {
    pending = true;
    return;
  }
  inflight = true;
  status.textContent = "checking…";
  try {
    const text = editor.value;
    const t0 = performance.now();
    const lints = await invoke("check", { text });
    const dt = (performance.now() - t0).toFixed(1);
    render(lints, text);
    status.textContent = `${lints.length} lint${lints.length === 1 ? "" : "s"} · ${dt} ms`;
  } catch (err) {
    status.textContent = "error";
    panel.innerHTML = `<div class="empty">error: ${String(err)}</div>`;
  } finally {
    inflight = false;
    if (pending) {
      pending = false;
      runCheck();
    }
  }
}

function render(lints, source) {
  if (!lints.length) {
    panel.innerHTML = `<div class="empty">no suggestions</div>`;
    return;
  }
  const chars = [...source];
  panel.innerHTML = lints
    .map((l, i) => {
      const slice = chars.slice(l.start, l.end).join("");
      const suggs = l.suggestions
        .map(
          (s, j) =>
            `<button class="sugg" data-lint="${i}" data-sugg="${j}">${
              s.kind === "remove" ? "⌫ remove" : escapeHtml(s.text)
            }</button>`
        )
        .join("");
      return `<div class="lint">
        <div class="kind">${l.kind}<span class="span">${escapeHtml(slice)}</span></div>
        <div class="msg">${escapeHtml(l.message)}</div>
        ${suggs ? `<div class="suggs">${suggs}</div>` : ""}
      </div>`;
    })
    .join("");

  panel.querySelectorAll(".sugg").forEach((btn) => {
    btn.addEventListener("click", () => {
      const lintIdx = parseInt(btn.dataset.lint, 10);
      const suggIdx = parseInt(btn.dataset.sugg, 10);
      applySuggestion(lints[lintIdx], lints[lintIdx].suggestions[suggIdx]);
    });
  });
}

function applySuggestion(lint, sugg) {
  const before = editor.value;
  const chars = [...before];
  let start = lint.start, end = lint.end;
  let replacement = sugg.text || "";
  if (sugg.kind === "replace") {
    chars.splice(lint.start, lint.end - lint.start, ...sugg.text);
  } else if (sugg.kind === "insert_after") {
    chars.splice(lint.end, 0, ...sugg.text);
    start = lint.end; end = lint.end;
  } else if (sugg.kind === "remove") {
    chars.splice(lint.start, lint.end - lint.start);
    replacement = "";
  }
  const after = chars.join("");
  editor.value = after;
  // Record into the personalization journal. Main-window applies mutate
  // editor.value directly (no AXUI round-trip) so we journal explicitly.
  invoke("journal_log", {
    kind: "apply",
    sourceText: before,
    appliedText: after,
    suggested: replacement,
    lintKind: lint.kind,
    lintMessage: lint.message,
    lintStart: start,
    lintEnd: end,
  })
    .then(() => refreshPersonal())
    .catch(() => {});
  runCheck();
}

function escapeHtml(s) {
  return s
    .replaceAll("&", "&amp;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;")
    .replaceAll('"', "&quot;");
}

function selectedOrAll() {
  const start = editor.selectionStart;
  const end = editor.selectionEnd;
  if (end > start) {
    return { text: editor.value.slice(start, end), start, end };
  }
  return { text: editor.value, start: 0, end: editor.value.length };
}

async function runRewrite() {
  const { text } = selectedOrAll();
  if (!text.trim()) return;
  rewriteBtn.disabled = true;
  rewriteOutput.classList.remove("hidden");
  rewriteText.textContent = "";
  rewriteText.classList.add("loading");
  const t0 = performance.now();
  try {
    const out = await invoke("rewrite", { text, instruction: null });
    const dt = (performance.now() - t0).toFixed(0);
    rewriteText.classList.remove("loading");
    rewriteText.textContent = out;
    rewriteHint.textContent = `${dt} ms`;
  } catch (err) {
    rewriteText.classList.remove("loading");
    rewriteText.textContent = `error: ${err}`;
    rewriteHint.textContent = "";
  } finally {
    rewriteBtn.disabled = false;
  }
}

rewriteBtn.addEventListener("click", runRewrite);
rewriteApply.addEventListener("click", () => {
  const out = rewriteText.textContent;
  if (!out) return;
  const sel = selectedOrAll();
  const before = editor.value;
  const after =
    editor.value.slice(0, sel.start) + out + editor.value.slice(sel.end);
  editor.value = after;
  rewriteOutput.classList.add("hidden");
  invoke("journal_log", {
    kind: "rewrite_apply",
    sourceText: before,
    appliedText: after,
    suggested: out,
  })
    .then(() => refreshPersonal())
    .catch(() => {});
  runCheck();
});
rewriteDismiss.addEventListener("click", () => {
  rewriteOutput.classList.add("hidden");
});

editor.addEventListener("input", debounce(runCheck, 250));

// ---- Personalization panel (v0.5 phase 1) --------------------------------
const personalCount = document.getElementById("personal-count");
const personalApplied = document.getElementById("personal-applied");
const personalRewrite = document.getElementById("personal-rewrite");
const personalRange = document.getElementById("personal-range");
const personalExport = document.getElementById("personal-export");
const personalClear = document.getElementById("personal-clear");
const personalTrain = document.getElementById("personal-train");
const personalPill = document.getElementById("personal-pill");

const trainModal = document.getElementById("train-modal");
const trainState = document.getElementById("train-state");
const trainElapsed = document.getElementById("train-elapsed");
const trainStage = document.getElementById("train-stage");
const trainError = document.getElementById("train-error");
const trainInstall = document.getElementById("train-install");
const trainDismiss = document.getElementById("train-dismiss");

function fmtTs(t) {
  if (!t) return "—";
  return t.slice(0, 10); // YYYY-MM-DD
}

async function refreshPersonal() {
  try {
    const s = await invoke("journal_stats");
    personalCount.textContent = String(s.count || 0);
    personalApplied.textContent = String(s.applied || 0);
    personalRewrite.textContent = String(s.rewrite_applied || 0);
    if (s.oldest_ts && s.newest_ts) {
      personalRange.textContent =
        s.oldest_ts === s.newest_ts
          ? `since ${fmtTs(s.oldest_ts)}`
          : `${fmtTs(s.oldest_ts)} → ${fmtTs(s.newest_ts)}`;
    } else {
      personalRange.textContent = "no events yet";
    }
    // Enable Train button only when there's enough data.
    if (personalTrain) {
      const enough = (s.applied || 0) + (s.rewrite_applied || 0) >= 10;
      personalTrain.disabled = !enough;
      personalTrain.title = enough
        ? "Train a personal LoRA adapter on Modal (~15 min, ~$0.20)"
        : `Need ≥10 applied edits to train (have ${(s.applied||0)+(s.rewrite_applied||0)})`;
    }
  } catch (e) {
    // journal not available — fail quiet
  }
}

personalExport.addEventListener("click", async () => {
  const ts = new Date().toISOString().replace(/[:.]/g, "-").slice(0, 19);
  const out = `~/Downloads/quill-training-${ts}.jsonl`.replace(/^~/, `${(await invoke("capabilities"))?.home ?? ""}`);
  // We can't read $HOME from JS — let Rust resolve it. Pass an absolute path.
  const path = `/tmp/quill-training-${ts}.jsonl`;
  try {
    const n = await invoke("journal_export", { outPath: path });
    personalExport.textContent = `✓ ${n} pairs → ${path}`;
    setTimeout(() => (personalExport.textContent = "⤓ Export"), 4000);
  } catch (e) {
    personalExport.textContent = `error: ${e}`;
    setTimeout(() => (personalExport.textContent = "⤓ Export"), 4000);
  }
});

personalClear.addEventListener("click", async () => {
  if (!confirm("Clear all learned edits? This cannot be undone.")) return;
  try {
    const bytes = await invoke("journal_clear");
    personalClear.textContent = `✓ cleared ${bytes}B`;
    setTimeout(() => (personalClear.textContent = "⌫ Reset"), 2500);
    refreshPersonal();
  } catch (e) {
    personalClear.textContent = `error: ${e}`;
  }
});

// ---- Personal training (v0.5 phase 3) ----------------------------------
let trainPollTimer = null;
const STATE_LABEL = {
  idle: "idle",
  running: "training in progress…",
  succeeded: "✓ training complete",
  failed: "training failed",
};

function fmtElapsed(s) {
  if (!s) return "";
  const sec = Math.floor(s);
  const m = Math.floor(sec / 60);
  return m > 0 ? `${m}m ${sec % 60}s elapsed` : `${sec}s elapsed`;
}

function renderTrainStatus(st) {
  trainState.textContent = STATE_LABEL[st.state] || st.state;
  trainElapsed.textContent = fmtElapsed(st.elapsed_secs);
  trainStage.textContent = st.stage || "";
  trainError.textContent = st.error || "";
  if (st.state === "succeeded") {
    trainInstall.classList.remove("hidden");
  } else {
    trainInstall.classList.add("hidden");
  }
}

async function pollTrainOnce() {
  try {
    const st = await invoke("train_personal_status");
    renderTrainStatus(st);
    if (st.state === "succeeded" || st.state === "failed") {
      clearInterval(trainPollTimer);
      trainPollTimer = null;
    }
  } catch (e) {
    trainError.textContent = String(e);
  }
}

personalTrain.addEventListener("click", async () => {
  trainModal.classList.remove("hidden");
  trainError.textContent = "";
  trainStage.textContent = "";
  trainState.textContent = "starting…";
  try {
    const st = await invoke("train_personal_start");
    renderTrainStatus(st);
    if (trainPollTimer) clearInterval(trainPollTimer);
    trainPollTimer = setInterval(pollTrainOnce, 3000);
  } catch (e) {
    trainState.textContent = "failed to start";
    trainError.textContent = String(e);
  }
});

trainInstall.addEventListener("click", async () => {
  try {
    const dest = await invoke("train_personal_install");
    trainStage.textContent = `installed → ${dest}\nQuit and relaunch Quill to load the adapter.`;
    trainInstall.disabled = true;
    trainInstall.textContent = "✓ Installed — relaunch Quill";
  } catch (e) {
    trainError.textContent = String(e);
  }
});

trainDismiss.addEventListener("click", () => {
  trainModal.classList.add("hidden");
  if (trainPollTimer) {
    clearInterval(trainPollTimer);
    trainPollTimer = null;
  }
  // Don't reset the actual job — user can re-open via "Train" while it's
  // still running. They can also explicitly reset via the menu (future).
});

probeCapabilities();
runCheck();
refreshPersonal();
setInterval(refreshPersonal, 5000);
