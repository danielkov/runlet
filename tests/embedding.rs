//! Host-embedding surface: JSON Schema lowering, JSON values, static tool
//! references, and run cancellation.

use runlet::*;
use serde_json::json;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

fn descriptor(name: &str, parameters: Vec<Schema>, execution: ExecutionPolicy) -> ToolDescriptor {
    ToolDescriptor {
        name: name.into(),
        summary: String::new(),
        input: CallSchema::positional(parameters),
        output: Schema::Any,
        execution,
        schema_version: "1".into(),
    }
}

fn value(json: serde_json::Value) -> CanonicalValue {
    CanonicalValue::from_json(json).unwrap()
}

#[test]
fn json_schema_lowers_a_typical_tool_schema() {
    let schema = Schema::from_json_schema(&json!({
        "type": "object",
        "properties": {
            "query": { "type": "string", "description": "Search text", "minLength": 1 },
            "state": { "enum": ["open", "closed"] },
            "limit": { "type": "integer", "minimum": 1, "maximum": 100 },
            "score": { "type": "number" },
            "assignee": { "type": ["string", "null"] },
            "owner": { "$ref": "#/$defs/user", "nullable": true },
            "labels": { "type": "array", "items": { "$ref": "#/$defs/label" }, "maxItems": 5 },
            "meta": { "type": "object", "additionalProperties": { "type": "boolean" } },
            "anything": {}
        },
        "required": ["query"],
        "additionalProperties": false,
        "$defs": {
            "label": { "type": "string" },
            "user": { "type": "object", "properties": { "id": { "type": "integer" } } }
        }
    }));
    let Schema::Object {
        properties,
        required,
        additional,
    } = &schema
    else {
        panic!("expected an object schema, got {schema:?}");
    };
    assert!(!additional);
    assert_eq!(required.iter().collect::<Vec<_>>(), ["query"]);
    assert_eq!(properties["query"].documentation, "Search text");
    assert_eq!(
        properties["state"].schema.type_notation(),
        r#""open" | "closed""#
    );
    assert_eq!(
        properties["limit"].schema,
        Schema::Integer {
            min: Some(1),
            max: Some(100)
        }
    );
    assert_eq!(properties["score"].schema.type_notation(), "int | number");
    assert_eq!(
        properties["assignee"].schema.type_notation(),
        "string | null"
    );
    assert_eq!(properties["labels"].schema.type_notation(), "string[]");
    assert_eq!(
        properties["meta"].schema,
        Schema::Map {
            values: Box::new(Schema::Boolean)
        }
    );
    assert_eq!(properties["anything"].schema, Schema::Any);

    // Widening only: everything the JSON Schema accepts is accepted.
    for accepted in [
        json!({ "query": "x" }),
        json!({ "query": "x", "score": 3, "assignee": null, "owner": null }),
        json!({ "query": "x", "score": 0.5, "owner": { "id": 1, "extra": true } }),
        json!({ "query": "x", "labels": ["a"], "meta": { "k": true }, "anything": [1] }),
    ] {
        assert!(schema.accepts(&value(accepted.clone())), "{accepted}");
    }
    for rejected in [
        json!({}),
        json!({ "query": "x", "unknown": 1 }),
        json!({ "query": "x", "state": "merged" }),
        json!({ "query": "x", "limit": 0 }),
    ] {
        assert!(!schema.accepts(&value(rejected.clone())), "{rejected}");
    }
}

#[test]
fn json_schema_widens_what_it_cannot_express() {
    let any = |schema| assert_eq!(Schema::from_json_schema(&schema), Schema::Any, "{schema}");
    any(json!(true));
    any(json!({}));
    any(json!({ "not": { "type": "string" } }));
    any(json!({ "$ref": "https://example.com/schema.json" }));
    any(json!({ "$ref": "#/$defs/missing" }));
    any(json!({ "allOf": [{ "type": "object" }, { "required": ["a"] }] }));
    any(json!({ "enum": ["a", { "b": 1 }] }));

    // A recursive schema terminates and still accepts recursive values.
    let tree = Schema::from_json_schema(&json!({
        "$ref": "#/$defs/node",
        "$defs": { "node": {
            "type": "object",
            "properties": { "children": { "type": "array", "items": { "$ref": "#/$defs/node" } } }
        } }
    }));
    assert!(tree.accepts(&value(json!({ "children": [{ "children": [] }] }))));

    // Structural keywords imply the type; tuples keep only the list shape.
    assert_eq!(
        Schema::from_json_schema(&json!({ "properties": { "a": { "const": 1 } } })).type_notation(),
        "{ a?: int }"
    );
    assert_eq!(
        Schema::from_json_schema(
            &json!({ "type": "array", "prefixItems": [{ "type": "string" }] })
        ),
        Schema::list(Schema::Any)
    );
}

#[test]
fn json_values_round_trip() {
    let source = json!({ "a": [1, 2.5, "x", null, true], "b": { "c": -3 } });
    let canonical = value(source.clone());
    assert_eq!(canonical.to_json().unwrap(), source);
    assert_eq!(
        CanonicalValue::Bytes(vec![1]).to_json(),
        Err(ValueError::NotJsonRepresentable)
    );
}

#[test]
fn compiled_program_lists_every_referenced_tool() {
    let mut registry = ToolRegistry::new();
    for name in [
        "crm.find",
        "crm.update",
        "mail.send",
        "audit.log",
        "never.used",
    ] {
        registry
            .register(descriptor(
                name,
                vec![Schema::Any],
                ExecutionPolicy::Idempotent,
            ))
            .unwrap();
    }
    let mut builder = Runtime::builder().registry(registry);
    for name in [
        "crm.find",
        "crm.update",
        "mail.send",
        "audit.log",
        "never.used",
    ] {
        builder = builder.tool(name, |_: &[CanonicalValue], _: &ToolContext| {
            Ok(CanonicalValue::Null)
        });
    }
    let runtime = builder.with_prelude().build().unwrap();
    let program = runtime
        .compile(
            r#"
customers = crm.find("active")
updated = for customer in customers {
    return boundary retry 1 {
        return crm.update(customer)
    } catch err {
        return audit.log(err.code)
    }
}
return mail.send(text.upper("done")) if false else updated
"#,
        )
        .unwrap();
    assert_eq!(
        program.referenced_tools.iter().collect::<Vec<_>>(),
        [
            "audit.log",
            "crm.find",
            "crm.update",
            "mail.send",
            "text.upper"
        ]
    );
}

/// Runtime with `work(x)`, which counts its dispatches, and `stop(x)`, which
/// additionally cancels the run from inside the handler.
fn cancellable_runtime(
    calls: &Arc<AtomicUsize>,
    configure: impl FnOnce(RuntimeBuilder) -> RuntimeBuilder,
) -> Runtime {
    let mut registry = ToolRegistry::new();
    for name in ["work", "stop", "flaky"] {
        registry
            .register(descriptor(
                name,
                vec![Schema::Any],
                ExecutionPolicy::Idempotent,
            ))
            .unwrap();
    }
    let builder = Runtime::builder()
        .registry(registry)
        .loop_concurrency(1)
        .tool("work", {
            let calls = calls.clone();
            move |args: &[CanonicalValue], _: &ToolContext| {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok(args[0].clone())
            }
        })
        .tool("stop", {
            let calls = calls.clone();
            move |args: &[CanonicalValue], context: &ToolContext| {
                calls.fetch_add(1, Ordering::SeqCst);
                context.cancellation.cancel();
                Ok(args[0].clone())
            }
        })
        .tool("flaky", {
            let calls = calls.clone();
            move |_: &[CanonicalValue], _: &ToolContext| {
                calls.fetch_add(1, Ordering::SeqCst);
                Err(ToolError::new("TEMP", "try again").retryable(true))
            }
        });
    configure(builder).build().unwrap()
}

fn run_cancellable(
    runtime: &Runtime,
    source: &str,
    token: &CancellationToken,
) -> Result<Execution, ToolError> {
    let program = runtime.compile(source).expect("valid source");
    runtime.run_with(&program, RunOptions::new().cancellation(token.clone()))
}

#[test]
fn an_uncancelled_token_does_not_change_the_result() {
    let calls = Arc::new(AtomicUsize::new(0));
    let runtime = cancellable_runtime(&calls, |builder| builder);
    let execution = run_cancellable(&runtime, "return work(7)", &CancellationToken::new()).unwrap();
    assert_eq!(execution.value, CanonicalValue::Integer(7));
}

#[test]
fn a_run_cancelled_before_it_starts_dispatches_nothing() {
    let calls = Arc::new(AtomicUsize::new(0));
    let runtime = cancellable_runtime(&calls, |builder| builder);
    let token = CancellationToken::new();
    token.cancel();
    let error = run_cancellable(&runtime, "return work(1)", &token).unwrap_err();
    assert_eq!(error.code, "RL6110");
    assert!(!error.retryable);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[test]
fn cancelling_mid_loop_stops_further_dispatches() {
    let calls = Arc::new(AtomicUsize::new(0));
    let runtime = cancellable_runtime(&calls, |builder| builder);
    let error = run_cancellable(
        &runtime,
        "return for n in [1, 2, 3, 4, 5, 6, 7, 8] { return stop(n) }",
        &CancellationToken::new(),
    )
    .unwrap_err();
    assert_eq!(error.code, "RL6110");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[test]
fn a_boundary_neither_retries_nor_catches_cancellation() {
    let calls = Arc::new(AtomicUsize::new(0));
    let runtime = cancellable_runtime(&calls, |builder| builder);
    let error = run_cancellable(
        &runtime,
        r#"
return boundary retry 3 {
    _ = stop(1)
    return flaky(1)
} catch err {
    return work("fallback")
}
"#,
        &CancellationToken::new(),
    )
    .unwrap_err();
    assert_eq!(error.code, "RL6110");
    // `stop` ran; `flaky` may have raced it, but nothing was retried and the
    // catch block's `work` never ran.
    assert!(calls.load(Ordering::SeqCst) <= 2);
}

#[test]
fn cancellation_interrupts_retry_backoff() {
    let calls = Arc::new(AtomicUsize::new(0));
    let runtime = cancellable_runtime(&calls, |builder| {
        builder.retry_backoff(Duration::from_secs(60), 1.0, Duration::from_secs(60))
    });
    let token = CancellationToken::new();
    let started = Instant::now();
    let error = std::thread::scope(|scope| {
        scope.spawn(|| {
            while calls.load(Ordering::SeqCst) == 0 {
                std::thread::sleep(Duration::from_millis(1));
            }
            token.cancel();
        });
        run_cancellable(
            &runtime,
            r#"return boundary retry 5 { return flaky(1) } catch err { return "caught" }"#,
            &token,
        )
        .unwrap_err()
    });
    assert_eq!(error.code, "RL6110");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(started.elapsed() < Duration::from_secs(30));
}

#[test]
fn progress_reports_a_cancelled_outcome() {
    let calls = Arc::new(AtomicUsize::new(0));
    let runtime = cancellable_runtime(&calls, |builder| builder);
    let program = runtime.compile("return work(stop(1))").unwrap();
    let (sender, mut progress) = progress_channel(256);
    let result = runtime.run_with(&program, RunOptions::new().progress(sender));
    assert_eq!(result.unwrap_err().code, "RL6110");
    let mut outcome = None;
    while let Ok(event) = progress.recv() {
        if let ProgressChange::Finished(finished) = event.change {
            outcome = Some(finished);
        }
    }
    assert_eq!(outcome, Some(ProgressOutcome::Cancelled));
}
