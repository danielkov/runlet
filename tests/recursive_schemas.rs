//! Recursive schemas: registry definitions, `Schema::Ref`, analysis,
//! runtime validation, and JSON Schema lowering.

use runlet::*;
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};

fn reference(name: &str) -> Schema {
    Schema::Ref { name: name.into() }
}

fn object(properties: &[(&str, Schema)], required: &[&str]) -> Schema {
    Schema::Object {
        properties: properties
            .iter()
            .map(|(name, schema)| (name.to_string(), Property::new(schema.clone())))
            .collect::<BTreeMap<_, _>>(),
        required: required
            .iter()
            .map(|name| name.to_string())
            .collect::<BTreeSet<_>>(),
        additional: false,
    }
}

/// `{ value: int, children: <name>[] }`
fn tree(name: &str) -> Schema {
    object(
        &[
            ("value", Schema::INTEGER),
            ("children", Schema::list(reference(name))),
        ],
        &["value", "children"],
    )
}

fn tool(name: &str, parameters: Vec<Schema>, output: Schema) -> ToolDescriptor {
    ToolDescriptor {
        name: name.into(),
        summary: String::new(),
        input: CallSchema::positional(parameters),
        output,
        execution: ExecutionPolicy::Idempotent,
        schema_version: "1".into(),
    }
}

fn value(json: serde_json::Value) -> CanonicalValue {
    CanonicalValue::from_json(json).unwrap()
}

/// `tree.get()` returns `fixture`; `tree.put`, `twin.put` and `labelled.put`
/// echo their argument. `twin` is a separately named copy of `tree`;
/// `labelled` additionally requires a `label`.
fn runtime(fixture: serde_json::Value) -> Runtime {
    let mut registry = ToolRegistry::new();
    registry.define("tree", tree("tree")).unwrap();
    registry.define("twin", tree("twin")).unwrap();
    registry
        .define(
            "labelled",
            object(
                &[
                    ("value", Schema::INTEGER),
                    ("label", Schema::string()),
                    ("children", Schema::list(reference("labelled"))),
                ],
                &["value", "label", "children"],
            ),
        )
        .unwrap();
    for descriptor in [
        tool("tree.get", vec![], reference("tree")),
        tool("tree.put", vec![reference("tree")], Schema::Any),
        tool("twin.put", vec![reference("twin")], Schema::Any),
        tool("labelled.put", vec![reference("labelled")], Schema::Any),
    ] {
        registry.register(descriptor).unwrap();
    }
    let echo = |args: &[CanonicalValue], _: &ToolContext| Ok(args[0].clone());
    Runtime::builder()
        .registry(registry)
        .tool("tree.get", move |_: &[CanonicalValue], _: &ToolContext| {
            Ok(value(fixture.clone()))
        })
        .tool("tree.put", echo)
        .tool("twin.put", echo)
        .tool("labelled.put", echo)
        .build()
        .unwrap()
}

fn sample() -> serde_json::Value {
    json!({ "value": 1, "children": [
        { "value": 2, "children": [{ "value": 3, "children": [] }] },
        { "value": 4, "children": [] }
    ] })
}

fn run(source: &str) -> String {
    let runtime = runtime(sample());
    let program = runtime
        .compile(source)
        .unwrap_or_else(|diagnostics| panic!("{diagnostics:#?}"));
    runtime
        .run(&program)
        .unwrap()
        .value
        .presentation_json()
        .unwrap()
}

fn error_codes(source: &str) -> Vec<String> {
    runtime(sample())
        .compile(source)
        .expect_err("source should not compile")
        .into_iter()
        .map(|diagnostic| diagnostic.code)
        .collect()
}

#[test]
fn projections_are_typed_at_any_depth() {
    assert_eq!(
        run("t = tree.get()\nreturn t.children[0].children[0].value + t.value"),
        "4"
    );
    assert_eq!(
        run("t = tree.get()\nreturn for child in t.children { return child.value * 2 }"),
        "[4,8]"
    );
    assert_eq!(
        run(r#"
t = tree.get()
return fold total = 0 for child in t.children {
    inner = fold sum = 0 for leaf in child.children { return sum + leaf.value }
    return total + child.value + inner
}
"#),
        "9"
    );
    // The same mistakes are caught three levels down as at the top.
    assert_eq!(error_codes("return tree.get().nope"), ["RL2103"]);
    assert_eq!(
        error_codes("return tree.get().children[0].children[0].nope"),
        ["RL2103"]
    );
    assert_eq!(
        error_codes("return tree.get().children[0].value.digits"),
        ["RL2103"]
    );
}

#[test]
fn recursive_arguments_are_checked_structurally() {
    // A reference, a projection of one, and a literal all fit.
    assert_eq!(run("return tree.put(tree.get()).value"), "1");
    assert_eq!(run("return tree.put(tree.get().children[0]).value"), "2");
    assert_eq!(
        run("return tree.put({ value: 9, children: [{ value: 8, children: [] }] }).children"),
        r#"[{"children":[],"value":8}]"#
    );
    // A differently named definition with the same shape is the same type.
    assert_eq!(run("return twin.put(tree.get()).value"), "1");
    // A nested literal that breaks the shape is rejected...
    assert_eq!(
        error_codes("return tree.put({ value: 9, children: [{ value: [], children: [] }] })"),
        ["RL2311"]
    );
    // ...and so is a recursive type that lacks a required property.
    assert_eq!(error_codes("return labelled.put(tree.get())"), ["RL2311"]);
}

#[test]
fn fold_accumulators_unify_with_recursive_values() {
    assert_eq!(
        run(r#"
t = tree.get()
last = fold acc = t for child in t.children { return child }
return last.value
"#),
        "4"
    );
    assert_eq!(
        run(r#"
t = tree.get()
found = fold acc = null for child in t.children { return child }
return found.value
"#),
        "4"
    );
}

#[test]
fn tool_output_is_validated_through_references() {
    let runtime = runtime(json!({ "value": 1, "children": [
        { "value": 2, "children": [{ "value": "three", "children": [] }] }
    ] }));
    let program = runtime.compile("return tree.get().value").unwrap();
    assert_eq!(runtime.run(&program).unwrap_err().code, "RL6103");
}

#[test]
fn registries_reject_dangling_and_unguarded_definitions() {
    let build = |registry: ToolRegistry| {
        Runtime::builder()
            .registry(registry)
            .tool("t", |_: &[CanonicalValue], _: &ToolContext| {
                Ok(CanonicalValue::Null)
            })
            .build()
            .map(|_| ())
    };
    let with = |definitions: Vec<(&str, Schema)>, output: Schema| {
        let mut registry = ToolRegistry::new();
        for (name, schema) in definitions {
            registry.define(name, schema).unwrap();
        }
        registry.register(tool("t", vec![], output)).unwrap();
        registry
    };

    let dangling = with(vec![], reference("missing"));
    assert!(
        build(dangling.clone())
            .unwrap_err()
            .contains("undefined schema `missing`")
    );
    // `compile` reports the same problem instead of analysing against it.
    let diagnostics = compile("return 1", &dangling, &[]).unwrap_err();
    assert_eq!(diagnostics[0].code, "RL8106");

    for unguarded in [
        vec![("a", reference("a"))],
        vec![
            (
                "a",
                Schema::Union {
                    variants: vec![reference("b"), Schema::Null],
                    discriminator: None,
                },
            ),
            ("b", reference("a")),
        ],
    ] {
        let error = build(with(unguarded, reference("a"))).unwrap_err();
        assert!(error.contains("refers to itself"), "{error}");
    }

    // Mutual recursion through a constructor is fine.
    let mutual = with(
        vec![
            ("a", object(&[("b", reference("b"))], &[])),
            (
                "b",
                Schema::Union {
                    variants: vec![reference("a"), Schema::Null],
                    discriminator: None,
                },
            ),
        ],
        reference("a"),
    );
    assert_eq!(build(mutual), Ok(()));

    let mut registry = ToolRegistry::new();
    registry.define("a", Schema::Null).unwrap();
    assert!(registry.define("a", Schema::Null).is_err());
}

#[test]
fn definitions_are_part_of_the_registry_digest() {
    let mut registry = ToolRegistry::new();
    registry.register(tool("t", vec![], Schema::Any)).unwrap();
    let plain = registry.digest();
    registry.define("tree", tree("tree")).unwrap();
    assert_ne!(registry.digest(), plain);
    // A reference needs its registry: on its own it accepts nothing.
    let leaf = value(json!({ "value": 1, "children": [] }));
    assert!(!reference("tree").accepts(&leaf));
    assert!(registry.accepts(&reference("tree"), &leaf));
}

fn recursive_document() -> serde_json::Value {
    json!({
        "type": "object",
        "properties": {
            "root": { "$ref": "#/$defs/node" },
            "owner": { "$ref": "#/$defs/user" }
        },
        "required": ["root"],
        "$defs": {
            "user": { "type": "object", "properties": { "id": { "type": "integer" } } },
            "node": {
                "type": "object",
                "properties": {
                    "value": { "type": "integer" },
                    "next": { "anyOf": [{ "$ref": "#/$defs/node" }, { "type": "null" }] },
                    "children": { "type": "array", "items": { "$ref": "#/$defs/node" } }
                },
                "required": ["value"]
            }
        }
    })
}

#[test]
fn json_schema_lowering_keeps_recursion_as_definitions() {
    let lowered = Schema::from_json_schema_recursive(&recursive_document(), "crm.tree");
    // Only the recursive reference is named; `user` stays inlined.
    assert_eq!(
        lowered.definitions.keys().collect::<Vec<_>>(),
        ["crm.tree#/$defs/node"]
    );
    assert_eq!(
        lowered.schema.type_notation(),
        "{ owner?: { id?: int }, root: crm.tree#/$defs/node }"
    );

    let mut registry = ToolRegistry::new();
    for (name, schema) in lowered.definitions {
        registry.define(name, schema).unwrap();
    }
    assert_eq!(registry.validate(), Ok(()));
    let deep = |leaf: serde_json::Value| {
        value(json!({ "root": { "value": 1, "next": null, "children": [
            { "value": 2, "next": { "value": 3, "children": [leaf] } }
        ] } }))
    };
    assert!(registry.accepts(&lowered.schema, &deep(json!({ "value": 4 }))));
    // Four levels down, where the non-recursive lowering has given up.
    let invalid = deep(json!({ "value": "four" }));
    assert!(!registry.accepts(&lowered.schema, &invalid));
    assert!(Schema::from_json_schema(&recursive_document()).accepts(&invalid));
}

#[test]
fn json_schema_lowering_handles_degenerate_recursion() {
    // Without recursion the two entry points agree and define nothing.
    let flat = json!({ "type": "array", "items": { "$ref": "#/$defs/id" }, "$defs": { "id": { "type": "integer" } } });
    let lowered = Schema::from_json_schema_recursive(&flat, "ns");
    assert!(lowered.definitions.is_empty());
    assert_eq!(lowered.schema, Schema::from_json_schema(&flat));

    // A self-reference with no constructor describes nothing: it widens.
    let unguarded = Schema::from_json_schema_recursive(
        &json!({
            "$ref": "#/$defs/a",
            "$defs": { "a": { "anyOf": [{ "$ref": "#/$defs/a" }, { "type": "null" }] } }
        }),
        "ns",
    );
    assert_eq!(unguarded.definitions["ns#/$defs/a"], Schema::Any);

    // Mutual recursion defines every participant that is re-entered.
    let mutual = Schema::from_json_schema_recursive(
        &json!({
            "$ref": "#/$defs/folder",
            "$defs": {
                "folder": { "type": "object", "properties": {
                    "entries": { "type": "array", "items": { "$ref": "#/$defs/entry" } }
                } , "additionalProperties": false },
                "entry": { "anyOf": [
                    { "$ref": "#/$defs/folder" },
                    { "type": "object", "properties": { "size": { "type": "integer" } }, "required": ["size"] }
                ] }
            }
        }),
        "fs",
    );
    let mut registry = ToolRegistry::new();
    for (name, schema) in mutual.definitions {
        registry.define(name, schema).unwrap();
    }
    assert_eq!(registry.validate(), Ok(()));
    assert!(registry.accepts(
        &mutual.schema,
        &value(json!({ "entries": [{ "size": 1 }, { "entries": [{ "size": 2 }] }] }))
    ));
    assert!(!registry.accepts(
        &mutual.schema,
        &value(json!({ "entries": [{ "entries": [{ "size": "big" }] }] }))
    ));
}
