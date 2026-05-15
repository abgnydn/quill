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
      label = "harper + llm";
      rewriteBtn.disabled = false;
      rewriteHint.textContent = "";
    } else if (c.llm_built && !c.model_loaded) {
      label = "harper + llm (no model)";
      rewriteHint.textContent = "set QUILL_MODEL to your .gguf and relaunch";
    } else {
      rewriteHint.textContent = "rebuild with --features llm to enable";
    }
    caps.textContent = label;
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
  const chars = [...editor.value];
  if (sugg.kind === "replace") {
    chars.splice(lint.start, lint.end - lint.start, ...sugg.text);
  } else if (sugg.kind === "insert_after") {
    chars.splice(lint.end, 0, ...sugg.text);
  } else if (sugg.kind === "remove") {
    chars.splice(lint.start, lint.end - lint.start);
  }
  editor.value = chars.join("");
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
  editor.value =
    editor.value.slice(0, sel.start) + out + editor.value.slice(sel.end);
  rewriteOutput.classList.add("hidden");
  runCheck();
});
rewriteDismiss.addEventListener("click", () => {
  rewriteOutput.classList.add("hidden");
});

editor.addEventListener("input", debounce(runCheck, 250));

probeCapabilities();
runCheck();
