use std::collections::HashMap;
use std::collections::HashSet;
use std::sync::Arc;
use std::sync::Mutex as StdMutex;
use std::sync::Weak;
use std::time::Duration;

use codex_core::CodexThread;
use codex_core::IdleTurnAdmission;
use codex_core::NotSubmittedReason;
use codex_core::StartIfIdleSubmission;
use codex_core::ThreadManager;
use codex_core::TurnInput;
use codex_core::TurnInputRequest;
use codex_extension_api::ExtensionEventSink;
use codex_extension_api::ExtensionFuture;
use codex_extension_api::ThreadIdleCause;
use codex_extension_api::ThreadIdleInput;
use codex_extension_api::ThreadLifecycleContributor;
use codex_extension_api::ThreadResumeInput;
use codex_protocol::ThreadId;
use codex_protocol::error::CodexErr;
use codex_protocol::models::snapshot_local_user_input;
use codex_protocol::protocol::AgentStatus;
use codex_protocol::protocol::Event;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::ThreadQueueChangedEvent;
use codex_protocol::protocol::W3cTraceContext;
use codex_protocol::user_input::MAX_USER_INPUT_TEXT_CHARS;
use codex_protocol::user_input::UserInput;
use codex_thread_store::MAX_QUEUE_ITEMS;

use codex_thread_store::QueueStore;
use codex_thread_store::QueuedUserSubmissionRecord;
use codex_thread_store::ThreadStoreError;
use thiserror::Error;
use tokio::sync::Mutex;
use tokio::sync::OwnedMutexGuard;
use tokio::sync::broadcast::error::TryRecvError;
use uuid::Uuid;

/// One user message waiting to start on its thread.
#[derive(Clone, Debug, PartialEq)]
pub struct QueuedItem {
    pub id: String,
    pub input: TurnInput,
}

#[derive(Debug, Error)]
pub enum QueueServiceError {
    #[error("queue storage failed: {0}")]
    Storage(#[from] ThreadStoreError),
    #[error("queued submission payload is invalid: {0}")]
    InvalidPayload(#[from] serde_json::Error),
    #[error("local queued attachment is invalid: {0}")]
    InvalidAttachment(#[from] std::io::Error),
    #[error("Core failed to submit queued user message: {0}")]
    CoreSubmissionError(#[from] CodexErr),
    #[error("only user input can be added to the user-message queue")]
    InvalidInput,
    #[error(
        "queued user input exceeds the maximum length of {MAX_USER_INPUT_TEXT_CHARS} characters ({actual_chars} provided)"
    )]
    InputTooLarge { actual_chars: usize },
}
/// Tracks a thread's dispatch lock and direct-input enqueue intents.
struct QueueDispatchState {
    lock: Arc<Mutex<()>>,
    pending_enqueues: StdMutex<usize>,
}
/// Couples the durable empty-queue reservation to Core's in-memory reservation.
struct QueueAutomaticAdmission {
    dispatch: Arc<QueueDispatchState>,
    queue_reservation: StdMutex<Option<Box<dyn Send>>>,
}

/// Retains the weakly indexed state until after its mutex guard is released.
struct QueueDispatchGuard {
    _guard: OwnedMutexGuard<()>,
    _state: Arc<QueueDispatchState>,
}

impl std::fmt::Debug for QueueAutomaticAdmission {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("QueueAutomaticAdmission")
    }
}

impl IdleTurnAdmission for QueueAutomaticAdmission {
    fn reserve_if_allowed(&self, reserve: &mut dyn FnMut()) -> bool {
        let pending = self
            .dispatch
            .pending_enqueues
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let admitted = *pending == 0;
        if admitted {
            reserve();
        }
        drop(pending);
        self.queue_reservation
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        admitted
    }
}

/// Marks a direct user-input enqueue that has been accepted for dispatch.
///
/// The pending count makes direct input win over lower-priority automatic work.
pub struct QueueEnqueueIntent {
    thread_id: ThreadId,
    dispatch: Arc<QueueDispatchState>,
    thread_manager: Weak<ThreadManager>,
    wake_on_drop: bool,
}

impl Drop for QueueEnqueueIntent {
    fn drop(&mut self) {
        let mut pending = self
            .dispatch
            .pending_enqueues
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *pending = pending
            .checked_sub(1)
            .expect("pending enqueue intent count must not underflow");
        let should_wake = self.wake_on_drop && *pending == 0;
        drop(pending);

        if should_wake && let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(wake_loaded_thread(
                self.thread_manager.clone(),
                self.thread_id,
            ));
        }
    }
}

#[derive(Clone)]
pub struct QueuedItemService {
    queue: Arc<dyn QueueStore>,
    thread_manager: Weak<ThreadManager>,
    event_sink: Arc<dyn ExtensionEventSink>,
    dispatch_states: Arc<StdMutex<HashMap<ThreadId, Weak<QueueDispatchState>>>>,
    resumed_threads: Arc<StdMutex<HashSet<ThreadId>>>,
}

impl QueuedItemService {
    pub fn new(
        queue: Arc<dyn QueueStore>,
        thread_manager: Weak<ThreadManager>,
        event_sink: Arc<dyn ExtensionEventSink>,
    ) -> Self {
        Self {
            queue,
            thread_manager,
            event_sink,
            dispatch_states: Arc::new(StdMutex::new(HashMap::new())),
            resumed_threads: Arc::new(StdMutex::new(HashSet::new())),
        }
    }

    // Check SQLite's inexpensive data version every 10 seconds, then use the
    // durable revision index to discover only changed threads. Independent
    // dispatch tasks keep a blocked or failed thread from starving other queues.
    pub(crate) async fn watch_external_messages(service: Weak<Self>) {
        let mut last_version = None;
        let mut last_revision = 0;
        let mut dispatches: HashMap<ThreadId, tokio::task::JoinHandle<()>> = HashMap::new();
        let mut interval = tokio::time::interval(Duration::from_secs(/*secs*/ 10));
        let mut manager_initialized = false;
        let mut thread_created = None;
        let mut newly_loaded_threads = HashSet::new();
        loop {
            interval.tick().await;
            let Some(service) = service.upgrade() else {
                return;
            };
            let Some(manager) = service.thread_manager.upgrade() else {
                if manager_initialized {
                    return;
                }
                drop(service);
                tokio::time::sleep(Duration::from_millis(/*millis*/ 1)).await;
                interval.reset_immediately();
                continue;
            };
            manager_initialized = true;
            let thread_created =
                thread_created.get_or_insert_with(|| manager.subscribe_thread_created());
            loop {
                match thread_created.try_recv() {
                    Ok(thread_id) => {
                        newly_loaded_threads.insert(thread_id);
                    }
                    Err(TryRecvError::Lagged(_)) => {
                        newly_loaded_threads.extend(manager.list_thread_ids().await);
                    }
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Closed) => return,
                }
            }
            newly_loaded_threads.extend(
                service
                    .resumed_threads
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .drain(),
            );

            let version = match service.queue.change_version().await {
                Ok(version) => version,
                Err(error) => {
                    tracing::warn!(%error, "failed to check queue change version");
                    continue;
                }
            };
            let version_changed = last_version != Some(version);
            if !version_changed && newly_loaded_threads.is_empty() {
                continue;
            }

            let thread_ids = manager.list_thread_ids().await;
            let mut changes = Vec::new();
            let mut observed_revision = last_revision;
            if version_changed {
                match service
                    .queue
                    .changes_since(last_revision, &thread_ids)
                    .await
                {
                    Ok(changed_threads) => {
                        if let Some((_, revision)) = changed_threads.last() {
                            observed_revision = *revision;
                        }
                        changes.extend(changed_threads);
                    }
                    Err(error) => {
                        tracing::warn!(%error, "failed to discover changed thread queues");
                        continue;
                    }
                }
            }
            if !newly_loaded_threads.is_empty() {
                let created_threads = thread_ids
                    .iter()
                    .copied()
                    .filter(|thread_id| newly_loaded_threads.contains(thread_id))
                    .collect::<Vec<_>>();
                match service
                    .queue
                    .changes_since(/*revision*/ 0, &created_threads)
                    .await
                {
                    Ok(changed_threads) => changes.extend(changed_threads),
                    Err(error) => {
                        tracing::warn!(%error, "failed to discover newly loaded thread queues");
                        continue;
                    }
                }
            }
            last_version = Some(version);
            last_revision = observed_revision;
            newly_loaded_threads.clear();
            dispatches.retain(|_, dispatch| !dispatch.is_finished());

            let mut changed_threads = HashSet::new();
            for (thread_id, _) in changes {
                if !changed_threads.insert(thread_id) {
                    continue;
                }
                service.emit_changed(thread_id);
                if dispatches
                    .get(&thread_id)
                    .is_some_and(|dispatch| !dispatch.is_finished())
                {
                    continue;
                }
                let service = Arc::downgrade(&service);
                let dispatch = tokio::spawn(async move {
                    loop {
                        {
                            let Some(service) = service.upgrade() else {
                                return;
                            };
                            let Some(manager) = service.thread_manager.upgrade() else {
                                return;
                            };
                            let Ok(thread) = manager.get_thread(thread_id).await else {
                                return;
                            };
                            if matches!(
                                thread.agent_status().await,
                                AgentStatus::Running
                                    | AgentStatus::Interrupted
                                    | AgentStatus::Shutdown
                                    | AgentStatus::NotFound
                            ) {
                                return;
                            }
                            match service
                                .queue
                                .list_page(thread_id, /*offset*/ 0, /*limit*/ 1)
                                .await
                            {
                                Ok(items) if items.is_empty() => return,
                                Ok(_) => service.wake_if_loaded(thread_id).await,
                                Err(error) => {
                                    tracing::warn!(%thread_id, %error, "failed to check queued user input");
                                }
                            }
                        }
                        tokio::time::sleep(Duration::from_secs(/*secs*/ 10)).await;
                    }
                });
                dispatches.insert(thread_id, dispatch);
            }
        }
    }

    fn dispatch_state(&self, thread_id: ThreadId) -> Arc<QueueDispatchState> {
        let mut states = self
            .dispatch_states
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        states.retain(|_, state| state.strong_count() != 0);
        if let Some(state) = states.get(&thread_id).and_then(Weak::upgrade) {
            return state;
        }
        let state = Arc::new(QueueDispatchState {
            lock: Arc::new(Mutex::new(())),
            pending_enqueues: StdMutex::new(0),
        });
        states.insert(thread_id, Arc::downgrade(&state));
        state
    }

    async fn dispatch_guard(&self, thread_id: ThreadId) -> QueueDispatchGuard {
        let state = self.dispatch_state(thread_id);
        let guard = state.lock.clone().lock_owned().await;
        QueueDispatchGuard {
            _guard: guard,
            _state: state,
        }
    }

    /// Registers a received direct-input enqueue before it can wait behind another request.
    pub fn register_enqueue_intent(&self, thread_id: ThreadId) -> QueueEnqueueIntent {
        let dispatch = self.dispatch_state(thread_id);
        *dispatch
            .pending_enqueues
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) += 1;
        QueueEnqueueIntent {
            thread_id,
            dispatch,
            thread_manager: self.thread_manager.clone(),
            wake_on_drop: true,
        }
    }

    pub async fn enqueue(
        &self,
        thread_id: ThreadId,
        input: TurnInput,
    ) -> Result<QueuedItem, QueueServiceError> {
        self.enqueue_with_intent(self.register_enqueue_intent(thread_id), input)
            .await
    }

    /// Prepares and persists an enqueue request that has already registered its intent.
    pub async fn enqueue_with_intent(
        &self,
        mut intent: QueueEnqueueIntent,
        input: TurnInput,
    ) -> Result<QueuedItem, QueueServiceError> {
        let thread_id = intent.thread_id;
        let input = prepare_queued_user_input(input).await?;
        let dispatch_guard = intent.dispatch.lock.clone().lock_owned().await;
        let payload = serde_json::to_string(&input)?;
        let item = queued_item_from_record(self.queue.enqueue(thread_id, payload).await?)?;
        self.emit_changed(thread_id);
        intent.wake_on_drop = false;
        drop(dispatch_guard);
        drop(intent);
        self.wake_if_loaded(thread_id).await;
        Ok(item)
    }

    pub async fn list(&self, thread_id: ThreadId) -> Result<Vec<QueuedItem>, QueueServiceError> {
        self.list_page(thread_id, /*offset*/ 0, MAX_QUEUE_ITEMS)
            .await
    }

    pub async fn list_page(
        &self,
        thread_id: ThreadId,
        offset: usize,
        limit: usize,
    ) -> Result<Vec<QueuedItem>, QueueServiceError> {
        self.queue
            .list_page(thread_id, offset, limit)
            .await?
            .into_iter()
            .map(queued_item_from_record)
            .collect()
    }

    pub async fn update(
        &self,
        thread_id: ThreadId,
        queued_item_id: String,
        input: TurnInput,
    ) -> Result<Option<QueuedItem>, QueueServiceError> {
        let mut input = prepare_queued_user_input(input).await?;
        let _dispatch_guard = self.dispatch_guard(thread_id).await;
        if let TurnInput::UserInput { client_id, .. } = &mut input {
            *client_id = self
                .list(thread_id)
                .await?
                .into_iter()
                .find_map(|item| match item {
                    QueuedItem {
                        id,
                        input: TurnInput::UserInput { client_id, .. },
                    } if id == queued_item_id => client_id,
                    _ => None,
                });
        }
        let payload = serde_json::to_string(&input)?;
        let item = self
            .queue
            .update(thread_id, queued_item_id, payload)
            .await?
            .map(queued_item_from_record)
            .transpose()?;
        if item.is_some() {
            self.emit_changed(thread_id);
        }
        Ok(item)
    }

    pub async fn delete(
        &self,
        thread_id: ThreadId,
        queued_item_id: String,
    ) -> Result<bool, QueueServiceError> {
        let _dispatch_guard = self.dispatch_guard(thread_id).await;
        self.delete_locked(thread_id, queued_item_id).await
    }

    async fn delete_locked(
        &self,
        thread_id: ThreadId,
        queued_item_id: String,
    ) -> Result<bool, QueueServiceError> {
        let deleted = self.queue.delete(thread_id, queued_item_id).await?;
        if deleted {
            self.emit_changed(thread_id);
        }
        Ok(deleted)
    }

    pub async fn reorder(
        &self,
        thread_id: ThreadId,
        ordered_ids: Vec<String>,
    ) -> Result<(), QueueServiceError> {
        let _dispatch_guard = self.dispatch_guard(thread_id).await;
        self.queue.reorder(thread_id, ordered_ids).await?;
        self.emit_changed(thread_id);
        Ok(())
    }

    /// Starts the selected queued message only when its thread is idle.
    pub async fn start(
        &self,
        thread: &CodexThread,
        queued_item_id: Option<String>,
        trace: Option<W3cTraceContext>,
    ) -> Result<StartIfIdleSubmission, QueueServiceError> {
        let thread_id = thread.session_configured().thread_id;
        let _dispatch_guard = self.dispatch_guard(thread_id).await;
        self.start_locked(thread, queued_item_id, trace).await
    }

    /// Starts the queued head when present, otherwise attempts lower-priority automatic work.
    pub async fn start_queued_or_automatic(
        &self,
        thread: &CodexThread,
        automatic_request: TurnInputRequest,
    ) -> Result<StartIfIdleSubmission, QueueServiceError> {
        let thread_id = thread.session_configured().thread_id;
        let dispatch = self.dispatch_state(thread_id);
        let _dispatch_guard = dispatch.lock.clone().lock_owned().await;
        if *dispatch
            .pending_enqueues
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            != 0
        {
            return Ok(StartIfIdleSubmission::NotSubmitted {
                reason: NotSubmittedReason::PendingTriggerTurn,
            });
        }
        if let Some(queue_reservation) = self.queue.reserve_if_empty(thread_id).await? {
            let admission = Arc::new(QueueAutomaticAdmission {
                dispatch,
                queue_reservation: StdMutex::new(Some(queue_reservation)),
            });
            Ok(thread
                .start_turn_if_idle(automatic_request.with_idle_turn_admission(admission))
                .await?)
        } else {
            self.start_locked(thread, /*queued_item_id*/ None, /*trace*/ None)
                .await
        }
    }

    async fn start_locked(
        &self,
        thread: &CodexThread,
        queued_item_id: Option<String>,
        trace: Option<W3cTraceContext>,
    ) -> Result<StartIfIdleSubmission, QueueServiceError> {
        let thread_id = thread.session_configured().thread_id;
        let item = self
            .list(thread_id)
            .await?
            .into_iter()
            .find(|item| queued_item_id.as_ref().is_none_or(|id| item.id == *id))
            .ok_or_else(|| ThreadStoreError::InvalidRequest {
                message: queued_item_id.as_ref().map_or_else(
                    || "queue is empty".to_string(),
                    |id| format!("queued submission not found: {id}"),
                ),
            })?;
        let queued_item_id = item.id.clone();
        let input @ TurnInput::UserInput { .. } = item.input else {
            return Err(QueueServiceError::InvalidInput);
        };
        let submission = thread
            .start_turn_if_idle(TurnInputRequest::new(input).with_trace(trace))
            .await?;
        if matches!(submission, StartIfIdleSubmission::Started { .. }) {
            self.delete_locked(thread_id, queued_item_id).await?;
        }
        Ok(submission)
    }

    async fn dispatch_if_idle(&self, thread_id: ThreadId) -> Result<bool, QueueServiceError> {
        let Some(manager) = self.thread_manager.upgrade() else {
            return Ok(false);
        };
        let Ok(thread) = manager.get_thread(thread_id).await else {
            return Ok(false);
        };

        loop {
            let Some(record) = self
                .queue
                .list_page(thread_id, /*offset*/ 0, /*limit*/ 1)
                .await?
                .into_iter()
                .next()
            else {
                return Ok(true);
            };
            let queued_item_id = record.id.clone();

            let input = match serde_json::from_str::<TurnInput>(&record.payload) {
                Ok(input) => input,
                Err(error) => {
                    tracing::warn!(%queued_item_id, %error, "discarding invalid queued item");
                    self.delete_locked(thread_id, queued_item_id).await?;
                    continue;
                }
            };
            if !matches!(input, TurnInput::UserInput { .. }) {
                tracing::warn!(%queued_item_id, "discarding non-user queued input");
                self.delete_locked(thread_id, queued_item_id).await?;
                continue;
            }

            match thread
                .start_turn_if_idle(TurnInputRequest::new(input))
                .await
            {
                Ok(StartIfIdleSubmission::Started { .. }) => {
                    self.delete_locked(thread_id, queued_item_id).await?;
                    return Ok(false);
                }
                Ok(StartIfIdleSubmission::NotSubmitted { reason }) => {
                    tracing::warn!(
                        %thread_id,
                        %queued_item_id,
                        ?reason,
                        "core could not start queued user input"
                    );
                    return Ok(false);
                }
                Err(error) => {
                    tracing::warn!(
                        %thread_id,
                        %queued_item_id,
                        %error,
                        "core could not start queued user input"
                    );
                    return Ok(false);
                }
            }
        }
    }

    async fn wake_if_loaded(&self, thread_id: ThreadId) {
        wake_loaded_thread(self.thread_manager.clone(), thread_id).await;
    }

    fn emit_changed(&self, thread_id: ThreadId) {
        self.event_sink.emit(Event {
            id: Uuid::now_v7().to_string(),
            msg: EventMsg::ThreadQueueChanged(ThreadQueueChangedEvent { thread_id }),
        });
    }
}

async fn wake_loaded_thread(thread_manager: Weak<ThreadManager>, thread_id: ThreadId) {
    let Some(manager) = thread_manager.upgrade() else {
        return;
    };
    if let Ok(thread) = manager.get_thread(thread_id).await
        && !matches!(
            thread.agent_status().await,
            AgentStatus::Interrupted | AgentStatus::Shutdown | AgentStatus::NotFound
        )
    {
        thread
            .emit_thread_idle_lifecycle_if_idle(ThreadIdleCause::Completed)
            .await;
    }
}

async fn prepare_queued_user_input(mut input: TurnInput) -> Result<TurnInput, QueueServiceError> {
    let TurnInput::UserInput { content, client_id } = &mut input else {
        return Err(QueueServiceError::InvalidInput);
    };
    if content.is_empty() {
        return Err(QueueServiceError::InvalidInput);
    }
    let actual_chars: usize = content
        .iter()
        .filter_map(|item| match item {
            UserInput::Text { text, .. } => Some(text.chars().count()),
            _ => None,
        })
        .sum();
    if actual_chars > MAX_USER_INPUT_TEXT_CHARS {
        return Err(QueueServiceError::InputTooLarge { actual_chars });
    }
    client_id.get_or_insert_with(|| Uuid::now_v7().to_string());
    if !content.iter().any(|item| {
        matches!(
            item,
            UserInput::LocalImage { .. } | UserInput::LocalAudio { .. }
        )
    }) {
        return Ok(input);
    }

    tokio::task::spawn_blocking(move || {
        let mut input = input;
        if let TurnInput::UserInput { content, .. } = &mut input {
            for item in content {
                snapshot_local_user_input(item)?;
            }
        }
        Ok::<TurnInput, std::io::Error>(input)
    })
    .await
    .map_err(|error| QueueServiceError::InvalidAttachment(std::io::Error::other(error)))?
    .map_err(QueueServiceError::InvalidAttachment)
}

impl<C> ThreadLifecycleContributor<C> for QueuedItemService
where
    C: Send + Sync + 'static,
{
    fn on_thread_resume<'a>(&'a self, input: ThreadResumeInput<'a>) -> ExtensionFuture<'a, ()> {
        Box::pin(async move {
            if let Ok(thread_id) = ThreadId::from_string(input.thread_store.level_id()) {
                self.resumed_threads
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .insert(thread_id);
            }
        })
    }

    fn on_thread_idle<'a>(&'a self, input: ThreadIdleInput<'a>) -> ExtensionFuture<'a, ()> {
        Box::pin(async move {
            if input.cause == ThreadIdleCause::Interrupted {
                return;
            }
            let Ok(thread_id) = ThreadId::from_string(input.thread_store.level_id()) else {
                tracing::warn!(
                    level_id = input.thread_store.level_id(),
                    "queue extension received an invalid thread id"
                );
                return;
            };
            let _guard = self.dispatch_guard(thread_id).await;
            if let Err(error) = self.dispatch_if_idle(thread_id).await {
                tracing::warn!(%thread_id, %error, "failed to dispatch queued user input");
            }
        })
    }
}

fn queued_item_from_record(
    record: QueuedUserSubmissionRecord,
) -> Result<QueuedItem, QueueServiceError> {
    Ok(QueuedItem {
        id: record.id,
        input: serde_json::from_str::<TurnInput>(&record.payload)?,
    })
}
