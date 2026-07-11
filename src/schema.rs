use crate::CanonicalValue;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Schema {
    Null,
    Boolean,
    Integer {
        min: Option<i64>,
        max: Option<i64>,
    },
    Number {
        min: Option<f64>,
        max: Option<f64>,
    },
    String {
        format: Option<String>,
        enumeration: Vec<String>,
        min_len: Option<usize>,
        max_len: Option<usize>,
    },
    Bytes,
    List {
        items: Box<Schema>,
        min_len: Option<usize>,
        max_len: Option<usize>,
    },
    Object {
        properties: BTreeMap<String, Property>,
        required: BTreeSet<String>,
        additional: bool,
    },
    Map {
        values: Box<Schema>,
    },
    Union {
        variants: Vec<Schema>,
        discriminator: Option<String>,
    },
    Any,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Property {
    pub schema: Schema,
    #[serde(default)]
    pub documentation: String,
    #[serde(default)]
    pub sensitive: bool,
    #[serde(default)]
    pub secret: bool,
    #[serde(default)]
    pub aliases: Vec<String>,
}

impl Property {
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
pub struct CallSchema {
    pub parameters: Vec<Schema>,
}
impl CallSchema {
    pub fn positional(parameters: Vec<Schema>) -> Self {
        Self { parameters }
    }
    pub fn one(schema: Schema) -> Self {
        Self {
            parameters: vec![schema],
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionPolicy {
    Pure,
    Idempotent,
    Recoverable,
    AtMostOnce,
    Unsafe,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolDescriptor {
    pub name: String,
    pub summary: String,
    pub input: CallSchema,
    pub output: Schema,
    pub execution: ExecutionPolicy,
    pub schema_version: String,
}

#[derive(Debug, Clone, Default)]
pub struct ToolRegistry {
    tools: BTreeMap<String, ToolDescriptor>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self::default()
    }
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
    pub fn get(&self, name: &str) -> Option<&ToolDescriptor> {
        self.tools.get(name)
    }
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.tools.keys().map(String::as_str)
    }
    pub fn roots(&self) -> BTreeSet<&str> {
        self.tools
            .keys()
            .filter_map(|n| n.split('.').next())
            .collect()
    }
    pub fn digest(&self) -> String {
        let bytes = serde_json::to_vec(&self.tools).expect("serializable");
        hex::encode(Sha256::digest(bytes))
    }
}

impl Schema {
    pub const INTEGER: Self = Self::Integer {
        min: None,
        max: None,
    };
    pub const NUMBER: Self = Self::Number {
        min: None,
        max: None,
    };
    pub fn string() -> Self {
        Self::String {
            format: None,
            enumeration: vec![],
            min_len: None,
            max_len: None,
        }
    }
    pub fn list(items: Schema) -> Self {
        Self::List {
            items: Box::new(items),
            min_len: None,
            max_len: None,
        }
    }
    pub fn accepts(&self, v: &CanonicalValue) -> bool {
        match (self, v) {
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
                    && xs.iter().all(|x| items.accepts(x))
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
                    && o.iter()
                        .all(|(k, v)| properties.get(k).is_none_or(|p| p.schema.accepts(v)))
            }
            (Self::Map { values }, CanonicalValue::Object(o)) => {
                o.values().all(|v| values.accepts(v))
            }
            (Self::Union { variants, .. }, v) => variants.iter().any(|s| s.accepts(v)),
            _ => false,
        }
    }
}
