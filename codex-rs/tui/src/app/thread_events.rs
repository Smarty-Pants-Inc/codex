//! Thread event buffering and replay state for the TUI app.
//!
//! This module owns the per-thread event store used when the TUI switches between the main
//! conversation, subagents, and side conversations. It keeps buffered app-server notifications,
//! pending interactive request replay state, active-turn tracking, saved composer state, and the
//! carried Oracle workflow replay ordering close together with the replay behavior that consumes
//! them.

use super::*;

#[derive(Debug, Clone)]
pub(super) struct ThreadEventSnapshot {
    pub(super) session: Option<ThreadSessionState>,
    pub(super) turns: Vec<Turn>,
    pub(super) events: Vec<ThreadBufferedEvent>,
    pub(super) replay_entries: Vec<ThreadReplayEntry>,
    pub(super) input_state: Option<ThreadInputState>,
}

#[derive(Debug, Clone)]
pub(super) enum ThreadBufferedEvent {
    Notification(ServerNotification),
    Request(ServerRequest),
    HistoryEntryResponse(GetHistoryEntryResponseEvent),
    FeedbackSubmission(FeedbackThreadEvent),
    OracleWorkflowEvent(OracleWorkflowThreadEvent),
}

#[derive(Debug, Clone)]
pub(super) enum ThreadReplayEntry {
    Turn(Turn),
    Event(Box<ThreadBufferedEvent>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct FeedbackThreadEvent {
    pub(super) category: FeedbackCategory,
    pub(super) include_logs: bool,
    pub(super) feedback_audience: FeedbackAudience,
    pub(super) result: Result<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct OracleWorkflowThreadEvent {
    pub(super) title: String,
    pub(super) details: Vec<String>,
}

#[derive(Debug)]
pub(super) struct ThreadEventStore {
    pub(super) session: Option<ThreadSessionState>,
    pub(super) turns: Vec<Turn>,
    pub(super) buffer: VecDeque<ThreadBufferedEvent>,
    pub(super) replay_entries: Vec<ThreadReplayEntry>,
    pub(super) pending_interactive_replay: PendingInteractiveReplayState,
    pub(super) active_turn_id: Option<String>,
    pub(super) input_state: Option<ThreadInputState>,
    pub(super) capacity: usize,
    pub(super) active: bool,
}

impl ThreadEventStore {
    pub(super) fn event_survives_session_refresh(event: &ThreadBufferedEvent) -> bool {
        matches!(
            event,
            ThreadBufferedEvent::Request(_)
                | ThreadBufferedEvent::Notification(ServerNotification::HookStarted(_))
                | ThreadBufferedEvent::Notification(ServerNotification::HookCompleted(_))
                | ThreadBufferedEvent::FeedbackSubmission(_)
                | ThreadBufferedEvent::OracleWorkflowEvent(_)
        )
    }

    pub(super) fn new(capacity: usize) -> Self {
        Self {
            session: None,
            turns: Vec::new(),
            buffer: VecDeque::new(),
            replay_entries: Vec::new(),
            pending_interactive_replay: PendingInteractiveReplayState::default(),
            active_turn_id: None,
            input_state: None,
            capacity,
            active: false,
        }
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub(super) fn new_with_session(
        capacity: usize,
        session: ThreadSessionState,
        turns: Vec<Turn>,
    ) -> Self {
        let mut store = Self::new(capacity);
        store.session = Some(session);
        store.set_turns(turns);
        store
    }

    pub(super) fn set_session(&mut self, session: ThreadSessionState, turns: Vec<Turn>) {
        self.session = Some(session);
        self.set_turns(turns);
    }

    pub(super) fn rebase_buffer_after_session_refresh(&mut self) {
        self.buffer.retain(Self::event_survives_session_refresh);
        self.replay_entries.retain(|entry| match entry {
            ThreadReplayEntry::Turn(_) => true,
            ThreadReplayEntry::Event(event) => Self::event_survives_session_refresh(event),
        });
    }

    pub(super) fn set_turns(&mut self, turns: Vec<Turn>) {
        self.active_turn_id = turns
            .iter()
            .rev()
            .find(|turn| matches!(turn.status, TurnStatus::InProgress))
            .map(|turn| turn.id.clone());
        self.sync_replay_entries_for_turn_refresh(&turns);
        self.turns = turns;
    }

    pub(super) fn push_notification(&mut self, notification: ServerNotification) {
        self.pending_interactive_replay
            .note_server_notification(&notification);
        match &notification {
            ServerNotification::TurnStarted(turn) => {
                self.active_turn_id = Some(turn.turn.id.clone());
            }
            ServerNotification::TurnCompleted(turn) => {
                if self.active_turn_id.as_deref() == Some(turn.turn.id.as_str()) {
                    self.active_turn_id = None;
                }
            }
            ServerNotification::ThreadClosed(_) => {
                self.active_turn_id = None;
            }
            _ => {}
        }
        self.push_buffered_event(ThreadBufferedEvent::Notification(notification));
    }

    pub(super) fn push_request(&mut self, request: ServerRequest) {
        self.pending_interactive_replay
            .note_server_request(&request);
        self.push_buffered_event(ThreadBufferedEvent::Request(request));
    }

    pub(super) fn pending_replay_requests(&self) -> Vec<ServerRequest> {
        self.buffer
            .iter()
            .filter_map(|event| match event {
                ThreadBufferedEvent::Request(request)
                    if self
                        .pending_interactive_replay
                        .should_replay_snapshot_request(request) =>
                {
                    Some(request.clone())
                }
                ThreadBufferedEvent::Request(_)
                | ThreadBufferedEvent::Notification(_)
                | ThreadBufferedEvent::HistoryEntryResponse(_)
                | ThreadBufferedEvent::FeedbackSubmission(_)
                | ThreadBufferedEvent::OracleWorkflowEvent(_) => None,
            })
            .collect()
    }

    pub(super) fn apply_thread_rollback(&mut self, response: &ThreadRollbackResponse) {
        self.turns = response.thread.turns.clone();
        self.buffer.clear();
        self.reseed_replay_entries_from_state();
        self.pending_interactive_replay = PendingInteractiveReplayState::default();
        self.active_turn_id = None;
    }

    pub(super) fn snapshot(&self) -> ThreadEventSnapshot {
        ThreadEventSnapshot {
            session: self.session.clone(),
            turns: self.turns.clone(),
            // Thread switches replay buffered events into a rebuilt ChatWidget. Only replay
            // interactive prompts that are still pending, or answered approvals/input will reappear.
            events: self
                .buffer
                .iter()
                .filter(|event| match event {
                    ThreadBufferedEvent::Request(request) => self
                        .pending_interactive_replay
                        .should_replay_snapshot_request(request),
                    ThreadBufferedEvent::Notification(_)
                    | ThreadBufferedEvent::HistoryEntryResponse(_)
                    | ThreadBufferedEvent::FeedbackSubmission(_)
                    | ThreadBufferedEvent::OracleWorkflowEvent(_) => true,
                })
                .cloned()
                .collect(),
            replay_entries: self.snapshot_replay_entries(),
            input_state: self.input_state.clone(),
        }
    }

    pub(super) fn note_outbound_op<T>(&mut self, op: T)
    where
        T: Into<AppCommand>,
    {
        self.pending_interactive_replay.note_outbound_op(op);
    }

    pub(super) fn op_can_change_pending_replay_state<T>(op: T) -> bool
    where
        T: Into<AppCommand>,
    {
        PendingInteractiveReplayState::op_can_change_state(op)
    }

    pub(super) fn has_pending_thread_approvals(&self) -> bool {
        self.pending_interactive_replay
            .has_pending_thread_approvals()
    }

    pub(super) fn side_parent_pending_status(&self) -> Option<SideParentStatus> {
        if self
            .pending_interactive_replay
            .has_pending_thread_user_input()
        {
            Some(SideParentStatus::NeedsInput)
        } else if self
            .pending_interactive_replay
            .has_pending_thread_approvals()
        {
            Some(SideParentStatus::NeedsApproval)
        } else {
            None
        }
    }

    pub(super) fn active_turn_id(&self) -> Option<&str> {
        self.active_turn_id.as_deref()
    }

    pub(super) fn clear_active_turn_id(&mut self) {
        self.active_turn_id = None;
    }

    pub(super) fn push_turn_replay(&mut self, turn: Turn) {
        self.replay_entries.push(ThreadReplayEntry::Turn(turn));
    }

    pub(super) fn push_buffered_event(&mut self, event: ThreadBufferedEvent) {
        self.buffer.push_back(event.clone());
        self.replay_entries
            .push(ThreadReplayEntry::Event(Box::new(event)));
        if self.buffer.len() > self.capacity
            && let Some(removed) = self.buffer.pop_front()
        {
            self.drop_oldest_replay_event();
            if let ThreadBufferedEvent::Request(request) = &removed {
                self.pending_interactive_replay
                    .note_evicted_server_request(request);
            }
        }
    }

    pub(super) fn should_replay_snapshot_event(&self, event: &ThreadBufferedEvent) -> bool {
        match event {
            ThreadBufferedEvent::Request(request) => self
                .pending_interactive_replay
                .should_replay_snapshot_request(request),
            ThreadBufferedEvent::Notification(_)
            | ThreadBufferedEvent::HistoryEntryResponse(_)
            | ThreadBufferedEvent::FeedbackSubmission(_)
            | ThreadBufferedEvent::OracleWorkflowEvent(_) => true,
        }
    }

    pub(super) fn snapshot_replay_entries(&self) -> Vec<ThreadReplayEntry> {
        self.replay_entries
            .iter()
            .filter_map(|entry| match entry {
                ThreadReplayEntry::Turn(turn) => Some(ThreadReplayEntry::Turn(turn.clone())),
                ThreadReplayEntry::Event(event) => self
                    .should_replay_snapshot_event(event)
                    .then(|| ThreadReplayEntry::Event(event.clone())),
            })
            .collect()
    }

    pub(super) fn sync_replay_entries_for_turn_refresh(&mut self, turns: &[Turn]) {
        let valid_turn_ids = turns
            .iter()
            .map(|turn| turn.id.as_str())
            .collect::<HashSet<_>>();
        self.replay_entries.retain(|entry| match entry {
            ThreadReplayEntry::Turn(turn) => valid_turn_ids.contains(turn.id.as_str()),
            ThreadReplayEntry::Event(_) => true,
        });

        let mut refreshed_turn_entries = turns
            .iter()
            .filter(|turn| {
                !self.replay_entries.iter().any(
                    |entry| matches!(entry, ThreadReplayEntry::Turn(existing) if existing == *turn),
                )
            })
            .cloned()
            .map(ThreadReplayEntry::Turn)
            .collect::<Vec<_>>();

        if refreshed_turn_entries.is_empty() {
            return;
        }

        if self
            .replay_entries
            .iter()
            .any(|entry| matches!(entry, ThreadReplayEntry::Turn(_)))
        {
            self.replay_entries.append(&mut refreshed_turn_entries);
        } else {
            self.replay_entries.splice(0..0, refreshed_turn_entries);
        }
    }

    pub(super) fn reseed_replay_entries_from_state(&mut self) {
        self.replay_entries = self
            .turns
            .iter()
            .cloned()
            .map(ThreadReplayEntry::Turn)
            .chain(
                self.buffer
                    .iter()
                    .cloned()
                    .map(|event| ThreadReplayEntry::Event(Box::new(event))),
            )
            .collect();
    }

    pub(super) fn drop_oldest_replay_event(&mut self) {
        if let Some(index) = self
            .replay_entries
            .iter()
            .position(|entry| matches!(entry, ThreadReplayEntry::Event(_)))
        {
            self.replay_entries.remove(index);
        }
    }
}

#[derive(Debug)]
pub(super) struct ThreadEventChannel {
    pub(super) sender: mpsc::Sender<ThreadBufferedEvent>,
    pub(super) receiver: Option<mpsc::Receiver<ThreadBufferedEvent>>,
    pub(super) store: Arc<Mutex<ThreadEventStore>>,
}

impl ThreadEventChannel {
    pub(super) fn new(capacity: usize) -> Self {
        let (sender, receiver) = mpsc::channel(capacity);
        Self {
            sender,
            receiver: Some(receiver),
            store: Arc::new(Mutex::new(ThreadEventStore::new(capacity))),
        }
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub(super) fn new_with_session(
        capacity: usize,
        session: ThreadSessionState,
        turns: Vec<Turn>,
    ) -> Self {
        let (sender, receiver) = mpsc::channel(capacity);
        Self {
            sender,
            receiver: Some(receiver),
            store: Arc::new(Mutex::new(ThreadEventStore::new_with_session(
                capacity, session, turns,
            ))),
        }
    }
}
