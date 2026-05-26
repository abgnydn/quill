//! Decide whether the currently-focused element is a place Quill should
//! engage. Mirrors Grammarly's mental model: real prose surfaces only —
//! no terminals, URL bars, password fields, search bars, code editors,
//! or system launchers.
//!
//! The policy is a pure function of (role, subrole, bundle_id), tested
//! exhaustively below. The focus tracker calls it on every poll; if it
//! returns false the snapshot is dropped before any lint work runs.

#![cfg(all(target_os = "macos", feature = "overlay"))]

/// Apps where Quill should never engage. Terminals, code IDEs, launchers,
/// chat-search bars, etc. — same surfaces Grammarly stays out of. A bundle
/// prefix in `DENIED_BUNDLE_PREFIXES` also blocks (JetBrains family).
pub const DENIED_BUNDLES: &[&str] = &[
    // Terminals
    "com.apple.Terminal",
    "com.googlecode.iterm2",
    "com.github.wez.wezterm",
    "com.mitchellh.ghostty",
    "dev.warp.Warp-Stable",
    "io.alacritty",
    "net.kovidgoyal.kitty",
    "com.zeit.hyper",
    // Launchers / search bars
    "com.raycast.macos",
    "com.apple.Spotlight",
    "com.runningwithcrayons.Alfred",
    // Code IDEs — predominantly source code, not prose
    "com.apple.dt.Xcode",
    "com.microsoft.VSCode",
    "com.microsoft.VSCodeInsiders",
    "com.todesktop.230313mzl4w4u92",  // Cursor
    "com.sublimetext.4",
    "com.sublimetext.3",
    "io.zed.dev",
    "dev.zed.Zed",
    // System utilities where text fields are tiny config inputs
    "com.apple.systempreferences",
    "com.apple.SystemSettings",
];

/// JetBrains ships dozens of products under com.jetbrains.* — block them
/// all by prefix rather than maintaining an exact list.
pub const DENIED_BUNDLE_PREFIXES: &[&str] = &[
    "com.jetbrains.",
];

/// Browsers expose URL bars and tons of incidental single-line `<input>`
/// elements as `AXTextField`. None of those are prose. In any browser
/// bundle we engage only on `AXTextArea` (where `<textarea>` and most
/// contenteditable rich-text composers — Gmail, Twitter, LinkedIn, etc. —
/// surface), and skip `AXTextField` outright. The handful of `<input>`
/// fields that *are* prose-worthy (rare) lose linting; the tradeoff is
/// 100% URL bar / search bar / address field suppression in every browser.
pub const BROWSER_BUNDLES: &[&str] = &[
    "com.apple.Safari",
    "com.apple.SafariTechnologyPreview",
    "com.google.Chrome",
    "com.google.Chrome.canary",
    "com.brave.Browser",
    "com.brave.Browser.beta",
    "com.brave.Browser.nightly",
    "com.microsoft.edgemac",
    "org.mozilla.firefox",
    "org.mozilla.firefoxdeveloperedition",
    "company.thebrowser.Browser",   // Arc
    "com.vivaldi.Vivaldi",
    "com.operasoftware.Opera",
    "ru.yandex.desktop.yandex-browser",
    "com.theduckduckgo.macos.browser",
];

/// Substrings (case-insensitive) in `AXRoleDescription` that signal a URL
/// bar / search box even when role+subrole don't. Chrome/Brave/Edge tag
/// the omnibox as "address and search field"; Safari uses similar text.
const ROLE_DESC_DENY_SUBSTRINGS: &[&str] = &[
    "address and search",
    "address bar",
    "url field",
    "search field",
    "search bar",
];

/// Pure decision: should the overlay engage on this focused element?
///
/// Rules, in order:
///   1. Bundle in [`DENIED_BUNDLES`] or matching a [`DENIED_BUNDLE_PREFIXES`] → no.
///   2. Subrole is `AXSearchField` (URL bars, in-app search) → no.
///   3. Subrole or role is `AXSecureTextField` (password) → no.
///   4. `role_description` contains a URL/search marker → no.
///   5. Role is `AXTextArea` or `AXTextField` → yes.
///   6. Anything else (buttons, lists, unknown) → no.
pub fn is_engageable(
    role: Option<&str>,
    subrole: Option<&str>,
    role_description: Option<&str>,
    bundle_id: Option<&str>,
) -> bool {
    if let Some(bid) = bundle_id {
        if DENIED_BUNDLES.iter().any(|d| *d == bid) {
            return false;
        }
        if DENIED_BUNDLE_PREFIXES.iter().any(|p| bid.starts_with(p)) {
            return false;
        }
    }

    if let Some(sr) = subrole {
        if sr == "AXSearchField" || sr == "AXSecureTextField" {
            return false;
        }
    }

    if let Some(desc) = role_description {
        let lower = desc.to_lowercase();
        if ROLE_DESC_DENY_SUBSTRINGS.iter().any(|s| lower.contains(s)) {
            return false;
        }
    }

    // Browser-specific rule: AXTextField in any browser is almost always
    // a URL bar, search overlay, or incidental web form input — none worth
    // linting. AXTextArea (real <textarea> + contenteditable composers)
    // still engages.
    if let Some(bid) = bundle_id {
        if BROWSER_BUNDLES.iter().any(|b| *b == bid) && role == Some("AXTextField") {
            return false;
        }
    }

    match role {
        Some("AXSecureTextField") => false,
        Some("AXTextArea") | Some("AXTextField") => true,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn engage(role: &str, bundle: &str) -> bool {
        is_engageable(Some(role), None, None, Some(bundle))
    }

    #[test]
    fn allows_textarea_in_prose_apps() {
        for bid in [
            "com.apple.Notes",
            "com.apple.mail",
            "com.tinyspeck.slackmacgap",
            "com.hnc.Discord",
            "notion.id",
            "md.obsidian",
        ] {
            assert!(engage("AXTextArea", bid), "should engage in {bid}");
        }
    }

    #[test]
    fn allows_textfield_when_subrole_safe() {
        assert!(engage("AXTextField", "com.apple.Notes"));
        assert!(is_engageable(Some("AXTextField"), None, None, None));
    }

    #[test]
    fn denies_safari_url_bar_via_subrole() {
        assert!(!is_engageable(Some("AXTextField"), Some("AXSearchField"), None, Some("com.apple.Safari")));
        assert!(!is_engageable(Some("AXTextField"), Some("AXSearchField"), None, Some("com.google.Chrome")));
    }

    #[test]
    fn denies_url_bar_in_every_browser_via_bundle_rule() {
        // Even with empty role_description / no subrole signal — the
        // browser-bundle + AXTextField combo is enough to block.
        for bid in [
            "com.apple.Safari",
            "com.google.Chrome",
            "com.brave.Browser",
            "com.microsoft.edgemac",
            "org.mozilla.firefox",
            "company.thebrowser.Browser",
            "com.vivaldi.Vivaldi",
        ] {
            assert!(
                !is_engageable(Some("AXTextField"), None, None, Some(bid)),
                "url bar leaked in {bid}"
            );
        }
    }

    #[test]
    fn still_engages_on_textarea_in_browsers() {
        // Gmail / Twitter / LinkedIn / Reddit composers surface as
        // AXTextArea via contenteditable. Those must keep linting.
        for bid in [
            "com.apple.Safari",
            "com.google.Chrome",
            "com.brave.Browser",
        ] {
            assert!(
                is_engageable(Some("AXTextArea"), None, None, Some(bid)),
                "composer skipped in {bid}"
            );
        }
    }

    #[test]
    fn denies_brave_url_bar_via_role_description() {
        // Brave/Chrome omnibox: role=AXTextField, subrole=None, but
        // role_description="address and search field".
        assert!(!is_engageable(
            Some("AXTextField"),
            None,
            Some("address and search field"),
            Some("com.brave.Browser"),
        ));
        // Capitalization shouldn't matter.
        assert!(!is_engageable(
            Some("AXTextField"),
            None,
            Some("Address and Search Field"),
            Some("com.google.Chrome"),
        ));
        // Variants we should still catch.
        for desc in ["address bar", "URL field", "Search bar"] {
            assert!(
                !is_engageable(Some("AXTextField"), None, Some(desc), Some("com.brave.Browser")),
                "role_desc leaked: {desc}"
            );
        }
    }

    #[test]
    fn allows_real_text_field_with_innocuous_role_description() {
        // A normal Notes/Mail field has role_description "text field" or
        // similar — must NOT be caught by the URL-bar substring match.
        assert!(is_engageable(
            Some("AXTextField"),
            None,
            Some("text field"),
            Some("com.apple.Notes"),
        ));
        assert!(is_engageable(
            Some("AXTextArea"),
            None,
            Some("text entry area"),
            Some("com.apple.mail"),
        ));
    }

    #[test]
    fn denies_password_fields() {
        assert!(!is_engageable(Some("AXTextField"), Some("AXSecureTextField"), None, Some("com.apple.Safari")));
        assert!(!is_engageable(Some("AXSecureTextField"), None, None, Some("com.apple.Safari")));
    }

    #[test]
    fn denies_terminal_apps_even_with_textarea_role() {
        for bid in [
            "com.apple.Terminal",
            "com.googlecode.iterm2",
            "dev.warp.Warp-Stable",
            "com.mitchellh.ghostty",
            "com.github.wez.wezterm",
            "io.alacritty",
            "net.kovidgoyal.kitty",
        ] {
            assert!(!engage("AXTextArea", bid), "terminal leaked: {bid}");
        }
    }

    #[test]
    fn denies_jetbrains_family_by_prefix() {
        assert!(!engage("AXTextArea", "com.jetbrains.intellij"));
        assert!(!engage("AXTextArea", "com.jetbrains.pycharm"));
        assert!(!engage("AXTextArea", "com.jetbrains.WebStorm"));
    }

    #[test]
    fn denies_code_editors() {
        assert!(!engage("AXTextArea", "com.apple.dt.Xcode"));
        assert!(!engage("AXTextArea", "com.microsoft.VSCode"));
        assert!(!engage("AXTextArea", "com.todesktop.230313mzl4w4u92"));
        assert!(!engage("AXTextArea", "dev.zed.Zed"));
    }

    #[test]
    fn denies_launchers_and_search() {
        assert!(!engage("AXTextField", "com.raycast.macos"));
        assert!(!engage("AXTextField", "com.runningwithcrayons.Alfred"));
        assert!(!engage("AXTextField", "com.apple.Spotlight"));
    }

    #[test]
    fn denies_non_text_roles() {
        assert!(!engage("AXButton", "com.apple.Notes"));
        assert!(!engage("AXList", "com.apple.Notes"));
        assert!(!engage("AXImage", "com.apple.Notes"));
        assert!(!is_engageable(None, None, None, Some("com.apple.Notes")));
    }

    #[test]
    fn denies_system_settings() {
        assert!(!engage("AXTextField", "com.apple.systempreferences"));
        assert!(!engage("AXTextField", "com.apple.SystemSettings"));
    }
}
