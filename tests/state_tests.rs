mod common;

use forge::state::{escape_tab_state, parse_tabs_state, unescape_tab_state, PaneLayout};

#[test]
fn test_escape_unescape_roundtrip() {
    let inputs = vec![
        "hello world",
        "line\nbreak",
        "tab\there",
        "back\\slash",
        "mixed\t\n\\all",
    ];

    for input in inputs {
        let escaped = escape_tab_state(input);
        let unescaped = unescape_tab_state(&escaped);
        assert_eq!(input, unescaped, "Roundtrip failed for: {}", input);
    }
}

#[test]
fn test_pane_layout_leaf_serialization() {
    let layout = PaneLayout::Leaf {
        dir: "/tmp".to_string(),
        sid: "123-456".to_string(),
        cwd_external: false,
        remote_name: None,
        custom_title: None,
        private_title: Some(true),
        cmds: None,
        pinned: None,
    };

    let json = serde_json::to_string(&layout).expect("Serialization failed");
    let deserialized: PaneLayout = serde_json::from_str(&json).expect("Deserialization failed");

    match deserialized {
        PaneLayout::Leaf {
            dir,
            sid,
            cmds,
            private_title,
            ..
        } => {
            assert_eq!(dir, "/tmp");
            assert_eq!(sid, "123-456");
            assert_eq!(cmds, None);
            assert_eq!(private_title, Some(true));
        }
        _ => panic!("Expected Leaf layout"),
    }
}

#[test]
fn test_pane_layout_leaf_with_commands() {
    let layout = PaneLayout::Leaf {
        dir: "/home".to_string(),
        sid: "789-012".to_string(),
        cwd_external: false,
        remote_name: None,
        custom_title: None,
        private_title: None,
        cmds: Some(vec!["nix".to_string(), "develop".to_string()]),
        pinned: None,
    };

    let json = serde_json::to_string(&layout).expect("Serialization failed");
    let deserialized: PaneLayout = serde_json::from_str(&json).expect("Deserialization failed");

    match deserialized {
        PaneLayout::Leaf { dir, sid, cmds, .. } => {
            assert_eq!(dir, "/home");
            assert_eq!(sid, "789-012");
            assert_eq!(cmds, Some(vec!["nix".to_string(), "develop".to_string()]));
        }
        _ => panic!("Expected Leaf layout"),
    }
}

#[test]
fn restorable_command_argv_round_trips_without_losing_boundaries() {
    // A ';' inside one remote argument must stay inside that argument; a
    // joined-string format would let it become a second local command.
    let argv = vec![
        "ssh".to_string(),
        "example.test".to_string(),
        "printf '%s, %s'; touch /tmp/stays-remote".to_string(),
    ];
    let layout = PaneLayout::Leaf {
        dir: "/tmp".to_string(),
        sid: "123-456".to_string(),
        cwd_external: false,
        remote_name: None,
        custom_title: None,
        private_title: None,
        cmds: Some(argv.clone()),
        pinned: None,
    };

    let encoded = serde_json::to_string(&layout).expect("Serialization failed");
    let decoded: PaneLayout = serde_json::from_str(&encoded).expect("Deserialization failed");
    match decoded {
        PaneLayout::Leaf { cmds, .. } => assert_eq!(cmds, Some(argv)),
        _ => panic!("Expected Leaf layout"),
    }
}

#[test]
fn legacy_joined_restore_command_is_loaded_but_not_replayed() {
    let legacy: PaneLayout = serde_json::from_str(
        r#"{"type":"leaf","dir":"/tmp","sid":"1-2","cmds":"ssh host; touch /tmp/local"}"#,
    )
    .expect("legacy snapshot must still load");
    match legacy {
        PaneLayout::Leaf {
            dir,
            sid,
            cwd_external,
            remote_name,
            custom_title,
            cmds,
            ..
        } => {
            assert_eq!(dir, "/tmp");
            assert_eq!(sid, "1-2");
            assert!(!cwd_external);
            assert_eq!(remote_name, None);
            assert_eq!(custom_title, None);
            assert_eq!(cmds, None, "joined command strings must never replay");
        }
        _ => panic!("Expected Leaf layout"),
    }
}

#[test]
fn test_pane_layout_split_serialization() {
    let layout = PaneLayout::Split {
        orientation: 'h',
        position: 500,
        start: Box::new(PaneLayout::Leaf {
            dir: "/tmp".to_string(),
            sid: "123-456".to_string(),
            cwd_external: false,
            remote_name: None,
            custom_title: None,
            private_title: None,
            cmds: None,
            pinned: None,
        }),
        end: Box::new(PaneLayout::Leaf {
            dir: "/home".to_string(),
            sid: "789-012".to_string(),
            cwd_external: false,
            remote_name: None,
            custom_title: None,
            private_title: None,
            cmds: Some(vec!["nix".to_string(), "develop".to_string()]),
            pinned: None,
        }),
    };

    let json = serde_json::to_string(&layout).expect("Serialization failed");
    let deserialized: PaneLayout = serde_json::from_str(&json).expect("Deserialization failed");

    match deserialized {
        PaneLayout::Split {
            orientation,
            position,
            start,
            end,
        } => {
            assert_eq!(orientation, 'h');
            assert_eq!(position, 500);

            match *start {
                PaneLayout::Leaf {
                    ref dir, ref sid, ..
                } => {
                    assert_eq!(dir, "/tmp");
                    assert_eq!(sid, "123-456");
                }
                _ => panic!("Expected Leaf in start"),
            }

            match *end {
                PaneLayout::Leaf {
                    ref dir,
                    ref sid,
                    ref cmds,
                    ..
                } => {
                    assert_eq!(dir, "/home");
                    assert_eq!(sid, "789-012");
                    assert_eq!(cmds, &Some(vec!["nix".to_string(), "develop".to_string()]));
                }
                _ => panic!("Expected Leaf in end"),
            }
        }
        _ => panic!("Expected Split layout"),
    }
}

#[test]
fn test_pane_layout_nested_splits() {
    let layout = PaneLayout::Split {
        orientation: 'h',
        position: 500,
        start: Box::new(PaneLayout::Leaf {
            dir: "/tmp".to_string(),
            sid: "123-456".to_string(),
            cwd_external: false,
            remote_name: None,
            custom_title: None,
            private_title: None,
            cmds: None,
            pinned: None,
        }),
        end: Box::new(PaneLayout::Split {
            orientation: 'v',
            position: 300,
            start: Box::new(PaneLayout::Leaf {
                dir: "/home".to_string(),
                sid: "789-012".to_string(),
                cwd_external: false,
                remote_name: None,
                custom_title: None,
                private_title: None,
                cmds: None,
                pinned: None,
            }),
            end: Box::new(PaneLayout::Leaf {
                dir: "/var".to_string(),
                sid: "345-678".to_string(),
                cwd_external: false,
                remote_name: None,
                custom_title: None,
                private_title: None,
                cmds: None,
                pinned: None,
            }),
        }),
    };

    let json = serde_json::to_string(&layout).expect("Serialization failed");
    let deserialized: PaneLayout = serde_json::from_str(&json).expect("Deserialization failed");

    // Verify structure is preserved
    match deserialized {
        PaneLayout::Split { orientation, .. } => {
            assert_eq!(orientation, 'h');
        }
        _ => panic!("Expected outer Split"),
    }
}

#[test]
fn test_parse_tabs_state_legacy_format() {
    let contents = r#"current_page=0
tab=Terminal 1	/tmp	123-456	nix develop
tab=Terminal 2	/home	789-012"#;

    let (current, tabs) = parse_tabs_state(contents);

    assert_eq!(current, Some(0));
    assert_eq!(tabs.len(), 2);

    // First tab: the legacy 4-field format stored a joined command string
    // whose argv boundaries cannot be recovered, so it loads without replay.
    match &tabs[0].1 {
        PaneLayout::Leaf { dir, sid, cmds, .. } => {
            assert_eq!(dir, "/tmp");
            assert_eq!(sid, "123-456");
            assert_eq!(cmds, &None);
        }
        _ => panic!("Expected Leaf"),
    }

    // Second tab
    match &tabs[1].1 {
        PaneLayout::Leaf { dir, sid, cmds, .. } => {
            assert_eq!(dir, "/home");
            assert_eq!(sid, "789-012");
            assert_eq!(cmds, &None);
        }
        _ => panic!("Expected Leaf"),
    }
}

#[test]
fn test_parse_tabs_state_new_json_format() {
    let leaf_json = serde_json::json!({
        "type": "leaf",
        "dir": "/tmp",
        "sid": "123-456",
        "cmds": ["nix", "develop"]
    });

    let contents = format!(
        r#"current_page=0
tab=Terminal 1	{}"#,
        leaf_json
    );

    let (current, tabs) = parse_tabs_state(&contents);

    assert_eq!(current, Some(0));
    assert_eq!(tabs.len(), 1);

    match &tabs[0].1 {
        PaneLayout::Leaf { dir, sid, cmds, .. } => {
            assert_eq!(dir, "/tmp");
            assert_eq!(sid, "123-456");
            assert_eq!(cmds, &Some(vec!["nix".to_string(), "develop".to_string()]));
        }
        _ => panic!("Expected Leaf"),
    }
}

#[test]
fn test_parse_tabs_state_legacy_joined_cmds_in_layout_json_loads_without_replay() {
    let leaf_json = serde_json::json!({
        "type": "leaf",
        "dir": "/tmp",
        "sid": "123-456",
        "cmds": "ssh host; touch /tmp/local"
    });

    let contents = format!("current_page=0\ntab=Terminal 1\t{}", leaf_json);
    let (_, tabs) = parse_tabs_state(&contents);

    assert_eq!(tabs.len(), 1);
    match &tabs[0].1 {
        PaneLayout::Leaf { dir, cmds, .. } => {
            assert_eq!(dir, "/tmp");
            assert_eq!(cmds, &None, "joined command strings must never replay");
        }
        _ => panic!("Expected Leaf"),
    }
}

#[test]
fn test_parse_tabs_state_empty() {
    let contents = "";
    let (current, tabs) = parse_tabs_state(contents);

    assert_eq!(current, None);
    assert_eq!(tabs.len(), 0);
}

#[test]
fn test_parse_tabs_state_only_current_page() {
    let contents = "current_page=2";
    let (current, tabs) = parse_tabs_state(contents);

    assert_eq!(current, Some(2));
    assert_eq!(tabs.len(), 0);
}
