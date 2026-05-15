use harper_core::linting::{LintGroup, Linter, Suggestion};
use harper_core::parsers::PlainEnglish;
use harper_core::spell::FstDictionary;
use harper_core::{Dialect, Document};
use serde::Serialize;

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

pub fn check_text(text: &str) -> Vec<WireLint> {
    let document = Document::new_curated(text, &PlainEnglish);
    let dict = FstDictionary::curated();
    let mut linter = LintGroup::new_curated(dict, Dialect::American);
    let lints = linter.lint(&document);

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

#[tauri::command]
fn check(text: &str) -> Vec<WireLint> {
    check_text(text)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![check])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flags_obvious_grammar_error() {
        let lints = check_text("This is an test.");
        assert!(
            !lints.is_empty(),
            "Harper should flag 'an test' as a lint"
        );
    }

    #[test]
    fn returns_replacement_suggestions() {
        let lints = check_text("I has a apple.");
        assert!(!lints.is_empty(), "expected lints for ungrammatical sentence");
        let any_replacement = lints
            .iter()
            .any(|l| l.suggestions.iter().any(|s| s.kind == "replace"));
        assert!(any_replacement, "expected at least one replace suggestion");
    }

    #[test]
    fn clean_text_returns_no_lints() {
        let lints = check_text("This is a perfectly normal sentence.");
        assert!(
            lints.is_empty(),
            "clean text should produce no lints, got: {:?}",
            lints.iter().map(|l| &l.message).collect::<Vec<_>>()
        );
    }
}
