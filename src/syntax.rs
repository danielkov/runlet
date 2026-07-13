use crate::Span;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Program {
    pub statements: Vec<Stmt>,
    pub result: Expr,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Block {
    pub statements: Vec<Stmt>,
    pub result: Box<Expr>,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Stmt {
    pub kind: StmtKind,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum StmtKind {
    Binding {
        name: String,
        value: Expr,
    },
    /// `skip [if condition]` — abandons the current `for` iteration (the
    /// element is dropped from the loop result) or `fold` iteration (the
    /// accumulator passes through unchanged). Only valid inside loop bodies
    /// and never across a `boundary`.
    Skip {
        condition: Option<Expr>,
    },
}

impl Stmt {
    /// The name/value pair when this statement is a binding.
    pub fn binding(&self) -> Option<(&String, &Expr)> {
        match &self.kind {
            StmtKind::Binding { name, value } => Some((name, value)),
            StmtKind::Skip { .. } => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Expr {
    pub kind: ExprKind,
    pub span: Span,
}

/// An object-literal property key: written literally (`name:` / `"name":`)
/// or computed at runtime (`[expr]:`). Computed keys evaluate to a string;
/// scalar values (integers, numbers, booleans) convert to their canonical
/// text form. When keys collide, the last entry wins.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ObjectKey {
    Static(String),
    Computed(Expr),
}

impl ObjectKey {
    /// The key expression when this key is computed.
    pub fn expr(&self) -> Option<&Expr> {
        match self {
            ObjectKey::Static(_) => None,
            ObjectKey::Computed(e) => Some(e),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ExprKind {
    Null,
    Boolean(bool),
    Integer(String),
    Number(String),
    String(String),
    Name(String),
    List(Vec<Expr>),
    Object(Vec<(ObjectKey, Expr)>),
    Member {
        target: Box<Expr>,
        field: String,
    },
    Index {
        target: Box<Expr>,
        index: Box<Expr>,
    },
    Call {
        callee: Box<Expr>,
        arguments: Vec<Expr>,
    },
    Unary {
        op: UnaryOp,
        value: Box<Expr>,
    },
    Binary {
        op: BinaryOp,
        left: Box<Expr>,
        right: Box<Expr>,
    },
    Conditional {
        then_expr: Box<Expr>,
        condition: Box<Expr>,
        else_expr: Box<Expr>,
    },
    /// `if condition { ... } else { ... }` — the block-bodied conditional
    /// expression, for branches too large for a postfix conditional. Each
    /// branch is a full block ending in `return`; only the selected branch
    /// evaluates (its effect roots included). A missing `else` yields
    /// `null`. `else if` chains parse as an else block whose result is a
    /// nested `If`.
    If {
        condition: Box<Expr>,
        then_block: Block,
        else_block: Option<Block>,
    },
    For {
        binding: String,
        collection: Box<Expr>,
        limit: Option<u32>,
        body: Block,
    },
    /// `fold acc = init for item in items { ... return next_acc }` — a
    /// sequential left fold. Each iteration binds a fresh accumulator; the
    /// body's return becomes the next accumulator; an empty collection yields
    /// the initial value. The sequential counterpart to `for`.
    Fold {
        accumulator: String,
        init: Box<Expr>,
        binding: String,
        collection: Box<Expr>,
        body: Block,
    },
    /// `fail(code, message[, details])` — raises a catchable, non-retryable
    /// error, exactly like a failing tool call. Type `Never`.
    Fail {
        arguments: Vec<Expr>,
    },
    Boundary {
        retries: u32,
        body: Block,
        error_binding: String,
        catch: Block,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum UnaryOp {
    Negate,
    Not,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BinaryOp {
    Add,
    Subtract,
    Multiply,
    Divide,
    Remainder,
    Equal,
    NotEqual,
    Less,
    LessEqual,
    Greater,
    GreaterEqual,
    In,
    And,
    Or,
}
