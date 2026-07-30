//! The binding policy manifest.
//!
//! The generator applies defaults by rule; this file is where a human
//! overrides them. Every exclusion carries a `reason`, and every reason is
//! reproduced in the coverage report — so "we bound the whole crate" stays an
//! auditable claim rather than an assertion.

use serde::Deserialize;
use std::collections::HashMap;

/// Top-level manifest, deserialized from `bindings.toml`.
#[derive(Debug, Deserialize)]
pub struct Policy {
    /// Global emission settings.
    pub options: Options,
    /// Per-module rules, keyed by top-level module name (`math`, `api`, …).
    #[serde(default)]
    pub module: HashMap<String, ModuleRule>,
    /// Per-type overrides, keyed by Rust type name.
    #[serde(default)]
    pub types: HashMap<String, TypeRule>,
}

/// Global emission settings.
#[derive(Debug, Deserialize)]
pub struct Options {
    /// The Rust crate being wrapped, as named in generated `use` paths.
    pub krate: String,
    /// The Python module name the extension registers as.
    pub python_module: String,
}

/// What to do with a whole module.
#[derive(Debug, Deserialize)]
pub struct ModuleRule {
    /// `"auto"` to generate, `"skip"` to exclude wholesale.
    pub strategy: Strategy,
    /// Required when `strategy = "skip"`; surfaced in the coverage report.
    #[serde(default)]
    pub reason: Option<String>,
}

/// What to do with a single type.
#[derive(Debug, Deserialize, Default)]
pub struct TypeRule {
    /// Overrides the module-level strategy for this type.
    #[serde(default)]
    pub strategy: Option<Strategy>,
    /// Required when skipping or deferring to a hand-written impl.
    #[serde(default)]
    pub reason: Option<String>,
    /// Python-visible class name. Defaults to the Rust type name.
    #[serde(default)]
    pub python_name: Option<String>,
    /// Concrete instantiations to emit for a generic type. A generic struct
    /// has no single ABI, so it cannot be wrapped directly — each
    /// monomorphization becomes its own Python class.
    #[serde(default)]
    pub monomorphize: Vec<Mono>,
}

/// One concrete instantiation of a generic type.
#[derive(Debug, Deserialize)]
pub struct Mono {
    /// Concrete type arguments, in declaration order (e.g. `["f32"]`).
    pub args: Vec<String>,
    /// Python class name for this instantiation.
    pub name: String,
    /// Extra Python-level names bound to the same class. Several Rust
    /// aliases often collapse onto one instantiation — `RealBounds`,
    /// `AngleBounds`, and `FractionBounds` are all `Bounds<f32>`.
    #[serde(default)]
    pub aliases: Vec<String>,
}

/// How a module or type should be handled.
#[derive(Debug, Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Strategy {
    /// Generate a wrapper from the rustdoc description.
    Auto,
    /// Do not generate; a hand-written wrapper is expected in `manual/`.
    Manual,
    /// Deliberately unbound.
    Skip,
}

impl Policy {
    /// Load and validate the manifest.
    pub fn load(path: &str) -> Result<Self, String> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("reading {path}: {e}"))?;
        let policy: Policy =
            toml::from_str(&text).map_err(|e| format!("parsing {path}: {e}"))?;

        // A skip with no reason is exactly the silent-gap failure the
        // coverage report exists to prevent — reject it at load time.
        for (name, rule) in &policy.module {
            if rule.strategy != Strategy::Auto && rule.reason.is_none() {
                return Err(format!("module `{name}`: {:?} requires a `reason`", rule.strategy));
            }
        }
        for (name, rule) in &policy.types {
            if matches!(rule.strategy, Some(s) if s != Strategy::Auto) && rule.reason.is_none() {
                return Err(format!("type `{name}`: non-auto strategy requires a `reason`"));
            }
        }
        Ok(policy)
    }

    /// Resolve the effective strategy for a type in a module.
    pub fn strategy_for(&self, module: &str, ty: &str) -> Strategy {
        if let Some(rule) = self.types.get(ty)
            && let Some(s) = rule.strategy
        {
            return s;
        }
        self.module.get(module).map(|m| m.strategy).unwrap_or(Strategy::Skip)
    }

    /// The reason a type or its module is not being generated.
    pub fn reason_for(&self, module: &str, ty: &str) -> String {
        self.types
            .get(ty)
            .and_then(|r| r.reason.clone())
            .or_else(|| self.module.get(module).and_then(|m| m.reason.clone()))
            .unwrap_or_else(|| "module not listed in policy".into())
    }

    /// Per-type override record, if one exists.
    pub fn rule(&self, ty: &str) -> Option<&TypeRule> {
        self.types.get(ty)
    }
}
