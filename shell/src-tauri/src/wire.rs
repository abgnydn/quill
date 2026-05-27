//! Types crossing the Tauri IPC boundary (Rust ↔ JS).
//!
//! Kept in one file so the wire schema stays cohesive and obvious — any
//! field added here is automatically a contract with the overlay frontend.

use serde::Serialize;

use harper_core::Document;
use harper_core::linting::{LintGroup, Linter, Suggestion};
use harper_core::parsers::PlainEnglish;

#[derive(Serialize, Clone, Debug)]
pub struct WireSuggestion {
    pub kind: &'static str,
    pub text: String,
}

#[derive(Serialize, Clone, Debug)]
pub struct WireLint {
    pub start: usize,
    pub end: usize,
    pub message: String,
    pub kind: String,
    pub priority: u8,
    pub suggestions: Vec<WireSuggestion>,
}

#[derive(Serialize)]
pub struct Capabilities {
    pub llm_built: bool,
    pub model_loaded: bool,
    /// True when a personal LoRA adapter is loaded on top of the base.
    pub personal_adapter_loaded: bool,
    /// True when QVAC Fabric binaries (BitNet inference + on-device LoRA
    /// training) are bundled in the .app and runnable.
    pub qvac_available: bool,
    /// Build-version string from `llama-cli --version` when available.
    pub qvac_version: Option<String>,
}

fn wire_lints_from<I: IntoIterator<Item = harper_core::linting::Lint>>(lints: I) -> Vec<WireLint> {
    lints
        .into_iter()
        .map(|l| WireLint {
            start: l.span.start,
            end: l.span.end,
            message: l.message,
            kind: format!("{:?}", l.lint_kind),
            priority: l.priority,
            suggestions: l
                .suggestions
                .into_iter()
                .map(|s| match s {
                    Suggestion::ReplaceWith(chars) => WireSuggestion {
                        kind: "replace",
                        text: chars.iter().collect(),
                    },
                    Suggestion::InsertAfter(chars) => WireSuggestion {
                        kind: "insert_after",
                        text: chars.iter().collect(),
                    },
                    Suggestion::Remove => WireSuggestion {
                        kind: "remove",
                        text: String::new(),
                    },
                })
                .collect(),
        })
        .collect()
}

/// Run Harper on `text` using the provided `linter`, returning lints in the
/// IPC wire format. Single source of truth — main window's `check` command
/// and the overlay focus tracker both go through here.
pub fn check_text_with(linter: &mut LintGroup, text: &str) -> Vec<WireLint> {
    check_text_filtered(linter, text, &[])
}

/// Same as [`check_text_with`] but drops any lint whose underlying text span
/// (case-insensitive) matches a word in `ignored`. Used to honor the user's
/// personal dictionary — names, jargon, codenames that Harper would
/// otherwise flag as spelling errors.
pub fn check_text_filtered(
    linter: &mut LintGroup,
    text: &str,
    ignored: &[String],
) -> Vec<WireLint> {
    let document = Document::new_curated(text, &PlainEnglish);
    let mut out = wire_lints_from(linter.lint(&document));
    if !ignored.is_empty() {
        let ignored_lower: Vec<String> = ignored.iter().map(|w| w.to_lowercase()).collect();
        out.retain(|lint| {
            let span = match text.get(lint.start..lint.end) {
                Some(s) => s.to_lowercase(),
                None => return true,
            };
            !ignored_lower.iter().any(|w| *w == span)
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capabilities_serializes_personal_field() {
        let c = Capabilities {
            llm_built: true,
            model_loaded: true,
            personal_adapter_loaded: true,
            qvac_available: true,
            qvac_version: Some("b1-3daef61".into()),
        };
        let s = serde_json::to_string(&c).unwrap();
        // JS-side reads exactly these field names; protect against drift.
        for f in [
            "llm_built", "model_loaded", "personal_adapter_loaded",
            "qvac_available", "qvac_version",
        ] {
            assert!(s.contains(&format!("\"{f}\":")), "missing {f} in {s}");
        }
    }

    #[test]
    fn ignored_words_filter_drops_matching_spans() {
        use harper_core::Dialect;
        use harper_core::spell::FstDictionary;
        let mut linter = LintGroup::new_curated(FstDictionary::curated(), Dialect::American);
        let text = "BitNet is fast";
        let unfiltered = check_text_with(&mut linter, text);
        let bitnet_flagged = unfiltered.iter().any(|l| {
            text.get(l.start..l.end).map(|s| s.eq_ignore_ascii_case("bitnet")).unwrap_or(false)
        });
        assert!(bitnet_flagged, "expected Harper to flag 'BitNet' as a spelling error");
        let filtered = check_text_filtered(&mut linter, text, &["bitnet".to_string()]);
        let still_flagged = filtered.iter().any(|l| {
            text.get(l.start..l.end).map(|s| s.eq_ignore_ascii_case("bitnet")).unwrap_or(false)
        });
        assert!(!still_flagged, "expected 'BitNet' lint to be filtered out");
    }

    #[test]
    fn empty_ignored_list_changes_nothing() {
        use harper_core::Dialect;
        use harper_core::spell::FstDictionary;
        let mut linter = LintGroup::new_curated(FstDictionary::curated(), Dialect::American);
        let text = "I has a apple.";
        let a = check_text_with(&mut linter, text);
        let b = check_text_filtered(&mut linter, text, &[]);
        assert_eq!(a.len(), b.len());
    }

    #[test]
    fn wire_lint_round_trip() {
        let lint = WireLint {
            start: 2,
            end: 5,
            message: "verb form".into(),
            kind: "Agreement".into(),
            priority: 10,
            suggestions: vec![WireSuggestion {
                kind: "replace",
                text: "have".into(),
            }],
        };
        let s = serde_json::to_string(&lint).unwrap();
        // Field-name contract for the JS overlay.
        for f in ["start", "end", "message", "kind", "priority", "suggestions"] {
            assert!(s.contains(&format!("\"{f}\":")), "missing {f}");
        }
    }
}
