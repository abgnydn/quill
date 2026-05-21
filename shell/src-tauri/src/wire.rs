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
    let document = Document::new_curated(text, &PlainEnglish);
    wire_lints_from(linter.lint(&document))
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
        };
        let s = serde_json::to_string(&c).unwrap();
        // JS-side reads exactly these field names; protect against drift.
        assert!(s.contains("\"llm_built\":true"));
        assert!(s.contains("\"model_loaded\":true"));
        assert!(
            s.contains("\"personal_adapter_loaded\":true"),
            "missing personal_adapter_loaded in wire payload: {s}"
        );
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
