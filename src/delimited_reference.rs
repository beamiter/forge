//! Recover the parenthesized destinations that a TUI may wrap before printing.
use std::ops::Range;
use std::sync::LazyLock;

const MAX_BYTES: usize = 4096;

pub(crate) struct Reference {
    pub target: String,
    pub web: bool,
    /// Reserve the visible region without authorizing a guessed target.
    pub blocked: bool,
    pub spans: Vec<(usize, Range<usize>)>,
}

pub(crate) fn detect(rows: &[(&str, bool)]) -> Vec<Reference> {
    static DESTINATION: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(
            r"\(((?:(?i:https?)://|(?:\.{0,2}|~)/|[\p{L}\p{N}_][\p{L}\p{N}\p{M}_.-]*/)[^()\r\n]*(?:\n[^()\r\n]+){0,7})\)",
        )
        .unwrap()
    });
    static FILE_END: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(
            r"(?i-u:\.(?:canvas\.tsx|tsx|jsx|mdx?|txt|rs|py|jsonc?|toml|ya?ml|css|html|svg|png|jpe?g|webp|ts|js|pdf|csv|sh))(?::[0-9]+){0,2}$",
        )
        .unwrap()
    });

    if !rows.iter().any(|(text, _)| text.contains('(')) {
        return Vec::new();
    }
    let mut text = String::new();
    let mut row_starts = Vec::with_capacity(rows.len());
    for (row, _) in rows {
        row_starts.push(text.len());
        text.push_str(row);
        text.push('\n');
    }

    let mut references = Vec::new();
    for matched in DESTINATION.captures_iter(&text) {
        let raw = matched.get(1).unwrap();
        if raw.len() > MAX_BYTES {
            continue;
        }
        let first_row = row_starts.partition_point(|&start| start <= raw.start()) - 1;
        let is_web = raw.as_str().split_once("://").is_some_and(|(scheme, _)| {
            scheme.eq_ignore_ascii_case("http") || scheme.eq_ignore_ascii_case("https")
        });
        // Row width cannot distinguish a hard newline from a soft wrap.
        // Retain exact text and reserve ambiguous regions without inventing a
        // different filesystem path, host name, or URL path by trimming.
        let spans = raw
            .as_str()
            .split('\n')
            .enumerate()
            .map(|(index, fragment)| {
                let row = first_row + index;
                let start = if index == 0 {
                    raw.start() - row_starts[row]
                } else {
                    0
                };
                (row, start..start + fragment.len())
            })
            .collect::<Vec<_>>();
        let target = raw.as_str().to_owned();
        let blocked = spans.len() > 1
            || if is_web {
                !jterm_core::link::is_openable_url(&target)
            } else {
                target.chars().any(char::is_control)
                    || target.contains("://")
                    || target.contains(['\\', '<', '>', '|'])
                    || !FILE_END.is_match(&target)
            };
        // Valid single-row URLs/tokens keep their native matchers. Spaced local
        // names and refused delimited regions need our exact span handling.
        if blocked || (!is_web && target.contains(char::is_whitespace)) {
            references.push(Reference {
                target,
                web: is_web,
                blocked,
                spans,
            });
        }
    }
    references
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn never_invents_authority_or_path_across_rows() {
        for full in [false, true] {
            for (head, tail) in [
                ("https://trusted. ", "evil.example/path"),
                ("https://trusted", ".evil.example/path"),
                ("https://example.org/a- ", " b"),
                ("https://example.org/", "https://other.example/a"),
            ] {
                let first = format!("({head}");
                let last = format!("{tail})");
                let found = detect(&[(&first, full), (&last, false)]);
                assert_eq!(found.len(), 1);
                assert!(found[0].web && found[0].blocked);
                assert_eq!(found[0].target, format!("{head}\n{tail}"));
                assert_eq!(found[0].spans[1], (1, 0..tail.len()));
            }
        }
        assert!(detect(&[("(https://example.org/path)", false)]).is_empty());
        assert!(detect(&[("(https://example.org/path )", false)])[0].blocked);
    }

    #[test]
    fn rejected_local_groups_still_block_partial_fallback() {
        for full in [false, true] {
            for tail in ["sub/file.bin)", "notes.md )", "sub/file.bin )"] {
                let refs = detect(&[("(/tmp/root-", full), (tail, false)]);
                assert_eq!(refs.len(), 1);
                assert!(refs[0].blocked);
                assert_eq!(refs[0].spans[1], (1, 0..tail.len() - 1));
            }
        }
    }

    #[test]
    fn joins_delimited_paths_with_wide_text_and_real_spaces() {
        let rows = [
            ("路线图 (/tmp/中 文/PMG-SOTA-", false),
            (
                "    roadmap.canvas.tsx) · 报告 (reports/接触 优化.md)",
                false,
            ),
        ];
        let references = detect(&rows);
        assert_eq!(references.len(), 2);
        assert_eq!(
            references[0].target,
            "/tmp/中 文/PMG-SOTA-\n    roadmap.canvas.tsx"
        );
        assert!(references[0].blocked);
        assert_eq!(references[0].spans[1], (1, 0..22));
        assert!(!references[1].blocked);
        assert_eq!(references[1].target, "reports/接触 优化.md");
    }

    #[test]
    fn joins_only_with_a_supported_boundary_and_delimiter() {
        assert!(detect(&[("(/tmp/first", false), ("second.md)", false)])[0].blocked);
        assert!(detect(&[("(/tmp/first-", false), ("second.md", false)]).is_empty());
        let rejected = detect(&[("(https://user@host/a-", false), ("b)", false)]);
        assert!(!jterm_core::link::is_openable_url(&rejected[0].target));
        let links = detect(&[("(https://example.org/one-", false), ("two)", false)]);
        assert!(links[0].web);
        assert!(links[0].blocked);
        assert_eq!(links[0].target, "https://example.org/one-\ntwo");
    }
}
