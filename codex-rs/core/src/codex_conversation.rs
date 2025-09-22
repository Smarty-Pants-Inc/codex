use crate::codex::Codex;
use crate::error::Result as CodexResult;
use crate::protocol::Event;
use crate::protocol::Op;
use crate::protocol::Submission;
use crate::remote::RemoteConversation;

enum ConversationBackend {
    Local(Codex),
    Remote(RemoteConversation),
}

pub struct CodexConversation {
    backend: ConversationBackend,
}

/// Conduit for the bidirectional stream of messages that compose a conversation
/// in Codex.
impl CodexConversation {
    pub(crate) fn new_local(codex: Codex) -> Self {
        Self {
            backend: ConversationBackend::Local(codex),
        }
    }

    pub(crate) fn new_remote(remote: RemoteConversation) -> Self {
        Self {
            backend: ConversationBackend::Remote(remote),
        }
    }

    pub async fn submit(&self, op: Op) -> CodexResult<String> {
        match &self.backend {
            ConversationBackend::Local(codex) => codex.submit(op).await,
            ConversationBackend::Remote(remote) => remote.submit(op).await,
        }
    }

    /// Use sparingly: this is intended to be removed soon.
    pub async fn submit_with_id(&self, sub: Submission) -> CodexResult<()> {
        match &self.backend {
            ConversationBackend::Local(codex) => codex.submit_with_id(sub).await,
            ConversationBackend::Remote(remote) => remote.submit_with_id(sub).await,
        }
    }

    pub async fn next_event(&self) -> CodexResult<Event> {
        match &self.backend {
            ConversationBackend::Local(codex) => codex.next_event().await,
            ConversationBackend::Remote(remote) => remote.next_event().await,
        }
    }
}
