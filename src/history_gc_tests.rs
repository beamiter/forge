use super::*;

fn namespace() -> NamespaceIdentity {
    NamespaceIdentity {
        device: 1,
        inode: 100,
    }
}
fn identity_for(name: &str) -> FileIdentity {
    let mut id = identity();
    id.inode = name.bytes().fold(10u64, |sum, byte| {
        sum.wrapping_mul(31).wrapping_add(u64::from(byte))
    });
    id
}
fn identity() -> FileIdentity {
    FileIdentity {
        device: 1,
        inode: 2,
        bytes: 3,
        changed_ns: 4,
        digest: [5; 32],
    }
}
fn owned(name: &str) -> OwnedFile {
    OwnedFile {
        name: name.into(),
        identity: identity_for(name),
    }
}
fn observed(name: &str) -> ObservedFile {
    ObservedFile {
        name: name.into(),
        identity: Some(identity_for(name)),
    }
}
fn run(
    owned: &[OwnedFile],
    observed: &[ObservedFile],
    refs: &[OsString],
    pending: &[OsString],
) -> Plan {
    plan(Snapshot {
        ownership_namespace: Some(namespace()),
        inventory_namespace: namespace(),
        reference_namespace: Some(namespace()),
        manifest_version: Some(1),
        owned,
        observed,
        references: Some(refs),
        live_or_pending: pending,
        inventory_complete: true,
        legacy_or_unknown_references: false,
    })
}
fn candidate_names(plan: &Plan) -> Vec<OsString> {
    plan.decisions
        .iter()
        .filter(|d| d.disposition == Disposition::Candidate)
        .map(|d| d.name.clone())
        .collect()
}
#[test]
fn only_explicitly_owned_unreferenced_matching_file_is_candidate() {
    let result = run(
        &[owned("b"), owned("a")],
        &[observed("b"), observed("a"), observed("unknown")],
        &["a".into()],
        &[],
    );
    assert_eq!(candidate_names(&result), vec![OsString::from("b")]);
    assert_eq!(
        result.decisions[0].disposition,
        Disposition::Preserve(PreserveReason::Referenced)
    );
    assert_eq!(
        result.decisions[2].disposition,
        Disposition::Preserve(PreserveReason::Unowned)
    );
}
#[test]
fn every_incomplete_global_authority_produces_no_candidates() {
    let owned = [owned("a")];
    let observed = [observed("a")];
    for case in 0..5 {
        let mut input = Snapshot {
            ownership_namespace: Some(namespace()),
            inventory_namespace: namespace(),
            reference_namespace: Some(namespace()),
            manifest_version: Some(1),
            owned: &owned,
            observed: &observed,
            references: Some(&[]),
            live_or_pending: &[],
            inventory_complete: true,
            legacy_or_unknown_references: false,
        };
        match case {
            0 => input.manifest_version = None,
            1 => input.manifest_version = Some(2),
            2 => input.references = None,
            3 => input.inventory_complete = false,
            _ => input.legacy_or_unknown_references = true,
        }
        let result = plan(input);
        assert!(!result.blockers.is_empty());
        assert!(candidate_names(&result).is_empty());
    }
}
#[test]
fn live_and_pending_owners_survive_even_without_restorable_reference() {
    for references in [vec![], vec![OsString::from("other")]] {
        let result = run(&[owned("a")], &[observed("a")], &references, &["a".into()]);
        assert_eq!(
            result.decisions[0].disposition,
            Disposition::Preserve(PreserveReason::LiveOrPending)
        );
    }
}
#[test]
fn changed_identity_is_never_eligible() {
    for field in 0..5 {
        let mut file = observed("a");
        let id = file.identity.as_mut().unwrap();
        match field {
            0 => id.device += 1,
            1 => id.inode += 1,
            2 => id.bytes += 1,
            3 => id.changed_ns += 1,
            _ => id.digest[0] += 1,
        }
        let result = run(&[owned("a")], &[file], &[], &[]);
        assert_eq!(
            result.decisions[0].disposition,
            Disposition::Preserve(PreserveReason::IdentityChanged)
        );
    }
}
#[test]
fn unsupported_file_observation_is_preserved() {
    // None covers symlinks, hardlinks, FIFO/devices, wrong owner and failed observation.
    let result = run(
        &[owned("a")],
        &[ObservedFile {
            name: "a".into(),
            identity: None,
        }],
        &[],
        &[],
    );
    assert_eq!(
        result.decisions[0].disposition,
        Disposition::Preserve(PreserveReason::UnsafeOrUnknownFile)
    );
}
#[test]
fn duplicate_records_even_identical_never_grant_authority() {
    for duplicate_owned in [false, true] {
        let owners = if duplicate_owned {
            vec![owned("a"), owned("a")]
        } else {
            vec![owned("a")]
        };
        let files = if duplicate_owned {
            vec![observed("a")]
        } else {
            vec![observed("a"), observed("a")]
        };
        assert_eq!(
            run(&owners, &files, &[], &[]).decisions[0].disposition,
            Disposition::Preserve(PreserveReason::ConflictingRecords)
        );
    }
}
#[test]
fn path_like_names_never_escape_namespace() {
    for name in ["", ".", "..", "../a", "a/b", "/a", "a/", "a\0b"] {
        let result = run(&[owned(name)], &[observed(name)], &[], &[]);
        assert_eq!(
            result.decisions[0].disposition,
            Disposition::Preserve(PreserveReason::InvalidName),
            "{name:?}"
        );
    }
}
#[test]
fn missing_owned_file_is_not_a_candidate() {
    assert_eq!(
        run(&[owned("a")], &[], &[], &[]).decisions[0].disposition,
        Disposition::Preserve(PreserveReason::MissingFile)
    );
}
#[test]
fn input_order_does_not_change_plan() {
    let first = run(
        &[owned("b"), owned("a")],
        &[observed("c"), observed("b"), observed("a")],
        &[],
        &[],
    );
    let second = run(
        &[owned("a"), owned("b")],
        &[observed("a"), observed("b"), observed("c")],
        &[],
        &[],
    );
    assert_eq!(first, second);
    assert_eq!(
        candidate_names(&first),
        vec![OsString::from("a"), OsString::from("b")]
    );
}
#[test]
fn reference_removal_only_enables_exact_owned_identity() {
    let owners = [owned("a")];
    let files = [observed("a"), observed("stranger")];
    assert!(candidate_names(&run(&owners, &files, &["a".into()], &[])).is_empty());
    assert_eq!(
        candidate_names(&run(&owners, &files, &[], &[])),
        vec![OsString::from("a")]
    );
}
#[test]
fn candidate_sets_shrink_when_references_are_added() {
    let owners = [owned("a"), owned("b")];
    let files = [observed("a"), observed("b")];
    for refs in [vec![], vec!["a".into()], vec!["a".into(), "b".into()]] {
        let result = run(&owners, &files, &refs, &[]);
        assert_eq!(candidate_names(&result).len(), 2 - refs.len());
    }
}

#[test]
fn malformed_reference_or_pending_name_blocks_entire_snapshot() {
    for pending in [false, true] {
        let bad = [OsString::from("../unknown")];
        let result = if pending {
            run(&[owned("a")], &[observed("a")], &[], &bad)
        } else {
            run(&[owned("a")], &[observed("a")], &bad, &[])
        };
        assert_eq!(result.blockers, vec![Blocker::IncompleteReferences]);
        assert!(candidate_names(&result).is_empty());
    }
}
#[test]
fn non_utf8_basename_is_not_lossily_collapsed() {
    use std::os::unix::ffi::OsStringExt;
    let name = OsString::from_vec(vec![0xff]);
    let owner = OwnedFile {
        name: name.clone(),
        identity: identity(),
    };
    let file = ObservedFile {
        name: name.clone(),
        identity: Some(identity()),
    };
    assert_eq!(
        candidate_names(&run(&[owner], &[file], &[], &[])),
        vec![name]
    );
}

#[test]
fn mixed_or_missing_namespace_blocks_all_candidates() {
    let owners = [owned("a")];
    let files = [observed("a")];
    for case in 0..5 {
        let other = NamespaceIdentity {
            device: 1,
            inode: 101,
        };
        let mut input = Snapshot {
            ownership_namespace: Some(namespace()),
            inventory_namespace: namespace(),
            reference_namespace: Some(namespace()),
            manifest_version: Some(1),
            owned: &owners,
            observed: &files,
            references: Some(&[]),
            live_or_pending: &[],
            inventory_complete: true,
            legacy_or_unknown_references: false,
        };
        match case {
            0 => input.ownership_namespace = None,
            1 => input.reference_namespace = None,
            2 => input.ownership_namespace = Some(other),
            3 => input.reference_namespace = Some(other),
            _ => input.inventory_namespace = other,
        }
        let result = plan(input);
        assert_eq!(result.blockers, vec![Blocker::NamespaceMismatch]);
        assert!(candidate_names(&result).is_empty());
    }
}
#[test]
fn cross_name_physical_aliases_are_preserved_even_when_other_metadata_differs() {
    for same_metadata in [true, false] {
        let first = owned("a");
        let mut second = owned("b");
        second.identity = first.identity.clone();
        if !same_metadata {
            second.identity.bytes += 1;
            second.identity.digest[0] += 1;
        }
        let files = [
            ObservedFile {
                name: first.name.clone(),
                identity: Some(first.identity.clone()),
            },
            ObservedFile {
                name: second.name.clone(),
                identity: Some(second.identity.clone()),
            },
        ];
        let result = run(&[first, second], &files, &[], &[]);
        assert!(candidate_names(&result).is_empty());
        assert!(result
            .decisions
            .iter()
            .all(|d| d.disposition == Disposition::Preserve(PreserveReason::AliasedIdentity)));
    }
}
#[test]
fn stale_ownership_alias_cannot_authorize_another_name() {
    let first = owned("a");
    let mut second = owned("b");
    second.identity = first.identity.clone();
    let result = run(&[first, second], &[observed("a")], &[], &[]);
    assert!(candidate_names(&result).is_empty());
}
