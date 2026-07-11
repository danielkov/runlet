# Runlet

This repository contains the executable semantic model specified by
[`DESIGN.md`](DESIGN.md): parsing, structural schema analysis, canonical values,
lazy root-reachable effects, dynamic branches and bounded concurrent loops,
error boundaries, and an inspectable real-time execution graph.

```rust
use runlet::{CanonicalValue, ExecutionPolicy, Runtime, Schema, ToolDescriptor,
             ToolRegistry, CallSchema};

let mut registry = ToolRegistry::new();
registry.register(ToolDescriptor {
    name: "hello".into(),
    summary: "Return a greeting".into(),
    input: CallSchema::positional(vec![Schema::string()]),
    output: Schema::string(),
    execution: ExecutionPolicy::Pure,
    schema_version: "1".into(),
})?;

let runtime = Runtime::builder()
    .registry(registry)
    .tool("hello", |args, _context| {
        let CanonicalValue::String(name) = &args[0] else { unreachable!() };
        Ok(CanonicalValue::String(format!("Hello, {name}!")))
    })
    .build()?;

let program = runtime.compile("return hello(\"Runlet\")")?;
let execution = runtime.run(&program)?;
assert_eq!(execution.value, CanonicalValue::String("Hello, Runlet!".into()));
# Ok::<(), Box<dyn std::error::Error>>(())
```

Run `cargo test` for canonical encoding fixtures and executable semantic
examples. Loop iterations use a local threaded executor; durable journals,
recovery, cancellation, and production executors remain later-phase work.

## Language demos

The [`examples/`](examples/) directory contains `.rnlt` programs that run with
an empty tool registry. They demonstrate values, operators, projections,
conditionals, bounded loops, and catchable compute failures using only the core
language.

Run one with the CLI:

```sh
cargo run -- ./examples/03_loops.rnlt
```

After installing the binary with `cargo install --path .`, the same command is:

```sh
runlet ./examples/03_loops.rnlt
```

### Live concurrent graph demo

[`examples/live/pipeline.rnlt`](examples/live/pipeline.rnlt) models a
multi-region ingestion pipeline with nested fan-out, deep dependency chains,
bounded concurrency, and fan-in stages. `demo.task(label, milliseconds, input)`
is a CLI-provided tool that simulates long-running host work while preserving
its input in the output envelope.

Watch the execution graph change while the pipeline runs:

```sh
cargo run -- graph ./examples/live/pipeline.rnlt
```

Runlet infers all scheduling from value dependencies. The outer `limit 3`
allows three region subgraphs to run concurrently; each region's inner
`limit 3` allows its three source chains to overlap. Per-source error boundaries
make retries explicit: every `orders` enrichment recovers on its second attempt,
while `sa-east/events` exhausts three attempts and follows the visible fallback
path. Results retain source order even when individual tasks finish out of
order. The full demo takes roughly half a minute, leaving time to follow each
transition in the dashboard.

The dashboard groups work by named `region › source` hierarchy. Separate
sections show active calls with elapsed time, currently materialized boundaries
and attempt counts, explicit failure/retry/recovery/catch events, and the latest
producer-to-consumer data-flow edges.

## Editor support

The dependency-free VS Code extension in [`editors/vscode`](editors/vscode/)
recognizes `.rnlt` files and provides syntax highlighting, comments, bracket
matching, automatic closing pairs, and folding. Build and install it locally
with:

```sh
(cd editors/vscode && npx --yes @vscode/vsce package \
  --out ../../runlet-language.vsix --allow-missing-repository --skip-license)
code --install-extension runlet-language.vsix
```

Its TextMate grammar lives at
[`editors/vscode/syntaxes/runlet.tmLanguage.json`](editors/vscode/syntaxes/runlet.tmLanguage.json)
and can be reused by other TextMate-compatible editors.

Native integrations are also available for
[`Zed`](editors/zed/README.md) and [`Vim/Neovim`](editors/vim/README.md).
Zed uses the Runlet Tree-sitter grammar in
[`editors/tree-sitter-runlet`](editors/tree-sitter-runlet/), while Vim ships a
traditional runtime syntax file. Each editor directory includes local install
instructions.
