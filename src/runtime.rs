use crate::analyzer::{compile, CompiledProgram, ExternalInput};
use crate::{
    BinaryOp, Block, CanonicalValue as V, Diagnostic, Expr, ExprKind, Graph, NodeKind, Schema,
    Span, ToolRegistry, UnaryOp,
};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use thiserror::Error;

#[derive(Debug, Clone)]
pub struct ToolContext {
    pub node_id: String,
    pub operation_id: String,
    pub dispatch_id: String,
    pub attempt: u32,
    pub schema_version: String,
}

#[derive(Debug, Clone, Error)]
#[error("{code}: {message}")]
pub struct ToolError {
    pub code: String,
    pub message: String,
    pub retryable: bool,
    pub uncertain: bool,
    pub details: BTreeMap<String, V>,
}
impl ToolError {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            retryable: false,
            uncertain: false,
            details: BTreeMap::new(),
        }
    }
    pub fn retryable(mut self, yes: bool) -> Self {
        self.retryable = yes;
        self
    }
}

type Handler = Arc<dyn Fn(&[V], &ToolContext) -> Result<V, ToolError> + Send + Sync>;

#[derive(Clone)]
pub struct Runtime {
    registry: ToolRegistry,
    handlers: BTreeMap<String, Handler>,
    inputs: BTreeMap<String, (Schema, V)>,
    default_loop_limit: u32,
    max_loop_limit: u32,
}
pub struct RuntimeBuilder {
    runtime: Runtime,
}

impl Runtime {
    pub fn builder() -> RuntimeBuilder {
        RuntimeBuilder {
            runtime: Runtime {
                registry: ToolRegistry::new(),
                handlers: BTreeMap::new(),
                inputs: BTreeMap::new(),
                default_loop_limit: 16,
                max_loop_limit: 1024,
            },
        }
    }
    pub fn compile(&self, source: &str) -> Result<CompiledProgram, Vec<Diagnostic>> {
        let inputs = self
            .inputs
            .iter()
            .map(|(name, (schema, _))| ExternalInput {
                name: name.clone(),
                schema: schema.clone(),
            })
            .collect::<Vec<_>>();
        compile(source, &self.registry, &inputs)
    }
    pub fn run(&self, program: &CompiledProgram) -> Result<Execution, ToolError> {
        if program.registry_digest != self.registry.digest() {
            return Err(ToolError::new(
                "RL7102",
                "compiled program registry digest does not match this runtime",
            ));
        }
        let mut ev = Evaluator {
            runtime: self,
            graph: Graph::default(),
            scopes: vec![],
            attempt: 0,
            owners: vec![],
            workflow: program.source_digest.clone(),
            dynamic: vec![],
            successful_operations: HashMap::new(),
            dispatch_generations: HashMap::new(),
        };
        let mut root = HashMap::new();
        for (n, (_, v)) in &self.inputs {
            root.insert(n.clone(), Binding::Value(v.clone()));
        }
        for s in &program.program.statements {
            root.insert(s.name.clone(), Binding::Expr(s.value.clone()));
        }
        ev.scopes.push(root);
        let node = ev
            .graph
            .begin(NodeKind::Root, "return", program.program.result.span, 0);
        ev.graph.running(node);
        match ev.eval(&program.program.result) {
            Ok(v) => {
                ev.graph.success(node, v.clone());
                Ok(Execution {
                    value: v,
                    graph: ev.graph,
                })
            }
            Err(e) => {
                ev.graph.fail(node, e.to_string());
                Err(e)
            }
        }
    }
}
impl RuntimeBuilder {
    pub fn registry(mut self, registry: ToolRegistry) -> Self {
        self.runtime.registry = registry;
        self
    }
    pub fn input(mut self, name: impl Into<String>, schema: Schema, value: V) -> Self {
        self.runtime.inputs.insert(name.into(), (schema, value));
        self
    }
    pub fn tool<F>(mut self, name: impl Into<String>, handler: F) -> Self
    where
        F: Fn(&[V], &ToolContext) -> Result<V, ToolError> + Send + Sync + 'static,
    {
        self.runtime.handlers.insert(name.into(), Arc::new(handler));
        self
    }
    pub fn loop_limits(mut self, default: u32, max: u32) -> Self {
        self.runtime.default_loop_limit = default;
        self.runtime.max_loop_limit = max;
        self
    }
    pub fn build(self) -> Result<Runtime, String> {
        for n in self.runtime.registry.names() {
            if !self.runtime.handlers.contains_key(n) {
                return Err(format!("registered tool `{n}` has no implementation"));
            }
        }
        for n in self.runtime.handlers.keys() {
            if self.runtime.registry.get(n).is_none() {
                return Err(format!("implementation `{n}` has no descriptor"));
            }
        }
        for (n, (s, v)) in &self.runtime.inputs {
            if !s.accepts(v) {
                return Err(format!("input `{n}` does not match its schema"));
            }
        }
        Ok(self.runtime)
    }
}

#[derive(Debug, Clone)]
pub struct Execution {
    pub value: V,
    pub graph: Graph,
}
#[derive(Clone)]
enum Binding {
    Expr(Expr),
    Value(V),
    Evaluating,
}
struct Evaluator<'a> {
    runtime: &'a Runtime,
    graph: Graph,
    scopes: Vec<HashMap<String, Binding>>,
    attempt: u32,
    owners: Vec<usize>,
    workflow: String,
    dynamic: Vec<String>,
    successful_operations: HashMap<String, V>,
    dispatch_generations: HashMap<String, u32>,
}

impl Evaluator<'_> {
    fn eval(&mut self, e: &Expr) -> Result<V, ToolError> {
        match &e.kind {
            ExprKind::Null => Ok(V::Null),
            ExprKind::Boolean(v) => Ok(V::Boolean(*v)),
            ExprKind::Integer(s) => s
                .parse()
                .map(V::Integer)
                .map_err(|_| lang("RL5101", "NUMERIC_OVERFLOW")),
            ExprKind::Number(s) => {
                V::number(s.parse().map_err(|_| lang("RL5103", "NON_FINITE_NUMBER"))?)
                    .map_err(|x| lang("RL5103", &x.to_string()))
            }
            ExprKind::String(s) => Ok(V::String(s.clone())),
            ExprKind::Name(n) => self.name(n),
            ExprKind::List(xs) => {
                let node = self.node(NodeKind::Composite, "list", e.span);
                let r = xs
                    .iter()
                    .map(|x| self.eval(x))
                    .collect::<Result<Vec<_>, _>>()
                    .map(V::List);
                self.finish(node, r)
            }
            ExprKind::Object(xs) => {
                let node = self.node(NodeKind::Composite, "object", e.span);
                let mut o = BTreeMap::new();
                for (k, x) in xs {
                    o.insert(
                        k.clone(),
                        match self.eval(x) {
                            Ok(v) => v,
                            Err(err) => {
                                self.graph.fail(node, err.to_string());
                                return Err(err);
                            }
                        },
                    );
                }
                self.finish(node, Ok(V::Object(o)))
            }
            ExprKind::Member { target, field } => {
                let node = self.node(NodeKind::Project, format!(".{field}"), e.span);
                let r = self
                    .eval(target)
                    .and_then(|v| project(v, &V::String(field.clone())));
                self.finish(node, r)
            }
            ExprKind::Index { target, index } => {
                let node = self.node(NodeKind::Project, "index", e.span);
                let r = (|| {
                    let v = self.eval(target)?;
                    let i = self.eval(index)?;
                    project(v, &i)
                })();
                self.finish(node, r)
            }
            ExprKind::Call { callee, arguments } => self.call(e.span, callee, arguments),
            ExprKind::Unary { op, value } => {
                let node = self.node(NodeKind::Compute, format!("{op:?}"), e.span);
                let r = self.eval(value).and_then(|v| unary(*op, v));
                self.finish(node, r)
            }
            ExprKind::Binary { op, left, right } => self.binary(e.span, *op, left, right),
            ExprKind::Conditional {
                then_expr,
                condition,
                else_expr,
            } => {
                let node = self.node(NodeKind::Branch, "if", e.span);
                let c = self.eval(condition)?;
                let chosen = match c {
                    V::Boolean(true) => then_expr,
                    V::Boolean(false) => else_expr,
                    _ => {
                        let er = lang("RL5202", "EXPECTED_BOOLEAN");
                        self.graph.fail(node, er.to_string());
                        return Err(er);
                    }
                };
                let r = self.eval(chosen);
                self.finish(node, r)
            }
            ExprKind::For {
                binding,
                collection,
                limit,
                body,
            } => self.loop_expr(
                e.span,
                binding,
                collection,
                limit.unwrap_or(self.runtime.default_loop_limit),
                body,
            ),
            ExprKind::Boundary {
                retries,
                body,
                error_binding,
                catch,
            } => self.boundary(e.span, *retries, body, error_binding, catch),
        }
    }
    fn name(&mut self, n: &str) -> Result<V, ToolError> {
        let found = (0..self.scopes.len())
            .rev()
            .find(|&i| self.scopes[i].contains_key(n))
            .ok_or_else(|| lang("RL8101", &format!("unknown runtime name `{n}`")))?;
        let binding = self.scopes[found].remove(n).unwrap();
        match binding {
            Binding::Value(v) => {
                self.scopes[found].insert(n.into(), Binding::Value(v.clone()));
                Ok(v)
            }
            Binding::Evaluating => Err(lang("RL4104", "internal binding cycle")),
            Binding::Expr(e) => {
                self.scopes[found].insert(n.into(), Binding::Evaluating);
                let r = self.eval(&e);
                match &r {
                    Ok(v) => {
                        self.scopes[found].insert(n.into(), Binding::Value(v.clone()));
                    }
                    Err(_) => {
                        self.scopes[found].insert(n.into(), Binding::Expr(e));
                    }
                }
                r
            }
        }
    }
    fn call(&mut self, span: Span, callee: &Expr, args: &[Expr]) -> Result<V, ToolError> {
        let name = path(callee).ok_or_else(|| lang("RL2104", "value is not callable"))?;
        let desc = self
            .runtime
            .registry
            .get(&name)
            .ok_or_else(|| lang("RL2102", &format!("unknown tool `{name}`")))?;
        let node = self.node(NodeKind::Call, &name, span);
        let mut values = vec![];
        for (a, expected) in args.iter().zip(&desc.input.parameters) {
            match self.eval(a).and_then(|v| self.convert(a.span, v, expected)) {
                Ok(v) => values.push(v),
                Err(e) => {
                    self.graph.fail(node, e.to_string());
                    return Err(e);
                }
            }
        }
        if values.len() != desc.input.parameters.len() {
            let e = lang("RL6102", "TOOL_INPUT_SCHEMA_MISMATCH");
            self.graph.fail(node, e.to_string());
            return Err(e);
        }
        self.graph.nodes[node].state = crate::NodeState::Dispatching;
        let canonical = V::List(values.clone())
            .rcve()
            .map_err(|x| lang("RL5207", &x.to_string()))?;
        let identity = format!(
            "{}\0{}\0{}\0{}\0{}",
            self.workflow,
            name,
            desc.schema_version,
            span.start,
            self.dynamic.join("/")
        );
        let op = hex::encode(Sha256::digest([identity.as_bytes(), &canonical].concat()));
        if let Some(value) = self.successful_operations.get(&op).cloned() {
            return self.finish(node, Ok(value));
        }
        let generation = self.dispatch_generations.entry(op.clone()).or_default();
        let dispatch_id = format!("{op}:{}", *generation);
        *generation += 1;
        let ctx = ToolContext {
            node_id: self.graph.nodes[node].id.clone(),
            operation_id: op.clone(),
            dispatch_id,
            attempt: self.attempt,
            schema_version: desc.schema_version.clone(),
        };
        self.graph.running(node);
        let handler = self
            .runtime
            .handlers
            .get(&name)
            .ok_or_else(|| lang("RL8103", "missing tool implementation"))?;
        let mut r = handler(&values, &ctx).and_then(|v| {
            if desc.output.accepts(&v) {
                Ok(v)
            } else {
                Err(lang("RL6103", "TOOL_OUTPUT_SCHEMA_MISMATCH"))
            }
        });
        if let Err(error) = &mut r {
            error.retryable &= matches!(
                desc.execution,
                crate::ExecutionPolicy::Pure
                    | crate::ExecutionPolicy::Idempotent
                    | crate::ExecutionPolicy::Recoverable
            );
        }
        if let Ok(value) = &r {
            self.successful_operations.insert(op, value.clone());
        }
        self.finish(node, r)
    }
    fn binary(
        &mut self,
        span: Span,
        op: BinaryOp,
        left: &Expr,
        right: &Expr,
    ) -> Result<V, ToolError> {
        let node = self.node(
            if matches!(op, BinaryOp::And | BinaryOp::Or) {
                NodeKind::Branch
            } else {
                NodeKind::Compute
            },
            format!("{op:?}"),
            span,
        );
        let l = self.eval(left)?;
        if let V::Boolean(b) = l {
            if op == BinaryOp::And && !b {
                return self.finish(node, Ok(V::Boolean(false)));
            }
            if op == BinaryOp::Or && b {
                return self.finish(node, Ok(V::Boolean(true)));
            }
        }
        let r = self.eval(right)?;
        let out = binary(op, l, r);
        self.finish(node, out)
    }
    fn convert(&mut self, span: Span, value: V, expected: &Schema) -> Result<V, ToolError> {
        if expected.accepts(&value) {
            return Ok(value);
        }
        let node = self.node(NodeKind::Convert, "convert", span);
        let result = convert_value(value, expected);
        self.finish(node, result)
    }
    fn loop_expr(
        &mut self,
        span: Span,
        binding: &str,
        collection: &Expr,
        limit: u32,
        body: &Block,
    ) -> Result<V, ToolError> {
        if limit == 0 || limit > self.runtime.max_loop_limit {
            return Err(lang("RL4102", "loop limit outside host policy"));
        }
        let node = self.node(NodeKind::Loop, format!("for limit {limit}"), span);
        let c = self.eval(collection)?;
        let values = match c {
            V::List(x) => x,
            V::Object(o) => o
                .into_iter()
                .map(|(k, v)| {
                    V::Object(BTreeMap::from([
                        ("key".into(), V::String(k)),
                        ("value".into(), v),
                    ]))
                })
                .collect(),
            _ => {
                let e = lang("RL5204", "NOT_ITERABLE");
                self.graph.fail(node, e.to_string());
                return Err(e);
            }
        };
        let mut out = Vec::with_capacity(values.len());
        for (i, v) in values.into_iter().enumerate() {
            let it = self.node(NodeKind::Iteration, format!("iteration {i}"), body.span);
            self.graph.contains(node, it);
            self.owners.push(it);
            self.dynamic
                .push(format!("{i}:{}", v.digest_hex().unwrap_or_default()));
            self.scopes
                .push(HashMap::from([(binding.into(), Binding::Value(v))]));
            let r = self.block(body);
            self.scopes.pop();
            self.dynamic.pop();
            self.owners.pop();
            match r {
                Ok(v) => {
                    self.graph.success(it, v.clone());
                    out.push(v)
                }
                Err(e) => {
                    self.graph.fail(it, e.to_string());
                    self.graph.fail(node, e.to_string());
                    return Err(e);
                }
            }
        }
        self.finish(node, Ok(V::List(out)))
    }
    fn boundary(
        &mut self,
        span: Span,
        retries: u32,
        body: &Block,
        error_binding: &str,
        catch: &Block,
    ) -> Result<V, ToolError> {
        let node = self.node(
            NodeKind::Boundary,
            format!("boundary retry {retries}"),
            span,
        );
        let mut last = None;
        for attempt in 0..=retries {
            self.attempt = attempt;
            self.owners.push(node);
            let r = self.block(body);
            self.owners.pop();
            match r {
                Ok(v) => {
                    self.attempt = 0;
                    return self.finish(node, Ok(v));
                }
                Err(e) => {
                    let retry = e.retryable && attempt < retries;
                    last = Some(e);
                    if !retry {
                        break;
                    }
                }
            }
        }
        let e = last.unwrap();
        let error = error_value(&e, self.attempt);
        self.scopes.push(HashMap::from([(
            error_binding.into(),
            Binding::Value(error),
        )]));
        self.owners.push(node);
        let r = self.block(catch);
        self.owners.pop();
        self.scopes.pop();
        self.attempt = 0;
        self.finish(node, r)
    }
    fn block(&mut self, b: &Block) -> Result<V, ToolError> {
        let mut scope = HashMap::new();
        for s in &b.statements {
            scope.insert(s.name.clone(), Binding::Expr(s.value.clone()));
        }
        self.scopes.push(scope);
        let r = self.eval(&b.result);
        self.scopes.pop();
        r
    }
    fn node(&mut self, k: NodeKind, l: impl Into<String>, s: Span) -> usize {
        let i = self.graph.begin(k, l, s, self.attempt);
        self.graph.running(i);
        if let Some(&p) = self.owners.last() {
            self.graph.contains(p, i)
        }
        i
    }
    fn finish(&mut self, node: usize, r: Result<V, ToolError>) -> Result<V, ToolError> {
        match &r {
            Ok(v) => self.graph.success(node, v.clone()),
            Err(e) => self.graph.fail(node, e.to_string()),
        }
        r
    }
}

fn path(e: &Expr) -> Option<String> {
    match &e.kind {
        ExprKind::Name(n) => Some(n.clone()),
        ExprKind::Member { target, field } => Some(format!("{}.{}", path(target)?, field)),
        _ => None,
    }
}
fn lang(code: &str, msg: &str) -> ToolError {
    ToolError::new(code, msg)
}
fn project(v: V, i: &V) -> Result<V, ToolError> {
    match (v, i) {
        (V::List(x), V::Integer(i)) => idx(x.len(), *i)
            .and_then(|n| x.get(n).cloned())
            .ok_or_else(|| lang("RL5205", "INDEX_OUT_OF_BOUNDS")),
        (V::String(s), V::Integer(i)) => {
            let x = s.chars().collect::<Vec<_>>();
            idx(x.len(), *i)
                .and_then(|n| x.get(n))
                .map(|c| V::String(c.to_string()))
                .ok_or_else(|| lang("RL5205", "INDEX_OUT_OF_BOUNDS"))
        }
        (V::Bytes(x), V::Integer(i)) => idx(x.len(), *i)
            .and_then(|n| x.get(n))
            .map(|x| V::Integer(*x as i64))
            .ok_or_else(|| lang("RL5205", "INDEX_OUT_OF_BOUNDS")),
        (V::Object(mut o), V::String(k)) => o
            .remove(k)
            .ok_or_else(|| lang("RL5206", &format!("KEY_NOT_FOUND: `{k}`"))),
        _ => Err(lang("RL5203", "NOT_INDEXABLE")),
    }
}
fn idx(len: usize, i: i64) -> Option<usize> {
    let n = if i < 0 { len as i64 + i } else { i };
    (n >= 0 && n < len as i64).then_some(n as usize)
}
fn unary(op: UnaryOp, v: V) -> Result<V, ToolError> {
    match (op, v) {
        (UnaryOp::Not, V::Boolean(x)) => Ok(V::Boolean(!x)),
        (UnaryOp::Negate, V::Integer(x)) => x
            .checked_neg()
            .map(V::Integer)
            .ok_or_else(|| lang("RL5101", "NUMERIC_OVERFLOW")),
        (UnaryOp::Negate, V::Number(x)) => {
            V::number(-x).map_err(|_| lang("RL5103", "NON_FINITE_NUMBER"))
        }
        _ => Err(lang("RL5201", "INVALID_OPERAND")),
    }
}
fn binary(op: BinaryOp, l: V, r: V) -> Result<V, ToolError> {
    use BinaryOp::*;
    match (op, l, r) {
        (Add, V::Integer(a), V::Integer(b)) => a
            .checked_add(b)
            .map(V::Integer)
            .ok_or_else(|| lang("RL5101", "NUMERIC_OVERFLOW")),
        (Subtract, V::Integer(a), V::Integer(b)) => a
            .checked_sub(b)
            .map(V::Integer)
            .ok_or_else(|| lang("RL5101", "NUMERIC_OVERFLOW")),
        (Multiply, V::Integer(a), V::Integer(b)) => a
            .checked_mul(b)
            .map(V::Integer)
            .ok_or_else(|| lang("RL5101", "NUMERIC_OVERFLOW")),
        (Divide, _, V::Integer(0))
        | (Divide, _, V::Number(0.0))
        | (Remainder, _, V::Integer(0)) => Err(lang("RL5102", "DIVISION_BY_ZERO")),
        (Remainder, V::Integer(a), V::Integer(b)) => a
            .checked_rem(b)
            .map(V::Integer)
            .ok_or_else(|| lang("RL5101", "NUMERIC_OVERFLOW")),
        (Add, V::String(a), V::String(b)) => Ok(V::String(a + &b)),
        (Add, V::String(a), b) => Ok(V::String(a + &format_value(&b)?)),
        (Add, a, V::String(b)) => Ok(V::String(format_value(&a)? + &b)),
        (Add, V::List(mut a), V::List(b)) => {
            a.extend(b);
            Ok(V::List(a))
        }
        (Equal, V::Integer(a), V::Number(b)) | (Equal, V::Number(b), V::Integer(a)) => {
            Ok(V::Boolean(exact(a)? == b))
        }
        (NotEqual, V::Integer(a), V::Number(b)) | (NotEqual, V::Number(b), V::Integer(a)) => {
            Ok(V::Boolean(exact(a)? != b))
        }
        (Equal, a, b) => Ok(V::Boolean(a == b)),
        (NotEqual, a, b) => Ok(V::Boolean(a != b)),
        (And, V::Boolean(a), V::Boolean(b)) => Ok(V::Boolean(a && b)),
        (Or, V::Boolean(a), V::Boolean(b)) => Ok(V::Boolean(a || b)),
        (In, a, V::List(b)) => Ok(V::Boolean(b.contains(&a))),
        (In, V::String(a), V::String(b)) => Ok(V::Boolean(b.contains(&a))),
        (In, V::String(a), V::Object(b)) => Ok(V::Boolean(b.contains_key(&a))),
        (op, a, b)
            if matches!(
                op,
                Add | Subtract | Multiply | Divide | Less | LessEqual | Greater | GreaterEqual
            ) =>
        {
            numeric_binary(op, a, b)
        }
        _ => Err(lang("RL5201", "INVALID_OPERANDS")),
    }
}
fn numeric_binary(op: BinaryOp, a: V, b: V) -> Result<V, ToolError> {
    let (a, b) = match (a, b) {
        (V::Integer(a), V::Integer(b)) => (a as f64, b as f64),
        (V::Integer(a), V::Number(b)) => (exact(a)?, b),
        (V::Number(a), V::Integer(b)) => (a, exact(b)?),
        (V::Number(a), V::Number(b)) => (a, b),
        (V::String(a), V::String(b)) => {
            return Ok(V::Boolean(match op {
                BinaryOp::Less => a < b,
                BinaryOp::LessEqual => a <= b,
                BinaryOp::Greater => a > b,
                BinaryOp::GreaterEqual => a >= b,
                _ => false,
            }))
        }
        _ => return Err(lang("RL5201", "INVALID_NUMERIC_OPERANDS")),
    };
    use BinaryOp::*;
    match op {
        Add => V::number(a + b),
        Subtract => V::number(a - b),
        Multiply => V::number(a * b),
        Divide => V::number(a / b),
        Less => return Ok(V::Boolean(a < b)),
        LessEqual => return Ok(V::Boolean(a <= b)),
        Greater => return Ok(V::Boolean(a > b)),
        GreaterEqual => return Ok(V::Boolean(a >= b)),
        _ => unreachable!(),
    }
    .map_err(|_| lang("RL5103", "NON_FINITE_NUMBER"))
}
fn exact(x: i64) -> Result<f64, ToolError> {
    let f = x as f64;
    if f as i64 == x {
        Ok(f)
    } else {
        Err(lang("RL5208", "LOSSY_NUMERIC_WIDENING"))
    }
}
fn format_value(v: &V) -> Result<String, ToolError> {
    match v {
        V::Null | V::Bytes(_) => Err(lang("RL5207", "NOT_JSON_REPRESENTABLE")),
        V::String(s) => Ok(s.clone()),
        V::Integer(x) => Ok(x.to_string()),
        V::Boolean(x) => Ok(x.to_string()),
        V::Number(_) | V::List(_) | V::Object(_) => v
            .presentation_json()
            .map_err(|e| lang("RL5207", &e.to_string())),
    }
}
fn convert_value(value: V, expected: &Schema) -> Result<V, ToolError> {
    if expected.accepts(&value) {
        return Ok(value);
    }
    match (value, expected) {
        (V::Integer(x), Schema::Number { .. }) => {
            V::number(exact(x)?).map_err(|_| lang("RL5103", "NON_FINITE_NUMBER"))
        }
        (
            v @ (V::Integer(_) | V::Number(_) | V::Boolean(_) | V::List(_) | V::Object(_)),
            Schema::String { .. },
        ) => Ok(V::String(format_value(&v)?)),
        (V::List(values), Schema::List { items, .. }) => values
            .into_iter()
            .map(|v| convert_value(v, items))
            .collect::<Result<Vec<_>, _>>()
            .map(V::List),
        (
            V::Object(values),
            Schema::Object {
                properties,
                required,
                additional,
            },
        ) => {
            if !required.iter().all(|key| values.contains_key(key)) {
                return Err(lang("RL5208", "missing required object property"));
            }
            let mut out = BTreeMap::new();
            for (key, value) in values {
                match properties.get(&key) {
                    Some(property) => {
                        out.insert(key, convert_value(value, &property.schema)?);
                    }
                    None if *additional => {
                        out.insert(key, value);
                    }
                    None => return Err(lang("RL5208", "unexpected object property")),
                }
            }
            Ok(V::Object(out))
        }
        (value, Schema::Union { variants, .. }) => {
            let converted = variants
                .iter()
                .filter_map(|schema| convert_value(value.clone(), schema).ok())
                .collect::<Vec<_>>();
            if converted.len() == 1 {
                Ok(converted.into_iter().next().unwrap())
            } else {
                Err(lang("RL5208", "ambiguous or invalid union conversion"))
            }
        }
        _ => Err(lang("RL5208", "value cannot be safely converted")),
    }
}
fn error_value(e: &ToolError, attempt: u32) -> V {
    let mut o = e.details.clone();
    for (k, v) in [
        ("code", V::String(e.code.clone())),
        ("message", V::String(e.message.clone())),
        ("retryable", V::Boolean(e.retryable)),
        ("node_id", V::String(String::new())),
        ("attempt", V::Integer(attempt as i64)),
        ("uncertain", V::Boolean(e.uncertain)),
    ] {
        o.insert(k.into(), v);
    }
    V::Object(o)
}
