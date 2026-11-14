//! `tokio` feature: async tool handlers and `Runtime::run_async`.

use runlet::*;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

struct SetOnDrop(Arc<AtomicBool>);
impl Drop for SetOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

/// `nap(ms)` sleeps on the Tokio timer and echoes its argument.
fn runtime(calls: &Arc<AtomicUsize>, dropped: &Arc<AtomicBool>) -> Runtime {
    let mut registry = ToolRegistry::new();
    registry
        .register(ToolDescriptor {
            name: "nap".into(),
            summary: String::new(),
            input: CallSchema::one(Schema::INTEGER),
            output: Schema::INTEGER,
            execution: ExecutionPolicy::Idempotent,
            schema_version: "1".into(),
        })
        .unwrap();
    let (calls, dropped) = (calls.clone(), dropped.clone());
    Runtime::builder()
        .registry(registry)
        .async_tool("nap", move |args, _context| {
            calls.fetch_add(1, Ordering::SeqCst);
            let guard = SetOnDrop(dropped.clone());
            async move {
                let _guard = guard;
                let CanonicalValue::Integer(ms) = args[0] else {
                    unreachable!("schema-checked")
                };
                tokio::time::sleep(Duration::from_millis(ms as u64)).await;
                Ok(CanonicalValue::Integer(ms))
            }
        })
        .build()
        .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn async_tools_run_concurrently_under_run_async() {
    let (calls, dropped) = Default::default();
    let runtime = runtime(&calls, &dropped);
    let program = runtime
        .compile("return for ms in [200, 200, 200, 200] { return nap(ms) }")
        .unwrap();
    let started = Instant::now();
    let execution = runtime
        .run_async(&program, RunOptions::new())
        .await
        .unwrap();
    assert_eq!(
        execution.value.presentation_json().unwrap(),
        "[200,200,200,200]"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 4);
    // Sequential execution would take 800ms.
    assert!(started.elapsed() < Duration::from_millis(700));
}

#[tokio::test(flavor = "current_thread")]
async fn async_tools_work_on_a_current_thread_runtime() {
    let (calls, dropped) = Default::default();
    let runtime = runtime(&calls, &dropped);
    let program = runtime.compile("return nap(1) + nap(2)").unwrap();
    let execution = runtime
        .run_async(&program, RunOptions::new())
        .await
        .unwrap();
    assert_eq!(execution.value, CanonicalValue::Integer(3));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dropping_run_async_cancels_in_flight_and_pending_calls() {
    let (calls, dropped) = Default::default();
    let runtime = runtime(&calls, &dropped).clone();
    let program = runtime
        .compile("return [nap(60000), nap(60001)] + for ms in [60002, 60003] { return nap(ms) }")
        .unwrap();
    let started = Instant::now();
    let timed_out = tokio::time::timeout(
        Duration::from_millis(100),
        runtime.run_async(&program, RunOptions::new()),
    )
    .await;
    assert!(timed_out.is_err());
    // The in-flight future is dropped at its next poll, not after a minute.
    while !dropped.load(Ordering::SeqCst) {
        assert!(started.elapsed() < Duration::from_secs(10));
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let dispatched = calls.load(Ordering::SeqCst);
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(calls.load(Ordering::SeqCst), dispatched);
    assert!(dispatched < 4);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_explicit_token_cancels_run_async() {
    let (calls, dropped) = Default::default();
    let runtime = runtime(&calls, &dropped);
    let program = runtime.compile("return nap(60000)").unwrap();
    let token = CancellationToken::new();
    let canceller = token.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        canceller.cancel();
    });
    let error = runtime
        .run_async(&program, RunOptions::new().cancellation(token))
        .await
        .unwrap_err();
    assert_eq!(error.code, "RL6110");
    assert!(dropped.load(Ordering::SeqCst));
}

#[test]
fn an_async_tool_without_a_runtime_fails_with_a_host_error() {
    let (calls, dropped) = Default::default();
    let runtime = runtime(&calls, &dropped);
    let program = runtime.compile("return nap(1)").unwrap();
    assert_eq!(runtime.run(&program).unwrap_err().code, "RL8105");
}
