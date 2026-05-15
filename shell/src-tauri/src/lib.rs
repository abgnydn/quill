use std::sync::Mutex;

use harper_core::linting::{LintGroup, Linter, Suggestion};
use harper_core::parsers::PlainEnglish;
use harper_core::spell::FstDictionary;
use harper_core::{Dialect, Document};
use serde::Serialize;
use tauri::{Manager, State};

#[derive(Serialize)]
pub struct WireSuggestion {
    pub kind: &'static str,
    pub text: String,
}

#[derive(Serialize)]
pub struct WireLint {
    pub start: usize,
    pub end: usize,
    pub message: String,
    pub kind: String,
    pub priority: u8,
    pub suggestions: Vec<WireSuggestion>,
}

pub struct CheckerState {
    pub linter: Mutex<LintGroup>,
}

impl CheckerState {
    pub fn new() -> Self {
        let dict = FstDictionary::curated();
        let linter = LintGroup::new_curated(dict, Dialect::American);
        Self {
            linter: Mutex::new(linter),
        }
    }
}

impl Default for CheckerState {
    fn default() -> Self {
        Self::new()
    }
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

pub fn check_text_with(linter: &mut LintGroup, text: &str) -> Vec<WireLint> {
    let document = Document::new_curated(text, &PlainEnglish);
    wire_lints_from(linter.lint(&document))
}

#[tauri::command]
fn check(text: &str, state: State<'_, CheckerState>) -> Vec<WireLint> {
    let mut linter = state
        .linter
        .lock()
        .expect("checker mutex poisoned");
    check_text_with(&mut linter, text)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .setup(|app| {
            app.manage(CheckerState::new());
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![check])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh_linter() -> LintGroup {
        LintGroup::new_curated(FstDictionary::curated(), Dialect::American)
    }

    #[test]
    fn flags_obvious_grammar_error() {
        let mut linter = fresh_linter();
        let lints = check_text_with(&mut linter, "This is an test.");
        assert!(!lints.is_empty(), "Harper should flag 'an test'");
    }

    #[test]
    fn clean_text_returns_no_lints() {
        let mut linter = fresh_linter();
        let lints = check_text_with(&mut linter, "This is a perfectly normal sentence.");
        assert!(lints.is_empty(), "clean text should produce no lints");
    }
}

