//! Type mapping and PyO3 wrapper emission.

use crate::policy::{Policy, Strategy};
use crate::rdoc::{impl_members, impl_trait_name, Crate};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt::Write as _;

/// A type selected for generation, with everything emission needs.
pub struct Target {
    /// Rust type name (`RealVector3d`).
    pub rust_name: String,
    /// Fully-qualified Rust path (`blam_tags::math::RealVector3d`).
    pub rust_path: String,
    /// Python class name.
    pub py_name: String,
    /// Generated wrapper struct name (`PyRealVector3d`).
    pub wrapper: String,
    /// Extra Python names bound to this same class.
    pub aliases: Vec<String>,
    /// Concrete generic arguments, for monomorphized types.
    pub generic_args: Vec<String>,
    /// Type-parameter substitutions (`T` → `f32`). A monomorphized type's
    /// fields are declared in terms of its parameters, so without this the
    /// field types resolve to nothing and the class comes out empty.
    pub generic_map: BTreeMap<String, String>,
    /// rustdoc item id.
    pub id: String,
    /// Doc comment.
    pub docs: String,
    /// `true` if this mirrors a fieldless enum rather than wrapping a struct.
    pub is_enum: bool,
    /// `true` if the wrapper is hand-written. Such a type is still registered
    /// so generated code can refer to it by name, but no class is emitted —
    /// and if `manual/` fails to define it, the build breaks loudly rather
    /// than the type silently disappearing from the API.
    pub is_manual: bool,
}

impl Target {
    /// The Rust path including any concrete generic arguments, for use in
    /// *type* position (`pub blam_tags::math::Bounds<f32>`).
    fn concrete_path(&self) -> String {
        if self.generic_args.is_empty() {
            self.rust_path.clone()
        } else {
            format!("{}<{}>", self.rust_path, self.generic_args.join(", "))
        }
    }

    /// How references to this type cross the boundary.
    fn as_mapped(&self) -> Mapped {
        if self.is_enum {
            Mapped::EnumWrapper {
                wrapper: self.wrapper.clone(),
                rust_path: self.concrete_expr_path(),
            }
        } else {
            Mapped::Wrapper(self.wrapper.clone())
        }
    }

    /// The same path for use in *expression* position — struct literals and
    /// associated-const paths — where generic arguments need a turbofish.
    fn concrete_expr_path(&self) -> String {
        if self.generic_args.is_empty() {
            self.rust_path.clone()
        } else {
            format!("{}::<{}>", self.rust_path, self.generic_args.join(", "))
        }
    }
}

/// How a Rust type crosses into Python.
#[derive(Clone, Debug)]
pub enum Mapped {
    /// A scalar that PyO3 converts natively.
    Prim(String),
    /// A string. The flag records whether the Rust side wants a borrow
    /// (`&str`) or an owned `String` — Python always hands over an owned
    /// `String`, so a borrowed target needs `.as_str()` on the way in.
    Str(bool),
    /// A generated newtype wrapper class.
    Wrapper(String),
    /// A generated mirror enum. Unlike a newtype wrapper it holds no Rust
    /// value, so conversion goes through `From` in both directions.
    EnumWrapper {
        /// Generated Rust enum name (`PyGame`).
        wrapper: String,
        /// Path of the mirrored `blam-tags` enum.
        rust_path: String,
    },
    /// A sequence. `None` is a `Vec<T>`; `Some(n)` is a fixed-size `[T; n]`,
    /// which Python can only supply as a variable-length list — so the
    /// inbound conversion is fallible and length-checked.
    Seq(Box<Mapped>, Option<usize>),
    /// `Option<T>` → `T | None`.
    Opt(Box<Mapped>),
    /// A tuple.
    Tup(Vec<Mapped>),
    /// A filesystem path. Covers `P: AsRef<Path>` parameters, which PyO3
    /// accepts from `str` or any `os.PathLike`.
    Path,
    /// A borrowed parameter. Recorded rather than erased because the callee
    /// signature decides this: passing an owned value where `&T` is expected
    /// does not compile, and a non-`Clone` wrapper cannot be passed any
    /// other way.
    Ref(Box<Mapped>),
}

impl Mapped {
    /// The type as written in the generated `#[pymethods]` signature.
    pub fn rust_sig(&self) -> String {
        match self {
            Mapped::Prim(p) => p.clone(),
            Mapped::Str(_) => "String".into(),
            Mapped::Wrapper(w) => w.clone(),
            Mapped::EnumWrapper { wrapper, .. } => wrapper.clone(),
            Mapped::Seq(inner, _) => format!("Vec<{}>", inner.rust_sig()),
            Mapped::Opt(inner) => format!("Option<{}>", inner.rust_sig()),
            Mapped::Tup(parts) => format!(
                "({})",
                parts.iter().map(Mapped::rust_sig).collect::<Vec<_>>().join(", ")
            ),
            Mapped::Path => "std::path::PathBuf".into(),
            Mapped::Ref(inner) => match **inner {
                // A pyclass argument crosses as a borrow; everything else is
                // taken owned from Python and borrowed at the call site.
                Mapped::Wrapper(_) | Mapped::EnumWrapper { .. } => {
                    format!("&{}", inner.rust_sig())
                }
                _ => inner.rust_sig(),
            },
        }
    }

    /// Wrap a native Rust value so it can be returned to Python.
    pub fn from_native(&self, expr: &str) -> String {
        match self {
            Mapped::Prim(_) => expr.into(),
            Mapped::Str(_) => format!("{expr}.to_string()"),
            Mapped::Wrapper(w) => format!("{w}({expr})"),
            Mapped::EnumWrapper { wrapper, .. } => format!("{wrapper}::from({expr})"),
            // `[T; N]` needs `.to_vec()`; `Vec<T>` is already owned.
            Mapped::Seq(inner, len) => {
                let base =
                    if len.is_some() { format!("{expr}.to_vec()") } else { expr.into() };
                match **inner {
                    Mapped::Prim(_) => base,
                    _ => format!(
                        "{base}.into_iter().map(|v| {}).collect()",
                        inner.from_native("v")
                    ),
                }
            }
            Mapped::Opt(inner) => match **inner {
                Mapped::Prim(_) => expr.into(),
                _ => format!("{expr}.map(|v| {})", inner.from_native("v")),
            },
            // `()` is a zero-element tuple. Rebuilding it from its parts
            // would drop `expr` — and when the expression *is* the call
            // being wrapped, that silently deletes the call.
            Mapped::Tup(parts) if parts.is_empty() => expr.into(),
            Mapped::Tup(parts) => {
                let fields = parts
                    .iter()
                    .enumerate()
                    .map(|(i, p)| p.from_native(&format!("{expr}.{i}")))
                    .collect::<Vec<_>>();
                format!("({})", fields.join(", "))
            }
            Mapped::Path => format!("{expr}.to_path_buf()"),
            // A returned `&[T]` has to be copied out; the borrow cannot
            // outlive the call.
            Mapped::Ref(inner) => match **inner {
                Mapped::Seq(_, _) => format!("{expr}.to_vec()"),
                _ => inner.from_native(expr),
            },
        }
    }

    /// `true` if converting *into* Rust can fail, which happens exactly when
    /// a fixed-size array is involved: Python hands over a list of arbitrary
    /// length and the arity has to be checked at runtime. Any signature
    /// touching such a type has to return `PyResult`.
    pub fn needs_try(&self) -> bool {
        match self {
            Mapped::Prim(_)
            | Mapped::Str(_)
            | Mapped::Wrapper(_)
            | Mapped::EnumWrapper { .. } => false,
            Mapped::Seq(inner, len) => len.is_some() || inner.needs_try(),
            Mapped::Opt(inner) => inner.needs_try(),
            Mapped::Tup(parts) => parts.iter().any(Mapped::needs_try),
            Mapped::Path => false,
            Mapped::Ref(inner) => inner.needs_try(),
        }
    }

    /// Unwrap a Python-supplied value into its native Rust form.
    pub fn to_native(&self, expr: &str) -> String {
        match self {
            Mapped::Prim(_) => expr.into(),
            Mapped::Str(borrowed) => {
                if *borrowed { format!("{expr}.as_str()") } else { expr.into() }
            }
            Mapped::Wrapper(_) => format!("{expr}.0"),
            Mapped::EnumWrapper { rust_path, .. } => format!("{rust_path}::from({expr})"),
            Mapped::Seq(inner, len) => {
                let elems = match &**inner {
                    Mapped::Prim(_) | Mapped::Str(_) => expr.to_string(),
                    _ if inner.needs_try() => format!(
                        "{expr}.into_iter().map(|v| Ok({})).collect::<PyResult<Vec<_>>>()?",
                        inner.to_native("v")
                    ),
                    _ => format!(
                        "{expr}.into_iter().map(|v| {}).collect::<Vec<_>>()",
                        inner.to_native("v")
                    ),
                };
                match len {
                    None => elems,
                    Some(n) => format!("__seq_to_array::<_, {n}>({elems})?"),
                }
            }
            Mapped::Opt(inner) => match **inner {
                Mapped::Prim(_) => expr.into(),
                _ => format!("{expr}.map(|v| {})", inner.to_native("v")),
            },
            Mapped::Tup(parts) if parts.is_empty() => expr.into(),
            Mapped::Tup(parts) => {
                let fields = parts
                    .iter()
                    .enumerate()
                    .map(|(i, p)| p.to_native(&format!("{expr}.{i}")))
                    .collect::<Vec<_>>();
                format!("({})", fields.join(", "))
            }
            Mapped::Path => expr.into(),
            Mapped::Ref(inner) => match &**inner {
                Mapped::Wrapper(_) => format!("&{expr}.0"),
                Mapped::Str(_) => inner.to_native(expr),
                other => format!("&{}", other.to_native(expr)),
            },
        }
    }

    /// The Python type annotation for `.pyi` stubs.
    pub fn py_type(&self) -> String {
        match self {
            Mapped::Prim(p) => match p.as_str() {
                "bool" => "bool".into(),
                "f32" | "f64" => "float".into(),
                _ => "int".into(),
            },
            Mapped::Str(_) => "str".into(),
            Mapped::Wrapper(w) => w.trim_start_matches("Py").to_string(),
            Mapped::EnumWrapper { wrapper, .. } => wrapper.trim_start_matches("Py").to_string(),
            Mapped::Seq(inner, _) => format!("list[{}]", inner.py_type()),
            Mapped::Opt(inner) => format!("{} | None", inner.py_type()),
            Mapped::Tup(parts) if parts.is_empty() => "None".into(),
            Mapped::Tup(parts) => format!(
                "tuple[{}]",
                parts.iter().map(Mapped::py_type).collect::<Vec<_>>().join(", ")
            ),
            Mapped::Path => "str | os.PathLike".into(),
            Mapped::Ref(inner) => inner.py_type(),
        }
    }
}

/// Scalars PyO3 converts without help.
const PRIMS: &[&str] = &[
    "bool", "f32", "f64", "i8", "i16", "i32", "i64", "i128", "isize", "u8", "u16", "u32", "u64",
    "u128", "usize", "char",
];

/// Rust operator traits mapped to their Python dunder equivalents.
const OPS: &[(&str, &str, &str)] = &[
    ("Add", "add", "__add__"),
    ("Sub", "sub", "__sub__"),
    ("Mul", "mul", "__mul__"),
    ("Div", "div", "__truediv__"),
    ("Neg", "neg", "__neg__"),
];

/// The emitter: owns the target registry and produces all three outputs.
pub struct Emitter<'a> {
    krate: &'a Crate,
    policy: &'a Policy,
    /// Rust type name → target, for resolving references between wrappers.
    registry: HashMap<String, Target>,
    /// Items reached but deliberately or unavoidably not generated.
    pub skipped: Vec<(String, String)>,
    /// Crate error types encountered in `Result` returns, each of which
    /// becomes a generated Python exception class.
    exceptions: std::cell::RefCell<BTreeSet<String>>,
    /// Wrapper name → (is `Copy`, is `Clone`) for the type it wraps. Reading a
    /// field out of `&self` has to copy or clone it; a type that is neither
    /// cannot be exposed as a property at all.
    wrapper_flags: HashMap<String, (bool, bool)>,
}

impl<'a> Emitter<'a> {
    /// Build a registry of every type the policy selects for generation.
    pub fn new(krate: &'a Crate, policy: &'a Policy) -> Self {
        let mut registry = HashMap::new();
        let mut skipped = Vec::new();

        for id in krate.ids_of_kind("struct").chain(krate.ids_of_kind("enum")) {
            let Some(item) = krate.item(id) else { continue };
            let Some(name) = Crate::name(item) else { continue };
            if !Crate::is_public(item) {
                continue;
            }
            let module = krate.top_module(id).unwrap_or("?").to_string();

            match policy.strategy_for(&module, name) {
                Strategy::Skip => {
                    skipped.push((
                        format!("{module}::{name}"),
                        policy.reason_for(&module, name),
                    ));
                    continue;
                }
                Strategy::Manual => {
                    skipped.push((
                        format!("{module}::{name}"),
                        format!("hand-written: {}", policy.reason_for(&module, name)),
                    ));
                }
                Strategy::Auto => {}
            }

            let rust_path = krate
                .paths
                .get(id)
                .and_then(|p| p.get("path"))
                .and_then(Value::as_array)
                .map(|segs| {
                    let mut parts: Vec<String> = segs
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::to_string)
                        .collect();
                    parts[0] = policy.options.krate.clone();
                    parts.join("::")
                })
                .unwrap_or_default();

            let docs = Crate::docs(item).unwrap_or("").to_string();
            let rule = policy.rule(name);
            let is_manual = policy.strategy_for(&module, name) == Strategy::Manual;

            // A data-carrying enum has no single mechanical Python form —
            // it could be a class hierarchy, a tagged dict, or a native
            // value, and only a human can pick. Fieldless enums do have
            // one: a Python enum.
            let is_enum = Crate::kind(item) == Some("enum");
            if is_enum && !enum_is_fieldless(krate, item) && !is_manual {
                skipped.push((
                    format!("{module}::{name}"),
                    "data-carrying enum — needs an explicit policy decision".into(),
                ));
                continue;
            }

            // A generic type has no single ABI — emit one class per
            // policy-declared instantiation instead of one for the type.
            let monos = rule.map(|r| r.monomorphize.as_slice()).unwrap_or(&[]);
            if monos.is_empty() {
                if type_is_generic(item) {
                    skipped.push((
                        format!("{module}::{name}"),
                        "generic type with no `monomorphize` entries in policy".into(),
                    ));
                    continue;
                }
                let py_name = rule
                    .and_then(|r| r.python_name.clone())
                    .unwrap_or_else(|| name.to_string());
                registry.insert(
                    name.to_string(),
                    Target {
                        rust_name: name.to_string(),
                        rust_path,
                        wrapper: format!("Py{py_name}"),
                        py_name,
                        aliases: Vec::new(),
                        generic_args: Vec::new(),
                        generic_map: BTreeMap::new(),
                        id: id.clone(),
                        docs,
                        is_enum,
                        is_manual,
                    },
                );
            } else {
                let param_names = type_param_names(item);
                for mono in monos {
                    let generic_map: BTreeMap<String, String> = param_names
                        .iter()
                        .cloned()
                        .zip(mono.args.iter().cloned())
                        .collect();
                    registry.insert(
                        // Keyed by Python name so each instantiation is
                        // independently addressable from the registry.
                        mono.name.clone(),
                        Target {
                            rust_name: name.to_string(),
                            rust_path: rust_path.clone(),
                            wrapper: format!("Py{}", mono.name),
                            py_name: mono.name.clone(),
                            aliases: mono.aliases.clone(),
                            generic_args: mono.args.clone(),
                            generic_map,
                            id: id.clone(),
                            docs: docs.clone(),
                            is_enum,
                            is_manual,
                        },
                    );
                }
            }
        }

        let impls = krate.impls_by_type();
        let wrapper_flags = registry
            .values()
            .map(|t| {
                let traits = trait_set(&impls, t);
                (t.wrapper.clone(), (traits.contains("Copy"), traits.contains("Clone")))
            })
            .collect();

        Self {
            krate,
            policy,
            registry,
            skipped,
            exceptions: std::cell::RefCell::new(BTreeSet::new()),
            wrapper_flags,
        }
    }

    /// How a field of this type must be read out of `&self`.
    ///
    /// `Copy` fields can be read directly; anything else has to be cloned,
    /// and a type that is neither `Copy` nor `Clone` cannot be exposed as a
    /// property at all.
    fn field_access(&self, mapped: &Mapped, access: &str) -> Option<String> {
        match mapped {
            // Scalars are `Copy`; strings and fixed arrays already copy as
            // part of their own conversion.
            Mapped::Prim(_) | Mapped::Str(_) => Some(access.to_string()),
            Mapped::Seq(_, Some(_)) => Some(access.to_string()),
            Mapped::Wrapper(w) | Mapped::EnumWrapper { wrapper: w, .. } => {
                match self.wrapper_flags.get(w) {
                    Some((true, _)) => Some(access.to_string()),
                    Some((_, true)) => Some(format!("{access}.clone()")),
                    _ => None,
                }
            }
            _ => Some(format!("{access}.clone()")),
        }
    }

    /// Map a rustdoc type description onto its Python-facing form.
    /// `self_ty` resolves `Self` inside an impl block.
    fn map_type(&self, ty: &Value, self_ty: &Target) -> Option<Mapped> {
        self.map_type_in(ty, self_ty, &BTreeMap::new())
    }

    /// As [`Self::map_type`], but with `locals` supplying resolutions for a
    /// method's own type parameters (`P: AsRef<Path>` → a path).
    fn map_type_in(
        &self,
        ty: &Value,
        self_ty: &Target,
        locals: &BTreeMap<String, Mapped>,
    ) -> Option<Mapped> {
        if let Some(p) = ty.get("primitive").and_then(Value::as_str) {
            // rustdoc classifies `str` as a primitive, not as a resolved path
            // to `String` — so it needs handling here or every `&str`
            // parameter silently drops its method.
            if p == "str" {
                return Some(Mapped::Str(true));
            }
            return PRIMS.contains(&p).then(|| Mapped::Prim(p.to_string()));
        }
        if let Some(g) = ty.get("generic").and_then(Value::as_str) {
            if g == "Self" {
                return Some(self_ty.as_mapped());
            }
            if let Some(m) = locals.get(g) {
                return Some(m.clone());
            }
            // A type parameter of a monomorphized type resolves through the
            // instantiation the policy declared.
            let concrete = self_ty.generic_map.get(g)?;
            if PRIMS.contains(&concrete.as_str()) {
                return Some(Mapped::Prim(concrete.clone()));
            }
            return self.registry.get(concrete).map(Target::as_mapped);
        }
        if let Some(r) = ty.get("borrowed_ref") {
            // `&str` / `&mut T` both reduce to their pointee here; the
            // wrapper owns its data, so borrows never escape.
            return match self.map_type_in(r.get("type")?, self_ty, locals)? {
                Mapped::Str(_) => Some(Mapped::Str(true)),
                other => Some(Mapped::Ref(Box::new(other))),
            };
        }
        if let Some(a) = ty.get("array") {
            let inner = self.map_type_in(a.get("type")?, self_ty, locals)?;
            // rustdoc stores the length as a string expression; only a plain
            // integer literal can be turned into a checked conversion.
            let len = a.get("len")?.as_str()?.parse::<usize>().ok()?;
            return Some(Mapped::Seq(Box::new(inner), Some(len)));
        }
        if let Some(s) = ty.get("slice") {
            let inner = self.map_type_in(s, self_ty, locals)?;
            return Some(Mapped::Seq(Box::new(inner), None));
        }
        if let Some(parts) = ty.get("tuple").and_then(Value::as_array) {
            let mapped: Option<Vec<_>> =
                parts.iter().map(|p| self.map_type_in(p, self_ty, locals)).collect();
            return Some(Mapped::Tup(mapped?));
        }
        if let Some(rp) = ty.get("resolved_path") {
            let path = rp.get("path")?.as_str()?;
            let last = path.rsplit("::").next()?;
            match last {
                "String" | "str" | "Cow" => return Some(Mapped::Str(false)),
                "Option" | "Vec" => {
                    let arg = first_type_arg(rp)?;
                    let inner = self.map_type_in(&arg, self_ty, locals)?;
                    return Some(if last == "Option" {
                        Mapped::Opt(Box::new(inner))
                    } else {
                        Mapped::Seq(Box::new(inner), None)
                    });
                }
                _ => {}
            }
            // A reference to another generated wrapper. Generic types are
            // registered under their Python names, so try both.
            if let Some(t) = self.registry.get(last) {
                return Some(t.as_mapped());
            }
            return None;
        }
        None
    }

    /// The Python exception a `Result`'s error half maps to.
    ///
    /// `std::io::Error` has an exact Python counterpart, so it uses the
    /// builtin. A named crate error becomes a generated exception of the same
    /// name, which lets callers catch precisely what `blam-tags` raises.
    /// Anything else — notably `Box<dyn Error>` — falls back to one catch-all
    /// so no error is silently widened into a bare `Exception`.
    fn exception_for(&self, err: &Value) -> String {
        // `std::io::Result<T>` names no error type at all.
        if err.is_null() {
            return "pyo3::exceptions::PyOSError".into();
        }
        if let Some(path) = err
            .get("resolved_path")
            .and_then(|r| r.get("path"))
            .and_then(Value::as_str)
        {
            let last = path.rsplit("::").next().unwrap_or(path);
            if path.starts_with("std::io") || last == "Error" && path.contains("io") {
                return "pyo3::exceptions::PyOSError".into();
            }
            // `Box<dyn Error>` and friends are containers, not error types —
            // naming the exception after the container would produce a
            // Python class called `Box`.
            let is_container = matches!(last, "Box" | "Rc" | "Arc" | "Cow");
            if !is_container && last != "Error" && !last.is_empty() {
                self.exceptions.borrow_mut().insert(last.to_string());
                return format!("crate::errors::{last}");
            }
        }
        self.exceptions.borrow_mut().insert("BlamTagsError".to_string());
        "crate::errors::BlamTagsError".into()
    }

    /// Generate the wrapper source, the stubs, and the coverage report.
    pub fn run(&mut self) -> (String, String, String) {
        let impls = self.krate.impls_by_type();
        let mut targets: Vec<&Target> = self.registry.values().collect();
        targets.sort_by(|a, b| a.py_name.cmp(&b.py_name));

        let mut rs = String::new();
        let mut pyi = String::new();
        let mut unmapped: Vec<(String, String)> = Vec::new();

        header(&mut rs, &mut pyi, &self.policy.options.python_module);

        for target in &targets {
            if target.is_manual {
                continue;
            }
            let (code, stub, skips) = self.emit_type(target, &impls);
            rs.push_str(&code);
            pyi.push_str(&stub);
            unmapped.extend(skips);
        }

        let (fn_rs, fn_pyi, fn_names, fn_skips) = self.emit_functions();
        rs.push_str(&fn_rs);
        pyi.push_str(&fn_pyi);
        unmapped.extend(fn_skips);

        let exceptions = self.exceptions.borrow().clone();

        // Module registration.
        let _ = writeln!(rs, "/// Register every generated class on the extension module.");
        let _ = writeln!(
            rs,
            "pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {{"
        );
        for target in &targets {
            if target.is_manual {
                continue;
            }
            let _ = writeln!(rs, "    m.add_class::<{}>()?;", target.wrapper);
        }
        for name in &fn_names {
            let _ = writeln!(
                rs,
                "    m.add_function(pyo3::wrap_pyfunction!({name}, m)?)?;"
            );
        }
        for target in &targets {
            for alias in &target.aliases {
                let _ = writeln!(
                    rs,
                    "    m.add(\"{alias}\", m.getattr(\"{}\")?)?;",
                    target.py_name
                );
            }
        }
        let _ = writeln!(rs, "    Ok(())\n}}");

        for target in &targets {
            for alias in &target.aliases {
                let _ = writeln!(pyi, "\n{alias} = {}", target.py_name);
            }
        }

        let mut coverage = self.coverage(&targets, &unmapped);
        let _ = writeln!(
            coverage,
            "\n## Exceptions required from `src/errors.rs` ({})\n",
            exceptions.len()
        );
        for exc in &exceptions {
            let _ = writeln!(coverage, "- `{exc}`");
        }
        (rs, pyi, coverage)
    }

    /// Emit the crate's module-level free functions as `#[pyfunction]`s.
    ///
    /// These live in `paths` rather than in any impl block, so they are
    /// collected separately from methods. `Self` cannot appear in one, so
    /// type resolution runs against a placeholder target.
    fn emit_functions(&self) -> (String, String, Vec<String>, Vec<(String, String)>) {
        let mut rs = String::new();
        let mut pyi = String::new();
        let mut names = Vec::new();
        let mut skips = Vec::new();

        let mut ids: Vec<&String> = self.krate.ids_of_kind("function").collect();
        ids.sort();

        for id in ids {
            let Some(item) = self.krate.item(id) else { continue };
            let Some(name) = Crate::name(item) else { continue };
            if !Crate::is_public(item) {
                continue;
            }
            let module = self.krate.top_module(id).unwrap_or("?");
            if self.policy.strategy_for(module, name) != Strategy::Auto {
                continue;
            }
            let rust_path = self.qualified_path(id);
            match self.emit_function(item, name, &rust_path) {
                Ok((code, stub)) => {
                    rs.push_str(&code);
                    pyi.push_str(&stub);
                    names.push(name.to_string());
                }
                Err(why) => skips.push((format!("{module}::{name}"), why)),
            }
        }
        (rs, pyi, names, skips)
    }

    /// The crate-qualified Rust path of a nameable item.
    fn qualified_path(&self, id: &str) -> String {
        self.krate
            .paths
            .get(id)
            .and_then(|p| p.get("path"))
            .and_then(Value::as_array)
            .map(|segs| {
                let mut parts: Vec<String> = segs
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect();
                if !parts.is_empty() {
                    parts[0] = self.policy.options.krate.clone();
                }
                parts.join("::")
            })
            .unwrap_or_default()
    }

    /// Emit one free function.
    fn emit_function(
        &self,
        item: &Value,
        name: &str,
        rust_path: &str,
    ) -> Result<(String, String), String> {
        let func = &item["inner"]["function"];
        let mut locals: BTreeMap<String, Mapped> = BTreeMap::new();
        for param in func["generics"]["params"].as_array().into_iter().flatten() {
            let pname = param["name"].as_str().unwrap_or("");
            if bound_is_as_ref_path(param) {
                locals.insert(pname.to_string(), Mapped::Path);
            } else {
                return Err(format!(
                    "generic parameter `{pname}` needs a policy-declared instantiation"
                ));
            }
        }

        // `Self` cannot occur in a free function, so the placeholder target
        // it resolves against is never consulted.
        let placeholder = Target {
            rust_name: String::new(),
            rust_path: String::new(),
            py_name: String::new(),
            wrapper: String::new(),
            aliases: Vec::new(),
            generic_args: Vec::new(),
            generic_map: BTreeMap::new(),
            id: String::new(),
            docs: String::new(),
            is_enum: false,
            is_manual: false,
        };

        let sig = &func["sig"];
        let inputs = sig["inputs"].as_array().ok_or("no inputs")?;
        let mut args = Vec::new();
        let mut call = Vec::new();
        let mut stub_args = Vec::new();
        for input in inputs {
            let raw_name = input[0].as_str().ok_or("unnamed arg")?;
            // A Rust param name may be a Python keyword (e.g. `from`), which is a
            // valid Rust identifier but breaks the `.pyi` and makes pyo3 expose an
            // uncallable kwarg. Rename it consistently on the Python-facing side.
            let aname = sanitize_py_ident(raw_name);
            let mapped = self
                .map_type_in(&input[1], &placeholder, &locals)
                .ok_or_else(|| format!("parameter `{raw_name}` has no Python mapping"))?;
            args.push(format!("{aname}: {}", mapped.rust_sig()));
            call.push(mapped.to_native(&aname));
            stub_args.push(format!("{aname}: {}", mapped.py_type()));
        }

        let raw_out = &sig["output"];
        let (out_owned, exception) = match split_result(raw_out) {
            Some((ok, err)) => (ok, Some(self.exception_for(&err))),
            None => (raw_out.clone(), None),
        };
        let mapped_out = if out_owned.is_null() {
            None
        } else {
            Some(
                self.map_type_in(&out_owned, &placeholder, &locals)
                    .ok_or("return type has no Python mapping")?,
            )
        };
        let (ret_sig, ret_py) = match &mapped_out {
            None => ("()".to_string(), "None".to_string()),
            Some(m) => (m.rust_sig(), m.py_type()),
        };

        let inner_call = format!("{rust_path}({})", call.join(", "));
        let call_expr = match &exception {
            None => inner_call,
            Some(exc) => format!("{inner_call}.map_err(|e| {exc}::new_err(e.to_string()))?"),
        };
        let body = match &mapped_out {
            None => call_expr,
            Some(m) => m.from_native(&call_expr),
        };
        let fallible = exception.is_some()
            || inputs
                .iter()
                .filter_map(|i| self.map_type_in(&i[1], &placeholder, &locals))
                .any(|m| m.needs_try());

        let mut rs = String::new();
        for line in Crate::docs(item).unwrap_or("").lines() {
            let _ = writeln!(rs, "/// {line}");
        }
        let _ = writeln!(rs, "#[pyfunction]");
        if fallible {
            let _ = writeln!(
                rs,
                "pub fn {name}({}) -> PyResult<{ret_sig}> {{ Ok({body}) }}\n",
                args.join(", ")
            );
        } else {
            let _ = writeln!(
                rs,
                "pub fn {name}({}) -> {ret_sig} {{ {body} }}\n",
                args.join(", ")
            );
        }
        let pyi = format!("def {name}({}) -> {ret_py}: ...\n", stub_args.join(", "));
        Ok((rs, pyi))
    }

    /// Emit one wrapper class.
    fn emit_type(
        &self,
        target: &Target,
        impls: &HashMap<String, Vec<&Value>>,
    ) -> (String, String, Vec<(String, String)>) {
        let mut rs = String::new();
        let mut pyi = String::new();
        let mut skips = Vec::new();
        let item = self.krate.item(&target.id).expect("target id in index");
        let path = target.concrete_path();
        let expr_path = target.concrete_expr_path();
        let wrapper = &target.wrapper;

        let traits = trait_set(impls, target);
        // The wrapper can only derive what the wrapped type supports.
        // `TagFile`, for instance, is `Debug` and nothing else — so it cannot
        // be cloned, cannot implement `FromPyObject`, and can only ever be
        // passed to Rust by reference.
        let clonable = traits.contains("Clone") || target.is_enum;
        let derives = {
            let mut d = Vec::new();
            if clonable {
                d.push("Clone");
            }
            if traits.contains("Copy") {
                d.push("Copy");
            }
            if traits.contains("PartialEq") {
                d.push("PartialEq");
            }
            d
        };
        let mut class_opts = vec![
            format!("name = \"{}\"", target.py_name),
            format!("module = \"{}\"", self.policy.options.python_module),
        ];
        // Wrappers are passed by value as method parameters and pulled back
        // out by `extract` in the operator dispatchers, both of which need
        // `FromPyObject`. PyO3 0.29 deprecates deriving it implicitly from
        // `Clone`, so opt in explicitly — but only where `Clone` exists.
        if clonable {
            class_opts.push("from_py_object".into());
        }
        if traits.contains("PartialEq") {
            class_opts.push("eq".into());
        }

        for line in target.docs.lines() {
            let _ = writeln!(rs, "/// {line}");
        }
        if target.is_enum {
            // A fieldless enum mirrors as a PyO3 "simple enum": the variants
            // are redeclared rather than wrapped, so `From` in both
            // directions is what carries values across the boundary.
            let variants = enum_variants(self.krate, item);
            let _ = writeln!(rs, "#[pyclass({}, eq_int, frozen)]", class_opts.join(", "));
            let _ = writeln!(rs, "#[derive(Clone, Copy, PartialEq, Eq)]");
            let _ = writeln!(rs, "pub enum {wrapper} {{");
            for v in &variants {
                let _ = writeln!(rs, "    {v},");
            }
            let _ = writeln!(rs, "}}\n");

            let _ = writeln!(rs, "impl From<{path}> for {wrapper} {{");
            let _ = writeln!(rs, "    fn from(v: {path}) -> Self {{");
            let _ = writeln!(rs, "        match v {{");
            for v in &variants {
                let _ = writeln!(rs, "            {expr_path}::{v} => Self::{v},");
            }
            let _ = writeln!(rs, "        }}\n    }}\n}}\n");

            let _ = writeln!(rs, "impl From<{wrapper}> for {path} {{");
            let _ = writeln!(rs, "    fn from(v: {wrapper}) -> Self {{");
            let _ = writeln!(rs, "        match v {{");
            for v in &variants {
                let _ = writeln!(rs, "            {wrapper}::{v} => Self::{v},");
            }
            let _ = writeln!(rs, "        }}\n    }}\n}}\n");
        } else {
            let _ = writeln!(rs, "#[pyclass({})]", class_opts.join(", "));
            if !derives.is_empty() {
                let _ = writeln!(rs, "#[derive({})]", derives.join(", "));
            }
            let _ = writeln!(rs, "pub struct {wrapper}(pub {path});\n");
        }
        let _ = writeln!(rs, "#[pymethods]");
        let _ = writeln!(rs, "impl {wrapper} {{");

        let base = if target.is_enum { "(enum.Enum)" } else { "" };
        let _ = writeln!(pyi, "\nclass {}{base}:", target.py_name);
        if !target.docs.is_empty() {
            let _ = writeln!(pyi, "    \"\"\"{}\"\"\"", target.docs.replace('\n', " ").trim());
        }
        if target.is_enum {
            for (i, v) in enum_variants(self.krate, item).iter().enumerate() {
                let _ = writeln!(pyi, "    {v} = {i}");
            }
        }

        // ---- fields → constructor + properties ----
        // Enum variants are declared above, not exposed as properties.
        let fields = if target.is_enum { Vec::new() } else { struct_fields(self.krate, item) };
        let mut ctor_args = Vec::new();
        let mut ctor_init = Vec::new();
        let is_tuple = fields.iter().all(|(n, _)| n.chars().all(char::is_numeric));

        // A struct with private fields cannot be built from its public ones,
        // so no constructor is derived — callers go through its associated
        // functions instead.
        let has_private_fields = struct_has_stripped_fields(item);
        let mut all_fields_exposed = true;

        for (fname, fty) in &fields {
            let Some(mapped) = self.map_type(fty, target) else {
                skips.push((
                    format!("{}.{fname}", target.py_name),
                    "field type has no Python mapping".into(),
                ));
                all_fields_exposed = false;
                continue;
            };
            // Reading a field out of `&self` copies or clones it; a field
            // whose type does neither cannot become a property.
            let Some(access) = self.field_access(&mapped, &format!("self.0.{fname}")) else {
                skips.push((
                    format!("{}.{fname}", target.py_name),
                    "field type is neither Copy nor Clone, so it cannot be read \
                     out of a shared reference"
                        .into(),
                ));
                all_fields_exposed = false;
                continue;
            };
            // Tuple-struct fields are positional in Rust but need a name in
            // Python; a single-field newtype reads best as `.value`.
            let py_field =
                if is_tuple && fields.len() == 1 { "value".to_string() } else { fname.clone() };
            let sig = mapped.rust_sig();

            let _ = writeln!(rs, "    #[getter({py_field})]");
            let _ = writeln!(
                rs,
                "    fn get_{py_field}(&self) -> {sig} {{ {} }}",
                mapped.from_native(&access)
            );

            // A setter needs to accept the value from Python, which requires
            // `FromPyObject` — and for a wrapper that means `Clone`.
            let settable = match &mapped {
                Mapped::Wrapper(w) | Mapped::EnumWrapper { wrapper: w, .. } => {
                    self.wrapper_flags.get(w).map(|(_, c)| *c).unwrap_or(false)
                }
                _ => true,
            };
            if settable {
                let _ = writeln!(rs, "    #[setter({py_field})]");
                if mapped.needs_try() {
                    let _ = writeln!(
                        rs,
                        "    fn set_{py_field}(&mut self, value: {sig}) -> PyResult<()> \
                         {{ self.0.{fname} = {}; Ok(()) }}",
                        mapped.to_native("value")
                    );
                } else {
                    let _ = writeln!(
                        rs,
                        "    fn set_{py_field}(&mut self, value: {sig}) {{ self.0.{fname} = {}; }}",
                        mapped.to_native("value")
                    );
                }
                ctor_args.push(format!("{py_field}: {sig}"));
                ctor_init.push(format!("{fname}: {}", mapped.to_native(&py_field)));
            } else {
                all_fields_exposed = false;
            }
            let _ = writeln!(pyi, "    {py_field}: {}", mapped.py_type());
        }

        if has_private_fields || !all_fields_exposed {
            if !fields.is_empty() {
                skips.push((
                    format!("{}.__init__", target.py_name),
                    "struct has fields that cannot be set from Python, so no \
                     constructor is derived"
                        .into(),
                ));
            }
            ctor_args.clear();
        }

        if !ctor_args.is_empty() {
            let ctor_fallible = fields
                .iter()
                .filter_map(|(_, t)| self.map_type(t, target))
                .any(|m| m.needs_try());
            let _ = writeln!(rs, "    #[new]");
            let body = if is_tuple {
                format!(
                    "{expr_path}({})",
                    ctor_init
                        .iter()
                        .map(|s| s.split_once(": ").unwrap().1.to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            } else {
                format!("{expr_path} {{ {} }}", ctor_init.join(", "))
            };
            if ctor_fallible {
                let _ = writeln!(
                    rs,
                    "    fn new({}) -> PyResult<Self> {{ Ok(Self({body})) }}",
                    ctor_args.join(", ")
                );
            } else {
                let _ = writeln!(
                    rs,
                    "    fn new({}) -> Self {{ Self({body}) }}",
                    ctor_args.join(", ")
                );
            }
            let stub_args: Vec<String> = fields
                .iter()
                .filter_map(|(n, t)| {
                    let m = self.map_type(t, target)?;
                    let name =
                        if is_tuple && fields.len() == 1 { "value".into() } else { n.clone() };
                    Some(format!("{name}: {}", m.py_type()))
                })
                .collect();
            let _ = writeln!(pyi, "    def __init__(self, {}) -> None: ...", stub_args.join(", "));
        }

        // ---- inherent methods and associated constants ----
        for imp in impls.get(&target.id).into_iter().flatten() {
            let inner = &imp["inner"]["impl"];
            if impl_trait_name(inner).is_some() || !impl_applies(inner, target) {
                continue;
            }
            for c in impl_members(self.krate, inner, "assoc_const") {
                let cname = Crate::name(c).unwrap_or("?");
                let _ = writeln!(rs, "    #[classattr]");
                let _ = writeln!(
                    rs,
                    "    #[allow(non_snake_case)]\n    fn {cname}() -> Self {{ Self({expr_path}::{cname}) }}"
                );
                let _ = writeln!(pyi, "    {cname}: {}", target.py_name);
            }
            for f in impl_members(self.krate, inner, "function") {
                match self.emit_method(f, target, &expr_path, !ctor_args.is_empty()) {
                    Ok((code, stub)) => {
                        rs.push_str(&code);
                        pyi.push_str(&stub);
                    }
                    Err(why) => skips.push((
                        format!("{}::{}", target.py_name, Crate::name(f).unwrap_or("?")),
                        why,
                    )),
                }
            }
        }

        // ---- operator overloads ----
        let (ops_rs, ops_pyi) = self.emit_operators(target, impls, &path);
        rs.push_str(&ops_rs);
        pyi.push_str(&ops_pyi);

        // `Debug` is near-universal in this crate and gives Python a
        // faithful repr for free.
        // `Debug` makes a faithful repr for a small value type, but for a
        // large owned one it dumps the entire structure — `TagFile`'s Debug
        // output is tens of kilobytes, which is unusable at a REPL. Value
        // types are `Copy`; everything else gets a concise repr.
        if traits.contains("Debug") && (traits.contains("Copy") || target.is_enum) {
            let inner = if target.is_enum {
                format!("{path}::from(*self)")
            } else {
                "self.0".to_string()
            };
            let _ = writeln!(
                rs,
                "    fn __repr__(&self) -> String {{ format!(\"{{:?}}\", {inner}) }}"
            );
            let _ = writeln!(pyi, "    def __repr__(self) -> str: ...");
        } else {
            let _ = writeln!(
                rs,
                "    fn __repr__(&self) -> String {{ \"<{} object>\".to_string() }}",
                target.py_name
            );
            let _ = writeln!(pyi, "    def __repr__(self) -> str: ...");
        }
        // PyO3 simple enums are already copyable from Python; adding
        // `__copy__` to one would collide with the generated variants.
        if traits.contains("Copy") && !target.is_enum {
            let _ = writeln!(rs, "    fn __copy__(&self) -> Self {{ *self }}");
            let _ = writeln!(pyi, "    def __copy__(self) -> {}: ...", target.py_name);
        }

        let _ = writeln!(rs, "}}\n");
        (rs, pyi, skips)
    }

    /// Emit a single inherent method, or explain why it cannot be bound.
    fn emit_method(
        &self,
        f: &Value,
        target: &Target,
        expr_path: &str,
        has_ctor: bool,
    ) -> Result<(String, String), String> {
        let name = Crate::name(f).ok_or("unnamed")?;
        let func = &f["inner"]["function"];

        // Most generic methods need a policy-declared instantiation, but the
        // `P: AsRef<Path>` shape is by far the most common one in this crate
        // and has a single obvious Python form.
        let mut locals: BTreeMap<String, Mapped> = BTreeMap::new();
        for param in func["generics"]["params"].as_array().into_iter().flatten() {
            let pname = param["name"].as_str().unwrap_or("");
            if bound_is_as_ref_path(param) {
                locals.insert(pname.to_string(), Mapped::Path);
            } else {
                return Err(format!(
                    "generic parameter `{pname}` needs a policy-declared instantiation"
                ));
            }
        }

        let sig = &func["sig"];
        let inputs = sig["inputs"].as_array().ok_or("no inputs")?;
        let takes_self = inputs
            .first()
            .and_then(|i| i.get(0))
            .and_then(Value::as_str)
            == Some("self");
        // An associated function with no receiver becomes a `@staticmethod`.
        // These are overwhelmingly alternative constructors
        // (`RealQuaternion::shortest_arc`, `Matrix4::from_loc_rot_scale`), so
        // dropping them would lose real API. The one exception is a function
        // literally named `new`, which would collide with the field-derived
        // `#[new]` constructor.
        if !takes_self && name == "new" && has_ctor {
            return Err("assoc fn `new` collides with the field-derived constructor".into());
        }

        let mut args = Vec::new();
        let mut call = Vec::new();
        let mut stub_args = Vec::new();
        for input in inputs.iter().skip(if takes_self { 1 } else { 0 }) {
            let aname = input[0].as_str().ok_or("unnamed arg")?;
            let mapped = self
                .map_type_in(&input[1], target, &locals)
                .ok_or_else(|| format!("parameter `{aname}` has no Python mapping"))?;
            args.push(format!("{aname}: {}", mapped.rust_sig()));
            call.push(mapped.to_native(aname));
            stub_args.push(format!("{aname}: {}", mapped.py_type()));
        }

        // A `Result` return becomes a raising Python method: the error half
        // selects an exception type and never reaches the caller as a value.
        let raw_out = &sig["output"];
        let (out_owned, exception) = match split_result(raw_out) {
            Some((ok, err)) => (ok, Some(self.exception_for(&err))),
            None => (raw_out.clone(), None),
        };
        let out = &out_owned;

        let mapped_out = if out.is_null() {
            None
        } else {
            Some(
                self.map_type_in(out, target, &locals)
                    .ok_or("return type has no Python mapping")?,
            )
        };
        let (ret_sig, ret_py) = match &mapped_out {
            None => ("()".to_string(), "None".to_string()),
            Some(m) => (m.rust_sig(), m.py_type()),
        };
        let inner_call = if !takes_self {
            format!("{expr_path}::{name}({})", call.join(", "))
        } else if target.is_enum {
            // The mirror enum holds no Rust value; convert back before
            // dispatching.
            format!("{expr_path}::from(*self).{name}({})", call.join(", "))
        } else {
            format!("self.0.{name}({})", call.join(", "))
        };

        let call_expr = match &exception {
            None => inner_call,
            Some(exc) => {
                format!("{inner_call}.map_err(|e| {exc}::new_err(e.to_string()))?")
            }
        };
        let body = match &mapped_out {
            None => call_expr,
            Some(m) => m.from_native(&call_expr),
        };

        let mut rs = String::new();
        for line in Crate::docs(f).unwrap_or("").lines() {
            let _ = writeln!(rs, "    /// {line}");
        }
        // `&mut self` in Rust must stay `&mut self` here, or the generated
        // call fails to borrow.
        let receiver = if inputs
            .first()
            .and_then(|i| i[1].get("borrowed_ref"))
            .and_then(|r| r.get("is_mutable"))
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            "&mut self"
        } else {
            "&self"
        };
        let arglist = match (takes_self, args.is_empty()) {
            (true, true) => receiver.to_string(),
            (true, false) => format!("{receiver}, {}", args.join(", ")),
            (false, _) => args.join(", "),
        };
        if !takes_self {
            let _ = writeln!(rs, "    #[staticmethod]");
        }
        // A fixed-size-array parameter makes the whole call fallible: the
        // length check happens at the boundary, not in `blam-tags`.
        let fallible = exception.is_some()
            || inputs
                .iter()
                .skip(if takes_self { 1 } else { 0 })
                .filter_map(|i| self.map_type_in(&i[1], target, &locals))
                .any(|m| m.needs_try());
        if fallible {
            let _ = writeln!(
                rs,
                "    fn {name}({arglist}) -> PyResult<{ret_sig}> {{ Ok({body}) }}"
            );
        } else {
            let _ = writeln!(rs, "    fn {name}({arglist}) -> {ret_sig} {{ {body} }}");
        }

        let stub_self = match (takes_self, stub_args.is_empty()) {
            (true, true) => "self".to_string(),
            (true, false) => format!("self, {}", stub_args.join(", ")),
            (false, _) => stub_args.join(", "),
        };
        let decorator = if takes_self { "" } else { "    @staticmethod\n" };
        let pyi = format!("{decorator}    def {name}({stub_self}) -> {ret_py}: ...\n");
        Ok((rs, pyi))
    }

    /// Emit operator dunders. Rust allows several impls of one operator on a
    /// type (differing in RHS); Python permits only one method, so multiple
    /// impls collapse into a single dunder that dispatches on argument type.
    fn emit_operators(
        &self,
        target: &Target,
        impls: &HashMap<String, Vec<&Value>>,
        path: &str,
    ) -> (String, String) {
        let mut by_dunder: BTreeMap<&str, Vec<(Mapped, Mapped, &str)>> = BTreeMap::new();

        for imp in impls.get(&target.id).into_iter().flatten() {
            let inner = &imp["inner"]["impl"];
            if !impl_applies(inner, target) {
                continue;
            }
            let Some(tr) = impl_trait_name(inner) else { continue };
            let Some((_, _, dunder)) = OPS.iter().find(|(t, _, _)| *t == tr) else { continue };
            let Some(f) = impl_members(self.krate, inner, "function").into_iter().next() else {
                continue;
            };
            let sig = &f["inner"]["function"]["sig"];
            let inputs = sig["inputs"].as_array().cloned().unwrap_or_default();

            // `Output` is an associated type on the impl, not in the fn
            // signature — resolve it from the impl's `assoc_type` member.
            let out = impl_members(self.krate, inner, "assoc_type")
                .into_iter()
                .find(|a| Crate::name(a) == Some("Output"))
                .and_then(|a| a["inner"]["assoc_type"].get("type").cloned())
                .unwrap_or(Value::Null);
            let Some(ret) = self.map_type(&out, target) else { continue };

            let rhs = match inputs.get(1) {
                None => None, // unary (Neg)
                Some(i) => match self.map_type(&i[1], target) {
                    Some(m) => Some(m),
                    None => continue,
                },
            };
            let op_sym = OPS.iter().find(|(t, _, _)| *t == tr).map(|(_, s, _)| *s).unwrap();
            match rhs {
                Some(r) => by_dunder.entry(dunder).or_default().push((r, ret, op_sym)),
                None => by_dunder.entry(dunder).or_default().push((
                    Mapped::Prim("()".into()),
                    ret,
                    op_sym,
                )),
            }
        }

        let mut rs = String::new();
        let mut pyi = String::new();
        let _ = path;

        for (dunder, mut variants) in by_dunder {
            if dunder == "__neg__" {
                let (_, ret, _) = &variants[0];
                let _ = writeln!(
                    rs,
                    "    fn __neg__(&self) -> {} {{ {} }}",
                    ret.rust_sig(),
                    ret.from_native("-self.0")
                );
                let _ = writeln!(pyi, "    def __neg__(self) -> {}: ...", ret.py_type());
                continue;
            }
            let sym = match dunder {
                "__add__" => "+",
                "__sub__" => "-",
                "__mul__" => "*",
                "__truediv__" => "/",
                _ => continue,
            };
            if variants.len() == 1 {
                let (rhs, ret, _) = &variants[0];
                let _ = writeln!(
                    rs,
                    "    fn {dunder}(&self, rhs: {}) -> {} {{ {} }}",
                    rhs.rust_sig(),
                    ret.rust_sig(),
                    ret.from_native(&format!("self.0 {sym} {}", rhs.to_native("rhs")))
                );
                let _ = writeln!(
                    pyi,
                    "    def {dunder}(self, rhs: {}) -> {}: ...",
                    rhs.py_type(),
                    ret.py_type()
                );
            } else {
                // Deterministic order so regeneration produces no diff.
                variants.sort_by_key(|(r, _, _)| r.rust_sig());
                let _ = writeln!(
                    rs,
                    "    fn {dunder}<'py>(&self, rhs: &Bound<'py, PyAny>) \
                     -> PyResult<Bound<'py, PyAny>> {{"
                );
                for (rhs, ret, _) in &variants {
                    let _ = writeln!(
                        rs,
                        "        if let Ok(v) = rhs.extract::<{}>() {{",
                        rhs.rust_sig()
                    );
                    let _ = writeln!(
                        rs,
                        "            return Ok({}.into_pyobject(rhs.py())?.into_any());",
                        ret.from_native(&format!("self.0 {sym} {}", rhs.to_native("v")))
                    );
                    let _ = writeln!(rs, "        }}");
                }
                let _ = writeln!(
                    rs,
                    "        Err(pyo3::exceptions::PyTypeError::new_err(\
                     \"unsupported operand type for {dunder}\"))"
                );
                let _ = writeln!(rs, "    }}");
                let union = variants
                    .iter()
                    .map(|(r, _, _)| r.py_type())
                    .collect::<BTreeSet<_>>()
                    .into_iter()
                    .collect::<Vec<_>>()
                    .join(" | ");
                let rets = variants
                    .iter()
                    .map(|(_, r, _)| r.py_type())
                    .collect::<BTreeSet<_>>()
                    .into_iter()
                    .collect::<Vec<_>>()
                    .join(" | ");
                let _ = writeln!(pyi, "    def {dunder}(self, rhs: {union}) -> {rets}: ...");
            }
        }
        (rs, pyi)
    }

    /// The coverage report — what was bound, what was not, and why.
    fn coverage(&self, targets: &[&Target], unmapped: &[(String, String)]) -> String {
        let mut out = String::new();
        let _ = writeln!(out, "# Binding coverage\n");
        let _ = writeln!(
            out,
            "Generated by `blam-tags-bindgen`. Do not edit by hand — \
             regenerate with `cargo run -p blam-tags-bindgen`.\n"
        );
        let _ = writeln!(out, "## Generated classes ({})\n", targets.len());
        for t in targets {
            let alias = if t.aliases.is_empty() {
                String::new()
            } else {
                format!(" (aliases: {})", t.aliases.join(", "))
            };
            let _ = writeln!(out, "- `{}` ← `{}`{alias}", t.py_name, t.concrete_path());
        }
        let _ = writeln!(out, "\n## Not generated ({})\n", self.skipped.len());
        let mut sk = self.skipped.clone();
        sk.sort();
        for (what, why) in &sk {
            let _ = writeln!(out, "- `{what}` — {why}");
        }
        let _ = writeln!(out, "\n## Members skipped within generated classes ({})\n", unmapped.len());
        let mut um = unmapped.to_vec();
        um.sort();
        for (what, why) in &um {
            let _ = writeln!(out, "- `{what}` — {why}");
        }
        out
    }
}

/// The set of trait names implemented for a type.
fn trait_set<'a>(
    impls: &'a HashMap<String, Vec<&Value>>,
    target: &Target,
) -> BTreeSet<&'a str> {
    impls
        .get(&target.id)
        .into_iter()
        .flatten()
        .filter(|imp| impl_applies(&imp["inner"]["impl"], target))
        .filter_map(|imp| impl_trait_name(&imp["inner"]["impl"]))
        .collect()
}

/// `true` if an impl block applies to this particular monomorphization.
///
/// A generic type's impls are not uniform across its instantiations:
/// `impl Bounds<f32>` provides `contains`/`range`, but `Bounds<i16>` has
/// neither. Without this filter every instantiation inherits every impl and
/// the generated code fails to compile — or worse, compiles against the
/// wrong one. A blanket `impl<T> Bounds<T>`, which is what the derives
/// produce, applies to all instantiations.
fn impl_applies(imp_inner: &Value, target: &Target) -> bool {
    let args = imp_inner
        .get("for")
        .and_then(|f| f.get("resolved_path"))
        .and_then(|r| r.get("args"))
        .and_then(|a| a.get("angle_bracketed"))
        .and_then(|a| a.get("args"))
        .and_then(Value::as_array);
    let Some(args) = args else { return true };
    if args.is_empty() {
        return true;
    }
    if target.generic_args.len() != args.len() {
        return false;
    }
    args.iter().zip(&target.generic_args).all(|(arg, want)| {
        let Some(t) = arg.get("type") else { return true };
        if t.get("generic").is_some() {
            return true;
        }
        t.get("primitive").and_then(Value::as_str) == Some(want.as_str())
            || t.get("resolved_path")
                .and_then(|r| r.get("path"))
                .and_then(Value::as_str)
                == Some(want.as_str())
    })
}

/// `true` if the item declares type parameters.
fn type_is_generic(item: &Value) -> bool {
    let kind = Crate::kind(item).unwrap_or("");
    item["inner"][kind]["generics"]["params"]
        .as_array()
        .map(|params| {
            params
                .iter()
                .any(|p| p.get("kind").and_then(|k| k.get("type")).is_some())
        })
        .unwrap_or(false)
}

/// A struct's public fields as `(name, type)` pairs.
fn struct_fields(krate: &Crate, item: &Value) -> Vec<(String, Value)> {
    let Some(kind) = item["inner"]["struct"]["kind"].as_object() else {
        return Vec::new();
    };
    let ids: Vec<String> = match kind.keys().next().map(String::as_str) {
        Some("plain") => item["inner"]["struct"]["kind"]["plain"]["fields"]
            .as_array()
            .map(|a| a.iter().map(|v| v.to_string()).collect())
            .unwrap_or_default(),
        Some("tuple") => item["inner"]["struct"]["kind"]["tuple"]
            .as_array()
            .map(|a| a.iter().filter(|v| !v.is_null()).map(|v| v.to_string()).collect())
            .unwrap_or_default(),
        _ => Vec::new(),
    };
    let is_tuple = kind.keys().next().map(String::as_str) == Some("tuple");
    ids.iter()
        .enumerate()
        .filter_map(|(i, id)| {
            let f = krate.item(id)?;
            if !Crate::is_public(f) {
                return None;
            }
            let name = if is_tuple {
                i.to_string()
            } else {
                Crate::name(f)?.to_string()
            };
            Some((name, f["inner"]["struct_field"].clone()))
        })
        .collect()
}

/// The first concrete type argument of a resolved path (`Vec<T>` → `T`).
fn first_type_arg(rp: &Value) -> Option<Value> {
    rp.get("args")?
        .get("angle_bracketed")?
        .get("args")?
        .as_array()?
        .iter()
        .find_map(|a| a.get("type").cloned())
}

/// Preamble shared by both generated files.
/// Make a Rust parameter name safe to use as a Python identifier. Names that
/// are Python keywords (but valid Rust identifiers, e.g. `from`) get a trailing
/// underscore so the emitted `.pyi` parses and pyo3 exposes a callable kwarg.
fn sanitize_py_ident(name: &str) -> String {
    const PY_KEYWORDS: &[&str] = &[
        "False", "None", "True", "and", "as", "assert", "async", "await", "break",
        "class", "continue", "def", "del", "elif", "else", "except", "finally",
        "for", "from", "global", "if", "import", "in", "is", "lambda", "nonlocal",
        "not", "or", "pass", "raise", "return", "try", "while", "with", "yield",
    ];
    if PY_KEYWORDS.contains(&name) {
        format!("{name}_")
    } else {
        name.to_string()
    }
}

fn header(rs: &mut String, pyi: &mut String, module: &str) {
    let banner = "// @generated by blam-tags-bindgen. DO NOT EDIT.\n\
                  // Regenerate with `cargo run -p blam-tags-bindgen`.\n";
    rs.push_str(banner);
    rs.push_str("#![allow(clippy::all)]\n");
    rs.push_str("use pyo3::prelude::*;\n");
    rs.push_str("use pyo3::types::PyAny;\n");
    rs.push_str("#[allow(unused_imports)]\nuse crate::manual::*;\n\n");
    rs.push_str(
        "/// Convert a Python-supplied list into a fixed-size Rust array,\n\
         /// reporting the arity mismatch as a Python `ValueError` rather than\n\
         /// panicking.\n\
         fn __seq_to_array<T, const N: usize>(v: Vec<T>) -> PyResult<[T; N]> {\n\
         \x20   let got = v.len();\n\
         \x20   <[T; N]>::try_from(v).map_err(|_| {\n\
         \x20       pyo3::exceptions::PyValueError::new_err(format!(\n\
         \x20           \"expected a sequence of {N} elements, got {got}\"\n\
         \x20       ))\n\
         \x20   })\n\
         }\n\n",
    );
    let _ = writeln!(
        pyi,
        "# @generated by blam-tags-bindgen. DO NOT EDIT.\n\
         # Python type stubs for `{module}`.\n\
         \nimport enum\nimport os\n"
    );
}

/// The names of a type's declared type parameters, in declaration order.
fn type_param_names(item: &Value) -> Vec<String> {
    let kind = Crate::kind(item).unwrap_or("");
    item["inner"][kind]["generics"]["params"]
        .as_array()
        .map(|params| {
            params
                .iter()
                .filter(|p| p.get("kind").and_then(|k| k.get("type")).is_some())
                .filter_map(|p| p.get("name").and_then(Value::as_str).map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// `true` if every variant of an enum is a unit variant, which is the only
/// shape PyO3 can mirror directly as a Python enum.
fn enum_is_fieldless(krate: &Crate, item: &Value) -> bool {
    item["inner"]["enum"]["variants"]
        .as_array()
        .map(|vs| {
            vs.iter().all(|vid| {
                krate
                    .item(&vid.to_string())
                    .and_then(|v| v["inner"]["variant"].get("kind"))
                    .and_then(Value::as_str)
                    == Some("plain")
            })
        })
        .unwrap_or(false)
}

/// An enum's variant names, in declaration order.
fn enum_variants(krate: &Crate, item: &Value) -> Vec<String> {
    item["inner"]["enum"]["variants"]
        .as_array()
        .map(|vs| {
            vs.iter()
                .filter_map(|vid| krate.item(&vid.to_string()))
                .filter_map(|v| Crate::name(v).map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// Split a `Result<T, E>` type into its ok half and its error half.
///
/// `std::io::Result<T>` supplies only one argument; the missing error half is
/// reported as null and resolved to `OSError` downstream.
fn split_result(ty: &Value) -> Option<(Value, Value)> {
    let rp = ty.get("resolved_path")?;
    let path = rp.get("path")?.as_str()?;
    if path.rsplit("::").next()? != "Result" {
        return None;
    }
    let args = rp.get("args")?.get("angle_bracketed")?.get("args")?.as_array()?;
    let ok = args.first()?.get("type")?.clone();
    let err = args.get(1).and_then(|a| a.get("type")).cloned().unwrap_or(Value::Null);
    Some((ok, err))
}

/// `true` if a generic parameter's only bound is `AsRef<Path>`.
fn bound_is_as_ref_path(param: &Value) -> bool {
    let Some(bounds) = param["kind"]["type"]["bounds"].as_array() else {
        return false;
    };
    bounds.len() == 1
        && bounds[0]
            .get("trait_bound")
            .and_then(|b| b.get("trait"))
            .map(|t| {
                t.get("path").and_then(Value::as_str) == Some("AsRef")
                    && serde_json::to_string(t).unwrap_or_default().contains("Path")
            })
            .unwrap_or(false)
}

/// `true` if rustdoc reports the struct as having fields it did not list —
/// i.e. private ones. Such a struct cannot be constructed from outside the
/// crate, so no `#[new]` may be derived from its public fields.
fn struct_has_stripped_fields(item: &Value) -> bool {
    let k = &item["inner"]["struct"]["kind"];
    k["plain"]["has_stripped_fields"].as_bool().unwrap_or(false)
        || k["tuple"]
            .as_array()
            .map(|a| a.iter().any(Value::is_null))
            .unwrap_or(false)
}
