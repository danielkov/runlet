use runlet::*;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, mpsc};

fn descriptor(name: &str) -> ToolDescriptor {
    ToolDescriptor {
        name: name.into(),
        summary: String::new(),
        input: CallSchema::positional(vec![Schema::string()]),
        output: Schema::string(),
        execution: ExecutionPolicy::Pure,
        schema_version: "1".into(),
    }
}

fn builder(names: &[&str]) -> RuntimeBuilder {
    let mut registry = ToolRegistry::new();
    for name in names {
        registry.register(descriptor(name)).unwrap();
    }
    Runtime::builder().registry(registry)
}

fn node(event: &ProgressEvent) -> Option<&ProgressNode> {
    match &event.change {
        ProgressChange::NodeAdded(n) | ProgressChange::NodeUpdated(n) => Some(n),
        _ => None,
    }
}

fn drain(receiver: &mut ProgressReceiver) -> Vec<ProgressEvent> {
    let mut events = Vec::new();
    loop {
        match receiver.recv() {
            Ok(event) => events.push(event),
            Err(ProgressRecvError::Closed) => return events,
            Err(e) => panic!("unexpected stream failure: {e}"),
        }
    }
}

fn complete(events: &[ProgressEvent], outcome: ProgressOutcome) {
    for (i, event) in events.iter().enumerate() {
        assert_eq!(event.sequence, i as u64 + 1);
    }
    assert_eq!(
        events.last().unwrap().change,
        ProgressChange::Finished(outcome)
    );
}

#[test]
fn dependent_call_stays_blocked_until_handler_output() {
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let release_rx = Mutex::new(release_rx);
    let runtime = builder(&["tool"])
        .tool("tool", move |args, ctx| {
            entered_tx.send(ctx.clone()).unwrap();
            release_rx.lock().unwrap().recv().unwrap();
            Ok(args[0].clone())
        })
        .build()
        .unwrap();
    // Outer call is created before evaluating the nested argument.
    let source = "return tool(tool(\"INPUT_SENTINEL\"))";
    let program = runtime.compile(source).unwrap();
    let (sender, mut receiver) = progress_channel(256);
    std::thread::scope(|scope| {
        let release_tx = release_tx;
        let run = scope.spawn(|| runtime.run_with_progress(&program, sender));
        let first = entered_rx.recv().unwrap();
        let mut events = Vec::new();
        loop {
            let event = receiver.recv().unwrap();
            let first_running = node(&event)
                .is_some_and(|n| n.id == first.node_id && n.state == ProgressState::Running);
            events.push(event);
            if first_running {
                break;
            }
        }
        while let Some(event) = receiver.try_recv().unwrap() {
            events.push(event);
        }
        let calls: Vec<_> = events
            .iter()
            .filter_map(node)
            .filter(|n| n.kind == NodeKind::Call)
            .collect();
        let outer = calls
            .iter()
            .find(|n| n.id != first.node_id)
            .unwrap()
            .id
            .clone();
        assert!(
            calls
                .iter()
                .filter(|n| n.id == outer)
                .all(|n| n.state == ProgressState::Blocked)
        );
        assert!(calls.iter().all(|n| n.state != ProgressState::Ready));
        release_tx.send(()).unwrap();
        let second = entered_rx.recv().unwrap();
        assert_eq!(second.node_id, outer);
        assert_ne!(first.operation_id, second.operation_id);
        release_tx.send(()).unwrap();
        events.extend(drain(&mut receiver));
        let execution = run.join().unwrap().unwrap();
        let first_done = events
            .iter()
            .position(|e| {
                node(e)
                    .is_some_and(|n| n.id == first.node_id && n.state == ProgressState::Succeeded)
            })
            .unwrap();
        let second_running = events
            .iter()
            .position(|e| {
                node(e).is_some_and(|n| n.id == second.node_id && n.state == ProgressState::Running)
            })
            .unwrap();
        assert!(first_done < second_running);
        for n in execution.graph.nodes {
            let last = events
                .iter()
                .filter_map(node)
                .rfind(|p| p.id == n.id)
                .unwrap();
            assert_eq!(last.state, ProgressState::Succeeded);
            assert_eq!(last.span, n.span);
        }
        complete(&events, ProgressOutcome::Succeeded);
    });
}

#[test]
fn saturated_dispatch_never_reports_waiting_call_running() {
    for first_fails in [false, true] {
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let release_rx = Mutex::new(release_rx);
        let runtime = builder(&["tool"])
            .dispatch_limit(1)
            .loop_concurrency(2)
            .tool("tool", move |args, ctx| {
                entered_tx.send(ctx.clone()).unwrap();
                if release_rx.lock().unwrap().recv().unwrap() {
                    Err(ToolError::new("FIRST_FAILED", "first dispatch failed"))
                } else {
                    Ok(args[0].clone())
                }
            })
            .build()
            .unwrap();
        let source = "return for x in [\"SAME_SENTINEL\", \"SAME_SENTINEL\"] { return tool(x) }";
        let program = runtime.compile(source).unwrap();
        let (sender, mut receiver) = progress_channel(256);
        std::thread::scope(|scope| {
            let release_tx = release_tx;
            let run = scope.spawn(|| runtime.run_with_progress(&program, sender));
            let first = entered_rx.recv().unwrap();
            let mut events = Vec::new();
            let mut waiting = HashSet::new();
            while waiting.len() < 2 {
                let event = receiver.recv().unwrap();
                if let Some(n) = node(&event) {
                    if n.kind == NodeKind::Call && n.state == ProgressState::WaitingForCapacity {
                        waiting.insert(n.id.clone());
                    }
                    if n.kind == NodeKind::Call && n.state == ProgressState::Running {
                        assert_eq!(n.id, first.node_id);
                    }
                }
                events.push(event);
            }
            // Capacity is saturated at the real handler boundary. Verify the
            // complete lifecycle ordering below, not an empty queue snapshot.
            release_tx.send(first_fails).unwrap();
            let second = entered_rx.recv().unwrap();
            assert_ne!(first.node_id, second.node_id);
            assert_ne!(first.operation_id, second.operation_id);
            release_tx.send(false).unwrap();
            events.extend(drain(&mut receiver));
            let result = run.join().unwrap();
            assert_eq!(result.is_err(), first_fails);
            let first_done = events
                .iter()
                .position(|event| {
                    node(event).is_some_and(|n| {
                        n.id == first.node_id
                            && n.state
                                == if first_fails {
                                    ProgressState::Failed
                                } else {
                                    ProgressState::Succeeded
                                }
                    })
                })
                .unwrap();
            let second_running = events
                .iter()
                .position(|event| {
                    node(event).is_some_and(|n| {
                        n.id == second.node_id && n.state == ProgressState::Running
                    })
                })
                .unwrap();
            assert!(first_done < second_running);
            let calls: Vec<_> = events
                .iter()
                .filter_map(|e| match &e.change {
                    ProgressChange::NodeAdded(n) if n.kind == NodeKind::Call => Some(n),
                    _ => None,
                })
                .collect();
            assert_eq!(calls.len(), 2);
            assert_eq!(calls[0].span, calls[1].span);
            assert_ne!(calls[0].id, calls[1].id);
            assert_eq!(&source[calls[0].span.start..calls[0].span.end], "tool(x)");
            let iterations: HashSet<_> = events
                .iter()
                .filter_map(node)
                .filter(|n| n.kind == NodeKind::Iteration)
                .map(|n| n.id.clone())
                .collect();
            assert_eq!(iterations.len(), 2);
            for call in calls {
                assert!(events.iter().any(|e| matches!(&e.change, ProgressChange::EdgeAdded(edge) if edge.kind == ProgressEdgeKind::Contains && iterations.contains(&edge.from) && edge.to == call.id)));
            }
            complete(
                &events,
                if first_fails {
                    ProgressOutcome::Failed
                } else {
                    ProgressOutcome::Succeeded
                },
            );
            assert!(
                !serde_json::to_string(&events)
                    .unwrap()
                    .contains("SAME_SENTINEL")
            );
        });
    }
}

#[test]
fn retries_cache_and_value_free_metadata_preserve_dynamic_identity() {
    let contexts = Arc::new(Mutex::new(Vec::new()));
    let runtime = builder(&["cached", "flaky"])
        .tool("cached", {
            let contexts = contexts.clone();
            move |_, ctx| {
                contexts.lock().unwrap().push(ctx.clone());
                Ok("OUTPUT_SENTINEL".into())
            }
        })
        .tool("flaky", {
            let contexts = contexts.clone();
            move |_, ctx| {
                contexts.lock().unwrap().push(ctx.clone());
                if ctx.attempt == 0 {
                    Err(ToolError::new("CODE_SENTINEL", "ERROR_SENTINEL").retryable(true))
                } else {
                    Ok("FINAL_SENTINEL".into())
                }
            }
        })
        .build()
        .unwrap();
    let program = runtime.compile("return boundary retry 1 { a = cached(\"INPUT_SENTINEL\"); return flaky(a) } catch err { return \"FALLBACK_SENTINEL\" }").unwrap();
    let (sender, mut receiver) = progress_channel(512);
    let result = runtime.run_with_progress(&program, sender).unwrap();
    assert_eq!(result.value, "FINAL_SENTINEL".into());
    let events = drain(&mut receiver);
    complete(&events, ProgressOutcome::Succeeded);
    let json = serde_json::to_string(&events).unwrap();
    for sentinel in [
        "INPUT_SENTINEL",
        "OUTPUT_SENTINEL",
        "FINAL_SENTINEL",
        "CODE_SENTINEL",
        "ERROR_SENTINEL",
        "FALLBACK_SENTINEL",
    ] {
        assert!(!json.contains(sentinel));
    }
    let contexts = contexts.lock().unwrap();
    assert_eq!(contexts.len(), 3);
    assert_eq!(contexts[1].operation_id, contexts[2].operation_id);
    assert_ne!(contexts[1].dispatch_id, contexts[2].dispatch_id);
    assert_ne!(contexts[1].node_id, contexts[2].node_id);
    for ctx in contexts.iter() {
        assert!(!json.contains(&ctx.operation_id));
    }
    let calls: HashMap<_, _> = events
        .iter()
        .filter_map(node)
        .filter(|n| n.kind == NodeKind::Call)
        .map(|n| (n.id.clone(), n))
        .collect();
    assert_eq!(calls.len(), 4);
    let cached = calls
        .values()
        .find(|n| !contexts.iter().any(|ctx| ctx.node_id == n.id))
        .unwrap();
    assert_eq!(cached.attempt, 1);
    assert_eq!(cached.state, ProgressState::Succeeded);
    assert!(
        events
            .iter()
            .filter_map(node)
            .filter(|n| n.id == cached.id)
            .all(|n| !matches!(
                n.state,
                ProgressState::Running | ProgressState::WaitingForCapacity
            ))
    );
    assert!(events.iter().any(|e| matches!(&e.change, ProgressChange::EdgeAdded(edge) if edge.kind == ProgressEdgeKind::RetryOf && edge.from == contexts[2].node_id && edge.to == contexts[1].node_id)));
}

#[test]
fn failures_rejection_and_unobserved_expressions_are_not_fabricated() {
    let runtime = builder(&["tool"])
        .tool("tool", |_, _| {
            Err(ToolError::new("CODE_SENTINEL", "ERROR_SENTINEL"))
        })
        .build()
        .unwrap();
    assert!(runtime.compile("return tool(").is_err());
    let program = runtime
        .compile("unused = tool(\"UNUSED_SENTINEL\"); return tool(\"INPUT_SENTINEL\")")
        .unwrap();
    let (sender, mut receiver) = progress_channel(128);
    assert!(runtime.run_with_progress(&program, sender).is_err());
    let events = drain(&mut receiver);
    complete(&events, ProgressOutcome::Failed);
    assert!(!serde_json::to_string(&events).unwrap().contains("SENTINEL"));
    let call_additions: Vec<_> = events
        .iter()
        .filter_map(|e| match &e.change {
            ProgressChange::NodeAdded(n) if n.kind == NodeKind::Call => Some(n),
            _ => None,
        })
        .collect();
    assert_eq!(call_additions.len(), 1);
    assert!(call_additions[0].span.start > 30);
    assert!(
        events
            .iter()
            .filter_map(node)
            .all(|n| n.state != ProgressState::Pruned)
    );
    let foreign = Runtime::builder().build().unwrap();
    let (sender, mut receiver) = progress_channel(1);
    assert_eq!(
        foreign
            .run_with_progress(&program, sender)
            .unwrap_err()
            .code,
        "RL7102"
    );
    let events = drain(&mut receiver);
    assert_eq!(events.len(), 1);
    complete(&events, ProgressOutcome::Failed);
}

#[test]
fn overflow_is_a_prefix_not_execution_backpressure() {
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let release_rx = Mutex::new(release_rx);
    let runtime = builder(&["tool"])
        .tool("tool", move |args, _| {
            entered_tx.send(()).unwrap();
            release_rx.lock().unwrap().recv().unwrap();
            Ok(args[0].clone())
        })
        .build()
        .unwrap();
    let program = runtime.compile("return tool(\"VALUE\")").unwrap();
    let (sender, mut receiver) = progress_channel(1);
    std::thread::scope(|scope| {
        let release_tx = release_tx;
        let run = scope.spawn(|| runtime.run_with_progress(&program, sender));
        entered_rx.recv().unwrap();
        assert_eq!(receiver.recv().unwrap().sequence, 1);
        // Disconnect is immediate on overflow, even with execution still live.
        assert_eq!(receiver.recv(), Err(ProgressRecvError::Lagged));
        assert_eq!(receiver.try_recv(), Err(ProgressRecvError::Lagged));
        release_tx.send(()).unwrap();
        assert_eq!(run.join().unwrap().unwrap().value, "VALUE".into());
    });
}

#[test]
fn receiver_drop_and_consumer_panic_are_isolated() {
    let runtime = builder(&["tool"])
        .tool("tool", |args, _| Ok(args[0].clone()))
        .build()
        .unwrap();
    let program = runtime.compile("return tool(\"VALUE\")").unwrap();
    let (sender, receiver) = progress_channel(1);
    drop(receiver);
    assert!(runtime.run_with_progress(&program, sender).is_ok());
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let release_rx = Mutex::new(release_rx);
    let gated = builder(&["tool"])
        .tool("tool", move |args, _| {
            entered_tx.send(()).unwrap();
            release_rx.lock().unwrap().recv().unwrap();
            Ok(args[0].clone())
        })
        .build()
        .unwrap();
    let (sender, mut receiver) = progress_channel(128);
    std::thread::scope(|scope| {
        let release_tx = release_tx;
        let run = scope.spawn(|| gated.run_with_progress(&program, sender));
        entered_rx.recv().unwrap();
        // Panic while execution is demonstrably still inside a host handler.
        let consumer = scope.spawn(move || {
            receiver.recv().unwrap();
            panic!("consumer panic");
        });
        assert!(consumer.join().is_err());
        release_tx.send(()).unwrap();
        assert!(run.join().unwrap().is_ok());
    });
    assert!(runtime.run(&program).is_ok());
}

#[test]
fn publisher_drop_and_handler_unwind_are_incomplete_not_success() {
    let (sender, mut receiver) = progress_channel(1);
    assert_eq!(receiver.try_recv(), Ok(None));
    drop(sender);
    assert_eq!(receiver.recv(), Err(ProgressRecvError::Incomplete));
    let runtime = builder(&["tool"])
        .dispatch_limit(1)
        .tool("tool", |args, _| {
            if args[0] == CanonicalValue::from("PANIC") {
                panic!("handler panic");
            }
            Ok(args[0].clone())
        })
        .build()
        .unwrap();
    let program = runtime.compile("return tool(\"PANIC\")").unwrap();
    let (sender, mut receiver) = progress_channel(128);
    assert!(
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(
            || runtime.run_with_progress(&program, sender)
        ))
        .is_err()
    );
    let mut events = Vec::new();
    loop {
        match receiver.recv() {
            Ok(e) => events.push(e),
            Err(ProgressRecvError::Incomplete) => break,
            Err(e) => panic!("{e}"),
        }
    }
    assert!(
        !events
            .iter()
            .any(|e| matches!(e.change, ProgressChange::Finished(_)))
    );
    assert!(
        events
            .iter()
            .filter_map(node)
            .any(|n| n.kind == NodeKind::Call && n.state == ProgressState::Running)
    );
    let program = runtime.compile("return tool(\"OK\")").unwrap();
    assert!(runtime.run(&program).is_ok());
}

#[test]
fn nested_boundaries_restore_the_enclosing_retry_attempt() {
    for inner_fails in [false, true] {
        let contexts = Arc::new(Mutex::new(Vec::new()));
        let runtime = builder(&["inner", "flaky"])
            .tool("inner", move |args, _| {
                if inner_fails {
                    Err(ToolError::new("INNER", "recover in the inner catch"))
                } else {
                    Ok(args[0].clone())
                }
            })
            .tool("flaky", {
                let contexts = contexts.clone();
                move |args, ctx| {
                    contexts.lock().unwrap().push(ctx.clone());
                    if ctx.attempt == 0 {
                        Err(ToolError::new("RETRY", "retry outer boundary").retryable(true))
                    } else {
                        Ok(args[0].clone())
                    }
                }
            })
            .build()
            .unwrap();
        let source = r#"return boundary retry 1 {
            return flaky(boundary retry 0 {
                return inner("value")
            } catch err {
                return "value"
            })
        } catch err {
            return "outer fallback"
        }"#;
        let program = runtime.compile(source).unwrap();
        let (sender, mut receiver) = progress_channel(512);
        let execution = runtime.run_with_progress(&program, sender).unwrap();
        assert_eq!(execution.value, CanonicalValue::from("value"));
        let events = drain(&mut receiver);
        complete(&events, ProgressOutcome::Succeeded);
        let contexts = contexts.lock().unwrap();
        assert_eq!(
            contexts.iter().map(|ctx| ctx.attempt).collect::<Vec<_>>(),
            [0, 1]
        );
        for ctx in contexts.iter() {
            let progress = events
                .iter()
                .filter_map(node)
                .find(|n| n.id == ctx.node_id)
                .unwrap();
            assert_eq!(progress.attempt, ctx.attempt);
        }
    }
}
