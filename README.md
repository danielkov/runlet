# Runlet

This repository contains the Phase 0 semantic executable model specified by
[`DESIGN.md`](DESIGN.md). It is a deterministic, single-threaded Rust model of
the language: parsing, structural schema analysis, canonical values, lazy
root-reachable effects, dynamic branches and loops, error boundaries, and an
inspectable execution graph.

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
examples. Async scheduling, journals, recovery, and production executors are
explicitly Phase 1–3 work in the design and are not claimed by this crate.

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
