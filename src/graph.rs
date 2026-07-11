use crate::{CanonicalValue, Span};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeKind {
    Root,
    Call,
    Compute,
    Convert,
    Project,
    Composite,
    Branch,
    Loop,
    Iteration,
    Boundary,
    External,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeState {
    Planned,
    Blocked,
    Ready,
    Dispatching,
    Running,
    Succeeded,
    Failed,
    Cancelling,
    Cancelled,
    Pruned,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Node {
    pub id: String,
    pub kind: NodeKind,
    pub label: String,
    pub span: Span,
    pub state: NodeState,
    pub attempt: u32,
    pub output: Option<CanonicalValue>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum EdgeKind {
    Data {
        producer_path: String,
        consumer_path: String,
    },
    Control {
        condition: String,
    },
    Contains,
    Orders,
    RetryOf,
    FallbackOf,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Edge {
    pub from: String,
    pub to: String,
    pub kind: EdgeKind,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Graph {
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
}

impl Graph {
    pub(crate) fn begin(
        &mut self,
        kind: NodeKind,
        label: impl Into<String>,
        span: Span,
        attempt: u32,
    ) -> usize {
        let i = self.nodes.len();
        self.nodes.push(Node {
            id: format!("n{i:08x}"),
            kind,
            label: label.into(),
            span,
            state: NodeState::Ready,
            attempt,
            output: None,
            error: None,
        });
        i
    }
    pub(crate) fn running(&mut self, i: usize) {
        self.nodes[i].state = NodeState::Running
    }
    pub(crate) fn success(&mut self, i: usize, v: CanonicalValue) {
        self.nodes[i].state = NodeState::Succeeded;
        self.nodes[i].output = Some(v)
    }
    pub(crate) fn fail(&mut self, i: usize, e: impl Into<String>) {
        self.nodes[i].state = NodeState::Failed;
        self.nodes[i].error = Some(e.into())
    }
    pub(crate) fn contains(&mut self, parent: usize, child: usize) {
        let from = self.nodes[parent].id.clone();
        let to = self.nodes[child].id.clone();
        self.edges.push(Edge {
            from,
            to,
            kind: EdgeKind::Contains,
        })
    }
}
