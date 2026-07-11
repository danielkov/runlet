use crate::{
    parse, BinaryOp, Diagnostic, Expr, ExprKind, Phase, Program, Schema, Severity, Span,
    ToolRegistry, UnaryOp,
};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone)]
pub struct ExternalInput {
    pub name: String,
    pub schema: Schema,
}

#[derive(Debug, Clone)]
pub struct CompiledProgram {
    pub(crate) program: Program,
    pub source: String,
    pub source_digest: String,
    pub registry_digest: String,
    pub diagnostics: Vec<Diagnostic>,
}

pub fn compile(
    source: &str,
    registry: &ToolRegistry,
    inputs: &[ExternalInput],
) -> Result<CompiledProgram, Vec<Diagnostic>> {
    let program = parse(source)?;
    let mut a = Analyzer {
        registry,
        diagnostics: vec![],
        scopes: vec![],
        registry_roots: registry.roots().into_iter().map(str::to_owned).collect(),
    };
    let mut root = BTreeMap::new();
    for i in inputs {
        if root.insert(i.name.clone(), i.schema.clone()).is_some() {
            a.diagnostics.push(Diagnostic::error(
                "RL8102",
                Phase::Analyze,
                Span::default(),
                "duplicate host input",
                format!("host input `{}` is defined more than once", i.name),
            ));
        }
    }
    a.scopes.push(root);
    a.analyze_program(&program);
    if a.diagnostics.iter().any(|d| d.severity == Severity::Error) {
        return Err(a.diagnostics);
    }
    Ok(CompiledProgram {
        program,
        source: source.into(),
        source_digest: hex::encode(Sha256::digest(source.as_bytes())),
        registry_digest: registry.digest(),
        diagnostics: a.diagnostics,
    })
}

struct Analyzer<'a> {
    registry: &'a ToolRegistry,
    diagnostics: Vec<Diagnostic>,
    scopes: Vec<BTreeMap<String, Schema>>,
    registry_roots: BTreeSet<String>,
}
impl Analyzer<'_> {
    fn analyze_program(&mut self, p: &Program) {
        self.block_bindings(&p.statements, &p.result, false);
    }
    fn block_bindings(&mut self, stmts: &[crate::Stmt], result: &Expr, push: bool) -> Schema {
        if push {
            self.scopes.push(BTreeMap::new())
        }
        let scope = self.scopes.len() - 1;
        let mut defs = BTreeMap::new();
        for s in stmts {
            if self.registry_roots.contains(&s.name) {
                self.diagnostics.push(
                    Diagnostic::error(
                        "RL2107",
                        Phase::Analyze,
                        s.span,
                        "binding collides with registry root",
                        format!("`{}` is a registered namespace root", s.name),
                    )
                    .with_fix(
                        Span::new(s.span.start, s.span.start + s.name.len()),
                        format!("{}_value", s.name),
                        "rename the binding",
                    ),
                );
            }
            if defs.insert(s.name.clone(), &s.value).is_some()
                || self.scopes[scope].contains_key(&s.name)
            {
                self.diagnostics.push(
                    Diagnostic::error(
                        "RL2106",
                        Phase::Analyze,
                        s.span,
                        "duplicate binding",
                        format!("`{}` is already declared in this scope", s.name),
                    )
                    .with_fix(s.span, "", "remove the duplicate binding"),
                );
                continue;
            }
            let ty = self.expr(&s.value);
            self.scopes[scope].insert(s.name.clone(), ty);
        }
        let ty = self.expr(result);
        let mut used = BTreeSet::new();
        collect_names(result, &defs, &mut used);
        for s in stmts {
            if !used.contains(&s.name) {
                let effect = contains_call(&s.value);
                let (code, title, msg) = if effect {
                    (
                        "RL1204",
                        "unreachable tool call",
                        "this effect is not reachable from the block return and will not run",
                    )
                } else {
                    (
                        "RL1205",
                        "unused binding",
                        "this local computation is pruned",
                    )
                };
                let mut d = if effect {
                    Diagnostic::error(code, Phase::Analyze, s.span, title, msg)
                } else {
                    Diagnostic::warning(code, Phase::Analyze, s.span, title, msg)
                };
                d = d.with_fix(s.span, "", "remove the unused binding");
                self.diagnostics.push(d)
            }
        }
        if push {
            self.scopes.pop();
        }
        ty
    }
    fn expr(&mut self, e: &Expr) -> Schema {
        match &e.kind {
            ExprKind::Null => Schema::Null,
            ExprKind::Boolean(_) => Schema::Boolean,
            ExprKind::Integer(s) => {
                if s.parse::<i64>().is_err() {
                    self.err(
                        "RL2301",
                        e.span,
                        "integer literal is outside the signed 64-bit range",
                    );
                }
                Schema::INTEGER
            }
            ExprKind::Number(s) => {
                if s.parse::<f64>().is_err() || !s.parse::<f64>().is_ok_and(f64::is_finite) {
                    self.err(
                        "RL2302",
                        e.span,
                        "number literal is not a finite binary64 value",
                    );
                }
                Schema::NUMBER
            }
            ExprKind::String(_) => Schema::string(),
            ExprKind::Name(n) => self.lookup(n).unwrap_or_else(|| {
                let c = closest(n, self.visible_names());
                self.diagnostics.push(
                    Diagnostic::error(
                        "RL2101",
                        Phase::Analyze,
                        e.span,
                        "unknown name",
                        format!("`{n}` is not a local, host input, or registered callable"),
                    )
                    .with_candidates(c),
                );
                Schema::Any
            }),
            ExprKind::List(xs) => {
                let ts = xs.iter().map(|x| self.expr(x)).collect::<Vec<_>>();
                Schema::list(union(ts))
            }
            ExprKind::Object(xs) => {
                let mut p = BTreeMap::new();
                let mut r = BTreeSet::new();
                for (k, v) in xs {
                    p.insert(k.clone(), crate::Property::new(self.expr(v)));
                    r.insert(k.clone());
                }
                Schema::Object {
                    properties: p,
                    required: r,
                    additional: false,
                }
            }
            ExprKind::Member { target, field } => {
                if let Some(path) = path_of(e) {
                    let prefix = path + ".";
                    if self.registry.names().any(|n| n.starts_with(&prefix)) {
                        return Schema::Any;
                    }
                }
                let t = self.expr(target);
                project_schema(&t, field).unwrap_or_else(|| {
                    self.diagnostics.push(
                        Diagnostic::error(
                            "RL2103",
                            Phase::Analyze,
                            e.span,
                            "unknown property",
                            format!("property `{field}` is not available on this value"),
                        )
                        .with_candidates(schema_fields(&t)),
                    );
                    Schema::Any
                })
            }
            ExprKind::Index { target, index } => {
                let t = self.expr(target);
                let i = self.expr(index);
                match t {
                    Schema::List { items, .. } if matches!(i, Schema::Integer { .. }) => *items,
                    Schema::String { .. } if matches!(i, Schema::Integer { .. }) => {
                        Schema::string()
                    }
                    Schema::Bytes if matches!(i, Schema::Integer { .. }) => Schema::INTEGER,
                    Schema::Map { values } if matches!(i, Schema::String { .. }) => *values,
                    Schema::Object { ref properties, .. } if matches!(i, Schema::String { .. }) => {
                        union(properties.values().map(|p| p.schema.clone()).collect())
                    }
                    Schema::Any => Schema::Any,
                    _ => {
                        self.err(
                            "RL2308",
                            e.span,
                            "target and index schemas are incompatible",
                        );
                        Schema::Any
                    }
                }
            }
            ExprKind::Call { callee, arguments } => {
                let Some(name) = path_of(callee) else {
                    self.err(
                        "RL2104",
                        callee.span,
                        "values are not callable; use a registered tool name",
                    );
                    return Schema::Any;
                };
                let Some(tool) = self.registry.get(&name) else {
                    self.diagnostics.push(
                        Diagnostic::error(
                            "RL2102",
                            Phase::Analyze,
                            callee.span,
                            "unknown callable",
                            format!("`{name}` is not registered"),
                        )
                        .with_candidates(closest(
                            &name,
                            self.registry.names().map(str::to_owned).collect(),
                        )),
                    );
                    return Schema::Any;
                };
                if arguments.len() != tool.input.parameters.len() {
                    self.err(
                        "RL2208",
                        e.span,
                        &format!(
                            "`{name}` expects {} arguments but received {}",
                            tool.input.parameters.len(),
                            arguments.len()
                        ),
                    );
                }
                for (a, expected) in arguments.iter().zip(&tool.input.parameters) {
                    let actual = self.expr(a);
                    if conversion_rank(&actual, expected).is_none() {
                        self.err(
                            "RL2311",
                            a.span,
                            "argument cannot be safely converted to the tool parameter schema",
                        )
                    }
                }
                tool.output.clone()
            }
            ExprKind::Unary { op, value } => {
                let t = self.expr(value);
                match (op, &t) {
                    (UnaryOp::Not, Schema::Boolean) => Schema::Boolean,
                    (UnaryOp::Negate, Schema::Integer { .. }) => Schema::INTEGER,
                    (UnaryOp::Negate, Schema::Number { .. }) => Schema::NUMBER,
                    _ => {
                        self.err("RL2304", e.span, "operator does not accept this schema");
                        Schema::Any
                    }
                }
            }
            ExprKind::Binary { op, left, right } => {
                let l = self.expr(left);
                if matches!(op, BinaryOp::And | BinaryOp::Or)
                    && !matches!(l, Schema::Boolean | Schema::Any)
                {
                    self.err(
                        "RL2305",
                        left.span,
                        "Boolean operator requires Boolean operands",
                    )
                }
                let r = self.expr(right);
                binary_schema(*op, &l, &r).unwrap_or_else(|| {
                    self.err(
                        "RL2304",
                        e.span,
                        "operator does not accept these operand schemas",
                    );
                    Schema::Any
                })
            }
            ExprKind::Conditional {
                then_expr,
                condition,
                else_expr,
            } => {
                let c = self.expr(condition);
                if !matches!(c, Schema::Boolean | Schema::Any) {
                    self.err("RL2305", condition.span, "condition must be Boolean")
                }
                let a = self.expr(then_expr);
                let b = self.expr(else_expr);
                union(vec![a, b])
            }
            ExprKind::For {
                binding,
                collection,
                limit,
                body,
            } => {
                if limit.is_some_and(|x| x == 0 || x > 1024) {
                    self.err("RL4102", e.span, "loop limit must be between 1 and 1024")
                }
                let c = self.expr(collection);
                let item = match c {
                    Schema::List { items, .. } => *items,
                    Schema::Map { values } => entry_schema(*values),
                    Schema::Object { properties, .. } => entry_schema(union(
                        properties.values().map(|p| p.schema.clone()).collect(),
                    )),
                    Schema::Any => Schema::Any,
                    _ => {
                        self.err(
                            "RL2309",
                            collection.span,
                            "for collection must be a list, object, or map",
                        );
                        Schema::Any
                    }
                };
                self.scopes.push(BTreeMap::from([(binding.clone(), item)]));
                let out = self.block_bindings(&body.statements, &body.result, false);
                self.scopes.pop();
                Schema::list(out)
            }
            ExprKind::Boundary {
                body,
                error_binding,
                catch,
                ..
            } => {
                self.scopes.push(BTreeMap::new());
                let a = self.block_bindings(&body.statements, &body.result, false);
                self.scopes.pop();
                self.scopes
                    .push(BTreeMap::from([(error_binding.clone(), error_schema())]));
                let b = self.block_bindings(&catch.statements, &catch.result, false);
                self.scopes.pop();
                union(vec![a, b])
            }
        }
    }
    fn lookup(&self, n: &str) -> Option<Schema> {
        self.scopes.iter().rev().find_map(|s| s.get(n).cloned())
    }
    fn visible_names(&self) -> Vec<String> {
        self.scopes
            .iter()
            .flat_map(|s| s.keys().cloned())
            .chain(self.registry.names().map(str::to_owned))
            .collect()
    }
    fn err(&mut self, code: &str, span: Span, msg: &str) {
        self.diagnostics.push(
            Diagnostic::error(code, Phase::Analyze, span, "schema error", msg).with_fix(
                span,
                "",
                "change this expression to an accepted shape",
            ),
        )
    }
}
fn path_of(e: &Expr) -> Option<String> {
    match &e.kind {
        ExprKind::Name(n) => Some(n.clone()),
        ExprKind::Member { target, field } => Some(format!("{}.{}", path_of(target)?, field)),
        _ => None,
    }
}
fn contains_call(e: &Expr) -> bool {
    match &e.kind {
        ExprKind::Call { .. } => true,
        ExprKind::List(x) => x.iter().any(contains_call),
        ExprKind::Object(x) => x.iter().any(|(_, e)| contains_call(e)),
        ExprKind::Member { target, .. } | ExprKind::Unary { value: target, .. } => {
            contains_call(target)
        }
        ExprKind::Index { target, index }
        | ExprKind::Binary {
            left: target,
            right: index,
            ..
        } => contains_call(target) || contains_call(index),
        ExprKind::Conditional {
            then_expr,
            condition,
            else_expr,
        } => [then_expr, condition, else_expr]
            .into_iter()
            .any(|x| contains_call(x)),
        ExprKind::For {
            collection, body, ..
        } => {
            contains_call(collection)
                || body.statements.iter().any(|s| contains_call(&s.value))
                || contains_call(&body.result)
        }
        ExprKind::Boundary { body, catch, .. } => [body, catch].into_iter().any(|b| {
            b.statements.iter().any(|s| contains_call(&s.value)) || contains_call(&b.result)
        }),
        _ => false,
    }
}
fn collect_names(e: &Expr, defs: &BTreeMap<String, &Expr>, used: &mut BTreeSet<String>) {
    match &e.kind {
        ExprKind::Name(n) => {
            let first_reference = used.insert(n.clone());
            if let (true, Some(x)) = (first_reference, defs.get(n)) {
                collect_names(x, defs, used)
            }
        }
        ExprKind::List(x) => x.iter().for_each(|e| collect_names(e, defs, used)),
        ExprKind::Object(x) => x.iter().for_each(|(_, e)| collect_names(e, defs, used)),
        ExprKind::Member { target, .. } | ExprKind::Unary { value: target, .. } => {
            collect_names(target, defs, used)
        }
        ExprKind::Index { target, index }
        | ExprKind::Binary {
            left: target,
            right: index,
            ..
        } => {
            collect_names(target, defs, used);
            collect_names(index, defs, used)
        }
        ExprKind::Call { arguments, .. } => {
            arguments.iter().for_each(|e| collect_names(e, defs, used))
        }
        ExprKind::Conditional {
            then_expr,
            condition,
            else_expr,
        } => [then_expr, condition, else_expr]
            .into_iter()
            .for_each(|e| collect_names(e, defs, used)),
        ExprKind::For {
            collection, body, ..
        } => {
            collect_names(collection, defs, used);
            collect_names(&body.result, defs, used)
        }
        ExprKind::Boundary { body, catch, .. } => {
            collect_names(&body.result, defs, used);
            collect_names(&catch.result, defs, used)
        }
        _ => {}
    }
}
fn union(mut x: Vec<Schema>) -> Schema {
    if x.is_empty() {
        return Schema::Any;
    }
    x.dedup();
    if x.len() == 1 {
        x.remove(0)
    } else {
        Schema::Union {
            variants: x,
            discriminator: None,
        }
    }
}
fn project_schema(s: &Schema, f: &str) -> Option<Schema> {
    match s {
        Schema::Object { properties, .. } => properties.get(f).map(|p| p.schema.clone()),
        Schema::Map { values } => Some(*values.clone()),
        Schema::Any => Some(Schema::Any),
        Schema::Union { variants, .. } => {
            let x = variants
                .iter()
                .map(|s| project_schema(s, f))
                .collect::<Option<Vec<_>>>()?;
            Some(union(x))
        }
        _ => None,
    }
}
fn schema_fields(s: &Schema) -> Vec<String> {
    match s {
        Schema::Object { properties, .. } => properties.keys().cloned().collect(),
        _ => vec![],
    }
}
fn conversion_rank(a: &Schema, e: &Schema) -> Option<u8> {
    if a == e || matches!(e, Schema::Any) {
        Some(1)
    } else if matches!(
        (a, e),
        (Schema::Integer { .. }, Schema::Number { .. })
            | (
                Schema::Integer { .. } | Schema::Number { .. } | Schema::Boolean,
                Schema::String { .. }
            )
    ) {
        Some(3)
    } else if matches!(e,Schema::Union{variants,..} if variants.iter().any(|v|conversion_rank(a,v).is_some()))
    {
        Some(2)
    } else {
        None
    }
}
fn binary_schema(op: BinaryOp, l: &Schema, r: &Schema) -> Option<Schema> {
    use BinaryOp::*;
    match op {
        And | Or if matches!((l, r), (Schema::Boolean, Schema::Boolean)) => Some(Schema::Boolean),
        Equal | NotEqual => Some(Schema::Boolean),
        Less | LessEqual | Greater | GreaterEqual
            if numeric(l) && numeric(r)
                || matches!((l, r), (Schema::String { .. }, Schema::String { .. })) =>
        {
            Some(Schema::Boolean)
        }
        In => Some(Schema::Boolean),
        Add if matches!((l, r), (Schema::String { .. }, Schema::String { .. })) => {
            Some(Schema::string())
        }
        Add if matches!(l, Schema::String { .. }) && string_formattable(r)
            || matches!(r, Schema::String { .. }) && string_formattable(l) =>
        {
            Some(Schema::string())
        }
        Add if matches!((l, r), (Schema::List { .. }, Schema::List { .. })) => {
            Some(Schema::list(Schema::Any))
        }
        Add | Subtract | Multiply if integer(l) && integer(r) => Some(Schema::INTEGER),
        Add | Subtract | Multiply | Divide if numeric(l) && numeric(r) => Some(Schema::NUMBER),
        Remainder if integer(l) && integer(r) => Some(Schema::INTEGER),
        _ => None,
    }
}
fn numeric(s: &Schema) -> bool {
    integer(s) || matches!(s, Schema::Number { .. })
}
fn integer(s: &Schema) -> bool {
    matches!(s, Schema::Integer { .. })
}
fn string_formattable(s: &Schema) -> bool {
    matches!(
        s,
        Schema::Integer { .. }
            | Schema::Number { .. }
            | Schema::Boolean
            | Schema::String { .. }
            | Schema::List { .. }
            | Schema::Object { .. }
            | Schema::Map { .. }
    )
}
fn entry_schema(v: Schema) -> Schema {
    let mut p = BTreeMap::new();
    p.insert("key".into(), crate::Property::new(Schema::string()));
    p.insert("value".into(), crate::Property::new(v));
    Schema::Object {
        properties: p,
        required: BTreeSet::from(["key".into(), "value".into()]),
        additional: false,
    }
}
fn error_schema() -> Schema {
    let mut p = BTreeMap::new();
    for (k, s) in [
        ("code", Schema::string()),
        ("message", Schema::string()),
        ("retryable", Schema::Boolean),
        ("node_id", Schema::string()),
        ("attempt", Schema::INTEGER),
        ("uncertain", Schema::Boolean),
    ] {
        p.insert(k.into(), crate::Property::new(s));
    }
    Schema::Object {
        required: p.keys().cloned().collect(),
        properties: p,
        additional: true,
    }
}
fn closest(s: &str, mut names: Vec<String>) -> Vec<String> {
    names.sort_by_key(|n| lev(s, n));
    names.truncate(5);
    names
}
fn lev(a: &str, b: &str) -> usize {
    let mut d = (0..=b.chars().count()).collect::<Vec<_>>();
    for (i, x) in a.chars().enumerate() {
        let mut last = i;
        d[0] = i + 1;
        for (j, y) in b.chars().enumerate() {
            let old = d[j + 1];
            d[j + 1] = (d[j + 1] + 1).min(d[j] + 1).min(last + usize::from(x != y));
            last = old
        }
    }
    *d.last().unwrap()
}
