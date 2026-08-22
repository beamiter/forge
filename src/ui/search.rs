//! Debounced, bounded terminal-history search UI and result feedback.
use adw::prelude::*;
use gtk4::glib;
use libadwaita as adw;
use std::time::Duration;
use vte4::TerminalExt;

use crate::block_view::{FindNavigationResult, FindProgress, FindSearchResult};

use super::*;

const SEARCH_DEBOUNCE: Duration = Duration::from_millis(150);
const SEARCH_QUERY_BYTE_LIMIT: usize = 8 * 1024;

fn query_exceeds_byte_limit(query: &str) -> bool {
    query.len() > SEARCH_QUERY_BYTE_LIMIT
}

fn find_status_text(result: FindSearchResult) -> String {
    match result {
        FindSearchResult::NoMatches => "No matches".to_string(),
        FindSearchResult::InvalidRegex => "Invalid regex".to_string(),
        FindSearchResult::ScanLimit => "Search incomplete (scan limit)".to_string(),
        FindSearchResult::Matches(progress) => progress_status_text(progress),
    }
}

fn progress_status_text(progress: FindProgress) -> String {
    if progress.scan_limited {
        format!("{} of {}+ (scan limit)", progress.current, progress.total)
    } else if progress.capped {
        format!("{} of {}+", progress.current, progress.total)
    } else {
        format!("{} of {}", progress.current, progress.total)
    }
}

fn terminal_status_text(found: bool) -> &'static str {
    if found {
        // VTE exposes navigation success but not a total count. This is an
        // honest lower bound for classic/non-Block terminals.
        "1 of 1+"
    } else {
        "No matches"
    }
}

impl UiState {
    pub(crate) fn toggle_search(&self) {
        let visible = self.search_bar.is_search_mode();
        self.search_bar.set_search_mode(!visible);
        if !visible {
            self.search_entry.grab_focus();
            if !self.search_entry.text().is_empty() {
                self.schedule_search_apply();
            }
        } else {
            self.cancel_pending_search();
            // Clear search highlight when closing
            if let Some(term) = self.current_terminal() {
                term.search_set_regex(None::<&vte4::Regex>, 0);
            }
            if let Some(term_view) = self.current_term_view() {
                term_view.clear_find();
            }
            self.search_status.set_text("");
            self.focus_current_terminal();
        }
    }

    fn cancel_pending_search(&self) {
        self.search_generation
            .set(self.search_generation.get().wrapping_add(1));
        if let Some(source) = self.search_debounce_source.borrow_mut().take() {
            source.remove();
        }
    }

    pub(crate) fn schedule_search_apply(&self) {
        self.cancel_pending_search();
        if self.search_entry.text().is_empty() {
            self.apply_search_now();
            return;
        }

        self.search_status.set_text("Searching…");
        let generation = self.search_generation.get();
        let current_generation = self.search_generation.clone();
        let source_slot = self.search_debounce_source.clone();
        let ui = self.clone();
        let source = glib::timeout_add_local_once(SEARCH_DEBOUNCE, move || {
            if current_generation.get() == generation {
                source_slot.borrow_mut().take();
                ui.apply_search_now();
            }
        });
        *self.search_debounce_source.borrow_mut() = Some(source);
    }

    pub(crate) fn search_apply(&self) {
        self.cancel_pending_search();
        self.apply_search_now();
    }

    fn apply_search_now(&self) {
        let text = self.search_entry.text();
        if text.is_empty() {
            // `search_changed` also fires when the user deletes the query.  Clear
            // both search backends here; otherwise the previous highlights stay
            // painted until the search bar itself is closed.
            if let Some(term_view) = self.current_term_view() {
                term_view.clear_find();
            }
            if let Some(term) = self.current_terminal() {
                term.search_set_regex(None::<&vte4::Regex>, 0);
            }
            self.search_status.set_text("");
            return;
        }
        if query_exceeds_byte_limit(&text) {
            if let Some(term_view) = self.current_term_view() {
                term_view.clear_find();
            }
            if let Some(term) = self.current_terminal() {
                term.search_set_regex(None::<&vte4::Regex>, 0);
            }
            self.search_status.set_text(&format!(
                "Query too long ({} KiB limit)",
                SEARCH_QUERY_BYTE_LIMIT / 1024
            ));
            return;
        }

        // Detect regex pattern: /pattern/ syntax
        let text_str = text.as_str();
        let (query, use_regex) =
            if text_str.starts_with('/') && text_str.ends_with('/') && text_str.len() > 2 {
                (text_str[1..text_str.len() - 1].to_string(), true)
            } else {
                (text_str.to_string(), false)
            };

        // Block mode: highlight every in-text match and focus the first one
        // (Warp's FindWithinBlock). Next/Prev step through them.
        if let Some(term_view) = self.current_term_view() {
            let result = term_view.find_in_blocks(&query, use_regex);
            match result {
                FindSearchResult::Matches(_)
                | FindSearchResult::InvalidRegex
                | FindSearchResult::ScanLimit => {
                    self.search_status.set_text(&find_status_text(result));
                    return;
                }
                FindSearchResult::NoMatches => {}
            }
        }

        // Fall back to the live VTE for prompts/classic terminal panes. VTE can
        // report whether navigation found a hit, but it does not expose a count.
        if let Some(term) = self.current_terminal() {
            let pattern = if use_regex {
                query
            } else {
                regex::escape(&query)
            };
            match vte4::Regex::for_search(
                &pattern,
                pcre2_sys::PCRE2_CASELESS | pcre2_sys::PCRE2_MULTILINE,
            ) {
                Ok(regex) => {
                    term.search_set_regex(Some(&regex), 0);
                    term.search_set_wrap_around(true);
                    let found = term.search_find_next();
                    self.search_status.set_text(terminal_status_text(found));
                }
                Err(_) => self
                    .search_status
                    .set_text(&find_status_text(FindSearchResult::InvalidRegex)),
            }
        } else {
            self.search_status
                .set_text(&find_status_text(FindSearchResult::NoMatches));
        }
    }

    pub(crate) fn search_next(&self) {
        if self.search_debounce_source.borrow().is_some() {
            self.search_apply();
            return;
        }
        if let Some(term_view) = self.current_term_view() {
            match term_view.find_next() {
                FindNavigationResult::Progress(progress) => {
                    self.search_status.set_text(&progress_status_text(progress));
                    return;
                }
                FindNavigationResult::Invalidated => {
                    // The card set moved under the search: a pane resize, an
                    // Expand, a filter, or a block that was removed. Rebuild
                    // the pass from the query the entry still holds instead of
                    // reporting "No matches" for text that is still on screen.
                    self.search_apply();
                    return;
                }
                FindNavigationResult::Inactive => {}
            }
        }
        if let Some(term) = self.current_terminal() {
            self.search_status
                .set_text(terminal_status_text(term.search_find_next()));
        }
    }

    pub(crate) fn search_prev(&self) {
        if self.search_debounce_source.borrow().is_some() {
            self.search_apply();
            return;
        }
        if let Some(term_view) = self.current_term_view() {
            match term_view.find_prev() {
                FindNavigationResult::Progress(progress) => {
                    self.search_status.set_text(&progress_status_text(progress));
                    return;
                }
                FindNavigationResult::Invalidated => {
                    // See `search_next`: rebuild rather than claim no matches.
                    self.search_apply();
                    return;
                }
                FindNavigationResult::Inactive => {}
            }
        }
        if let Some(term) = self.current_terminal() {
            self.search_status
                .set_text(terminal_status_text(term.search_find_previous()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{find_status_text, query_exceeds_byte_limit, terminal_status_text};
    use crate::block_view::{FindProgress, FindSearchResult};

    #[test]
    fn search_status_distinguishes_empty_invalid_exact_and_capped_results() {
        assert_eq!(find_status_text(FindSearchResult::NoMatches), "No matches");
        assert_eq!(
            find_status_text(FindSearchResult::InvalidRegex),
            "Invalid regex"
        );
        assert_eq!(
            find_status_text(FindSearchResult::Matches(FindProgress {
                current: 3,
                total: 8,
                capped: false,
                scan_limited: false,
            })),
            "3 of 8"
        );
        assert_eq!(
            find_status_text(FindSearchResult::Matches(FindProgress {
                current: 7,
                total: 10_000,
                capped: true,
                scan_limited: false,
            })),
            "7 of 10000+"
        );
        assert_eq!(
            find_status_text(FindSearchResult::ScanLimit),
            "Search incomplete (scan limit)"
        );
        assert_eq!(
            find_status_text(FindSearchResult::Matches(FindProgress {
                current: 4,
                total: 27,
                capped: true,
                scan_limited: true,
            })),
            "4 of 27+ (scan limit)"
        );
    }

    #[test]
    fn classic_terminal_status_is_an_honest_lower_bound() {
        assert_eq!(terminal_status_text(true), "1 of 1+");
        assert_eq!(terminal_status_text(false), "No matches");
    }

    #[test]
    fn query_byte_limit_accepts_exact_and_rejects_one_over() {
        assert!(!query_exceeds_byte_limit(&"x".repeat(8 * 1024)));
        assert!(query_exceeds_byte_limit(&"x".repeat(8 * 1024 + 1)));
    }
}
