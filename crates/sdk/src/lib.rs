//! The headers and link stubs a Darwin program is compiled against, generated
//! from the frameworks' own declarations ([`exports::decl`]).

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::{self, Write as _};
use std::path::PathBuf;

use exports::Recording;
use exports::decl::{Api, Definition, Enum, Function, Named, Type, Variable};

/// One framework as an SDK carries it.
pub struct Framework {
    pub api: &'static Api,
    pub install_name: &'static str,
    /// Its current and compatibility version, which a program records when it
    /// links and dyld checks when it loads.
    pub version: &'static str,
    /// Every symbol it exports, in the Mach-O spelling.
    pub symbols: BTreeSet<String>,
}

impl Framework {
    /// Its headers and stub, by path relative to the SDK root.
    pub fn files(&self) -> Result<BTreeMap<PathBuf, String>, Error> {
        check_exported(self.api, &self.symbols)?;
        let name = self.api.framework;
        let root = PathBuf::from(format!("System/Library/Frameworks/{name}.framework"));
        let mut out = BTreeMap::new();
        for (include, text) in headers(self.api)? {
            let file = include
                .strip_prefix(&format!("{name}/"))
                .unwrap_or(&include)
                .to_string();
            out.insert(root.join("Headers").join(file), text);
        }
        let stub = stub(self.install_name, self.version, &self.symbols);
        out.insert(root.join(format!("{name}.tbd")), stub);
        Ok(out)
    }
}

/// CoreFoundation, as the `corefoundation` crate implements it.
///
/// Its data symbols are only known once the crate has registered its classes,
/// so this bootstraps it.
pub fn corefoundation() -> Framework {
    let mut recording = Recording::default();
    corefoundation::provide(&mut recording);
    let mut symbols: BTreeSet<String> = recording.symbols().into_iter().map(String::from).collect();
    symbols.extend(
        corefoundation::data_symbols()
            .into_iter()
            .map(|(name, _)| name),
    );
    Framework {
        api: &corefoundation::decl::API,
        install_name: "/System/Library/Frameworks/CoreFoundation.framework/Versions/A/CoreFoundation",
        // macOS's: dyld refuses a library older than the one a program was
        // linked against, so a program linked here still loads there.
        version: "150",
        symbols,
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Error {
    /// A declaration names a header outside the framework being generated.
    ForeignHeader {
        item: &'static str,
        header: &'static str,
    },
    /// `void` where only a value type can go.
    VoidValue { item: &'static str },
    /// A C `enum` constant must fit an `int`.
    EnumOutOfRange { case: &'static str, value: i64 },
    /// Headers that include each other cannot order their typedefs.
    IncludeCycle { header: String },
    /// A declaration names a symbol the framework does not export.
    NotExported { symbol: String },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::ForeignHeader { item, header } => {
                write!(
                    f,
                    "`{item}` is declared in `{header}`, outside this framework"
                )
            }
            Error::VoidValue { item } => write!(f, "`{item}` has type `void`"),
            Error::EnumOutOfRange { case, value } => {
                write!(f, "`{case}` = {value} does not fit a C enum constant")
            }
            Error::IncludeCycle { header } => write!(f, "`{header}` includes itself"),
            Error::NotExported { symbol } => {
                write!(f, "`{symbol}` is declared but not exported")
            }
        }
    }
}

impl std::error::Error for Error {}

/// Every header `api` declares, plus its umbrella, by include path.
pub fn headers(api: &Api) -> Result<BTreeMap<String, String>, Error> {
    let prefix = format!("{}/", api.framework);
    let mut contents: BTreeMap<&'static str, Contents> = BTreeMap::new();
    let own = |item: &'static str, header: &'static str| match header.starts_with(&prefix) {
        true => Ok(header),
        false => Err(Error::ForeignHeader { item, header }),
    };
    for ty in api.types {
        let named = ty.named().expect("`c_types!` declares only typedefs");
        contents
            .entry(own(named.name, named.header)?)
            .or_default()
            .types
            .push(named);
    }
    for e in api.enums {
        let name = e.ty.named().map_or("enum", |n| n.name);
        contents
            .entry(own(name, e.header)?)
            .or_default()
            .enums
            .push(e);
    }
    for v in api.variables {
        contents
            .entry(own(v.name, v.header)?)
            .or_default()
            .variables
            .push(v);
    }
    for f in api.functions {
        contents
            .entry(own(f.name, f.header)?)
            .or_default()
            .functions
            .push(f);
    }
    for fragment in api.fragments {
        let header = own("fragment", fragment.header)?;
        contents
            .entry(header)
            .or_default()
            .fragments
            .push(fragment.text);
    }

    let includes: BTreeMap<&str, BTreeSet<&str>> = contents
        .iter()
        .map(|(header, c)| (*header, c.includes(header)))
        .collect();
    for header in includes.keys() {
        reject_cycle(header, &includes, &mut Vec::new())?;
    }

    let mut out = BTreeMap::new();
    for (header, c) in &contents {
        out.insert(header.to_string(), c.render(header, &includes[header])?);
    }
    let umbrella = format!("{prefix}{}.h", api.framework);
    let members: Vec<&str> = contents.keys().copied().collect();
    out.insert(umbrella.clone(), render_umbrella(&umbrella, &members));
    Ok(out)
}

/// Check that every symbol `api` declares is among `exported`, in the Mach-O
/// spelling.
pub fn check_exported(api: &Api, exported: &BTreeSet<String>) -> Result<(), Error> {
    let declared = api
        .functions
        .iter()
        .map(|f| f.name)
        .chain(api.variables.iter().map(|v| v.name));
    for name in declared {
        let symbol = format!("_{name}");
        if !exported.contains(&symbol) {
            return Err(Error::NotExported { symbol });
        }
    }
    Ok(())
}

/// The targets a stub answers for.
const STUB_TARGETS: &str = "[ x86_64-macos, arm64-macos ]";

/// A text-based dylib stub (`.tbd`, version 4) a linker resolves against.
pub fn stub(install_name: &str, version: &str, symbols: &BTreeSet<String>) -> String {
    let list = symbols
        .iter()
        .map(|s| format!("'{s}'"))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "--- !tapi-tbd\n\
         tbd-version: 4\n\
         targets: {STUB_TARGETS}\n\
         install-name: '{install_name}'\n\
         current-version: {version}\n\
         compatibility-version: {version}\n\
         exports:\n  \
         - targets: {STUB_TARGETS}\n    \
         symbols: [ {list} ]\n\
         ...\n"
    )
}

#[derive(Default)]
struct Contents {
    types: Vec<&'static Named>,
    enums: Vec<&'static Enum>,
    variables: Vec<&'static Variable>,
    functions: Vec<&'static Function>,
    fragments: Vec<&'static str>,
}

impl Contents {
    /// The other headers whose typedefs this one's declarations name.
    fn includes(&self, own: &str) -> BTreeSet<&'static str> {
        let mut named = Vec::new();
        for n in &self.types {
            definition_types(&n.definition, &mut named);
        }
        for e in &self.enums {
            named.extend(e.ty.named());
        }
        for v in &self.variables {
            outermost_named(v.ty, &mut named);
        }
        for f in &self.functions {
            f.returns
                .into_iter()
                .chain(f.params.iter().map(|p| p.ty))
                .for_each(|t| outermost_named(t, &mut named));
        }
        named
            .into_iter()
            .map(|n| n.header)
            .filter(|h| *h != own)
            .collect()
    }

    fn render(&self, header: &str, includes: &BTreeSet<&str>) -> Result<String, Error> {
        let mut body = String::new();
        let order = typedef_order(&self.types, header);
        for named in &order {
            let text = typedef(named)?;
            body.push_str(&text);
            // A struct reads better set apart; a run of one-line typedefs does not.
            if text.lines().count() > 1 {
                body.push('\n');
            }
        }
        if !order.is_empty() && !body.ends_with("\n\n") {
            body.push('\n');
        }
        for e in &self.enums {
            body.push_str(&enumeration(e)?);
        }
        for text in &self.fragments {
            let _ = writeln!(body, "{text}\n");
        }
        for v in &self.variables {
            let _ = writeln!(body, "extern {};", declarator(&constant(v.ty), v.name));
        }
        if !self.variables.is_empty() {
            body.push('\n');
        }
        for f in &self.functions {
            let _ = writeln!(body, "extern {};", prototype(f)?);
        }
        Ok(wrap(header, includes, &body))
    }
}

/// The typedefs a definition names, without following them further.
fn definition_types(definition: &Definition, out: &mut Vec<&'static Named>) {
    match definition {
        Definition::Alias(ty) => outermost_named(ty, out),
        Definition::Opaque { .. } => {}
        Definition::Struct(fields) => fields.iter().for_each(|f| outermost_named(f.ty, out)),
        Definition::Callback { returns, params } => returns
            .iter()
            .chain(params.iter())
            .for_each(|t| outermost_named(t, out)),
    }
}

/// The typedef a type spelling names, through any pointers to it.
fn outermost_named(ty: &'static Type, out: &mut Vec<&'static Named>) {
    match ty {
        Type::Builtin(_) => {}
        Type::Named(named) => out.push(named),
        Type::Pointer { to, .. } => outermost_named(to, out),
    }
}

fn reject_cycle<'a>(
    header: &'a str,
    includes: &BTreeMap<&'a str, BTreeSet<&'a str>>,
    path: &mut Vec<&'a str>,
) -> Result<(), Error> {
    if path.contains(&header) {
        return Err(Error::IncludeCycle {
            header: header.to_string(),
        });
    }
    path.push(header);
    for next in includes.get(header).into_iter().flatten() {
        reject_cycle(next, includes, path)?;
    }
    path.pop();
    Ok(())
}

/// `types` with each after the typedefs of the same header it names.
fn typedef_order(types: &[&'static Named], header: &str) -> Vec<&'static Named> {
    fn visit(named: &'static Named, header: &str, out: &mut Vec<&'static Named>) {
        if named.header != header || out.iter().any(|n| std::ptr::eq(*n, named)) {
            return;
        }
        let mut deps = Vec::new();
        definition_types(&named.definition, &mut deps);
        for dep in deps {
            visit(dep, header, out);
        }
        out.push(named);
    }
    let mut out = Vec::new();
    for named in types {
        visit(named, header, &mut out);
    }
    out
}

/// The C spelling of `ty`.
///
/// `const` goes before a base type and after a pointer, the one position that
/// qualifies the right thing in both cases: `const char *`, `char * const *`.
fn spell(ty: &Type) -> String {
    match ty {
        Type::Builtin(name) => name.to_string(),
        Type::Named(named) => named.name.to_string(),
        Type::Pointer { to, constant } => {
            let inner = spell(to);
            match (constant, to) {
                (false, _) => format!("{inner} *"),
                (true, Type::Pointer { .. }) => format!("{inner} const *"),
                (true, _) => format!("const {inner} *"),
            }
        }
    }
}

/// `ty` made `const` at its outermost level, spelled.
fn constant(ty: &Type) -> String {
    match ty {
        Type::Pointer { .. } => format!("{} const", spell(ty)),
        _ => format!("const {}", spell(ty)),
    }
}

/// A spelled type followed by a name, with no space after a `*`.
fn declarator(spelled: &str, name: &str) -> String {
    match spelled.ends_with('*') {
        true => format!("{spelled}{name}"),
        false => format!("{spelled} {name}"),
    }
}

fn value_type(ty: &Type, item: &'static str) -> Result<String, Error> {
    match ty {
        Type::Builtin("void") => Err(Error::VoidValue { item }),
        _ => Ok(spell(ty)),
    }
}

fn returns(ty: Option<&Type>, item: &'static str) -> Result<String, Error> {
    ty.map_or(Ok("void".to_string()), |t| value_type(t, item))
}

fn parameter_list(
    params: impl ExactSizeIterator<Item = Result<String, Error>>,
) -> Result<String, Error> {
    match params.len() {
        0 => Ok("void".to_string()),
        _ => Ok(params.collect::<Result<Vec<_>, _>>()?.join(", ")),
    }
}

fn typedef(named: &Named) -> Result<String, Error> {
    let name = named.name;
    Ok(match named.definition {
        Definition::Alias(ty) => format!("typedef {};\n", declarator(&value_type(ty, name)?, name)),
        Definition::Opaque { tag, constant } => {
            let qualifier = if constant { "const " } else { "" };
            format!("typedef {qualifier}struct {tag} *{name};\n")
        }
        Definition::Struct(fields) => {
            let mut out = String::from("typedef struct {\n");
            for field in fields {
                let _ = writeln!(
                    out,
                    "    {};",
                    declarator(&value_type(field.ty, name)?, field.name)
                );
            }
            let _ = writeln!(out, "}} {name};");
            out
        }
        Definition::Callback {
            returns: ret,
            params,
        } => {
            let params = parameter_list(params.iter().map(|t| value_type(t, name)))?;
            format!("typedef {} (*{name})({params});\n", returns(ret, name)?)
        }
    })
}

fn enumeration(e: &Enum) -> Result<String, Error> {
    let mut out = String::from("enum {\n");
    for &(case, value) in e.cases {
        if i32::try_from(value).is_err() {
            return Err(Error::EnumOutOfRange { case, value });
        }
        let _ = writeln!(out, "    {case} = {value},");
    }
    out.push_str("};\n\n");
    Ok(out)
}

fn prototype(f: &Function) -> Result<String, Error> {
    let params = parameter_list(
        f.params
            .iter()
            .map(|p| value_type(p.ty, f.name).map(|t| declarator(&t, p.name))),
    )?;
    Ok(declarator(
        &returns(f.returns, f.name)?,
        &format!("{}({params})", f.name),
    ))
}

/// Apple's guard spelling, which code testing for a header's presence checks:
/// `CoreFoundation/CFBase.h` is `__COREFOUNDATION_CFBASE__`.
fn guard(header: &str) -> String {
    let stem = header.strip_suffix(".h").unwrap_or(header);
    let mangled: String = stem
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect();
    format!("__{mangled}__")
}

fn wrap(header: &str, includes: &BTreeSet<&str>, body: &str) -> String {
    let guard = guard(header);
    let mut out = format!(
        "/* Generated by Rosemary from its framework declarations. Do not edit. */\n\n\
         #ifndef {guard}\n#define {guard}\n\n"
    );
    for include in includes {
        let _ = writeln!(out, "#include <{include}>");
    }
    if !includes.is_empty() {
        out.push('\n');
    }
    out.push_str("#ifdef __cplusplus\nextern \"C\" {\n#endif\n\n");
    out.push_str(body);
    out.push_str("\n#ifdef __cplusplus\n}\n#endif\n\n#endif\n");
    out
}

fn render_umbrella(header: &str, members: &[&str]) -> String {
    let guard = guard(header);
    let mut out = format!(
        "/* Generated by Rosemary from its framework declarations. Do not edit. */\n\n\
         #ifndef {guard}\n#define {guard}\n\n"
    );
    for member in members {
        let _ = writeln!(out, "#include <{member}>");
    }
    out.push_str("\n#endif\n");
    out
}

#[cfg(test)]
mod tests;
