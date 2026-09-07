//! Bounded, value-free observation of one execution.
//!
//! Node IDs and sequence numbers are local to one run. Hosts must namespace
//! them with their compose-call identity AND execution incarnation, and retain
//! the exact compiled source for byte spans. Neither a span nor a tool name is
//! a dynamic execution identity. Unobserved expressions have unknown state;
//! absence is not evidence of pruning, blocking, or completion.

use crate::{Edge, EdgeKind, GraphChange, Node, NodeKind, NodeState, Span};
use serde::{Deserialize, Serialize};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc::{self, Receiver, SyncSender, TrySendError},
};

/// Value-free lifecycle state. Structural/computation `Running` means an
/// evaluation scope is active (possibly evaluating children), NOT a handler.
/// For calls (including intrinsics), `Running` is emitted with a dispatch permit
/// held, immediately before invoking the handler. It does not imply host code
/// has already executed. The permit remains held through terminal publication.
/// Cached calls go directly to `Succeeded`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProgressState {
    /// Created, not yet considered for evaluation.
    Planned,
    /// Arguments/dependencies are unresolved; no specific blocker is claimed.
    Blocked,
    /// Eligible for evaluation.
    Ready,
    /// Call has resolved inputs and needs a dispatch permit; not executing.
    WaitingForCapacity,
    /// Active evaluation, or a permitted call about to invoke its handler.
    Running,
    /// Completed successfully (possibly from cache).
    Succeeded,
    /// Completed with a typed runtime failure; error contents are omitted.
    Failed,
    /// Cancellation requested.
    Cancelling,
    /// Cancelled.
    Cancelled,
    /// Explicitly excluded by the runtime, never inferred from absence.
    Pruned,
}

impl From<NodeState> for ProgressState {
    fn from(state: NodeState) -> Self {
        match state {
            NodeState::Planned => Self::Planned,
            NodeState::Blocked => Self::Blocked,
            NodeState::Ready => Self::Ready,
            NodeState::Dispatching => Self::WaitingForCapacity,
            NodeState::Running => Self::Running,
            NodeState::Succeeded => Self::Succeeded,
            NodeState::Failed => Self::Failed,
            NodeState::Cancelling => Self::Cancelling,
            NodeState::Cancelled => Self::Cancelled,
            NodeState::Pruned => Self::Pruned,
        }
    }
}

/// Metadata for a unique dynamic node. Deliberately excludes labels, values,
/// errors, tool names, operation hashes, and dispatch IDs derived from inputs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProgressNode {
    /// Run-local identity; joins directly to [`crate::ToolContext::node_id`].
    pub id: String,
    /// Semantic role, not an inference from descendants.
    pub kind: NodeKind,
    /// Byte span in the exact compiled source.
    pub span: Span,
    /// Authoritative state at this event's sequence.
    pub state: ProgressState,
    /// Zero-based enclosing retry attempt; not a dispatch generation.
    pub attempt: u32,
}

impl From<&Node> for ProgressNode {
    fn from(node: &Node) -> Self {
        Self {
            id: node.id.clone(),
            kind: node.kind,
            span: node.span,
            state: node.state.into(),
            attempt: node.attempt,
        }
    }
}

/// Relationship without value-bearing paths or condition labels. Edges do not
/// assert that a consumer is currently waiting on the producer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProgressEdgeKind {
    /// Producer to consumer; known value provenance, not a live wait reason.
    Data,
    /// Conditional gating; condition contents omitted.
    Control,
    /// Container to child (including loop to iteration and iteration to call).
    Contains,
    /// Prerequisite to dependent.
    Orders,
    /// New attempt to previous attempt (the runtime's actual edge direction).
    RetryOf,
    /// Failure recovery relationship.
    FallbackOf,
}

/// Run-local directed relationship.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProgressEdge {
    /// Source node ID.
    pub from: String,
    /// Destination node ID.
    pub to: String,
    /// Value-free relationship kind.
    pub kind: ProgressEdgeKind,
}

impl From<&Edge> for ProgressEdge {
    fn from(edge: &Edge) -> Self {
        Self {
            from: edge.from.clone(),
            to: edge.to.clone(),
            kind: match edge.kind {
                EdgeKind::Data { .. } => ProgressEdgeKind::Data,
                EdgeKind::Control { .. } => ProgressEdgeKind::Control,
                EdgeKind::Contains => ProgressEdgeKind::Contains,
                EdgeKind::Orders => ProgressEdgeKind::Orders,
                EdgeKind::RetryOf => ProgressEdgeKind::RetryOf,
                EdgeKind::FallbackOf => ProgressEdgeKind::FallbackOf,
            },
        }
    }
}

/// Normal run outcome, without result or error contents.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProgressOutcome {
    /// Run returned successfully.
    Succeeded,
    /// Run returned an error, including a rejected registry digest.
    Failed,
}

/// One value-free mutation or run outcome.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum ProgressChange {
    /// Initial state, without a synthetic ready transition.
    NodeAdded(ProgressNode),
    /// Updated metadata (may equal previous metadata after a label/value change).
    NodeUpdated(ProgressNode),
    /// A known relationship was added.
    EdgeAdded(ProgressEdge),
    /// Normal completion, not a synthetic terminal update for unfinished nodes.
    Finished(ProgressOutcome),
}

impl From<&GraphChange> for ProgressChange {
    fn from(change: &GraphChange) -> Self {
        match change {
            GraphChange::NodeAdded(n) => Self::NodeAdded(n.into()),
            GraphChange::NodeUpdated(n) => Self::NodeUpdated(n.into()),
            GraphChange::EdgeAdded(e) => Self::EdgeAdded(e.into()),
        }
    }
}

/// Ordered metadata from one run; never contains arguments, results or errors.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProgressEvent {
    /// Contiguous sequence starting at 1. No events are resumed after overflow.
    pub sequence: u64,
    /// Metadata change.
    pub change: ProgressChange,
}

/// End of a progress stream. Receiving a prefix never proves current state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ProgressRecvError {
    /// The terminal `Finished` event was consumed.
    #[error("progress stream completed")]
    Closed,
    /// Buffer filled; queued prefix is complete, but the remainder was lost.
    /// Execution continues. There is no replay/resynchronization API.
    #[error("progress buffer overflowed; state after the received prefix is unknown")]
    Lagged,
    /// Publisher dropped without a terminal event (e.g. unwind or unused sender).
    #[error("progress publisher closed without a terminal event")]
    Incomplete,
}

/// One-shot publisher consumed by [`crate::Runtime::run_with_progress`].
/// Not cloneable: each channel belongs to exactly one observation run.
pub struct ProgressSender {
    sender: Option<SyncSender<ProgressEvent>>,
    lagged: Arc<AtomicBool>,
}

/// Host-owned receiver. Consume concurrently for live progress; neither a slow
/// consumer nor its panic/drop blocks or cancels execution. Capacity bounds
/// queued events, not the runtime's existing execution graph. No hidden replay
/// buffer exists. Use `try_recv` when integrating with a host event loop.
pub struct ProgressReceiver {
    receiver: Receiver<ProgressEvent>,
    lagged: Arc<AtomicBool>,
    finished: bool,
}

/// Creates a bounded, nonblocking-on-publication stream. `capacity` must be
/// positive (zero panics). Choose a capacity and drain rate appropriate to the
/// host; overflow terminates observation, NOT execution. Compilation itself
/// emits no events: compile before creating the channel.
pub fn progress_channel(capacity: usize) -> (ProgressSender, ProgressReceiver) {
    assert!(capacity > 0, "progress capacity must be positive");
    let (sender, receiver) = mpsc::sync_channel(capacity);
    let lagged = Arc::new(AtomicBool::new(false));
    (
        ProgressSender {
            sender: Some(sender),
            lagged: lagged.clone(),
        },
        ProgressReceiver {
            receiver,
            lagged,
            finished: false,
        },
    )
}

impl ProgressSender {
    pub(crate) fn send(&mut self, event: ProgressEvent) {
        let Some(sender) = &self.sender else {
            return;
        };
        match sender.try_send(event) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => {
                self.lagged.store(true, Ordering::Release);
                self.sender = None;
            }
            Err(TrySendError::Disconnected(_)) => self.sender = None,
        }
    }
}

impl ProgressReceiver {
    fn disconnected(&self) -> ProgressRecvError {
        if self.lagged.load(Ordering::Acquire) {
            ProgressRecvError::Lagged
        } else {
            ProgressRecvError::Incomplete
        }
    }

    fn received(&mut self, event: ProgressEvent) -> ProgressEvent {
        self.finished = matches!(event.change, ProgressChange::Finished(_));
        event
    }

    /// Waits for the next event, or reports terminal/gap/incomplete status after
    /// the queued prefix is drained. Does not wait for execution after overflow.
    pub fn recv(&mut self) -> Result<ProgressEvent, ProgressRecvError> {
        if self.finished {
            return Err(ProgressRecvError::Closed);
        }
        let event = self.receiver.recv().map_err(|_| self.disconnected())?;
        Ok(self.received(event))
    }

    /// Returns `Ok(None)` only when the stream is open but currently empty.
    pub fn try_recv(&mut self) -> Result<Option<ProgressEvent>, ProgressRecvError> {
        if self.finished {
            return Err(ProgressRecvError::Closed);
        }
        match self.receiver.try_recv() {
            Ok(event) => Ok(Some(self.received(event))),
            Err(mpsc::TryRecvError::Empty) => Ok(None),
            Err(mpsc::TryRecvError::Disconnected) => Err(self.disconnected()),
        }
    }
}
