//! Rust types from the Codex app-server's JSON Schema bundle, for the methods Slopty speaks.
//!
//! The bundle is what `schemars` makes of Codex's own Rust types, so its shapes are few:
//! objects with properties, string enums, `oneOf` unions tagged by a single-value property or
//! by their one key, `anyOf` with `null` for an option, and references, alone or wrapped in a
//! one-member `allOf`. Each maps to the serde shape that reads it back. Only the definitions the
//! chosen methods reach are written, so a method Slopty does not speak changes nothing here.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::{self, Write as _};

use anyhow::{Context as _, Result, bail, ensure};
use serde_json::{Map, Value};

/// The requests Slopty sends.
const CLIENT_REQUESTS: [&str; 17] = [
    "initialize",
    "thread/start",
    "thread/resume",
    "thread/unarchive",
    "thread/fork",
    "thread/read",
    "thread/list",
    "thread/loaded/list",
    "thread/unsubscribe",
    "thread/name/set",
    "thread/settings/update",
    "thread/goal/get",
    "turn/start",
    "turn/steer",
    "turn/interrupt",
    "model/list",
    "account/usage/read",
];

/// The requests the app-server sends that Slopty answers: approvals and questions.
const SERVER_REQUESTS: [&str; 5] = [
    "item/commandExecution/requestApproval",
    "item/fileChange/requestApproval",
    "item/permissions/requestApproval",
    "item/tool/requestUserInput",
    "mcpServer/elicitation/request",
];

/// The notifications Slopty reads.
const SERVER_NOTIFICATIONS: [&str; 34] = [
    "error",
    "warning",
    "thread/started",
    "thread/status/changed",
    "thread/closed",
    "thread/name/updated",
    "thread/settings/updated",
    "thread/reverted",
    "thread/goal/updated",
    "thread/goal/cleared",
    "thread/tokenUsage/updated",
    "thread/compacted",
    "thread/queue/changed",
    "turn/started",
    "turn/completed",
    "hook/started",
    "hook/completed",
    "turn/diff/updated",
    "turn/plan/updated",
    "item/started",
    "item/completed",
    "item/agentMessage/delta",
    "item/plan/delta",
    "item/reasoning/summaryTextDelta",
    "item/reasoning/summaryPartAdded",
    "item/reasoning/textDelta",
    "item/commandExecution/outputDelta",
    "item/fileChange/outputDelta",
    "item/fileChange/patchUpdated",
    "item/mcpToolCall/progress",
    "serverRequest/resolved",
    "mcpServer/startupStatus/updated",
    "account/rateLimits/updated",
    "model/rerouted",
];

/// Names a definition may not keep: they would shadow the prelude.
const TAKEN: [&str; 6] = ["Option", "Result", "Vec", "String", "Box", "Value"];

/// One method of a union: its wire name, and the definition its params are.
#[derive(Debug)]
struct Method {
    name: String,
    params: Option<String>,
}

/// The bundle's definitions by name, `v2` folded in.
struct Defs {
    defs: BTreeMap<String, Value>,
}

impl Defs {
    fn of(bundle: &Value) -> Result<Self> {
        let top = bundle.get("definitions").and_then(Value::as_object).context("no definitions")?;
        let mut defs: BTreeMap<String, Value> = BTreeMap::new();
        if let Some(v2) = top.get("v2").and_then(Value::as_object) {
            for (name, def) in v2 {
                defs.insert(name.clone(), def.clone());
            }
        }
        for (name, def) in top.iter().filter(|(name, _)| *name != "v2") {
            match defs.get(name) {
                Some(have) if bare(have) != bare(def) => {
                    bail!("{name} is defined twice, differently");
                }
                Some(_) => {}
                None => {
                    defs.insert(name.clone(), def.clone());
                }
            }
        }
        Ok(Self { defs })
    }

    fn get(&self, name: &str) -> Result<&Value> {
        self.defs.get(name).with_context(|| format!("no definition {name}"))
    }

    /// The methods of the union `name` (`ClientRequest`, `ServerNotification`, …).
    fn methods(&self, name: &str) -> Result<Vec<Method>> {
        let members = self.get(name)?.get("oneOf").and_then(Value::as_array);
        let members = members.with_context(|| format!("{name} is no union"))?;
        members
            .iter()
            .map(|m| {
                let props = m.get("properties").context("a method with no properties")?;
                let method = props.pointer("/method/enum/0").and_then(Value::as_str);
                let method = method.context("a method with no name")?.to_owned();
                let params = props.get("params").and_then(params_of);
                Ok(Method { name: method, params })
            })
            .collect()
    }
}

/// `def` without what names it where it stands, to compare two copies of one definition.
fn bare(def: &Value) -> Value {
    let mut def = def.clone();
    if let Some(map) = def.as_object_mut() {
        map.remove("$schema");
        map.remove("title");
    }
    def
}

/// The definition a method's `params` are. Params that may be left out are an `anyOf` of
/// that definition and `null`; Slopty always sends them.
fn params_of(schema: &Value) -> Option<String> {
    reference(schema).or_else(|| {
        let members = schema.get("anyOf")?.as_array()?;
        let mut defined = members.iter().filter(|m| !is_null(m));
        let only = defined.next()?;
        defined.next().is_none().then(|| reference(only)).flatten()
    })
}

/// The definition `schema` refers to, by name.
fn reference(schema: &Value) -> Option<String> {
    let path = schema.get("$ref")?.as_str()?;
    let name = path.strip_prefix("#/definitions/")?;
    Some(name.strip_prefix("v2/").unwrap_or(name).to_owned())
}

/// A Rust type, as written.
#[derive(Clone, PartialEq, Eq, Debug)]
enum Ty {
    Named(String),
    String,
    Bool,
    Int(&'static str),
    Float,
    Json,
    Vec(Box<Self>),
    Map(Box<Self>),
    Option(Box<Self>),
    Boxed(Box<Self>),
}

impl Ty {
    fn render(&self) -> String {
        match self {
            Self::Named(name) => name.clone(),
            Self::String => "String".to_owned(),
            Self::Bool => "bool".to_owned(),
            Self::Int(name) => (*name).to_owned(),
            Self::Float => "f64".to_owned(),
            Self::Json => "serde_json::Value".to_owned(),
            Self::Vec(inner) => format!("Vec<{}>", inner.render()),
            Self::Map(inner) => format!("BTreeMap<String, {}>", inner.render()),
            Self::Option(inner) => format!("Option<{}>", inner.render()),
            Self::Boxed(inner) => format!("Box<{}>", inner.render()),
        }
    }

    const fn is_option(&self) -> bool {
        matches!(self, Self::Option(_))
    }
}

/// One field of a struct or a struct variant.
#[derive(Debug)]
struct Field {
    wire: String,
    ty: Ty,
    doc: Option<String>,
    /// The union an object is besides its own properties, read from the same object.
    flatten: bool,
}

/// One variant of an enum.
#[derive(Debug)]
struct Variant {
    wire: String,
    doc: Option<String>,
    body: Body,
}

/// What a variant carries.
#[derive(Debug)]
enum Body {
    Unit,
    Fields(Vec<Field>),
    Newtype(Ty),
}

/// How a definition is written.
#[derive(Debug)]
enum Shape {
    Struct(Vec<Field>),
    /// Serde's own tagging: a unit variant as its name, any other as `{name: body}`.
    External(Vec<Variant>),
    /// A property names the variant.
    Tagged {
        tag: String,
        variants: Vec<Variant>,
    },
    /// The first variant that reads it.
    Untagged(Vec<Variant>),
    Alias(Ty),
}

/// A definition to write.
#[derive(Debug)]
struct Item {
    name: String,
    doc: Option<String>,
    shape: Shape,
}

/// Turns the reached definitions into items, naming the inline ones after where they stand.
struct Builder<'a> {
    defs: &'a Defs,
    items: BTreeMap<String, Item>,
    /// Definitions being written, so one that refers back to itself is not begun again.
    begun: BTreeSet<String>,
    renames: BTreeMap<String, String>,
}

impl<'a> Builder<'a> {
    fn new(defs: &'a Defs) -> Self {
        let renames = TAKEN.iter().map(|t| ((*t).to_owned(), format!("Codex{t}"))).collect();
        Self { defs, items: BTreeMap::new(), begun: BTreeSet::new(), renames }
    }

    fn name(&self, def: &str) -> String {
        self.renames.get(def).cloned().unwrap_or_else(|| pascal(def))
    }

    fn define(&mut self, def: &str) -> Result<()> {
        let name = self.name(def);
        if !self.begun.insert(name.clone()) {
            return Ok(());
        }
        let schema = self.defs.get(def)?.clone();
        let item = self.item(&name, &schema).with_context(|| format!("definition {def}"))?;
        self.items.insert(name, item);
        Ok(())
    }

    fn item(&mut self, name: &str, schema: &Value) -> Result<Item> {
        let doc = description(schema);
        let shape = self.shape(name, schema)?;
        Ok(Item { name: name.to_owned(), doc, shape })
    }

    fn shape(&mut self, name: &str, schema: &Value) -> Result<Shape> {
        if schema.get("properties").is_some() {
            return Ok(Shape::Struct(self.fields(name, schema, None)?));
        }
        if let Some(members) = schema.get("oneOf").and_then(Value::as_array) {
            return self.union(name, members);
        }
        if let Some(members) = schema.get("anyOf").and_then(Value::as_array) {
            let (nulls, rest): (Vec<&Value>, Vec<&Value>) =
                members.iter().partition(|m| is_null(m));
            if nulls.is_empty() {
                return self.untagged(name, members);
            }
            if rest.len() > 1 {
                // The type is written as the option of its own non-null members, so `ty` does
                // not define the definition's name over the definition itself.
                let inner = serde_json::json!({ "anyOf": rest });
                let ty = self.ty(&format!("{name}Value"), &inner)?;
                return Ok(Shape::Alias(Ty::Option(Box::new(ty))));
            }
        }
        if let Some(values) = string_values(schema) {
            return Ok(Shape::External(values.into_iter().map(unit).collect()));
        }
        Ok(Shape::Alias(self.ty(name, schema)?))
    }

    /// A `oneOf`, by what its members have in common.
    fn union(&mut self, name: &str, members: &[Value]) -> Result<Shape> {
        let members: Vec<Value> = members.iter().map(|m| self.inline(m)).collect::<Result<_>>()?;
        if let Some(tag) = tag_of(&members) {
            let variants = members
                .iter()
                .map(|m| {
                    let wire = m
                        .pointer(&format!("/properties/{tag}/enum/0"))
                        .and_then(Value::as_str)
                        .context("a variant with no tag")?
                        .to_owned();
                    let fields = self.fields(&format!("{name}{}", pascal(&wire)), m, Some(&tag))?;
                    let body = if fields.is_empty() { Body::Unit } else { Body::Fields(fields) };
                    Ok(Variant { wire, doc: description(m), body })
                })
                .collect::<Result<_>>()?;
            return Ok(Shape::Tagged { tag, variants });
        }
        if members.iter().all(|m| string_values(m).is_some() || single_key(m).is_some()) {
            let mut variants = Vec::new();
            for m in &members {
                if let Some(values) = string_values(m) {
                    let doc = description(m);
                    variants.extend(values.into_iter().map(|(wire, own)| Variant {
                        wire,
                        doc: own.or_else(|| doc.clone()),
                        body: Body::Unit,
                    }));
                } else if let Some((key, inner)) = single_key(m) {
                    let ty = self.ty(&format!("{name}{}", pascal(&key)), &inner)?;
                    let body = match ty {
                        Ty::Named(_) | Ty::Boxed(_) => Body::Newtype(ty),
                        other => Body::Newtype(other),
                    };
                    variants.push(Variant { wire: key, doc: description(m), body });
                }
            }
            return Ok(Shape::External(variants));
        }
        self.untagged(name, &members)
    }

    /// A `oneOf` or `anyOf` read by trying each member in turn.
    fn untagged(&mut self, name: &str, members: &[Value]) -> Result<Shape> {
        let variants = members
            .iter()
            .enumerate()
            .map(|(at, m)| {
                let label = m
                    .get("title")
                    .and_then(Value::as_str)
                    .map(pascal)
                    .or_else(|| reference(m).map(|r| self.name(&r)))
                    .or_else(|| {
                        let kind = m.get("type").and_then(Value::as_str)?;
                        let alone = members
                            .iter()
                            .filter(|o| o.get("type").and_then(Value::as_str) == Some(kind))
                            .count()
                            == 1;
                        alone.then(|| pascal(kind))
                    })
                    .unwrap_or_else(|| format!("Variant{at}"));
                let ty = self.ty(&format!("{name}{label}"), m)?;
                Ok(Variant { wire: label, doc: description(m), body: Body::Newtype(ty) })
            })
            .collect::<Result<_>>()?;
        Ok(Shape::Untagged(variants))
    }

    /// `member` with a reference followed, to look inside it.
    fn inline(&self, member: &Value) -> Result<Value> {
        match reference(member) {
            Some(def) => Ok(self.defs.get(&def)?.clone()),
            None => Ok(member.clone()),
        }
    }

    /// The fields of the object `schema`, but `skip`, the tag a variant is named by.
    fn fields(&mut self, owner: &str, schema: &Value, skip: Option<&str>) -> Result<Vec<Field>> {
        let required: BTreeSet<&str> = schema
            .get("required")
            .and_then(Value::as_array)
            .map(|r| r.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        let empty = Map::new();
        let props = schema.get("properties").and_then(Value::as_object).unwrap_or(&empty);
        let mut fields = props
            .iter()
            .filter(|(wire, _)| Some(wire.as_str()) != skip)
            .map(|(wire, prop)| {
                let ty = self.ty(&format!("{owner}{}", pascal(wire)), prop)?;
                let ty = if required.contains(wire.as_str()) || ty.is_option() {
                    ty
                } else {
                    Ty::Option(Box::new(ty))
                };
                Ok(Field { wire: wire.clone(), ty, doc: description(prop), flatten: false })
            })
            .collect::<Result<Vec<_>>>()?;
        fields.extend(self.composed(owner, schema)?);
        Ok(fields)
    }

    /// The union an object with properties also is (`oneOf` or `anyOf` beside `properties`), as
    /// a field flattened into it: named by the union's tag where it has one.
    fn composed(&mut self, owner: &str, schema: &Value) -> Result<Option<Field>> {
        let Some(members) =
            ["oneOf", "anyOf"].iter().find_map(|key| schema.get(*key).and_then(Value::as_array))
        else {
            return Ok(None);
        };
        let inlined: Vec<Value> = members.iter().map(|m| self.inline(m)).collect::<Result<_>>()?;
        let wire = tag_of(&inlined).unwrap_or_else(|| "form".to_owned());
        let name = format!("{owner}{}", pascal(&wire));
        let shape = self.union(&name, members)?;
        let item = Item { name: name.clone(), doc: None, shape };
        self.items.insert(name.clone(), item);
        let doc = Some(format!("What else it is, by `{wire}`."));
        Ok(Some(Field { wire, ty: Ty::Named(name), doc, flatten: true }))
    }

    /// The type of `schema`, an inline one defined as `name`.
    fn ty(&mut self, name: &str, schema: &Value) -> Result<Ty> {
        if let Some(def) = reference(schema) {
            self.define(&def)?;
            return Ok(Ty::Named(self.name(&def)));
        }
        // `schemars` wraps a reference that carries its own description or default.
        if let Some([only]) = schema.get("allOf").and_then(Value::as_array).map(Vec::as_slice) {
            return self.ty(name, only);
        }
        for key in ["anyOf", "oneOf"] {
            if schema.get("properties").is_some() {
                break;
            }
            if let Some(members) = schema.get(key).and_then(Value::as_array) {
                let (nulls, rest): (Vec<&Value>, Vec<&Value>) =
                    members.iter().partition(|m| is_null(m));
                if !nulls.is_empty() && rest.len() == 1 {
                    let inner = rest.first().context("a member")?;
                    return Ok(Ty::Option(Box::new(self.ty(name, inner)?)));
                }
                let members: Vec<Value> = rest.into_iter().cloned().collect();
                let shape = if key == "oneOf" {
                    self.union(name, &members)?
                } else {
                    self.untagged(name, &members)?
                };
                let item = Item { name: name.to_owned(), doc: description(schema), shape };
                self.items.insert(name.to_owned(), item);
                let ty = Ty::Named(name.to_owned());
                return Ok(if nulls.is_empty() { ty } else { Ty::Option(Box::new(ty)) });
            }
        }
        if string_values(schema).is_some() || schema.get("properties").is_some() {
            let item = self.item(name, schema)?;
            self.items.insert(name.to_owned(), item);
            return Ok(Ty::Named(name.to_owned()));
        }
        let (kind, nullable) = match schema.get("type") {
            Some(Value::String(kind)) => (Some(kind.as_str()), false),
            Some(Value::Array(kinds)) => {
                let kinds: Vec<&str> = kinds.iter().filter_map(Value::as_str).collect();
                let nullable = kinds.contains(&"null");
                let real: Vec<&str> = kinds.into_iter().filter(|k| *k != "null").collect();
                (if real.len() == 1 { real.first().copied() } else { None }, nullable)
            }
            _ => (None, false),
        };
        let ty = match kind {
            Some("string") => Ty::String,
            Some("boolean") => Ty::Bool,
            Some("integer") => Ty::Int(int(schema.get("format").and_then(Value::as_str))),
            Some("number") => Ty::Float,
            Some("array") => match schema.get("items") {
                Some(items) => Ty::Vec(Box::new(self.ty(&format!("{name}Item"), items)?)),
                None => Ty::Vec(Box::new(Ty::Json)),
            },
            Some("object") => match schema.get("additionalProperties") {
                Some(Value::Object(_)) => {
                    let inner = schema.get("additionalProperties").context("values")?;
                    Ty::Map(Box::new(self.ty(&format!("{name}Value"), inner)?))
                }
                _ => Ty::Map(Box::new(Ty::Json)),
            },
            _ => Ty::Json,
        };
        Ok(if nullable { Ty::Option(Box::new(ty)) } else { ty })
    }
}

fn unit((wire, doc): (String, Option<String>)) -> Variant {
    Variant { wire, doc, body: Body::Unit }
}

fn is_null(schema: &Value) -> bool {
    schema.get("type").and_then(Value::as_str) == Some("null")
}

/// The values of a string enum, each with its own description where `oneOf` gave one.
fn string_values(schema: &Value) -> Option<Vec<(String, Option<String>)>> {
    if schema.get("type").and_then(Value::as_str) != Some("string") {
        return None;
    }
    let values = schema.get("enum")?.as_array()?;
    values.iter().map(|v| Some((v.as_str()?.to_owned(), None))).collect()
}

/// A member that is an object of one required key: an externally tagged variant.
fn single_key(member: &Value) -> Option<(String, Value)> {
    let props = member.get("properties")?.as_object()?;
    let required = member.get("required")?.as_array()?;
    let [(key, inner)] = props.iter().collect::<Vec<_>>()[..] else { return None };
    (required.len() == 1 && required.first()?.as_str() == Some(key.as_str()))
        .then(|| (key.clone(), inner.clone()))
}

/// The property every member names itself by, with one string value each.
fn tag_of(members: &[Value]) -> Option<String> {
    let first = members.first()?.get("properties")?.as_object()?;
    first
        .keys()
        .find(|key| {
            members.iter().all(|m| {
                let one = m.pointer(&format!("/properties/{key}/enum"));
                let required = m.get("required").and_then(Value::as_array);
                one.and_then(Value::as_array).is_some_and(|e| e.len() == 1)
                    && required.is_some_and(|r| r.iter().any(|k| k.as_str() == Some(key)))
            })
        })
        .cloned()
}

fn int(format: Option<&str>) -> &'static str {
    match format {
        Some("int8") => "i8",
        Some("uint8") => "u8",
        Some("int16") => "i16",
        Some("uint16") => "u16",
        Some("int32") => "i32",
        Some("uint32") => "u32",
        Some("uint64" | "uint") => "u64",
        _ => "i64",
    }
}

fn description(schema: &Value) -> Option<String> {
    let text = schema.get("description")?.as_str()?.trim();
    (!text.is_empty()).then(|| text.to_owned())
}

/// `wire` as a type or variant name: `item/agentMessage/delta` is `ItemAgentMessageDelta`.
fn pascal(wire: &str) -> String {
    let mut out = String::new();
    let mut up = true;
    for c in wire.chars() {
        if c.is_ascii_alphanumeric() {
            if up {
                out.extend(c.to_uppercase());
            } else {
                out.push(c);
            }
            up = false;
        } else {
            up = true;
        }
    }
    if out.starts_with(|c: char| c.is_ascii_digit()) {
        out.insert(0, 'N');
    }
    out
}

/// `wire` as a field name: `turnId` is `turn_id`.
fn snake(wire: &str) -> String {
    let mut out = String::new();
    let mut last_lower = false;
    for c in wire.chars() {
        if c.is_ascii_uppercase() {
            if last_lower {
                out.push('_');
            }
            out.push(c.to_ascii_lowercase());
            last_lower = false;
        } else if c.is_ascii_alphanumeric() {
            out.push(c);
            last_lower = c.is_ascii_lowercase() || c.is_ascii_digit();
        } else {
            if !out.ends_with('_') && !out.is_empty() {
                out.push('_');
            }
            last_lower = false;
        }
    }
    let out = out.trim_end_matches('_').to_owned();
    match out.as_str() {
        "type" | "ref" | "match" | "move" | "loop" | "fn" | "use" | "mod" | "impl" | "struct"
        | "enum" | "trait" | "where" | "async" | "await" | "dyn" | "box" | "yield" | "try"
        | "gen" | "abstract" | "final" | "override" | "macro" | "static" | "const" | "in"
        | "for" | "if" | "else" | "while" | "return" | "break" | "continue" | "let" | "mut"
        | "pub" | "unsafe" | "extern" | "true" | "false" | "as" | "virtual" | "priv" | "typeof"
        | "unsized" | "do" | "become" => format!("r#{out}"),
        "self" | "crate" | "super" => format!("{out}_"),
        "" => "value".to_owned(),
        _ => out,
    }
}

/// `text` as doc lines rustdoc and clippy read as prose: code-looking words in backticks,
/// nothing a test or a link would be made of.
fn doc_lines(text: &str) -> Vec<String> {
    let mut lines = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        let mut out = String::new();
        let mut quoted = false;
        for word in line.split(' ') {
            if !out.is_empty() {
                out.push(' ');
            }
            let ticks = word.matches('`').count();
            if quoted || ticks > 0 {
                out.push_str(word);
                if ticks % 2 == 1 {
                    quoted = !quoted;
                }
                continue;
            }
            let core = word.trim_end_matches(['.', ',', ';', ':', ')', '?', '!']);
            let tail = word.get(core.len()..).unwrap_or_default();
            let (lead, core) = match core.strip_prefix('(') {
                Some(rest) => ("(", rest),
                None => ("", core),
            };
            if code_like(core) {
                for piece in [lead, "`", core, "`", tail] {
                    out.push_str(piece);
                }
            } else {
                out.push_str(word);
            }
        }
        if quoted {
            out.push('`');
        }
        let out = if out.starts_with("```") { "```text".to_owned() } else { out };
        lines.push(out);
    }
    if lines.iter().filter(|l| l.starts_with("```")).count() % 2 == 1 {
        lines.push("```".to_owned());
    }
    lines
}

fn code_like(word: &str) -> bool {
    if word.is_empty() {
        return false;
    }
    let special = word.contains(['_', '/', '<', '>', '[', ']', '{', '}', '=', '*', '#', '|', '\\'])
        || word.contains("::")
        || word.contains('(')
        || word.starts_with("http");
    let chars: Vec<char> = word.chars().collect();
    let camel =
        chars.windows(2).any(|w| matches!(w, [a, b] if a.is_lowercase() && b.is_uppercase()))
            || (chars.iter().filter(|c| c.is_uppercase()).count() > 1
                && chars.iter().any(|c| c.is_lowercase()));
    special || camel
}

fn write_doc(out: &mut String, indent: &str, doc: Option<&str>, fallback: &str) -> fmt::Result {
    let text = doc.unwrap_or(fallback);
    for line in doc_lines(text) {
        if line.is_empty() {
            writeln!(out, "{indent}///")?;
        } else {
            writeln!(out, "{indent}/// {line}")?;
        }
    }
    Ok(())
}

/// The traits each written type can derive, from what it holds.
struct Traits {
    /// No float anywhere in it.
    eq: BTreeMap<String, bool>,
    /// Nothing in it owns memory.
    copy: BTreeMap<String, bool>,
    /// No float and no JSON value anywhere in it.
    hash: BTreeMap<String, bool>,
    /// A struct whose every field has a default.
    default: BTreeMap<String, bool>,
}

impl Traits {
    fn of(items: &BTreeMap<String, Item>) -> Self {
        let mut eq: BTreeMap<String, bool> = items.keys().map(|k| (k.clone(), true)).collect();
        let mut copy: BTreeMap<String, bool> = items.keys().map(|k| (k.clone(), true)).collect();
        let mut hash: BTreeMap<String, bool> = items.keys().map(|k| (k.clone(), true)).collect();
        let mut default: BTreeMap<String, bool> = items
            .iter()
            .map(|(k, item)| (k.clone(), matches!(item.shape, Shape::Struct(_))))
            .collect();
        // A fixed point: each pass takes away what a field's type cannot give.
        loop {
            let mut moved = false;
            for item in items.values() {
                let tys = types(&item.shape);
                let item_eq = tys.iter().all(|t| ty_eq(t, &eq));
                let item_copy = tys.iter().all(|t| ty_copy(t, &copy));
                let item_hash = tys.iter().all(|t| ty_hash(t, &hash));
                let item_default = tys.iter().all(|t| ty_default(t, &default));
                if eq.get(&item.name) == Some(&true) && !item_eq {
                    eq.insert(item.name.clone(), false);
                    moved = true;
                }
                if copy.get(&item.name) == Some(&true) && !item_copy {
                    copy.insert(item.name.clone(), false);
                    moved = true;
                }
                if hash.get(&item.name) == Some(&true) && !item_hash {
                    hash.insert(item.name.clone(), false);
                    moved = true;
                }
                if default.get(&item.name) == Some(&true) && !item_default {
                    default.insert(item.name.clone(), false);
                    moved = true;
                }
            }
            if !moved {
                return Self { eq, copy, hash, default };
            }
        }
    }
}

fn types(shape: &Shape) -> Vec<&Ty> {
    match shape {
        Shape::Struct(fields) => fields.iter().map(|f| &f.ty).collect(),
        Shape::External(v) | Shape::Untagged(v) | Shape::Tagged { variants: v, .. } => {
            variant_types(v)
        }
        Shape::Alias(ty) => vec![ty],
    }
}

fn variant_types(variants: &[Variant]) -> Vec<&Ty> {
    variants
        .iter()
        .flat_map(|v| match &v.body {
            Body::Unit => Vec::new(),
            Body::Fields(fields) => fields.iter().map(|f| &f.ty).collect(),
            Body::Newtype(ty) => vec![ty],
        })
        .collect()
}

fn ty_eq(ty: &Ty, eq: &BTreeMap<String, bool>) -> bool {
    match ty {
        Ty::Float => false,
        Ty::Named(name) => eq.get(name).copied().unwrap_or(true),
        Ty::Vec(inner) | Ty::Map(inner) | Ty::Option(inner) | Ty::Boxed(inner) => ty_eq(inner, eq),
        Ty::String | Ty::Bool | Ty::Int(_) | Ty::Json => true,
    }
}

fn ty_hash(ty: &Ty, hash: &BTreeMap<String, bool>) -> bool {
    match ty {
        Ty::Float | Ty::Json => false,
        Ty::Named(name) => hash.get(name).copied().unwrap_or(true),
        Ty::Vec(inner) | Ty::Map(inner) | Ty::Option(inner) | Ty::Boxed(inner) => {
            ty_hash(inner, hash)
        }
        Ty::String | Ty::Bool | Ty::Int(_) => true,
    }
}

fn ty_default(ty: &Ty, default: &BTreeMap<String, bool>) -> bool {
    match ty {
        Ty::Named(name) => default.get(name).copied().unwrap_or(false),
        Ty::Boxed(inner) => ty_default(inner, default),
        Ty::String
        | Ty::Bool
        | Ty::Int(_)
        | Ty::Float
        | Ty::Json
        | Ty::Vec(_)
        | Ty::Map(_)
        | Ty::Option(_) => true,
    }
}

fn ty_copy(ty: &Ty, copy: &BTreeMap<String, bool>) -> bool {
    match ty {
        Ty::Bool | Ty::Int(_) | Ty::Float => true,
        Ty::Named(name) => copy.get(name).copied().unwrap_or(false),
        Ty::Option(inner) => ty_copy(inner, copy),
        Ty::String | Ty::Json | Ty::Vec(_) | Ty::Map(_) | Ty::Boxed(_) => false,
    }
}

/// Box what a type holds of itself directly, so every type has a size.
fn box_cycles(items: &mut BTreeMap<String, Item>) {
    let direct = |shape: &Shape| -> BTreeSet<String> {
        let mut names = BTreeSet::new();
        for ty in types(shape) {
            let mut at = ty;
            while let Ty::Option(inner) = at {
                at = inner;
            }
            if let Ty::Named(name) = at {
                names.insert(name.clone());
            }
        }
        names
    };
    let graph: BTreeMap<String, BTreeSet<String>> =
        items.iter().map(|(name, item)| (name.clone(), direct(&item.shape))).collect();
    let reaches = |from: &str, to: &str| -> bool {
        let mut seen = BTreeSet::new();
        let mut stack = vec![from.to_owned()];
        while let Some(at) = stack.pop() {
            for next in graph.get(&at).into_iter().flatten() {
                if next == to {
                    return true;
                }
                if seen.insert(next.clone()) {
                    stack.push(next.clone());
                }
            }
        }
        false
    };
    let mut to_box: Vec<(String, String)> = Vec::new();
    for (name, targets) in &graph {
        for target in targets {
            if target == name || reaches(target, name) {
                to_box.push((name.clone(), target.clone()));
            }
        }
    }
    for (owner, target) in to_box {
        let Some(item) = items.get_mut(&owner) else { continue };
        for ty in types_mut(&mut item.shape) {
            boxed(ty, &target);
        }
    }
}

fn types_mut(shape: &mut Shape) -> Vec<&mut Ty> {
    match shape {
        Shape::Struct(fields) => fields.iter_mut().map(|f| &mut f.ty).collect(),
        Shape::External(v) | Shape::Untagged(v) | Shape::Tagged { variants: v, .. } => v
            .iter_mut()
            .flat_map(|v| match &mut v.body {
                Body::Unit => Vec::new(),
                Body::Fields(fields) => fields.iter_mut().map(|f| &mut f.ty).collect(),
                Body::Newtype(ty) => vec![ty],
            })
            .collect(),
        Shape::Alias(ty) => vec![ty],
    }
}

fn boxed(ty: &mut Ty, target: &str) {
    match ty {
        Ty::Option(inner) => boxed(inner, target),
        Ty::Named(name) if name == target => *ty = Ty::Boxed(Box::new(ty.clone())),
        _ => {}
    }
}

fn derives(name: &str, traits: &Traits) -> String {
    let mut list = vec!["Clone"];
    if traits.copy.get(name) == Some(&true) {
        list.push("Copy");
    }
    list.push("PartialEq");
    if traits.eq.get(name) == Some(&true) {
        list.push("Eq");
    }
    if traits.hash.get(name) == Some(&true) {
        list.push("Hash");
    }
    if traits.default.get(name) == Some(&true) {
        list.push("Default");
    }
    list.extend(["Debug", "Serialize", "Deserialize"]);
    format!("#[derive({})]", list.join(", "))
}

fn write_fields(out: &mut String, indent: &str, fields: &[Field], public: bool) -> fmt::Result {
    let vis = if public { "pub " } else { "" };
    for field in fields {
        let rust = snake(&field.wire);
        write_doc(out, indent, field.doc.as_deref(), &format!("`{}`.", field.wire))?;
        let bare = rust.trim_start_matches("r#");
        let mut attrs = Vec::new();
        if field.flatten {
            attrs.push("flatten".to_owned());
        } else if bare != field.wire {
            attrs.push(format!("rename = \"{}\"", field.wire));
        }
        if field.ty.is_option() {
            attrs.push("default".to_owned());
            attrs.push("skip_serializing_if = \"Option::is_none\"".to_owned());
        }
        if !attrs.is_empty() {
            writeln!(out, "{indent}#[serde({})]", attrs.join(", "))?;
        }
        writeln!(out, "{indent}{vis}{rust}: {},", field.ty.render())?;
    }
    Ok(())
}

fn write_variants(out: &mut String, variants: &[Variant], untagged: bool) -> fmt::Result {
    let mut seen = BTreeSet::new();
    for variant in variants {
        let mut name = pascal(&variant.wire);
        while !seen.insert(name.clone()) {
            name.push('_');
        }
        write_doc(out, "    ", variant.doc.as_deref(), &format!("`{}`.", variant.wire))?;
        if !untagged && name != variant.wire {
            writeln!(out, "    #[serde(rename = \"{}\")]", variant.wire)?;
        }
        match &variant.body {
            Body::Unit => {
                writeln!(out, "    {name},")?;
            }
            Body::Newtype(ty) => {
                writeln!(out, "    {name}({}),", ty.render())?;
            }
            Body::Fields(fields) => {
                writeln!(out, "    {name} {{")?;
                write_fields(out, "        ", fields, false)?;
                writeln!(out, "    }},")?;
            }
        }
    }
    Ok(())
}

fn write_item(out: &mut String, item: &Item, traits: &Traits) -> fmt::Result {
    let fallback = format!("`{}`, as Codex's schema names it.", item.name);
    write_doc(out, "", item.doc.as_deref(), &fallback)?;
    match &item.shape {
        Shape::Alias(ty) => {
            writeln!(out, "pub type {} = {};", item.name, ty.render())?;
        }
        Shape::Struct(fields) => {
            writeln!(out, "{}", derives(&item.name, traits))?;
            if fields.is_empty() {
                writeln!(out, "pub struct {} {{}}", item.name)?;
            } else {
                writeln!(out, "pub struct {} {{", item.name)?;
                write_fields(out, "    ", fields, true)?;
                writeln!(out, "}}")?;
            }
        }
        Shape::External(variants) => {
            writeln!(out, "{}", derives(&item.name, traits))?;
            writeln!(out, "pub enum {} {{", item.name)?;
            write_variants(out, variants, false)?;
            writeln!(out, "}}")?;
        }
        Shape::Tagged { tag, variants } => {
            writeln!(out, "{}", derives(&item.name, traits))?;
            writeln!(out, "#[serde(tag = \"{tag}\")]")?;
            writeln!(out, "pub enum {} {{", item.name)?;
            write_variants(out, variants, false)?;
            writeln!(out, "}}")?;
        }
        Shape::Untagged(variants) => {
            writeln!(out, "{}", derives(&item.name, traits))?;
            writeln!(out, "#[serde(untagged)]")?;
            writeln!(out, "pub enum {} {{", item.name)?;
            write_variants(out, variants, true)?;
            writeln!(out, "}}")?;
        }
    }
    out.push('\n');
    Ok(())
}

/// `methods` picked by name from a union, each found.
fn pick<'m>(methods: &'m [Method], names: &[&str], union: &str) -> Result<Vec<&'m Method>> {
    names
        .iter()
        .map(|name| {
            methods
                .iter()
                .find(|m| m.name == *name)
                .with_context(|| format!("{union} has no method {name}"))
        })
        .collect()
}

/// The response a request of params `params` gets: `XParams` answers `XResponse`.
fn response_of(defs: &Defs, params: &str) -> Result<String> {
    let base = params.strip_suffix("Params").with_context(|| format!("{params} is no params"))?;
    let response = format!("{base}Response");
    ensure!(defs.defs.contains_key(&response), "{params} has no {response}");
    Ok(response)
}

/// The Rust types of the methods Slopty speaks, from `bundle`, the schema bundle Codex
/// `version` generated.
pub fn generate(bundle: &Value, version: &str) -> Result<String> {
    let defs = Defs::of(bundle)?;
    let client = defs.methods("ClientRequest")?;
    let server = defs.methods("ServerRequest")?;
    let notes = defs.methods("ServerNotification")?;
    let client = pick(&client, &CLIENT_REQUESTS, "ClientRequest")?;
    let server = pick(&server, &SERVER_REQUESTS, "ServerRequest")?;
    let notes = pick(&notes, &SERVER_NOTIFICATIONS, "ServerNotification")?;

    let mut builder = Builder::new(&defs);
    let mut calls = Vec::new();
    for method in client.iter().chain(&server) {
        let params =
            method.params.clone().with_context(|| format!("{} takes no params", method.name))?;
        let response = response_of(&defs, &params)?;
        builder.define(&params)?;
        builder.define(&response)?;
        calls.push((method.name.clone(), builder.name(&params), builder.name(&response)));
    }
    let mut notifications = Vec::new();
    for note in &notes {
        let params =
            note.params.clone().with_context(|| format!("{} carries nothing", note.name))?;
        builder.define(&params)?;
        notifications.push((note.name.clone(), builder.name(&params)));
    }
    let mut items = builder.items;
    box_cycles(&mut items);
    let traits = Traits::of(&items);

    let mut out = String::new();
    writeln!(
        out,
        "//! The Codex app-server protocol (v2) as Codex {version} defines it: the types of the \
         methods Slopty speaks.\n//!\n//! Generated by `cargo xtask codex schema` from \
         `codex app-server generate-json-schema --experimental`; do not edit. A change here is \
         a change to Codex's wire.\n"
    )?;
    out.push_str(
        "#![allow(\n    clippy::doc_markdown,\n    clippy::large_enum_variant,\n    \
         clippy::struct_excessive_bools,\n    clippy::struct_field_names,\n    \
         clippy::module_name_repetitions,\n    clippy::too_long_first_doc_paragraph,\n    \
         clippy::enum_variant_names,\n    clippy::doc_paragraphs_missing_punctuation,\n    \
         reason = \"generated from Codex's schema, whose names and prose are Codex's\"\n)]\n\n",
    );
    out.push_str("use std::collections::BTreeMap;\n\nuse serde::{Deserialize, Serialize};\n\n");
    writeln!(out, "/// The Codex version these types are generated from.")?;
    writeln!(out, "pub const VERSION: &str = \"{version}\";\n")?;
    out.push_str(
        "/// A request one side sends the other, with what the other answers.\npub trait Method: \
         Serialize {\n    /// Its name on the wire.\n    const METHOD: &'static str;\n    /// \
         What it is answered with.\n    type Response;\n}\n\n",
    );
    for (method, params, response) in &calls {
        writeln!(
            out,
            "impl Method for {params} {{\n    const METHOD: &'static str = \"{method}\";\n    \
             type Response = {response};\n}}\n"
        )?;
    }
    let requests = server_calls(&calls, &server);
    let request_doc = "A request the app-server sends Slopty.";
    write_union(&mut out, &traits, "ServerRequest", request_doc, &requests)?;
    write_union(
        &mut out,
        &traits,
        "ServerNotification",
        "A notification the app-server sends Slopty.",
        &notifications,
    )?;
    for item in items.values() {
        write_item(&mut out, item, &traits)?;
    }
    Ok(out)
}

fn server_calls(calls: &[(String, String, String)], server: &[&Method]) -> Vec<(String, String)> {
    calls
        .iter()
        .filter(|(method, ..)| server.iter().any(|m| m.name == *method))
        .map(|(method, params, _)| (method.clone(), params.clone()))
        .collect()
}

/// An enum of `members` (method, params type) with the way to read one from its method and
/// params.
fn write_union(
    out: &mut String,
    traits: &Traits,
    name: &str,
    doc: &str,
    members: &[(String, String)],
) -> fmt::Result {
    let eq = members.iter().all(|(_, params)| traits.eq.get(params).copied().unwrap_or(false));
    let eq = if eq { ", Eq" } else { "" };
    writeln!(out, "/// {doc}\n#[derive(Clone, PartialEq{eq}, Debug)]\npub enum {name} {{")?;
    for (method, params) in members {
        writeln!(out, "    /// `{method}`.\n    {}({params}),", pascal(method))?;
    }
    writeln!(out, "}}\n\nimpl {name} {{")?;
    writeln!(
        out,
        "    /// The one `method` names, read from its `params`; `None` for a method these \
         types do not name.\n    ///\n    /// # Errors\n    ///\n    /// When the params are not \
         what the method takes.\n    pub fn read(method: &str, params: serde_json::Value) -> \
         Option<Result<Self, serde_json::Error>> {{\n        Some(match method {{"
    )?;
    for (method, _) in members {
        writeln!(
            out,
            "            \"{method}\" => serde_json::from_value(params).map(Self::{}),",
            pascal(method)
        )?;
    }
    writeln!(out, "            _ => return None,\n        }})\n    }}\n")?;
    writeln!(
        out,
        "    /// Its name on the wire.\n    #[must_use]\n    pub const fn method(&self) -> &'static str {{\n        match self {{"
    )?;
    for (method, _) in members {
        writeln!(out, "            Self::{}(_) => \"{method}\",", pascal(method))?;
    }
    writeln!(out, "        }}\n    }}\n}}\n")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_follow_rust() {
        assert_eq!(pascal("item/agentMessage/delta"), "ItemAgentMessageDelta");
        assert_eq!(pascal("on-request"), "OnRequest");
        assert_eq!(snake("turnId"), "turn_id");
        assert_eq!(snake("type"), "r#type");
        assert_eq!(snake("threadID"), "thread_id");
    }

    #[test]
    fn prose_keeps_code_in_backticks_and_makes_no_tests() {
        let lines = doc_lines("Use thread/start with turnId set.\n    indented\n```\nx\n```");
        assert_eq!(
            lines.first().map(String::as_str),
            Some("Use `thread/start` with `turnId` set.")
        );
        assert_eq!(lines.get(1).map(String::as_str), Some("indented"));
        assert_eq!(lines.get(2).map(String::as_str), Some("```text"));
    }
}
