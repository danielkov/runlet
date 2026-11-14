use crate::CanonicalValue;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
/// Runtime value schema used to validate inputs and analyze programs.
pub enum Schema {
    /// The `null` value.
    Null,
    /// A Boolean value.
    Boolean,
    /// A signed 64-bit integer with optional inclusive bounds.
    Integer {
        /// Inclusive lower bound.
        min: Option<i64>,
        /// Inclusive upper bound.
        max: Option<i64>,
    },
    /// A finite binary64 number with optional inclusive bounds.
    Number {
        /// Inclusive lower bound.
        min: Option<f64>,
        /// Inclusive upper bound.
        max: Option<f64>,
    },
    /// A Unicode string with optional semantic and size constraints.
    String {
        /// Host-defined semantic format name.
        format: Option<String>,
        /// Allowed values; empty means unrestricted.
        enumeration: Vec<String>,
        /// Minimum number of Unicode scalar values.
        min_len: Option<usize>,
        /// Maximum number of Unicode scalar values.
        max_len: Option<usize>,
    },
    /// An opaque byte sequence.
    Bytes,
    /// A homogeneous ordered list.
    List {
        /// Schema shared by every element.
        items: Box<Schema>,
        /// Minimum element count.
        min_len: Option<usize>,
        /// Maximum element count.
        max_len: Option<usize>,
    },
    /// An object with named properties.
    Object {
        /// Known property definitions.
        properties: BTreeMap<String, Property>,
        /// Properties that must be present.
        required: BTreeSet<String>,
        /// Whether properties absent from `properties` are accepted.
        additional: bool,
    },
    /// An object whose arbitrary keys share one value schema.
    Map {
        /// Schema accepted by every map value.
        values: Box<Schema>,
    },
    /// A value accepted by any of several schemas.
    Union {
        /// Alternative accepted schemas.
        variants: Vec<Schema>,
        /// Optional object property used to distinguish variants.
        discriminator: Option<String>,
    },
    /// Any canonical Runlet value.
    Any,
    /// A named schema defined in the [`ToolRegistry`], the only way to
    /// express a recursive shape such as a tree. A reference is resolved by
    /// the registry that owns the definition, so checks that have no registry
    /// (plain [`Schema::accepts`]) reject it.
    Ref {
        /// Definition name passed to [`ToolRegistry::define`].
        name: String,
    },
    /// The type of expressions that never produce a value (`fail(...)`).
    /// Never unifies with anything: a branch that fails contributes nothing
    /// to the surrounding schema. No runtime value has this schema.
    Never,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
/// Schema and host metadata for an object property.
pub struct Property {
    /// Value schema for the property.
    pub schema: Schema,
    #[serde(default)]
    /// Human-readable guidance for tool callers.
    pub documentation: String,
    #[serde(default)]
    /// Whether observability systems should treat the value as sensitive.
    pub sensitive: bool,
    #[serde(default)]
    /// Whether the value is a secret that must not be exposed.
    pub secret: bool,
    #[serde(default)]
    /// Alternative names accepted by a host adapter.
    pub aliases: Vec<String>,
}

impl Property {
    /// Creates a property with no metadata flags or aliases.
    pub fn new(schema: Schema) -> Self {
        Self {
            schema,
            documentation: String::new(),
            sensitive: false,
            secret: false,
            aliases: vec![],
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
/// Ordered parameter schema for a tool call.
pub struct CallSchema {
    /// Schemas for positional parameters in call order.
    pub parameters: Vec<Schema>,
    /// How many leading parameters a call must provide; the rest are
    /// optional trailing parameters. `None` means every parameter is
    /// required.
    #[serde(default)]
    pub required: Option<usize>,
}
impl CallSchema {
    /// Creates a call schema in which every parameter is required.
    pub fn positional(parameters: Vec<Schema>) -> Self {
        Self {
            parameters,
            required: None,
        }
    }
    /// Creates a required single-parameter call schema.
    pub fn one(schema: Schema) -> Self {
        Self::positional(vec![schema])
    }
    /// A schema whose trailing parameters past `required` may be omitted.
    pub fn optional_trailing(parameters: Vec<Schema>, required: usize) -> Self {
        debug_assert!(required <= parameters.len());
        Self {
            parameters,
            required: Some(required),
        }
    }
    /// The minimum number of arguments a call must provide.
    pub fn required_count(&self) -> usize {
        self.required.unwrap_or(self.parameters.len())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
/// Effect and retry semantics declared by a host tool.
pub enum ExecutionPolicy {
    /// No external effects; unused calls may be pruned and results reused.
    Pure,
    /// Repeating the operation is safe and produces the same external effect.
    Idempotent,
    /// The host can recover or reconcile an interrupted operation.
    Recoverable,
    /// The operation must not be dispatched more than once.
    AtMostOnce,
    /// The operation has effects without stronger retry guarantees.
    Unsafe,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
/// Host-facing declaration of a callable tool.
pub struct ToolDescriptor {
    /// Fully qualified source name, such as `profile.lookup`.
    pub name: String,
    /// Concise description for callers and generated tool catalogs.
    pub summary: String,
    /// Positional input schema.
    pub input: CallSchema,
    /// Successful output schema.
    pub output: Schema,
    /// Effect and retry policy.
    pub execution: ExecutionPolicy,
    /// Host-controlled version included in operation identity.
    pub schema_version: String,
}

#[derive(Debug, Clone, Default)]
/// Deterministically ordered collection of tool descriptors.
pub struct ToolRegistry {
    tools: BTreeMap<String, ToolDescriptor>,
    definitions: BTreeMap<String, Schema>,
}

/// A JSON Schema lowered by [`Schema::from_json_schema_recursive`].
#[derive(Debug, Clone, PartialEq)]
pub struct LoweredSchema {
    /// Schema of the document root.
    pub schema: Schema,
    /// Recursive definitions `schema` refers to, keyed by reference name.
    /// Register each with [`ToolRegistry::define`].
    pub definitions: BTreeMap<String, Schema>,
}

impl ToolRegistry {
    /// Creates an empty registry.
    pub fn new() -> Self {
        Self::default()
    }
    /// Registers a descriptor, rejecting malformed or duplicate names.
    pub fn register(&mut self, descriptor: ToolDescriptor) -> Result<(), String> {
        if descriptor.name.split('.').any(|x| x.is_empty()) {
            return Err("tool names must contain non-empty path segments".into());
        }
        if self.tools.contains_key(&descriptor.name) {
            return Err(format!("duplicate tool `{}`", descriptor.name));
        }
        self.tools.insert(descriptor.name.clone(), descriptor);
        Ok(())
    }
    /// Looks up a descriptor by its fully qualified name.
    pub fn get(&self, name: &str) -> Option<&ToolDescriptor> {
        self.tools.get(name)
    }
    /// Iterates over fully qualified tool names in lexical order.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.tools.keys().map(String::as_str)
    }
    /// Returns the top-level namespace segments used by registered tools.
    pub fn roots(&self) -> BTreeSet<&str> {
        self.tools
            .keys()
            .filter_map(|n| n.split('.').next())
            .collect()
    }
    /// Returns a deterministic hex digest of all descriptors and definitions.
    pub fn digest(&self) -> String {
        // Registries without definitions keep their pre-definition digest.
        let bytes = if self.definitions.is_empty() {
            serde_json::to_vec(&self.tools)
        } else {
            serde_json::to_vec(&(&self.tools, &self.definitions))
        }
        .expect("serializable");
        hex::encode(Sha256::digest(bytes))
    }
    /// Defines the schema that [`Schema::Ref`] `name` stands for.
    ///
    /// Definitions may refer to themselves and to each other, in any order.
    /// Names are host-chosen and global to the registry, so adapters for
    /// several catalogs should namespace them. [`Self::validate`] checks the
    /// finished set.
    pub fn define(&mut self, name: impl Into<String>, schema: Schema) -> Result<(), String> {
        let name = name.into();
        if self.definitions.contains_key(&name) {
            return Err(format!("duplicate schema definition `{name}`"));
        }
        self.definitions.insert(name, schema);
        Ok(())
    }
    /// Looks up a schema definition by name.
    pub fn definition(&self, name: &str) -> Option<&Schema> {
        self.definitions.get(name)
    }
    /// Checks that every reference in a tool or definition is defined and
    /// that no definition is *unguarded*: reachable from itself through
    /// references and unions alone (`a = a | null`), which describes no
    /// finite value shape and could not be resolved.
    pub fn validate(&self) -> Result<(), String> {
        let mut dangling = None;
        let mut check = |owner: &str, schema: &Schema| {
            schema.each_reference(&mut |name| {
                if dangling.is_none() && !self.definitions.contains_key(name) {
                    dangling = Some(format!("{owner} refers to undefined schema `{name}`"));
                }
            })
        };
        for (name, tool) in &self.tools {
            for schema in tool.input.parameters.iter().chain([&tool.output]) {
                check(&format!("tool `{name}`"), schema);
            }
        }
        for (name, schema) in &self.definitions {
            check(&format!("schema definition `{name}`"), schema);
        }
        if let Some(error) = dangling {
            return Err(error);
        }
        match unguarded_definition(&self.definitions) {
            Some(name) => Err(format!(
                "schema definition `{name}` refers to itself without an enclosing \
                 list, map, or object"
            )),
            None => Ok(()),
        }
    }
    /// Returns whether `value` satisfies `schema`, resolving references
    /// against this registry's definitions.
    pub fn accepts(&self, schema: &Schema, value: &CanonicalValue) -> bool {
        schema.accepts_in(value, Some(&self.definitions))
    }
    /// Replaces a reference, or references among a union's variants, with
    /// the schemas they stand for, so the result's outermost constructor is
    /// structural. Nested references are left in place. Undefined and
    /// unguarded references widen to `Any`.
    pub(crate) fn head(&self, schema: Schema) -> Schema {
        match schema {
            Schema::Ref { .. } => {
                let mut current = &schema;
                // A guarded chain is shorter than the definition count.
                for _ in 0..=self.definitions.len() {
                    match current {
                        Schema::Ref { name } => match self.definitions.get(name) {
                            Some(definition) => current = definition,
                            None => return Schema::Any,
                        },
                        other => return self.head(other.clone()),
                    }
                }
                Schema::Any
            }
            Schema::Union { variants, .. }
                if variants.iter().any(|v| matches!(v, Schema::Ref { .. })) =>
            {
                crate::analyzer::union(variants.into_iter().map(|v| self.head(v)).collect())
            }
            other => other,
        }
    }
}

impl Schema {
    /// Unbounded signed 64-bit integer schema.
    pub const INTEGER: Self = Self::Integer {
        min: None,
        max: None,
    };
    /// Unbounded finite binary64 number schema.
    pub const NUMBER: Self = Self::Number {
        min: None,
        max: None,
    };
    /// Creates an unconstrained string schema.
    pub fn string() -> Self {
        Self::String {
            format: None,
            enumeration: vec![],
            min_len: None,
            max_len: None,
        }
    }
    /// Creates an unconstrained list with the given element schema.
    pub fn list(items: Schema) -> Self {
        Self::List {
            items: Box::new(items),
            min_len: None,
            max_len: None,
        }
    }
    /// Compact human-readable type notation, for diagnostics: `string`,
    /// `int[]`, `{ id: string, total?: int }`, `"a" | "b"`,
    /// `{ [key]: number }`. Deliberately lossy — bounds and formats are
    /// omitted; nesting deeper than two levels collapses to kind names.
    pub fn type_notation(&self) -> String {
        self.notation(0)
    }

    fn notation(&self, depth: usize) -> String {
        const MAX_DEPTH: usize = 2;
        match self {
            Self::Integer { .. } => "int".to_string(),
            Self::String { enumeration, .. } if !enumeration.is_empty() => enumeration
                .iter()
                .map(|v| format!("\"{v}\""))
                .collect::<Vec<_>>()
                .join(" | "),
            Self::List { items, .. } => match items.as_ref() {
                Self::Object { .. } | Self::Union { .. } if depth < MAX_DEPTH => {
                    format!("{}[]", items.notation(depth + 1))
                }
                Self::Object { .. } | Self::Union { .. } => "object[]".to_string(),
                other => format!("{}[]", other.notation(depth + 1)),
            },
            Self::Object {
                properties,
                required,
                ..
            } => {
                if depth >= MAX_DEPTH || properties.is_empty() {
                    return "object".to_string();
                }
                let fields = properties
                    .iter()
                    .map(|(name, property)| {
                        let optional = if required.contains(name) { "" } else { "?" };
                        format!("{name}{optional}: {}", property.schema.notation(depth + 1))
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("{{ {fields} }}")
            }
            Self::Map { values } => format!("{{ [key]: {} }}", values.notation(depth + 1)),
            Self::Union { variants, .. } => variants
                .iter()
                .map(|v| v.notation(depth + 1))
                .collect::<Vec<_>>()
                .join(" | "),
            Self::Ref { name } => name.clone(),
            other => other.kind_name().to_string(),
        }
    }

    /// Short lowercase name of the schema's kind, for diagnostics.
    pub fn kind_name(&self) -> &'static str {
        match self {
            Self::Any => "any",
            Self::Null => "null",
            Self::Boolean => "boolean",
            Self::Integer { .. } => "integer",
            Self::Number { .. } => "number",
            Self::String { .. } => "string",
            Self::Bytes => "bytes",
            Self::List { .. } => "list",
            Self::Map { .. } => "map",
            Self::Object { .. } => "object",
            Self::Union { .. } => "union",
            Self::Never => "never",
            Self::Ref { .. } => "reference",
        }
    }

    /// Lowers a JSON Schema document into Runlet's smaller schema vocabulary.
    ///
    /// The conversion is total and only ever *widens*: every JSON value the
    /// source schema accepts is accepted by the result. It is intended for
    /// adapting external tool catalogs (MCP `inputSchema`/`outputSchema`,
    /// OpenAPI).
    ///
    /// - `type` (including type arrays and OpenAPI `nullable`), `enum`,
    ///   `const`, `anyOf`/`oneOf`, `allOf`, and local `$ref`s are lowered, and
    ///   keywords that appear together are intersected.
    /// - Numeric `enum`/`const` values become their covering range.
    /// - `number` becomes `integer | number`, because canonical values keep
    ///   integral JSON numbers as integers.
    /// - Tuples become a list over the union of their element schemas,
    ///   length-bounded when extra elements are forbidden.
    /// - `additionalProperties: false` closes an object; a schema-valued
    ///   `additionalProperties` with no `properties` becomes a map.
    /// - Property `description`s become [`Property::documentation`].
    ///
    /// Precision is lost, never soundness, in these cases:
    ///
    /// - A recursive `$ref` is expanded once and then cut with `Any`; use
    ///   [`Self::from_json_schema_recursive`] to keep it.
    /// - Remote `$ref`s, `not`, and `if`/`then`/`else` become `Any`.
    /// - `pattern`, `multipleOf`, `uniqueItems`, exclusive bounds on
    ///   non-integers, and per-position tuple types are dropped.
    pub fn from_json_schema(schema: &serde_json::Value) -> Self {
        lower_json_schema(schema)
    }

    /// Lowers a JSON Schema document like [`Self::from_json_schema`], but
    /// keeps recursive local `$ref`s as [`Schema::Ref`]s instead of cutting
    /// them with `Any`.
    ///
    /// Each recursive reference becomes a definition named
    /// `{namespace}{reference}`, for example `crm.tree#/$defs/node`.
    /// `namespace` must be unique per lowered document within one registry,
    /// because definition names are registry-global. Non-recursive
    /// references are still inlined. A self-reference with no enclosing
    /// list, map, or object describes no value and is defined as `Any`.
    pub fn from_json_schema_recursive(
        schema: &serde_json::Value,
        namespace: &str,
    ) -> LoweredSchema {
        let mut lowering = JsonSchemaLowering::new(schema, Some(namespace));
        let schema = lowering.lower(schema);
        let mut definitions = lowering.definitions;
        while let Some(name) = unguarded_definition(&definitions) {
            definitions.insert(name, Schema::Any);
        }
        LoweredSchema {
            schema,
            definitions,
        }
    }

    /// Calls `visit` with the name of every reference in this schema.
    fn each_reference(&self, visit: &mut impl FnMut(&str)) {
        match self {
            Self::Ref { name } => visit(name),
            Self::List { items, .. } => items.each_reference(visit),
            Self::Map { values } => values.each_reference(visit),
            Self::Object { properties, .. } => properties
                .values()
                .for_each(|property| property.schema.each_reference(visit)),
            Self::Union { variants, .. } => variants.iter().for_each(|v| v.each_reference(visit)),
            _ => {}
        }
    }

    /// Returns whether a canonical value satisfies this schema.
    ///
    /// A [`Schema::Ref`] accepts nothing here, because it can only be
    /// resolved by its registry; use [`ToolRegistry::accepts`] for schemas
    /// that may contain references.
    pub fn accepts(&self, v: &CanonicalValue) -> bool {
        self.accepts_in(v, None)
    }

    fn accepts_in(
        &self,
        v: &CanonicalValue,
        definitions: Option<&BTreeMap<String, Schema>>,
    ) -> bool {
        match (self, v) {
            // Terminates on validated definitions: each resolution either
            // descends into the finite value or follows a guarded chain.
            (Self::Ref { name }, v) => definitions
                .and_then(|definitions| definitions.get(name))
                .is_some_and(|definition| definition.accepts_in(v, definitions)),
            (Self::Never, _) => false,
            (Self::Any, _) => true,
            (Self::Null, CanonicalValue::Null) => true,
            (Self::Boolean, CanonicalValue::Boolean(_)) => true,
            (Self::Integer { min, max }, CanonicalValue::Integer(x)) => {
                min.is_none_or(|m| *x >= m) && max.is_none_or(|m| *x <= m)
            }
            (Self::Number { min, max }, CanonicalValue::Number(x)) => {
                x.is_finite() && min.is_none_or(|m| *x >= m) && max.is_none_or(|m| *x <= m)
            }
            (
                Self::String {
                    enumeration,
                    min_len,
                    max_len,
                    ..
                },
                CanonicalValue::String(x),
            ) => {
                (enumeration.is_empty() || enumeration.contains(x))
                    && min_len.is_none_or(|m| x.chars().count() >= m)
                    && max_len.is_none_or(|m| x.chars().count() <= m)
            }
            (Self::Bytes, CanonicalValue::Bytes(_)) => true,
            (
                Self::List {
                    items,
                    min_len,
                    max_len,
                },
                CanonicalValue::List(xs),
            ) => {
                min_len.is_none_or(|m| xs.len() >= m)
                    && max_len.is_none_or(|m| xs.len() <= m)
                    && xs.iter().all(|x| items.accepts_in(x, definitions))
            }
            (
                Self::Object {
                    properties,
                    required,
                    additional,
                },
                CanonicalValue::Object(o),
            ) => {
                required.iter().all(|k| o.contains_key(k))
                    && (*additional || o.keys().all(|k| properties.contains_key(k)))
                    && o.iter().all(|(k, v)| {
                        properties
                            .get(k)
                            .is_none_or(|p| p.schema.accepts_in(v, definitions))
                    })
            }
            (Self::Map { values }, CanonicalValue::Object(o)) => {
                o.values().all(|v| values.accepts_in(v, definitions))
            }
            (Self::Union { variants, .. }, v) => {
                variants.iter().any(|s| s.accepts_in(v, definitions))
            }
            _ => false,
        }
    }
}

/// Nesting depth after which JSON Schema lowering widens to `Any`.
const JSON_SCHEMA_DEPTH_LIMIT: usize = 64;

type JsonObject = serde_json::Map<String, serde_json::Value>;

struct JsonSchemaLowering<'a> {
    root: &'a serde_json::Value,
    /// Local references currently being expanded. Re-entering one is a
    /// recursive schema, which Runlet cannot name, so the cycle is cut with
    /// `Any` after a single expansion instead of unrolling it.
    active_references: Vec<&'a str>,
    depth: usize,
    /// Prefix for definition names; `None` cuts recursion with `Any`.
    namespace: Option<&'a str>,
    /// Active references that were re-entered and so need a definition.
    recursive: BTreeSet<&'a str>,
    definitions: BTreeMap<String, Schema>,
}

impl<'a> JsonSchemaLowering<'a> {
    fn new(root: &'a serde_json::Value, namespace: Option<&'a str>) -> Self {
        Self {
            root,
            active_references: vec![],
            depth: 0,
            namespace,
            recursive: BTreeSet::new(),
            definitions: BTreeMap::new(),
        }
    }

    fn lower(&mut self, node: &'a serde_json::Value) -> Schema {
        let serde_json::Value::Object(object) = node else {
            return Schema::Any;
        };
        if self.depth >= JSON_SCHEMA_DEPTH_LIMIT {
            return Schema::Any;
        }
        self.depth += 1;
        let schema = self.lower_object(object);
        self.depth -= 1;
        schema
    }

    /// Every applicable keyword contributes one schema; a value must satisfy
    /// all of them, so the result is their intersection.
    fn lower_object(&mut self, object: &'a JsonObject) -> Schema {
        use crate::analyzer::union;
        use serde_json::Value as J;
        let mut parts = vec![];
        if let Some(constant) = object.get("const") {
            parts.push(json_literal_schema(std::slice::from_ref(constant)));
        }
        if let Some(J::Array(values)) = object.get("enum") {
            parts.push(json_literal_schema(values));
        }
        if let Some(J::String(reference)) = object.get("$ref") {
            parts.push(self.reference(reference));
        }
        for keyword in ["anyOf", "oneOf"] {
            if let Some(J::Array(variants)) = object.get(keyword) {
                let variants = variants.iter().map(|v| self.lower(v)).collect();
                parts.push(union(variants));
            }
        }
        if let Some(J::Array(all)) = object.get("allOf") {
            for member in all {
                parts.push(self.lower(member));
            }
        }
        parts.push(self.typed(object));
        let schema = parts
            .into_iter()
            .reduce(intersect_schemas)
            .unwrap_or(Schema::Any);
        if object.get("nullable") == Some(&J::Bool(true)) {
            union(vec![schema, Schema::Null])
        } else {
            schema
        }
    }

    fn reference(&mut self, reference: &'a str) -> Schema {
        let Some(target) = reference
            .strip_prefix('#')
            .and_then(|pointer| self.root.pointer(pointer))
        else {
            return Schema::Any;
        };
        let Some(namespace) = self.namespace else {
            if self.active_references.contains(&reference) {
                return Schema::Any;
            }
            self.active_references.push(reference);
            let schema = self.lower(target);
            self.active_references.pop();
            return schema;
        };
        let name = format!("{namespace}{reference}");
        if self.definitions.contains_key(&name) {
            return Schema::Ref { name };
        }
        if self.active_references.contains(&reference) {
            self.recursive.insert(reference);
            return Schema::Ref { name };
        }
        self.active_references.push(reference);
        let schema = self.lower(target);
        self.active_references.pop();
        // Only references that turned out to be recursive are named; the
        // rest stay inlined so ordinary schemas lower exactly as before.
        if self.recursive.remove(reference) {
            self.definitions.insert(name.clone(), schema);
            Schema::Ref { name }
        } else {
            schema
        }
    }

    /// Schema implied by `type`, or by structural keywords when it is absent.
    fn typed(&mut self, object: &'a JsonObject) -> Schema {
        use serde_json::Value as J;
        match object.get("type") {
            Some(J::String(name)) => self.by_type(name, object),
            Some(J::Array(names)) => {
                let variants = names
                    .iter()
                    .map(|name| match name.as_str() {
                        Some(name) => self.by_type(name, object),
                        None => Schema::Any,
                    })
                    .collect();
                crate::analyzer::union(variants)
            }
            _ if ["properties", "required", "additionalProperties"]
                .iter()
                .any(|keyword| object.contains_key(*keyword)) =>
            {
                self.by_type("object", object)
            }
            _ if object.contains_key("items") || object.contains_key("prefixItems") => {
                self.by_type("array", object)
            }
            _ => Schema::Any,
        }
    }

    fn by_type(&mut self, name: &str, object: &'a JsonObject) -> Schema {
        use serde_json::Value as J;
        match name {
            "null" => Schema::Null,
            "boolean" => Schema::Boolean,
            "integer" => {
                // Exclusive integer bounds are exact as inclusive neighbours.
                let exclusive = |keyword, step: i64| {
                    object
                        .get(keyword)
                        .and_then(J::as_i64)
                        .and_then(|bound| bound.checked_add(step))
                };
                let inclusive = |keyword| object.get(keyword).and_then(J::as_i64);
                Schema::Integer {
                    min: tighter(
                        inclusive("minimum"),
                        exclusive("exclusiveMinimum", 1),
                        i64::max,
                    ),
                    max: tighter(
                        inclusive("maximum"),
                        exclusive("exclusiveMaximum", -1),
                        i64::min,
                    ),
                }
            }
            // Canonical values keep integral JSON numbers as integers.
            "number" => {
                let bound = |keyword| object.get(keyword).and_then(J::as_f64);
                crate::analyzer::union(vec![
                    Schema::INTEGER,
                    Schema::Number {
                        min: tighter(bound("minimum"), bound("exclusiveMinimum"), f64::max),
                        max: tighter(bound("maximum"), bound("exclusiveMaximum"), f64::min),
                    },
                ])
            }
            "string" => Schema::String {
                format: object.get("format").and_then(J::as_str).map(str::to_owned),
                enumeration: vec![],
                min_len: json_usize(object.get("minLength")),
                max_len: json_usize(object.get("maxLength")),
            },
            "array" => self.array(object),
            "object" => self.object(object),
            _ => Schema::Any,
        }
    }

    /// Lists and tuples. A tuple lowers to a list over the union of its
    /// positional and trailing element schemas, closed to the tuple's length
    /// when it forbids extra elements.
    fn array(&mut self, object: &'a JsonObject) -> Schema {
        use serde_json::Value as J;
        // 2020-12 tuples use `prefixItems` + `items`; earlier drafts use an
        // array-valued `items` + `additionalItems`.
        let (prefix, rest) = match (object.get("prefixItems"), object.get("items")) {
            (Some(J::Array(prefix)), rest) => (Some(prefix), rest),
            (_, Some(J::Array(prefix))) => (Some(prefix), object.get("additionalItems")),
            (_, rest) => (None, rest),
        };
        let closed = rest == Some(&J::Bool(false));
        let mut elements = vec![];
        for element in prefix.into_iter().flatten() {
            elements.push(self.lower(element));
        }
        if !closed {
            elements.push(rest.map_or(Schema::Any, |rest| self.lower(rest)));
        }
        let max_items = json_usize(object.get("maxItems"));
        Schema::List {
            // A closed empty tuple has no elements to describe.
            items: Box::new(crate::analyzer::union(elements)),
            min_len: json_usize(object.get("minItems")),
            max_len: match prefix {
                Some(prefix) if closed => tighter(max_items, Some(prefix.len()), usize::min),
                _ => max_items,
            },
        }
    }

    fn object(&mut self, object: &'a JsonObject) -> Schema {
        use serde_json::Value as J;
        let mut properties = BTreeMap::new();
        if let Some(J::Object(declared)) = object.get("properties") {
            for (name, schema) in declared {
                let mut property = Property::new(self.lower(schema));
                if let Some(text) = schema.get("description").and_then(J::as_str) {
                    property.documentation = text.to_owned();
                }
                properties.insert(name.clone(), property);
            }
        }
        let required: BTreeSet<String> = match object.get("required") {
            Some(J::Array(names)) => names
                .iter()
                .filter_map(|name| name.as_str().map(str::to_owned))
                .collect(),
            _ => BTreeSet::new(),
        };
        let patterned = object.contains_key("patternProperties");
        match object.get("additionalProperties") {
            Some(values @ J::Object(_))
                if properties.is_empty() && required.is_empty() && !patterned =>
            {
                Schema::Map {
                    values: Box::new(self.lower(values)),
                }
            }
            additional => Schema::Object {
                properties,
                required,
                additional: patterned || additional != Some(&J::Bool(false)),
            },
        }
    }
}

fn lower_json_schema(root: &serde_json::Value) -> Schema {
    JsonSchemaLowering::new(root, None).lower(root)
}

/// Name of a definition reachable from itself through references and union
/// variants alone, if any. Such a definition has no structural constructor
/// to bound resolution.
fn unguarded_definition(definitions: &BTreeMap<String, Schema>) -> Option<String> {
    fn exposed<'a>(schema: &'a Schema, out: &mut Vec<&'a str>) {
        match schema {
            Schema::Ref { name } => out.push(name),
            Schema::Union { variants, .. } => variants.iter().for_each(|v| exposed(v, out)),
            _ => {}
        }
    }
    for start in definitions.keys() {
        let mut seen = BTreeSet::new();
        let mut pending = vec![start.as_str()];
        while let Some(name) = pending.pop() {
            let Some(definition) = definitions.get(name) else {
                continue;
            };
            let mut next = vec![];
            exposed(definition, &mut next);
            for name in next {
                if name == start {
                    return Some(start.clone());
                }
                if seen.insert(name) {
                    pending.push(name);
                }
            }
        }
    }
    None
}

fn json_usize(value: Option<&serde_json::Value>) -> Option<usize> {
    value?.as_u64()?.try_into().ok()
}

/// Combines two optional bounds, keeping the stricter when both are present.
fn tighter<T>(a: Option<T>, b: Option<T>, pick: impl FnOnce(T, T) -> T) -> Option<T> {
    match (a, b) {
        (Some(a), Some(b)) => Some(pick(a, b)),
        (a, b) => a.or(b),
    }
}

/// Schema for a JSON Schema `enum`/`const` literal list: exact for strings,
/// the covering range for numbers, and the kind for everything else.
fn json_literal_schema(values: &[serde_json::Value]) -> Schema {
    use serde_json::Value as J;
    let mut strings = vec![];
    let mut integers: Option<(i64, i64)> = None;
    let mut numbers: Option<(f64, f64)> = None;
    let mut others = vec![];
    for value in values {
        match value {
            J::String(text) => {
                if !strings.contains(text) {
                    strings.push(text.clone());
                }
            }
            J::Null => others.push(Schema::Null),
            J::Bool(_) => others.push(Schema::Boolean),
            J::Number(n) => match (n.as_i64(), n.as_f64()) {
                (Some(i), _) => {
                    let (low, high) = integers.unwrap_or((i, i));
                    integers = Some((low.min(i), high.max(i)));
                }
                (None, Some(x)) => {
                    let (low, high) = numbers.unwrap_or((x, x));
                    numbers = Some((low.min(x), high.max(x)));
                }
                (None, None) => return Schema::Any,
            },
            J::Array(_) | J::Object(_) => return Schema::Any,
        }
    }
    if let Some((min, max)) = integers {
        others.push(Schema::Integer {
            min: Some(min),
            max: Some(max),
        });
    }
    if let Some((min, max)) = numbers {
        others.push(Schema::Number {
            min: Some(min),
            max: Some(max),
        });
    }
    if !strings.is_empty() {
        others.push(Schema::String {
            format: None,
            enumeration: strings,
            min_len: None,
            max_len: None,
        });
    }
    crate::analyzer::union(others)
}

/// Schema accepted by both `a` and `b`, or a widening of it. Where the two
/// cannot be combined structurally the result is `a`, which is always a
/// valid widening of the intersection.
fn intersect_schemas(a: Schema, b: Schema) -> Schema {
    use Schema as S;
    if a == b {
        return a;
    }
    match (a, b) {
        (S::Any, other) | (other, S::Any) => other,
        (
            S::Integer { min, max },
            S::Integer {
                min: other_min,
                max: other_max,
            },
        ) => S::Integer {
            min: tighter(min, other_min, i64::max),
            max: tighter(max, other_max, i64::min),
        },
        (
            S::Number { min, max },
            S::Number {
                min: other_min,
                max: other_max,
            },
        ) => S::Number {
            min: tighter(min, other_min, f64::max),
            max: tighter(max, other_max, f64::min),
        },
        (
            S::String {
                format,
                enumeration,
                min_len,
                max_len,
            },
            S::String {
                format: other_format,
                enumeration: other_enumeration,
                min_len: other_min_len,
                max_len: other_max_len,
            },
        ) => {
            let common: Vec<String> = enumeration
                .iter()
                .filter(|value| other_enumeration.contains(value))
                .cloned()
                .collect();
            S::String {
                format: format.or(other_format),
                // An empty enumeration means unrestricted, so a disjoint
                // pair keeps the left side instead of becoming unrestricted.
                enumeration: if enumeration.is_empty() {
                    other_enumeration
                } else if other_enumeration.is_empty() || common.is_empty() {
                    enumeration
                } else {
                    common
                },
                min_len: tighter(min_len, other_min_len, usize::max),
                max_len: tighter(max_len, other_max_len, usize::min),
            }
        }
        (
            S::List {
                items,
                min_len,
                max_len,
            },
            S::List {
                items: other_items,
                min_len: other_min_len,
                max_len: other_max_len,
            },
        ) => S::List {
            items: Box::new(intersect_schemas(*items, *other_items)),
            min_len: tighter(min_len, other_min_len, usize::max),
            max_len: tighter(max_len, other_max_len, usize::min),
        },
        (
            S::Object {
                mut properties,
                mut required,
                additional,
            },
            S::Object {
                properties: other_properties,
                required: other_required,
                additional: other_additional,
            },
        ) => {
            for (name, other) in other_properties {
                let merged = match properties.remove(&name) {
                    Some(mut property) => {
                        property.schema = intersect_schemas(property.schema, other.schema);
                        if property.documentation.is_empty() {
                            property.documentation = other.documentation;
                        }
                        property
                    }
                    None => other,
                };
                properties.insert(name, merged);
            }
            required.extend(other_required);
            S::Object {
                properties,
                required,
                additional: additional && other_additional,
            }
        }
        (S::Union { variants, .. }, other) => {
            let others = match &other {
                S::Union { variants, .. } => variants.clone(),
                single => vec![single.clone()],
            };
            let mut kept = vec![];
            for variant in &variants {
                for candidate in &others {
                    if std::mem::discriminant(variant) == std::mem::discriminant(candidate) {
                        kept.push(intersect_schemas(variant.clone(), candidate.clone()));
                    } else if overlap_without_combining(variant, candidate) {
                        kept.push(variant.clone());
                    }
                }
            }
            if kept.is_empty() {
                S::Union {
                    variants,
                    discriminator: None,
                }
            } else {
                crate::analyzer::union(kept)
            }
        }
        (single, other @ S::Union { .. }) => {
            match intersect_schemas(other, single.clone()) {
                // No variant matched: the union came back unchanged.
                S::Union { .. } => single,
                narrowed => narrowed,
            }
        }
        (a, _) => a,
    }
}

/// Different kinds that can still share values: objects with maps, and a
/// reference with anything, since its shape is not known here.
fn overlap_without_combining(a: &Schema, b: &Schema) -> bool {
    let keyed = |schema: &Schema| matches!(schema, Schema::Object { .. } | Schema::Map { .. });
    let reference = |schema: &Schema| matches!(schema, Schema::Ref { .. });
    (keyed(a) && keyed(b)) || reference(a) || reference(b)
}
