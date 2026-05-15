const { invoke } = window.__TAURI__.core;

const editor = document.getElementById("editor");
const panel = document.getElementById("suggestions");
const status = document.getElementById("status");

let debounceTimer = null;
let inflight = false;
let pending = false;

function debounce(fn, ms) {
  return (...args) => {
    clearTimeout(debounceTimer);
    debounceTimer = setTimeout(() => fn(...args), ms);
  };
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

editor.addEventListener("input", debounce(runCheck, 250));
runCheck();
