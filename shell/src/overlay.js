// Quill overlay — runtime.
//
// All state + DOM logic for the click-through overlay window. Receives
// `focus-update` events from the Rust focus_tracker, `cursor-{enter,move,
// leave}-hot` events from the mouse_arbiter, and round-trips diagnostic
// pings back via the `overlay_ping` Tauri command.

(() => {
  const $ = (id) => document.getElementById(id);
  const corner = $("corner");
  const underlinesEl = $("underlines");
  const fallbackEl = $("fallback");
  const fbHeader = $("fb-header");
  const fbList = $("fb-list");
  const popover = $("popover");
  const popBulk = $("pop-bulk");
  const popBulkAccept = $("pop-bulk-accept");
  const popBulkDismiss = $("pop-bulk-dismiss");
  const popBulkCount = $("pop-bulk-count");
  const popKind = $("pop-kind");
  const popMsg = $("pop-msg");
  const popSuggs = $("pop-suggs");
  const popWhyToggle = $("pop-why-toggle");
  const popWhyBody = $("pop-why-body");
  const aiBtn = $("ai-btn");
  const aiBtnLabel = $("ai-btn-label");
  const aiBtnSpinner = $("ai-btn-spinner");
  const aiOut = $("ai-out");
  const aiText = $("ai-text");
  const aiApply = $("ai-apply");
  const aiDismiss = $("ai-dismiss");

  if (!window.__TAURI__ || !window.__TAURI__.event || !window.__TAURI__.core) {
    corner.textContent = "?T";
    throw new Error("no tauri api");
  }
  const { listen } = window.__TAURI__.event;
  const { invoke } = window.__TAURI__.core;

  // ---- diagnostic ping --------------------------------------------------
  const ping = (stage, count, detail) =>
    invoke("overlay_ping", { stage, count, detail: detail || null }).catch(() => {});
  ping("script-loaded", 0);

  // ---- state -----------------------------------------------------------
  let eventCount = 0;
  let currentText = "";
  let currentLints = [];
  let currentFieldBounds = null;
  let activeLintIdx = -1;
  let hoverHideTimer = null;
  let cornerIdleTimer = null;
  let lastRewrite = "";

  // ---- corner badge with idle fade ------------------------------------
  const flashCorner = () => {
    corner.classList.remove("idle");
    corner.classList.add("hot");
    clearTimeout(cornerIdleTimer);
    cornerIdleTimer = setTimeout(() => {
      corner.classList.remove("hot");
      corner.classList.add("idle");
    }, 2500);
  };
  const updateCorner = () => {
    const n = currentLints.length;
    corner.textContent = n ? `Quill · ${n}` : "Quill";
  };
  updateCorner();
  flashCorner();

  // ---- helpers --------------------------------------------------------
  const escapeHtml = (s) =>
    String(s)
      .replaceAll("&", "&amp;")
      .replaceAll("<", "&lt;")
      .replaceAll(">", "&gt;");

  // Word-level LCS diff for the AI rewrite output. Splits on whitespace
  // boundaries but preserves separators so reassembling is exact.
  function tokenizeWords(s) {
    return s.match(/\s+|\S+/g) || [];
  }
  function diffWords(oldStr, newStr) {
    const a = tokenizeWords(oldStr);
    const b = tokenizeWords(newStr);
    const m = a.length, n = b.length;
    const dp = Array(m + 1).fill(null).map(() => new Uint32Array(n + 1));
    for (let i = m - 1; i >= 0; i--) {
      for (let j = n - 1; j >= 0; j--) {
        dp[i][j] = a[i] === b[j] ? dp[i+1][j+1] + 1 : Math.max(dp[i+1][j], dp[i][j+1]);
      }
    }
    const out = [];
    let i = 0, j = 0;
    while (i < m && j < n) {
      if (a[i] === b[j]) { out.push({ type: "same", text: a[i] }); i++; j++; }
      else if (dp[i+1][j] >= dp[i][j+1]) { out.push({ type: "del", text: a[i] }); i++; }
      else { out.push({ type: "ins", text: b[j] }); j++; }
    }
    while (i < m) { out.push({ type: "del", text: a[i++] }); }
    while (j < n) { out.push({ type: "ins", text: b[j++] }); }
    const merged = [];
    for (const seg of out) {
      const last = merged[merged.length - 1];
      if (last && last.type === seg.type) last.text += seg.text;
      else merged.push({ ...seg });
    }
    return merged;
  }
  function renderDiffHtml(oldStr, newStr) {
    return diffWords(oldStr, newStr).map((s) => {
      const t = escapeHtml(s.text);
      if (s.type === "del") return `<del>${t}</del>`;
      if (s.type === "ins") return `<ins>${t}</ins>`;
      return t;
    }).join("");
  }

  const kindClass = (kind) => {
    const k = String(kind).toLowerCase();
    if (k.includes("spell")) return "spelling";
    if (k.includes("agreement")) return "agreement";
    if (k.includes("style") || k.includes("punct") || k.includes("article")) return "style";
    return "misc";
  };

  // Static "Why?" lookup, keyed by Harper's `LintKind` Debug name (see
  // shell/src-tauri/src/wire.rs → `format!("{:?}", l.lint_kind)`). When a
  // kind isn't covered, the popover falls back to the lint's own `message`.
  const WHY_MAP = {
    Agreement: "Subjects and verbs must agree in number. 'I has' uses the wrong verb form for the singular 'I' — singular subjects take 'have'/'is', plural subjects take 'have'/'are'.",
    Spelling: "This word isn't in the dictionary or contains a typo. Dictionary words read as more credible and avoid distracting your reader.",
    Typo: "Your brain knew the right word but your fingers slipped — like 'can be seem' instead of 'can be seen'. The suggested form is the intended one.",
    Capitalization: "Proper nouns, sentence starts, and 'I' need capital letters. Inconsistent casing makes prose look careless.",
    Punctuation: "Punctuation guides the reader's pacing — missing or extra marks can change meaning or stall the eye. Hyphens also matter for compound adjectives (e.g. 'face-first' before a noun).",
    Grammar: "This construction breaks a syntactic rule (tense, case, agreement, word order, etc.). The suggested edit restores standard English.",
    Style: "Both forms are technically correct, but one is preferred for clarity or formal writing — e.g. expanding 'min' to 'minimum' in a formal context.",
    Redundancy: "Words that repeat meaning already expressed weaken your prose. 'Free gift' and 'basic fundamentals' say the same thing twice.",
    Repetition: "The same word or phrase appears twice in a row, almost always by accident. Removing the duplicate usually fixes it.",
    WordChoice: "A different word fits the context better — sharper meaning, fewer connotations, or more natural collocation.",
    Usage: "Standard English prefers a particular collocation here (e.g. 'by accident' vs. 'on accident'). The suggestion matches the conventional form.",
    Enhancement: "Not an error, but a tightening — this edit makes the sentence clearer or more impactful without changing its meaning.",
    Readability: "This sentence is harder to parse than it needs to be — shorter clauses, plainer words, or active voice usually help.",
    BoundaryError: "Words are joined or split at the wrong boundaries — like 'each and everyone' for 'each and every one'. The suggestion separates or joins them correctly.",
    Eggcorn: "A similar-sounding word or phrase has crept in that almost makes sense ('egg corn' for 'acorn'). The suggestion restores the original idiom.",
    Malapropism: "A similar-sounding word with a different meaning slipped in — like 'eluded to' instead of 'alluded to'. The suggested word is the intended one.",
    Nonstandard: "This form is recognized but falls outside standard written English. Use the suggestion when you want the conventional spelling or phrasing.",
    Regionalism: "This spelling or phrasing is standard in some regions but not others (e.g. 'colour' vs. 'color'). The suggestion matches the dialect Quill is set to.",
    Formatting: "Whitespace, quotes, dashes, or other formatting characters don't match prose conventions — like straight quotes where curly quotes are preferred.",
    WrongQuotes: "Smart quotes (curly) are preferred over straight quotes in prose. Most word processors auto-substitute them; the suggestion does the same.",
    Miscellaneous: "Harper flagged this against a rule that doesn't fit the other categories. The suggestion is the rule's recommended replacement.",
  };
  const whyFor = (lint) => {
    if (!lint) return "";
    return WHY_MAP[lint.kind] || lint.message || "";
  };

  const renderChip = (s, lintIdx, suggIdx) => {
    const label = s.kind === "remove" ? "⌫ remove" : escapeHtml(s.text || "");
    const removeClass = s.kind === "remove" ? " remove" : "";
    return `<button class="sugg${removeClass}" data-lint="${lintIdx}" data-sugg="${suggIdx}">${label}</button>`;
  };

  const lintAtPoint = (x, y) => {
    for (let i = 0; i < currentLints.length; i++) {
      const r = currentLints[i].rect;
      if (!r) continue;
      const PAD = 4, TOP = 16;
      if (x >= r.x - PAD && x <= r.x + r.w + PAD &&
          y >= r.y + r.h - TOP - PAD && y <= r.y + r.h + PAD) {
        return i;
      }
    }
    return -1;
  };

  // ---- hot regions (driver for the Rust mouse arbiter) ----------------
  const pushHotRegions = () => {
    const rects = [];
    underlinesEl.querySelectorAll(".underline").forEach((u) => {
      const r = u.getBoundingClientRect();
      rects.push({ x: r.left - 2, y: r.top - 4, w: r.width + 4, h: r.height + 12 });
    });
    if (popover.classList.contains("visible")) {
      const r = popover.getBoundingClientRect();
      rects.push({ x: r.left - 4, y: r.top - 4, w: r.width + 8, h: r.height + 8 });
    }
    if (fallbackEl.classList.contains("visible")) {
      const r = fallbackEl.getBoundingClientRect();
      rects.push({ x: r.left - 4, y: r.top - 4, w: r.width + 8, h: r.height + 8 });
    }
    invoke("overlay_set_hot_regions", { rects }).catch(() => {});
  };

  // ---- popover --------------------------------------------------------
  const hidePopover = () => {
    popover.classList.remove("visible");
    aiOut.classList.remove("visible");
    activeLintIdx = -1;
    requestAnimationFrame(pushHotRegions);
  };
  const showPopover = (lintIdx) => {
    const lint = currentLints[lintIdx];
    if (!lint || !lint.rect) return;
    activeLintIdx = lintIdx;
    clearTimeout(hoverHideTimer);
    flashCorner();

    const slice = [...currentText].slice(lint.start, lint.end).join("");
    const cls = kindClass(lint.kind);
    popKind.className = "pop-kind " + cls;
    popKind.innerHTML =
      `<span>${escapeHtml(lint.kind)}</span>` +
      `<span class="pop-target">${escapeHtml(slice)}</span>`;
    popMsg.textContent = lint.message;
    popSuggs.innerHTML = (lint.suggestions || [])
      .map((s, j) => renderChip(s, lintIdx, j))
      .join("");
    aiOut.classList.remove("visible");

    // Why? — collapsed by default; click expands an explanation block.
    popWhyBody.textContent = whyFor(lint);
    popWhyBody.hidden = true;
    popWhyToggle.setAttribute("aria-expanded", "false");
    popWhyToggle.textContent = "Why?";

    // Bulk toolbar — only useful when there are 2+ lints in the field.
    const n = currentLints.length;
    if (n >= 2) {
      popBulk.hidden = false;
      popBulkCount.textContent = `${n} issues`;
      popBulkAccept.disabled = false;
      popBulkAccept.textContent = "Accept all";
    } else {
      popBulk.hidden = true;
    }

    // Position above the underline; flip below if not enough room.
    const r = lint.rect;
    const W = window.innerWidth, H = window.innerHeight;
    const pw = 280 + 24, ph = 170;
    let x = r.x + r.w / 2 - pw / 2;
    let y = r.y - ph - 8;
    if (y < 8) y = r.y + r.h + 8;
    x = Math.max(8, Math.min(x, W - pw - 8));
    popover.style.left = x + "px";
    popover.style.top = y + "px";
    popover.classList.add("visible");
    requestAnimationFrame(pushHotRegions);
  };

  const scheduleHide = () => {
    clearTimeout(hoverHideTimer);
    hoverHideTimer = setTimeout(hidePopover, 220);
  };
  const cancelHide = () => clearTimeout(hoverHideTimer);

  popover.addEventListener("mouseenter", cancelHide);
  popover.addEventListener("mouseleave", scheduleHide);

  // ---- suggestion click → AXUI write-back -----------------------------
  // Builds the applied_text optimistically so the journal captures the
  // (pre, post) pair without needing a second AXUI read.
  const applySuggestion = async (lintIdx, suggIdx, btn) => {
    const lint = currentLints[lintIdx];
    if (!lint) return;
    const s = lint.suggestions[suggIdx];
    if (!s) return;
    let start = lint.start, end = lint.end, replacement = s.text || "";
    if (s.kind === "remove") replacement = "";
    else if (s.kind === "insert_after") { start = lint.end; end = lint.end; }

    const chars = [...currentText];
    const applied_text =
      chars.slice(0, start).join("") + replacement + chars.slice(end).join("");

    if (btn) btn.classList.add("applied");
    try {
      await invoke("apply_suggestion", {
        start, end, replacement,
        context: {
          kind: "apply",
          source_text: currentText,
          applied_text,
          lint_kind: lint.kind,
          lint_message: lint.message,
        },
      });
      ping("apply-ok", lintIdx, `${lint.kind} -> "${replacement.slice(0,40)}"`);
      setTimeout(hidePopover, 280);
    } catch (err) {
      if (btn) btn.classList.remove("applied");
      ping("apply-err", lintIdx, String(err));
    }
  };

  popSuggs.addEventListener("click", (e) => {
    const t = e.target;
    if (!t || !t.classList.contains("sugg")) return;
    applySuggestion(activeLintIdx, parseInt(t.dataset.sugg, 10), t);
  });
  fbList.addEventListener("click", (e) => {
    const t = e.target;
    if (!t || !t.classList.contains("sugg")) return;
    applySuggestion(parseInt(t.dataset.lint, 10), parseInt(t.dataset.sugg, 10), t);
  });

  // ---- "Why?" expansion -----------------------------------------------
  popWhyToggle.addEventListener("click", () => {
    const open = !popWhyBody.hidden;
    popWhyBody.hidden = open;
    popWhyToggle.setAttribute("aria-expanded", open ? "false" : "true");
    popWhyToggle.textContent = open ? "Why?" : "Why? ▾";
    // The popover changed height — refresh hot regions so the cursor
    // arbiter doesn't drop us before the user clicks Apply.
    requestAnimationFrame(pushHotRegions);
  });

  // ---- bulk Accept all / Dismiss all ----------------------------------
  // Apply the first suggestion of every lint that has one. Lints share a
  // single text buffer, so earlier edits shift later character offsets —
  // we apply in REVERSE start order to keep the remaining offsets valid.
  // Snapshot the (lint, sugg) pairs up-front: each AXUI write fires a
  // focus-update that rewrites `currentLints` mid-loop, so we can't trust
  // currentLints[idx] later — we need to capture the offsets eagerly.
  popBulkAccept.addEventListener("click", async () => {
    if (popBulkAccept.disabled) return;
    const sourceText = currentText;
    const snapshots = currentLints
      .filter((l) => l && l.suggestions && l.suggestions.length > 0)
      .map((l) => ({ lint: l, sugg: l.suggestions[0] }))
      .sort((a, b) => b.lint.start - a.lint.start);
    if (!snapshots.length) {
      hidePopover();
      return;
    }
    popBulkAccept.disabled = true;
    popBulkAccept.textContent = `Applying 0/${snapshots.length}…`;
    let ok = 0;
    let runningText = sourceText;
    for (let k = 0; k < snapshots.length; k++) {
      const { lint, sugg } = snapshots[k];
      let start = lint.start, end = lint.end, replacement = sugg.text || "";
      if (sugg.kind === "remove") replacement = "";
      else if (sugg.kind === "insert_after") { start = lint.end; end = lint.end; }
      const chars = [...runningText];
      const applied_text =
        chars.slice(0, start).join("") + replacement + chars.slice(end).join("");
      try {
        await invoke("apply_suggestion", {
          start, end, replacement,
          context: {
            kind: "apply",
            source_text: runningText,
            applied_text,
            lint_kind: lint.kind,
            lint_message: lint.message,
          },
        });
        runningText = applied_text;
        ok++;
      } catch (err) {
        ping("bulk-apply-err", k, String(err));
      }
      popBulkAccept.textContent = `Applying ${k + 1}/${snapshots.length}…`;
    }
    ping("bulk-apply-done", ok, `total=${snapshots.length}`);
    setTimeout(hidePopover, 200);
  });
  popBulkDismiss.addEventListener("click", () => {
    ping("bulk-dismiss", currentLints.length);
    hidePopover();
  });

  // ---- AI rewrite (streamed) -----------------------------------------
  function makeSession() {
    return (crypto.randomUUID ? crypto.randomUUID() : String(Date.now() + Math.random()));
  }

  aiBtn.addEventListener("click", async () => {
    if (!currentText) return;
    aiBtn.disabled = true;
    aiBtnLabel.textContent = "streaming";
    aiBtnSpinner.style.display = "inline-block";
    aiOut.classList.add("visible");
    aiText.textContent = "";
    aiText.classList.add("streaming");

    const session = makeSession();
    const unlisten = await listen("rewrite-token", (evt) => {
      const p = evt.payload || {};
      if (p.session !== session) return;
      if (p.done) {
        aiText.classList.remove("streaming");
        return;
      }
      if (p.delta) {
        aiText.textContent += p.delta;
        requestAnimationFrame(pushHotRegions);
      }
    });

    try {
      const out = await invoke("rewrite", {
        text: currentText, instruction: null, session,
      });
      lastRewrite = String(out || "");
      if (!aiText.textContent) aiText.textContent = lastRewrite;
      // Once streaming completes, replace raw text with inline diff so the
      // user can SEE what changed instead of having to mentally compare.
      if (currentText && lastRewrite && currentText !== lastRewrite) {
        aiText.innerHTML = renderDiffHtml(currentText, lastRewrite);
      }
    } catch (err) {
      aiText.textContent = "error: " + String(err);
    } finally {
      unlisten();
      aiText.classList.remove("streaming");
      aiBtn.disabled = false;
      aiBtnLabel.textContent = "Rewrite with AI";
      aiBtnSpinner.style.display = "none";
      requestAnimationFrame(pushHotRegions);
    }
  });
  aiApply.addEventListener("click", async () => {
    if (!lastRewrite || !currentText) return;
    try {
      await invoke("apply_suggestion", {
        start: 0,
        end: [...currentText].length,
        replacement: lastRewrite,
        context: {
          kind: "rewrite_apply",
          source_text: currentText,
          applied_text: lastRewrite,
        },
      });
      hidePopover();
    } catch (err) {
      ping("rewrite-apply-err", 0, String(err));
    }
  });
  aiDismiss.addEventListener("click", () => {
    aiOut.classList.remove("visible");
    requestAnimationFrame(pushHotRegions);
  });

  // ---- inline underline rendering ------------------------------------
  const FAT = 14;  // hover-target height (visible wavy stroke is 4px)
  const renderUnderlines = () => {
    underlinesEl.innerHTML = "";
    let rendered = 0;
    for (let i = 0; i < currentLints.length; i++) {
      const l = currentLints[i];
      if (!l.rect || l.rect.w < 1 || l.rect.h < 1) continue;
      const u = document.createElement("div");
      u.className = "underline " + kindClass(l.kind);
      u.dataset.lintIdx = String(i);
      u.style.left = l.rect.x + "px";
      u.style.top = (l.rect.y + l.rect.h - FAT) + "px";
      u.style.width = l.rect.w + "px";
      u.style.height = FAT + "px";
      u.title = l.message;
      underlinesEl.appendChild(u);
      rendered++;
    }
    renderFallback(rendered);
    pushHotRegions();
  };

  const renderFallback = (renderedInlineCount) => {
    if (!currentLints.length || renderedInlineCount > 0 || !currentFieldBounds) {
      fallbackEl.classList.remove("visible");
      return;
    }
    fbHeader.textContent = `Quill — ${currentLints.length}`;
    fbList.innerHTML = currentLints.map((l, i) => {
      const slice = [...currentText].slice(l.start, l.end).join("");
      const suggs = (l.suggestions || []).map((s, j) => renderChip(s, i, j)).join("");
      return `<div class="fb-row ${kindClass(l.kind)}">
        <div class="fb-msg"><b>${escapeHtml(slice)}</b> — ${escapeHtml(l.message)}</div>
        ${suggs ? `<div class="fb-suggs">${suggs}</div>` : ""}
      </div>`;
    }).join("");
    const b = currentFieldBounds;
    const W = window.innerWidth, H = window.innerHeight;
    const fw = 300 + 24;
    let x = b.x + b.w + 12;
    let y = b.y;
    if (x + fw > W) { x = b.x; y = b.y + b.h + 12; }
    if (y + 300 > H) y = Math.max(8, H - 320);
    fallbackEl.style.left = Math.max(8, x) + "px";
    fallbackEl.style.top  = Math.max(8, y) + "px";
    fallbackEl.classList.add("visible");
  };

  // ---- listeners ------------------------------------------------------
  listen("cursor-enter-hot", (evt) => {
    const { x, y } = evt.payload || {};
    const idx = lintAtPoint(x, y);
    ping("cursor-enter-hot", idx, `x=${x|0} y=${y|0}`);
    if (idx >= 0) showPopover(idx);
  });
  listen("cursor-move-hot", (evt) => {
    const { x, y } = evt.payload || {};
    const idx = lintAtPoint(x, y);
    if (idx >= 0 && idx !== activeLintIdx) showPopover(idx);
  });
  listen("cursor-leave-hot", () => {
    ping("cursor-leave-hot", activeLintIdx);
    scheduleHide();
  });

  listen("focus-update", (evt) => {
    eventCount++;
    const p = evt.payload || {};
    currentText = p.text || "";
    currentLints = p.lints || [];
    currentFieldBounds = p.bounds || null;
    updateCorner();
    flashCorner();
    const inline = currentLints.filter((l) => l.rect).length;
    ping("event", eventCount,
      `lints=${currentLints.length} inline=${inline} text_len=${currentText.length}`);

    if (lastRewrite && currentText && lastRewrite !== currentText) {
      aiOut.classList.remove("visible");
      lastRewrite = "";
    }
    if (!currentFieldBounds) fallbackEl.classList.remove("visible");
    renderUnderlines();
  }).then(() => ping("listen-registered", 0))
    .catch((err) => ping("listen-error", 0, String(err)));
})();
