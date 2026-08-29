//! The AI panel's multi-chat runtime state.
//!
//! The state machine itself is `jterm_core::ai::chat_store`, the union of the
//! four terminals' formerly duplicated copies: forge's hardening (the global
//! live-history budget with real compaction, persistence that compacts before
//! serialising, typed archive/delete outcomes, the at-capacity archive guard)
//! plus anvil's in-store streaming (`push_delta`/`active_partial`), library
//! query filtering, and the prefix-idempotence rule in draft merging. GTK still
//! owns one transcript and composer; the store owns every chat's provider
//! history, Block context, draft, archive state, streamed partial and request
//! token, keyed by `(chat_id, epoch)` so a background reply cannot cross into
//! the chat the user happens to be viewing.
//!
//! Only forge's construction choice stays here, because it is a panel property
//! rather than a store property: see [`BUSY_POLICY`].

use jterm_core::ai::{BusyChatPolicy, ConversationSnapshot};

pub(crate) use jterm_core::ai::{
    bounded_live_message, ChatStatus, ChatStore, ChatStoreError, ChatSummary, RequestToken,
    MAX_LIVE_MESSAGE_BYTES,
};

/// forge archives and deletes chats that still have a request in flight.
///
/// Every such path in the panel cancels the request first — the Delete dialog
/// cancels before removing the chat, `cancel_all_requests` cancels the whole
/// map at teardown — so refusing the mutation here (anvil/ember/frost's
/// `Refuse`, which is the store's default) would only reject work the user has
/// already confirmed. The cancelled request's late reply is still discarded,
/// because its epoch no longer matches.
const BUSY_POLICY: BusyChatPolicy = BusyChatPolicy::Allow;

/// A fresh chat library under forge's busy policy.
pub(crate) fn new_store() -> ChatStore {
    ChatStore::with_busy_policy(BUSY_POLICY)
}

/// Restore a persisted library under forge's busy policy. The snapshot's
/// structural invariants are guaranteed by `ConversationSnapshot` itself.
pub(crate) fn restore_store(snapshot: ConversationSnapshot) -> ChatStore {
    ChatStore::restore_with_busy_policy(snapshot, BUSY_POLICY)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The store's own behaviour is covered by `jterm_core`; what forge owns
    /// is the policy, so this pins the one decision the shim makes.
    #[test]
    fn forge_lets_a_cancelled_but_still_busy_chat_be_archived_and_deleted() {
        let mut store = new_store();
        let start = store
            .begin_turn("question".into(), None, "Thinking…".into(), true)
            .expect("a fresh chat accepts a turn");
        assert!(store.is_active_busy());

        // No cancel_request first: the panel has already cancelled the worker,
        // and the store must not second-guess it.
        let archived = store
            .toggle_archive_active()
            .expect("archiving a busy chat is allowed for forge");
        assert!(archived.archived);
        assert_ne!(archived.active_chat_id, start.token.chat_id);

        store.select_chat(start.token.chat_id);
        let deleted = store
            .delete_active()
            .expect("deleting a busy chat is allowed for forge");
        assert_eq!(deleted.deleted_chat_id, start.token.chat_id);
        // The late reply still cannot land: its chat is gone.
        assert_eq!(store.complete_success(start.token, "late".into()), None);
    }
}
