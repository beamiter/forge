mod common;

use forge::config::TerminalMode;

#[test]
fn test_terminal_mode_block() {
    let mode = TerminalMode::Block;
    match mode {
        TerminalMode::Block => {
            // Expected
        }
        TerminalMode::Vte | TerminalMode::Unified | TerminalMode::Chat => {
            panic!("Expected Block mode");
        }
    }
}

#[test]
fn test_terminal_mode_vte() {
    let mode = TerminalMode::Vte;
    match mode {
        TerminalMode::Vte => {
            // Expected
        }
        TerminalMode::Block | TerminalMode::Unified | TerminalMode::Chat => {
            panic!("Expected Vte mode");
        }
    }
}

#[test]
fn test_terminal_mode_clone() {
    let mode1 = TerminalMode::Block;
    let mode2 = mode1.clone();
    match mode2 {
        TerminalMode::Block => {
            // Expected
        }
        _ => panic!("Clone failed"),
    }
}

#[test]
fn test_terminal_mode_chat_is_distinct_from_block() {
    assert_eq!(TerminalMode::Chat.clone(), TerminalMode::Chat);
    assert_ne!(TerminalMode::Chat, TerminalMode::Block);
}
