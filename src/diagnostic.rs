use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub struct Span {
    pub start: usize,
    pub end: usize,
}

impl Span {
    pub const fn new(start: usize, end: usize) -> Self {
        Self { start, end }
    }
    pub fn join(self, other: Self) -> Self {
        Self {
            start: self.start.min(other.start),
            end: self.end.max(other.end),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Error,
    Warning,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Parse,
    Analyze,
    Plan,
    Execute,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Fix {
    pub span: Span,
    pub replacement: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Diagnostic {
    pub code: String,
    pub title: String,
    pub message: String,
    pub phase: Phase,
    pub severity: Severity,
    pub primary_span: Span,
    #[serde(default)]
    pub fixes: Vec<Fix>,
    #[serde(default)]
    pub candidates: Vec<String>,
}

impl Diagnostic {
    pub fn error(
        code: &str,
        phase: Phase,
        span: Span,
        title: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            code: code.into(),
            title: title.into(),
            message: message.into(),
            phase,
            severity: Severity::Error,
            primary_span: span,
            fixes: vec![],
            candidates: vec![],
        }
    }
    pub fn warning(
        code: &str,
        phase: Phase,
        span: Span,
        title: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        let mut d = Self::error(code, phase, span, title, message);
        d.severity = Severity::Warning;
        d
    }
    pub fn with_fix(
        mut self,
        span: Span,
        replacement: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        self.fixes.push(Fix {
            span,
            replacement: replacement.into(),
            message: message.into(),
        });
        self
    }
    pub fn with_candidates(mut self, candidates: Vec<String>) -> Self {
        self.candidates = candidates;
        self
    }
}
