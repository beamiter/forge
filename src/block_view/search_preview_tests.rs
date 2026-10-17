//! Headless regression coverage for the exact retained-text preview model.
use super::*;

fn snapshot_with(fields: &[(u64, Field, &str)]) -> Snapshot {
    let mut snapshot = Snapshot::new().expect("snapshot id");
    for &(record, field, text) in fields {
        snapshot
            .push(record, field, text, false)
            .expect("bounded fixture");
    }
    snapshot
}

fn hits(snapshot: &Snapshot, query: &str, regex: bool) -> Vec<Hit> {
    search(snapshot, query, regex, || false)
        .expect("complete search")
        .hits
}

fn assert_error(snapshot: &Snapshot, query: &str, regex: bool, expected: PreviewError) {
    assert_eq!(
        search(snapshot, query, regex, || false).unwrap_err(),
        expected
    );
}

#[test]
fn repeated_same_line_hits_navigate_in_both_directions_and_wrap() {
    let snapshot = snapshot_with(&[(7, Field::Output, "hit hit hit")]);
    let found = hits(&snapshot, "hit", false);
    assert_eq!(
        found,
        vec![
            Hit {
                document: 0,
                bytes: 0..3,
                chars: 0..3
            },
            Hit {
                document: 0,
                bytes: 4..7,
                chars: 4..7
            },
            Hit {
                document: 0,
                bytes: 8..11,
                chars: 8..11
            },
        ]
    );
    assert_eq!(step(0, 3, false), Some(1));
    assert_eq!(step(1, 3, false), Some(2));
    assert_eq!(step(2, 3, false), Some(0));
    assert_eq!(step(0, 3, true), Some(2));
    assert_eq!(step(2, 3, true), Some(1));
    assert_eq!(step(1, 3, true), Some(0));
    assert_eq!(step(0, 0, false), None);
    assert_eq!(step(3, 3, true), None);
    assert_eq!(step(0, 1, true), Some(0));
    assert_eq!(step(0, 1, false), Some(0));
}

#[test]
fn unicode_ranges_count_bytes_and_scalars_including_combining_marks() {
    let snapshot = snapshot_with(&[(1, Field::Output, "中🙂e\u{301} 中🙂e\u{301}\n🙂中e\u{301}")]);
    assert_eq!(
        hits(&snapshot, "中🙂e\u{301}", false),
        vec![
            Hit {
                document: 0,
                bytes: 0..10,
                chars: 0..4
            },
            Hit {
                document: 0,
                bytes: 11..21,
                chars: 5..9
            },
        ]
    );
    assert_eq!(
        hits(&snapshot, "🙂", false),
        vec![
            Hit {
                document: 0,
                bytes: 3..7,
                chars: 1..2
            },
            Hit {
                document: 0,
                bytes: 14..18,
                chars: 6..7
            },
            Hit {
                document: 0,
                bytes: 22..26,
                chars: 10..11
            },
        ]
    );
    assert_eq!(
        hits(&snapshot, "e\u{301}", false),
        vec![
            Hit {
                document: 0,
                bytes: 7..10,
                chars: 2..4
            },
            Hit {
                document: 0,
                bytes: 18..21,
                chars: 7..9
            },
            Hit {
                document: 0,
                bytes: 29..32,
                chars: 12..14
            },
        ]
    );
    for found in hits(&snapshot, "中|🙂|e\u{301}", true) {
        let text = &snapshot.documents[found.document].text;
        assert!(text.is_char_boundary(found.bytes.start));
        assert!(text.is_char_boundary(found.bytes.end));
        assert_eq!(
            text[..found.bytes.start].chars().count(),
            found.chars.start as usize
        );
        assert_eq!(
            text[..found.bytes.end].chars().count(),
            found.chars.end as usize
        );
    }
    // The model searches exact displayed scalars; it does not normalize accents.
    assert!(hits(&snapshot, "é", false).is_empty());
}

#[test]
fn hard_newlines_cannot_be_crossed_by_literal_whitespace_or_negated_classes() {
    let snapshot = snapshot_with(&[(1, Field::Output, "a\nb\na\tb\n\n中\n🙂\n")]);
    assert!(hits(&snapshot, "a\nb", false).is_empty());
    assert!(hits(&snapshot, r"a\nb", true).is_empty());
    assert_eq!(
        hits(&snapshot, r"a\sb", true),
        vec![Hit {
            document: 0,
            bytes: 4..7,
            chars: 4..7
        },]
    );
    assert!(hits(&snapshot, r"中[^x]🙂", true).is_empty());
    assert_eq!(
        hits(&snapshot, r"(?s)a.b", true),
        vec![Hit {
            document: 0,
            bytes: 4..7,
            chars: 4..7
        },]
    );
    for found in hits(&snapshot, "[^x]+", true) {
        assert!(!snapshot.documents[0].text[found.bytes].contains('\n'));
    }
    assert_eq!(hits(&snapshot, r"\s", true).len(), 1);
}

#[test]
fn consuming_anchors_work_but_empty_capable_patterns_fail_closed() {
    let snapshot = snapshot_with(&[(1, Field::Command, "foo\nfoo\nfood\n\n")]);
    assert_eq!(
        hits(&snapshot, "^foo$", true),
        vec![
            Hit {
                document: 0,
                bytes: 0..3,
                chars: 0..3
            },
            Hit {
                document: 0,
                bytes: 4..7,
                chars: 4..7
            },
        ]
    );
    assert_eq!(hits(&snapshot, "f?oo", true).len(), 3);
    for query in ["^", "$", r"\b", "a?", "a*", "(?:foo)?", "foo|", "^$"] {
        assert_error(&snapshot, query, true, PreviewError::ZeroWidth);
    }
    assert!(hits(&snapshot, "", true).is_empty());
    assert!(hits(&snapshot, "", false).is_empty());
}

#[test]
fn unsupported_regex_constructs_are_rejected_and_literals_are_escaped() {
    let snapshot = snapshot_with(&[(1, Field::Output, "a.*b (a) aa")]);
    for query in ["(?=a)", "(?!a)", "(?<=a)b", "(?<!a)b", r"(a)\1", "["] {
        assert_error(&snapshot, query, true, PreviewError::InvalidRegex);
    }
    assert_eq!(
        hits(&snapshot, "a.*b", false),
        vec![Hit {
            document: 0,
            bytes: 0..4,
            chars: 0..4
        }]
    );
}

#[test]
fn literal_and_regex_search_are_case_insensitive() {
    let snapshot = snapshot_with(&[(1, Field::Output, "FoO foo FOO Ä ä")]);
    assert_eq!(hits(&snapshot, "foo", false).len(), 3);
    assert_eq!(hits(&snapshot, "f[o]+", true).len(), 3);
    assert_eq!(hits(&snapshot, "ä", false).len(), 2);
}

#[test]
fn fields_and_records_never_form_a_cross_boundary_match() {
    let snapshot = snapshot_with(&[
        (1, Field::Prompt, "ab"),
        (1, Field::Command, "cd"),
        (1, Field::Output, "ab"),
        (2, Field::Output, "cd"),
    ]);
    assert!(hits(&snapshot, "abcd", false).is_empty());
    assert!(hits(&snapshot, "ab.*cd", true).is_empty());
    assert_eq!(
        hits(&snapshot, "ab", false),
        vec![
            Hit {
                document: 0,
                bytes: 0..2,
                chars: 0..2
            },
            Hit {
                document: 2,
                bytes: 0..2,
                chars: 0..2
            },
        ]
    );
    assert_eq!(snapshot.documents[0].record_id, 1);
    assert_eq!(snapshot.documents[0].field, Field::Prompt);
    assert_eq!(snapshot.documents[3].record_id, 2);
}

#[test]
fn controls_and_bidi_are_visible_and_searched_as_displayed() {
    let mut snapshot = Snapshot::new().unwrap();
    snapshot
        .push(4, Field::Output, "A\0\r\u{1b}\u{202e}\t\nB", true)
        .unwrap();
    let text = &snapshot.documents[0].text;
    assert_eq!(text, "A\\u{0}\\u{d}\\u{1b}\\u{202e}\t\nB");
    assert!(snapshot.documents[0].retained_only);
    assert!(hits(&snapshot, "\0", false).is_empty());
    assert_eq!(
        hits(&snapshot, r"\u{0}", false),
        vec![Hit {
            document: 0,
            bytes: 1..6,
            chars: 1..6
        }]
    );
    assert_eq!(snapshot.display_bytes, text.len());
}

#[test]
fn raw_text_limit_is_aggregate_and_exact_boundary_is_allowed() {
    let mut snapshot = Snapshot::new().unwrap();
    snapshot
        .push(1, Field::Command, &"a".repeat(TEXT_LIMIT / 2), false)
        .unwrap();
    snapshot
        .push(1, Field::Output, &"b".repeat(TEXT_LIMIT / 2), true)
        .unwrap();
    assert_eq!(snapshot.raw_bytes, TEXT_LIMIT);
    assert_eq!(snapshot.display_bytes, TEXT_LIMIT);
    assert_eq!(
        snapshot.push(2, Field::Output, "c", false),
        Err(PreviewError::TextLimit)
    );
    assert_eq!(snapshot.documents.len(), 2);
    let mut unicode = Snapshot::new().unwrap();
    assert_eq!(
        unicode.push(1, Field::Output, &"🙂".repeat(TEXT_LIMIT / 4 + 1), false),
        Err(PreviewError::TextLimit)
    );
    assert!(unicode.documents.is_empty());
}

#[test]
fn escaped_display_limit_is_independent_of_raw_limit() {
    let mut snapshot = Snapshot::new().unwrap();
    let source = format!(
        "{}{}",
        "\0".repeat(TEXT_LIMIT / 5),
        "a".repeat(TEXT_LIMIT % 5)
    );
    assert!(source.len() < TEXT_LIMIT);
    snapshot.push(1, Field::Output, &source, false).unwrap();
    assert_eq!(snapshot.display_bytes, TEXT_LIMIT);
    assert_eq!(snapshot.raw_bytes, source.len());
    assert_eq!(
        snapshot.push(2, Field::Output, "a", false),
        Err(PreviewError::TextLimit)
    );
    let mut overflow = Snapshot::new().unwrap();
    assert_eq!(
        overflow.push(1, Field::Output, &"\0".repeat(TEXT_LIMIT / 5 + 1), false),
        Err(PreviewError::TextLimit)
    );
    assert!(overflow.documents.is_empty());
}

#[test]
fn field_count_and_query_byte_limits_fail_closed() {
    let mut snapshot = Snapshot::new().unwrap();
    for i in 0..DOCUMENT_LIMIT {
        snapshot.push(i as u64, Field::Output, "", false).unwrap();
    }
    assert_eq!(
        snapshot.push(0, Field::Output, "", false),
        Err(PreviewError::TextLimit)
    );
    assert_error(
        &snapshot,
        &"a".repeat(QUERY_LIMIT + 1),
        false,
        PreviewError::QueryLimit,
    );
    assert_error(
        &snapshot,
        &"🙂".repeat(QUERY_LIMIT / 4 + 1),
        true,
        PreviewError::QueryLimit,
    );
}

#[test]
fn exactly_hit_limit_is_complete_and_one_extra_discards_all_hits() {
    let exact = snapshot_with(&[(1, Field::Output, &"a".repeat(HIT_LIMIT))]);
    assert_eq!(hits(&exact, "a", false).len(), HIT_LIMIT);
    let excess = snapshot_with(&[(1, Field::Output, &"a".repeat(HIT_LIMIT + 1))]);
    assert_error(&excess, "a", false, PreviewError::MatchLimit);
}

#[test]
fn cancellation_discards_partial_results_and_time_limit_is_an_error() {
    let snapshot = snapshot_with(&[(1, Field::Output, "a a a a a a")]);
    assert_eq!(
        search(&snapshot, "a", false, || true).unwrap_err(),
        PreviewError::Cancelled
    );
    let mut checks = 0;
    assert_eq!(
        search(&snapshot, "a", false, || {
            checks += 1;
            checks >= 5
        })
        .unwrap_err(),
        PreviewError::Cancelled
    );
    assert_eq!(checks, 5);
    assert_eq!(
        search(&snapshot, "a", false, || {
            std::thread::sleep(SCAN_TIME + Duration::from_millis(10));
            false
        })
        .unwrap_err(),
        PreviewError::TimeLimit
    );
}

#[test]
fn stale_reply_requires_snapshot_generation_and_live_owner() {
    let first = Snapshot::new().unwrap();
    let second = Snapshot::new().unwrap();
    assert_ne!(first.id, second.id);
    let reply = Reply {
        snapshot_id: first.id,
        generation: 8,
        result: Ok(Matches::default()),
    };
    assert!(reply_is_current(&reply, first.id, 8, true));
    assert!(!reply_is_current(&reply, second.id, 8, true));
    assert!(!reply_is_current(&reply, first.id, 7, true));
    assert!(!reply_is_current(&reply, first.id, 8, false));
}

fn wait_until(mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !condition() {
        assert!(Instant::now() < deadline, "headless worker did not settle");
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn one_worker_keeps_latest_query_and_stops_without_gui() {
    let snapshot = Arc::new(snapshot_with(&[(1, Field::Output, "first latest latest")]));
    let worker = Worker::new(snapshot.clone()).unwrap();
    let worker_generation = worker.generation.clone();
    let mut final_generation = 0;
    for index in 0..1000 {
        final_generation = worker
            .submit(if index == 999 { "latest" } else { "first" }, false)
            .unwrap();
    }
    // The worker and producer share one latest-request slot, not a query queue.
    if let Some(request) = worker.request.lock().unwrap().as_ref() {
        assert_eq!(request.generation, final_generation);
        assert_eq!(request.query, "latest");
    }
    let mut result = None;
    wait_until(|| {
        if let Some(reply) = worker.take_reply() {
            if reply_is_current(&reply, snapshot.id, final_generation, true) {
                result = Some(reply.result);
                return true;
            }
        }
        false
    });
    assert_eq!(
        result.unwrap().unwrap().hits,
        vec![
            Hit {
                document: 0,
                bytes: 6..12,
                chars: 6..12
            },
            Hit {
                document: 0,
                bytes: 13..19,
                chars: 13..19
            },
        ]
    );
    assert!(worker.take_reply().is_none());
    worker.stop();
    assert_eq!(
        worker.submit("first", false),
        Err(PreviewError::WorkerUnavailable)
    );
    // Once the worker exits, only this test and the Worker own generation.
    wait_until(|| Arc::strong_count(&worker_generation) == 2);
    assert!(worker.take_reply().is_none());
}

#[test]
fn invalid_latest_request_invalidates_prior_result_and_drop_stops_worker() {
    let worker = Worker::new(Arc::new(snapshot_with(&[(1, Field::Output, "a")]))).unwrap();
    let first = worker.submit("a", false).unwrap();
    wait_until(|| worker.reply.lock().unwrap().is_some());
    assert_eq!(
        worker.submit(&"a".repeat(QUERY_LIMIT + 1), false),
        Err(PreviewError::QueryLimit)
    );
    assert!(worker.generation.load(Ordering::Acquire) > first);
    assert!(worker.take_reply().is_none());
    assert!(worker.request.lock().unwrap().is_none());
    let stopped = worker.stopped.clone();
    drop(worker);
    assert!(stopped.load(Ordering::Acquire));
    wait_until(|| Arc::strong_count(&stopped) == 1);
}

#[test]
fn failed_snapshot_construction_cannot_publish_a_partial_total() {
    let mut snapshot = snapshot_with(&[(1, Field::Output, "hit")]);
    assert_eq!(
        snapshot.push(2, Field::Output, &"x".repeat(TEXT_LIMIT), false),
        Err(PreviewError::TextLimit)
    );
    assert_error(&snapshot, "hit", false, PreviewError::TextLimit);
    assert_eq!(
        snapshot.push(3, Field::Output, "hit", false),
        Err(PreviewError::TextLimit)
    );
}

#[test]
fn generation_exhaustion_never_reuses_a_stale_identity() {
    let counter = AtomicU64::new(u64::MAX - 1);
    assert_eq!(next_generation(&counter), Some(u64::MAX));
    assert_eq!(next_generation(&counter), None);
    assert_eq!(counter.load(Ordering::Acquire), u64::MAX);
}

#[test]
fn long_logical_line_limit_discards_earlier_matches() {
    let exact = snapshot_with(&[(1, Field::Output, &"a".repeat(LINE_LIMIT))]);
    assert!(hits(&exact, "z", false).is_empty());
    let overflow = snapshot_with(&[
        (1, Field::Output, "hit"),
        (2, Field::Output, &"a".repeat(LINE_LIMIT + 1)),
    ]);
    assert_error(&overflow, "hit", false, PreviewError::LineLimit);
}

#[test]
fn regex_compilation_size_limit_does_not_return_partial_hits() {
    let snapshot = snapshot_with(&[(1, Field::Output, "a")]);
    assert_error(
        &snapshot,
        "(?:a{1000}){1000}",
        true,
        PreviewError::InvalidRegex,
    );
}
