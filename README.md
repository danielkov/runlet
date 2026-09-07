<p align="center">
  <img src="assets/runlet-logo.png" alt="Runlet logo" width="220">
</p>

# Runlet

Runlet is a small orchestration language for LLM agents. It lets an agent
replace a sequence of individual tool calls with one program that the host can
check, execute concurrently, observe, and return as a single structured
result.

```runlet
issues = linear.search_issues({ assignee: user, state: "open" })
pulls = github.search_pull_requests({ author: user, state: "open" })

pulls_with_checks = for pull in pulls {
    checks = github.checks(pull.number)
    return {
        number: pull.number,
        title: pull.title,
        checks
    }
}

return {
    issues,
    pull_requests: pulls_with_checks
}
```

There is no `async` or `await`. References create data dependencies: the
Linear and GitHub searches can start together, and each checks lookup starts
when its pull request is available. The host bounds fan-out; only work that
contributes to the returned value runs.

## Why a language for tool composition?

A conventional agent loop sends every tool result back through the model. For
a multi-step task, that means repeated inference round-trips, a growing
transcript full of intermediate data, and asking the model to manually carry
state between calls.

Runlet moves that mechanical work into the agent runtime:

```text
model → one Runlet program → many host tool calls → one structured result
```

The model still decides what should happen. Runlet gives that decision a
small, purpose-built execution format instead of making the model supervise
every iteration, dependency, retry, and join.

This is especially useful for:

- list-then-detail and other N+1 API access patterns;
- joining, projecting, and enriching structured results;
- independent calls that should run concurrently;
- bounded fan-out across many records;
- read/transform/write workflows;
- retryable operations with an explicit fallback; and
- joining data from built-in tools, MCP servers, and application APIs.

The motivation is borne out by AgentKit's
[`compose` case study](https://github.com/danielkov/agentkit/blob/main/docs/compose-case-study.md).
Its sandboxed Lua composition tool reduced model round-trips, context growth,
and cost for tool-heavy tasks. On the study's initial six-scenario run,
composition reduced cost by 38–77% while maintaining or improving accuracy.
The broader model sweep also found an important boundary: composition was
consistently valuable for N+1 fan-out, but could be counterproductive for
exploratory investigations. Runlet targets the same runtime-enrichment
mechanism with schemas, implicit dataflow, structured concurrency, and an
inspectable execution graph built into the language.

## Design goals

| Goal | How Runlet approaches it |
| --- | --- |
| Make correct concurrency the default | Tool outputs behave like ordinary values; references create graph edges and independent nodes can run together. |
| Keep programs easy for models to produce | The language has bindings, expressions, objects, lists, `for`, conditionals, boundaries, and `return`—but no imports, classes, functions, threads, or visible type syntax. |
| Catch mistakes before tools run | The host registers every tool with input and output schemas. The analyzer checks names, calls, projections, operators, branches, and returned values. |
| Avoid accidental effects, keep intended ones | Pure work is lazy and root-reachable: an unused named pure computation is pruned with a warning, while `_ = expression` explicitly evaluates and discards any result. Statements containing effectful calls and discard statements are implicit roots — independent roots run concurrently, while `after prerequisite { ... }` expresses required ordering. |
| Bound dynamic work | The host caps active `for` iterations while preserving result order. |
| Make failure handling explicit | `boundary retry N { ... } catch err { ... }` owns a subgraph, retries retryable failures, and produces a normal fallback value. |
| Let hosts retain control | The embedder supplies the complete tool registry, implementations, schemas, execution policies, and external inputs. |
| Make execution observable | The runtime emits ordered graph events for nodes, dependencies, attempts, failures, recoveries, and results. |

## Language tour

Runlet programs are immutable expressions ending in one `return`:

```runlet
customer = crm.get_customer(customer_id)
orders = commerce.list_orders({ customer_id: customer.id })

enriched = for order in orders {
    result = boundary retry 2 {
        return risk.score({ customer, order })
    } catch err {
        return {
            status: "unavailable",
            code: err.code,
            attempt: err.attempt
        }
    }

    return {
        id: order.id,
        total: order.total,
        risk: result
    }
}

return {
    customer: customer.name,
    orders: enriched
}
```

The main rules are deliberately compact:

- Assignments create immutable bindings. `_ = expression` discards the value but always evaluates the expression; it may repeat and introduces no readable name.
- A program and every block end with `return`.
- Objects and lists are ordinary structured values; `{ [expr]: value }`
  computes a property key, and `left + right` merges two objects shallowly
  (the right side wins).
- `value if condition else alternative` evaluates only the selected branch;
  `else` is optional and defaults to `null`. For larger branches,
  `if cond { ... } else { ... }` is the block-bodied expression form — each
  branch ends with `return`.
- `for item in items { ... }` returns an ordered list with host-bounded
  concurrency; `skip if
  condition` drops an element.
- `fold acc = init for item in items { ... }` reduces sequentially; the
  body's `return` becomes the next accumulator, while
  `break value if condition` stops early with a final accumulator.
- Independent implicit effect and discard roots run concurrently. `after prerequisite { ...
  return value }` gates calls lexically created in its block (including nested
  branch/loop calls) without retroactively gating calls declared outside it.
  Lists or objects can join multiple prerequisites.
- `boundary retry N { ... } catch err { ... }` turns a failed subgraph into a
  fallback value; `fail(code, message)` raises one.
- `assert(condition, message)` checks an invariant without returning its
  intermediate values.
- Tool namespaces such as `crm.get_customer` come entirely from the host.
- A small pure intrinsic library (`text.*`, `regex.*`, `list.*`, `json.*`,
  `number.*`, `time.*`) covers data shaping; see [`STDLIB.md`](STDLIB.md).

See [`examples/`](examples/) for executable programs and
[`DESIGN.md`](DESIGN.md) for the complete language and runtime semantics.

## Embedding Runlet

The Rust crate provides the parser, analyzer, schemas, canonical values,
runtime, and execution graph. A host describes its tool surface, connects each
descriptor to an implementation, compiles agent-produced source, and executes
the resulting program:

```rust
use runlet::{
    CallSchema, CanonicalValue, ExecutionPolicy, Runtime, Schema,
    ToolDescriptor, ToolRegistry,
};

fn main() {
    let mut tools = ToolRegistry::new();
    tools.register(ToolDescriptor {
        name: "profile.lookup".into(),
        summary: "Look up a user profile".into(),
        input: CallSchema::positional(vec![Schema::string()]),
        output: Schema::Any,
        execution: ExecutionPolicy::Pure,
        schema_version: "1".into(),
    }).unwrap();

    let runtime = Runtime::builder()
        .registry(tools)
        .input("user_id", Schema::string(), "usr_123".into())
        .tool("profile.lookup", |args, _context| {
            let CanonicalValue::String(id) = &args[0] else { unreachable!() };
            Ok(CanonicalValue::Object([
                ("id".into(), id.clone().into()),
                ("name".into(), "Ada".into()),
            ].into()))
        })
        .build()
        .unwrap();

    let program = runtime.compile(
        "profile = profile.lookup(user_id)\nreturn profile"
    ).unwrap();
    let execution = runtime.run(&program).unwrap();

    println!("{}", execution.value.presentation_json().unwrap());
}
```

Tool handlers also receive stable operation, dispatch, schema-version, and
attempt context. `run_observed` exposes the live graph event stream for logs,
traces, dashboards, or an agent UI. `.with_prelude()` installs the
deterministic intrinsics from [`STDLIB.md`](STDLIB.md) (host registrations of
the same names win), and `.retry_backoff(base, factor, cap)` configures
exponential backoff between boundary retry attempts.

### Source-attributed progress

`Runtime::run_with_progress(&program, sender)` accepts the one-shot sender from
`progress_channel(capacity)`. Drain the receiver concurrently with `recv()`, or
poll `try_recv()` from a host event loop. The bounded queue never waits for the
consumer; a slow, dropped, or panicking consumer does not stop execution.

```rust
use runlet::{progress_channel, ProgressRecvError, Runtime};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let runtime = Runtime::builder().with_prelude().build()?;
    let source = r#"return text.upper("hello")"#;
    let program = runtime.compile(source).expect("valid source");
    let (sender, mut progress) = progress_channel(256);
    let _execution = std::thread::scope(|scope| {
        let run = scope.spawn(|| runtime.run_with_progress(&program, sender));
        loop {
            match progress.recv() {
                Ok(event) => println!("{event:?}"), // metadata only
                Err(ProgressRecvError::Closed) => break,
                Err(error) => {
                    eprintln!("observation incomplete: {error}");
                    break; // execution still continues
                }
            }
        }
        run.join().expect("executor did not panic")
    })?;
    Ok(())
}
```

The typed stream contains node IDs, exact source byte spans, node kinds/states,
retry attempts, value-free relationships, sequence numbers starting at 1, and a
terminal run outcome. It excludes labels, tool names, inputs, outputs, error text,
condition/path strings, and argument-derived operation/dispatch identities.
`ToolContext.node_id` joins a real handler dispatch to its progress node; keep
`operation_id` (logical operation) distinct from `dispatch_id` (actual attempt).
Cached operations emit success without dispatch/waiting/running events.

**Identity is run-local.** Namespace IDs with the host compose call **and execution
incarnation**, and retain the exact compiled source. Identical spans can have
many dynamic nodes across loop iterations and retries. `Contains` edges identify
iteration ownership; `RetryOf` points from the new node to the previous attempt.
Do not infer running state from descendants. For calls, `WaitingForCapacity`
means resolved inputs awaiting a permit, while `Running` is emitted after permit
acquisition immediately before handler invocation (including intrinsics). Capacity
is released only after the call's terminal state is published. For
structural/computation nodes, `Running` only means an active evaluation scope.
`Blocked` does not name a blocker; provenance edges are not current wait reasons.
Unobserved source expressions remain unknown, not implicitly pruned or blocked.

**Loss is explicit.** After overflow the queued contiguous prefix drains, then
`Lagged` is returned; publication never resumes. There is no replay or live
resynchronization API. `Incomplete` means the publisher closed without a terminal
event, such as on unwind. Only receipt of `Finished` establishes normal run
completion (`Closed` follows it); `Failed` also covers registry rejection.
Compilation errors happen before execution and produce no progress. A terminal
run failure does not invent terminal states for unfinished nodes. Capacity must
be positive and bounds queued metadata, not the existing execution graph.

`run`, `run_observed`, and execution graph snapshots remain available. The legacy
`run_observed` callback contains values and executes under the graph lock; it must
not block, reenter, or panic. Use the bounded stream for isolated host progress.

## Enriching an AgentKit runtime

[AgentKit](https://github.com/danielkov/agentkit) provides the surrounding
agent loop, model adapters, tools, permissions, MCP integration, reporting,
compaction, and task management. Runlet is intended to sit at the composition
boundary of that runtime:

1. Adapt the AgentKit tool catalog into Runlet `ToolDescriptor`s.
2. Expose a `runlet` composition tool to the model alongside the granular
   tools.
3. Compile the submitted program against the tools visible for that turn.
4. Dispatch Runlet call nodes through AgentKit's existing executor so
   permissions, approvals, cancellation, and reporting remain authoritative.
5. Return only the program's final structured value to the model transcript.

That integration is not part of this repository yet; the current crate
provides the executable language model and an in-process threaded executor.
The separation is intentional: Runlet plans and explains the work, while the
agent host owns capabilities and policy.

### Compact results with TOON

Tool composition prevents intermediate results from filling the transcript.
The final result can be made smaller too. The
[`serde_toon2`](https://docs.rs/serde_toon2/latest/serde_toon2/) crate provides
Serde-compatible Token-Oriented Object Notation, so a host can encode the
structured Runlet result before adding it to the next model turn:

```rust
let json = execution.value.presentation_json()?;
let value: serde_json::Value = serde_json::from_str(&json)?;
let model_context = serde_toon2::to_string(&value)?;
```

Together, the three projects cover distinct layers of the enrichment story:

- AgentKit runs the model loop and governs tools.
- Runlet composes tool work into a checked execution graph.
- `serde_toon2` encodes the selected result for efficient model context.

## Run the project

Run the language examples with the built-in CLI:

```sh
cargo run -- ./examples/03_loops.rnlt
cargo run -- ./examples/05_boundaries.rnlt
```

Watch a larger concurrent pipeline as a live execution graph:

```sh
cargo run -- graph ./examples/live/pipeline.rnlt
```

Run the test suite:

```sh
cargo test
```

The current implementation includes parsing, schema analysis, canonical
values, lazy root-reachable execution, dynamic branches, bounded concurrent
loops, retry boundaries, and live graph events. Durable journals, recovery,
cancellation, and production executor interfaces remain roadmap work described
in [`DESIGN.md`](DESIGN.md).

## Editor support

- [`editors/vscode`](editors/vscode/) provides a dependency-free VS Code
  extension and reusable TextMate grammar.
- [`editors/zed`](editors/zed/) provides a Zed extension backed by Tree-sitter.
- [`editors/vim`](editors/vim/) provides Vim and Neovim syntax support.
- [`editors/tree-sitter-runlet`](editors/tree-sitter-runlet/) contains the
  Tree-sitter grammar source; generated parser files live only on the
  `tree-sitter` branch.

## License

Runlet is available under the MIT or Apache-2.0 license.
