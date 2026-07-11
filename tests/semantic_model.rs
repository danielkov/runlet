use runlet::*;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

fn object(fields: Vec<(&str, Schema)>) -> Schema {
    let properties = fields
        .into_iter()
        .map(|(k, s)| (k.into(), Property::new(s)))
        .collect::<BTreeMap<_, _>>();
    Schema::Object {
        required: properties.keys().cloned().collect::<BTreeSet<_>>(),
        properties,
        additional: false,
    }
}

fn descriptor(name: &str, input: Vec<Schema>, output: Schema) -> ToolDescriptor {
    ToolDescriptor {
        name: name.into(),
        summary: String::new(),
        input: CallSchema::positional(input),
        output,
        execution: ExecutionPolicy::Pure,
        schema_version: "1".into(),
    }
}

#[test]
fn unresolved_outputs_form_a_lazy_reachable_graph() {
    let mut registry = ToolRegistry::new();
    registry
        .register(descriptor(
            "github.me",
            vec![],
            object(vec![("login", Schema::string())]),
        ))
        .unwrap();
    registry
        .register(descriptor(
            "linear.me",
            vec![],
            object(vec![("id", Schema::string())]),
        ))
        .unwrap();
    registry
        .register(descriptor(
            "github.prs",
            vec![object(vec![("owner", Schema::string())])],
            Schema::list(Schema::string()),
        ))
        .unwrap();
    registry
        .register(descriptor(
            "linear.issues",
            vec![object(vec![("owner", Schema::string())])],
            Schema::list(Schema::string()),
        ))
        .unwrap();
    let calls = Arc::new(Mutex::new(Vec::new()));
    let runtime = Runtime::builder()
        .registry(registry)
        .tool("github.me", |_, _| Ok(VObj::one("login", "octo")))
        .tool("linear.me", |_, _| Ok(VObj::one("id", "usr-1")))
        .tool("github.prs", {
            let calls = calls.clone();
            move |_, _| {
                calls.lock().unwrap().push("prs");
                Ok(CanonicalValue::List(vec!["pr".into()]))
            }
        })
        .tool("linear.issues", {
            let calls = calls.clone();
            move |_, _| {
                calls.lock().unwrap().push("issues");
                Ok(CanonicalValue::List(vec!["issue".into()]))
            }
        })
        .build()
        .unwrap();
    let source = "me = github.me()\nlinear_me = linear.me()\nmy_issues = linear.issues({ owner: linear_me.id })\nmy_prs = github.prs({ owner: me.login })\nreturn my_issues + my_prs";
    let execution = runtime.run(&runtime.compile(source).unwrap()).unwrap();
    assert_eq!(
        execution.value,
        CanonicalValue::List(vec!["issue".into(), "pr".into()])
    );
    assert_eq!(calls.lock().unwrap().len(), 2);
    assert_eq!(
        execution
            .graph
            .nodes
            .iter()
            .filter(|n| n.kind == NodeKind::Call)
            .count(),
        4
    );
}

#[test]
fn unreachable_effect_is_rejected_and_never_runs() {
    let mut registry = ToolRegistry::new();
    registry
        .register(descriptor("audit.log", vec![], Schema::Boolean))
        .unwrap();
    let runtime = Runtime::builder()
        .registry(registry)
        .tool("audit.log", |_, _| panic!("must not run"))
        .build()
        .unwrap();
    let diagnostics = runtime
        .compile("unused = audit.log()\nreturn true")
        .unwrap_err();
    assert!(diagnostics.iter().any(|d| d.code == "RL1204"));
}

#[test]
fn loops_are_ordered_and_boundaries_catch_failures() {
    let mut registry = ToolRegistry::new();
    registry
        .register(descriptor("work", vec![Schema::INTEGER], Schema::INTEGER))
        .unwrap();
    let runtime = Runtime::builder()
        .registry(registry)
        .tool("work", |args, _| match args[0] {
            CanonicalValue::Integer(2) => Err(ToolError::new("BOOM", "failed")),
            CanonicalValue::Integer(x) => Ok(CanonicalValue::Integer(x * 2)),
            _ => unreachable!(),
        })
        .build()
        .unwrap();
    let source="result = boundary {\n values = for item in [1, 2, 3] limit 2 { return work(item) }\n return values\n} catch err { return [err.code] }\nreturn result";
    let execution = runtime.run(&runtime.compile(source).unwrap()).unwrap();
    assert_eq!(execution.value, CanonicalValue::List(vec!["BOOM".into()]));
}

#[test]
fn boundary_retry_reuses_successful_operations() {
    let mut registry = ToolRegistry::new();
    registry
        .register(descriptor("stable", vec![], Schema::INTEGER))
        .unwrap();
    registry
        .register(descriptor("flaky", vec![], Schema::INTEGER))
        .unwrap();
    let stable_calls = Arc::new(Mutex::new(0));
    let flaky_calls = Arc::new(Mutex::new(0));
    let runtime = Runtime::builder()
        .registry(registry)
        .tool("stable", {
            let calls = stable_calls.clone();
            move |_, _| {
                *calls.lock().unwrap() += 1;
                Ok(1.into())
            }
        })
        .tool("flaky", {
            let calls = flaky_calls.clone();
            move |_, _| {
                let mut n = calls.lock().unwrap();
                *n += 1;
                if *n == 1 {
                    Err(ToolError::new("TEMP", "try again").retryable(true))
                } else {
                    Ok(2.into())
                }
            }
        })
        .build()
        .unwrap();
    let source="result = boundary retry 1 { a = stable()\n b = flaky()\n return a + b } catch err { return -1 }\nreturn result";
    let execution = runtime.run(&runtime.compile(source).unwrap()).unwrap();
    assert_eq!(execution.value, 3.into());
    assert_eq!(*stable_calls.lock().unwrap(), 1);
    assert_eq!(*flaky_calls.lock().unwrap(), 2);
    assert!(execution
        .graph
        .edges
        .iter()
        .any(|edge| edge.kind == EdgeKind::RetryOf));
}

#[test]
fn conversions_and_short_circuiting_are_observable() {
    let mut registry = ToolRegistry::new();
    registry
        .register(descriptor("accept", vec![Schema::NUMBER], Schema::NUMBER))
        .unwrap();
    registry
        .register(descriptor("must_not_run", vec![], Schema::Boolean))
        .unwrap();
    let runtime = Runtime::builder()
        .registry(registry)
        .tool("accept", |args, _| Ok(args[0].clone()))
        .tool("must_not_run", |_, _| panic!("short-circuited call ran"))
        .build()
        .unwrap();
    let execution=runtime.run(&runtime.compile("number = accept(42)\nflag = false and must_not_run()\nreturn { number, label: \"count: \" + 3, flag }").unwrap()).unwrap();
    let CanonicalValue::Object(value) = execution.value else {
        panic!()
    };
    assert_eq!(value["number"], CanonicalValue::Number(42.0));
    assert_eq!(value["label"], CanonicalValue::String("count: 3".into()));
    assert!(execution
        .graph
        .nodes
        .iter()
        .any(|n| n.kind == NodeKind::Convert));
}

#[test]
fn loop_limits_bound_parallel_execution_and_stream_graph_events() {
    let mut registry = ToolRegistry::new();
    registry
        .register(descriptor("slow", vec![Schema::INTEGER], Schema::INTEGER))
        .unwrap();
    let active = Arc::new(AtomicUsize::new(0));
    let maximum = Arc::new(AtomicUsize::new(0));
    let runtime = Runtime::builder()
        .registry(registry)
        .tool("slow", {
            let active = active.clone();
            let maximum = maximum.clone();
            move |args, _| {
                let now = active.fetch_add(1, Ordering::SeqCst) + 1;
                maximum.fetch_max(now, Ordering::SeqCst);
                std::thread::sleep(Duration::from_millis(30));
                active.fetch_sub(1, Ordering::SeqCst);
                Ok(args[0].clone())
            }
        })
        .build()
        .unwrap();
    let program = runtime
        .compile("values = for x in [1, 2, 3, 4, 5, 6] limit 3 { return slow(x) }\nreturn values")
        .unwrap();
    let events = Arc::new(Mutex::new(Vec::new()));
    let execution = runtime
        .run_observed(&program, {
            let events = events.clone();
            move |event| events.lock().unwrap().push(event.clone())
        })
        .unwrap();

    assert_eq!(
        execution.value,
        CanonicalValue::List((1..=6).map(CanonicalValue::Integer).collect())
    );
    assert_eq!(maximum.load(Ordering::SeqCst), 3);
    let events = events.lock().unwrap();
    assert!(events
        .windows(2)
        .all(|pair| pair[0].sequence < pair[1].sequence));
    assert!(events.iter().any(|event| matches!(
        event.change,
        GraphChange::NodeUpdated(ref node) if node.state == NodeState::Running
    )));
}

#[test]
fn producer_consumer_edges_are_recorded_for_tool_chains() {
    let mut registry = ToolRegistry::new();
    registry
        .register(descriptor("step", vec![Schema::INTEGER], Schema::INTEGER))
        .unwrap();
    let runtime = Runtime::builder()
        .registry(registry)
        .tool("step", |args, _| Ok(args[0].clone()))
        .build()
        .unwrap();
    let execution = runtime
        .run(
            &runtime
                .compile("first = step(1)\nsecond = step(first)\nreturn second")
                .unwrap(),
        )
        .unwrap();
    let calls = execution
        .graph
        .nodes
        .iter()
        .filter(|node| node.kind == NodeKind::Call)
        .collect::<Vec<_>>();
    assert_eq!(calls.len(), 2);
    assert!(execution.graph.edges.iter().any(|edge| {
        calls.iter().any(|node| node.id == edge.from)
            && calls.iter().any(|node| node.id == edge.to)
            && edge.from != edge.to
            && matches!(edge.kind, EdgeKind::Data { .. })
    }));
}

struct VObj;
impl VObj {
    fn one(key: &str, value: &str) -> CanonicalValue {
        CanonicalValue::Object(BTreeMap::from([(key.into(), value.into())]))
    }
}
