//! Thin accessors over rustdoc's JSON output.
//!
//! Deliberately built on [`serde_json::Value`] rather than a mirrored set of
//! `#[derive(Deserialize)]` structs. The rustdoc JSON schema is explicitly
//! unstable (this generator targets `format_version` 60) and gains fields
//! every few releases; a strict struct mirror turns every additive schema
//! change into a hard deserialization failure, whereas key lookups simply
//! keep working. The version is asserted once, up front, so a *breaking*
//! change still fails loudly instead of silently generating nothing.

use serde_json::Value;
use std::collections::HashMap;

/// The rustdoc JSON schema version this generator was written against.
pub const EXPECTED_FORMAT_VERSION: u64 = 60;

/// A parsed rustdoc JSON dump, indexed for lookup by item id.
pub struct Crate {
    /// `index`: every documented item, keyed by id.
    pub index: HashMap<String, Value>,
    /// `paths`: id → module path + item kind, for nameable items.
    pub paths: HashMap<String, Value>,
}

impl Crate {
    /// Parse a rustdoc JSON dump, asserting the schema version matches.
    pub fn load(path: &str) -> Result<Self, String> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("reading {path}: {e}"))?;
        let root: Value = serde_json::from_str(&text)
            .map_err(|e| format!("parsing {path}: {e}"))?;

        let found = root.get("format_version").and_then(Value::as_u64).unwrap_or(0);
        if found != EXPECTED_FORMAT_VERSION {
            return Err(format!(
                "rustdoc JSON format_version is {found}, generator targets \
                 {EXPECTED_FORMAT_VERSION}. Re-check the schema before trusting output."
            ));
        }

        let obj_map = |key: &str| -> HashMap<String, Value> {
            root.get(key)
                .and_then(Value::as_object)
                .map(|m| m.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
                .unwrap_or_default()
        };

        Ok(Self { index: obj_map("index"), paths: obj_map("paths") })
    }

    /// Look up an item in the index by id.
    pub fn item(&self, id: &str) -> Option<&Value> {
        self.index.get(id)
    }

    /// The single key of an item's `inner` object — its kind
    /// (`"struct"`, `"enum"`, `"function"`, `"impl"`, …).
    pub fn kind(item: &Value) -> Option<&str> {
        item.get("inner")?.as_object()?.keys().next().map(String::as_str)
    }

    /// An item's name, if it has one.
    pub fn name(item: &Value) -> Option<&str> {
        item.get("name").and_then(Value::as_str)
    }

    /// An item's doc comment, with trailing whitespace trimmed.
    pub fn docs(item: &Value) -> Option<&str> {
        item.get("docs").and_then(Value::as_str)
    }

    /// `true` if the item is reachable from outside the crate.
    ///
    /// Trait-impl members are marked `"default"` rather than `"public"` —
    /// they inherit the visibility of the trait being implemented, and for a
    /// public trait on a public type that means public. Treating `"default"`
    /// as private silently drops every operator overload and associated type.
    pub fn is_public(item: &Value) -> bool {
        match item.get("visibility") {
            None => true,
            Some(Value::String(s)) => s == "public" || s == "default",
            // `{"restricted": {...}}` — crate- or module-private.
            Some(_) => false,
        }
    }

    /// The top-level module a nameable item lives in (`math`, `api`, …).
    pub fn top_module(&self, id: &str) -> Option<&str> {
        let path = self.paths.get(id)?.get("path")?.as_array()?;
        path.get(1)?.as_str()
    }

    /// Every id in `paths` whose item kind matches `kind`.
    pub fn ids_of_kind<'a>(&'a self, kind: &'a str) -> impl Iterator<Item = &'a String> + 'a {
        self.paths.iter().filter_map(move |(id, p)| {
            (p.get("kind").and_then(Value::as_str) == Some(kind)
                && p.get("crate_id").and_then(Value::as_u64) == Some(0))
            .then_some(id)
        })
    }

    /// All `impl` blocks in the crate, paired with the id of the type they
    /// are implemented *for*.
    pub fn impls_by_type(&self) -> HashMap<String, Vec<&Value>> {
        let mut out: HashMap<String, Vec<&Value>> = HashMap::new();
        for item in self.index.values() {
            if Self::kind(item) != Some("impl") {
                continue;
            }
            let imp = &item["inner"]["impl"];
            let Some(id) = imp
                .get("for")
                .and_then(|f| f.get("resolved_path"))
                .and_then(|r| r.get("id"))
            else {
                continue;
            };
            out.entry(id.to_string()).or_default().push(item);
        }
        out
    }
}

/// The trait an `impl` block implements, or `None` for an inherent impl.
pub fn impl_trait_name(imp: &Value) -> Option<&str> {
    imp.get("trait")?.get("path")?.as_str()
}

/// The public members of an `impl` block, split by kind.
pub fn impl_members<'a>(
    krate: &'a Crate,
    imp: &Value,
    want: &str,
) -> Vec<&'a Value> {
    let Some(items) = imp.get("items").and_then(Value::as_array) else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|id| krate.item(&id.to_string()))
        .filter(|it| Crate::kind(it) == Some(want) && Crate::is_public(it))
        .collect()
}
