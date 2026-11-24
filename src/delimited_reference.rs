mod tests {
    use jterm_core::link::{detect_delimited_references, DelimitedReference};

    fn detect(rows: &[(&str, bool)]) -> Vec<DelimitedReference> {
        let lines: Vec<_> = rows.iter().map(|&(line, _)| line).collect();
        detect_delimited_references(&lines)
    }

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
