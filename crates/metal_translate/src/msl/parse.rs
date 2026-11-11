//! A recursive-descent parser for the C++ subset Metal source is written in.

use super::ast::*;
use super::lex::{Kind, Token};
use super::syntax_error;
use crate::TranslateError;
use crate::ir::{Access, Stage, TextureDimension, TextureType, Type};

pub(super) fn parse(tokens: &[Token]) -> Result<Program> {
    let mut parser = Parser {
        tokens,
        at: 0,
        program: Program::default(),
        file_samplers: Vec::new(),
        local_samplers: Vec::new(),
        aliases: std::collections::HashMap::new(),
        array_templates: std::collections::HashSet::new(),
        type_params: Vec::new(),
    };
    parser.program()?;
    Ok(parser.program)
}

struct Parser<'a> {
    tokens: &'a [Token],
    at: usize,
    program: Program,
    /// File-scope `constexpr sampler`s declared so far, by name, each an index
    /// into [`Program::inline_samplers`].
    file_samplers: Vec<(String, usize)>,
    /// Those declared in the function being parsed.
    local_samplers: Vec<(String, usize, usize)>,
    /// `typedef` names of types other than structures.
    aliases: std::collections::HashMap<String, Ty>,
    /// Class templates which are an array of `N` `T`s and nothing more, as
    /// SPIRV-Cross's `spvUnsafeArray` is: an array with value semantics.
    array_templates: std::collections::HashSet<String>,
    /// The type parameters of the function template being parsed.
    type_params: Vec<String>,
}

type Result<T> = std::result::Result<T, TranslateError>;

impl Parser<'_> {
    fn line(&self) -> usize {
        self.tokens
            .get(self.at)
            .or(self.tokens.last())
            .map_or(1, |token| token.line)
    }

    fn peek(&self) -> Option<&Kind> {
        self.tokens.get(self.at).map(|token| &token.kind)
    }

    fn peek_at(&self, offset: usize) -> Option<&Kind> {
        self.tokens.get(self.at + offset).map(|token| &token.kind)
    }

    fn is_punct(&self, punct: &str) -> bool {
        matches!(self.peek(), Some(Kind::Punct(p)) if *p == punct)
    }

    fn is_word(&self, word: &str) -> bool {
        matches!(self.peek(), Some(Kind::Ident(w)) if w == word)
    }

    fn eat(&mut self, punct: &str) -> bool {
        if self.is_punct(punct) {
            self.at += 1;
            true
        } else {
            false
        }
    }

    fn eat_word(&mut self, word: &str) -> bool {
        if self.is_word(word) {
            self.at += 1;
            true
        } else {
            false
        }
    }

    fn expect(&mut self, punct: &str) -> Result<()> {
        if self.eat(punct) {
            Ok(())
        } else {
            Err(self.error(format!("expected `{punct}`")))
        }
    }

    fn ident(&mut self) -> Result<String> {
        match self.peek() {
            Some(Kind::Ident(name)) => {
                let name = name.clone();
                self.at += 1;
                Ok(name)
            }
            _ => Err(self.error("expected an identifier")),
        }
    }

    fn error(&self, message: impl Into<String>) -> TranslateError {
        let found = match self.peek() {
            Some(Kind::Ident(name)) => format!(" at `{name}`"),
            Some(Kind::Punct(punct)) => format!(" at `{punct}`"),
            Some(Kind::Int { value, .. }) => format!(" at `{value}`"),
            Some(Kind::Float { value, .. }) => format!(" at `{value}`"),
            None => " at the end of the source".to_string(),
        };
        syntax_error(self.line(), format!("{}{found}", message.into()))
    }

    fn program(&mut self) -> Result<()> {
        while self.peek().is_some() {
            if self.eat(";") {
                continue;
            }
            if self.eat_word("using") {
                // `using namespace metal;` is the only form Metal source uses.
                while !self.eat(";") {
                    if self.peek().is_none() {
                        return Err(self.error("unterminated using declaration"));
                    }
                    self.at += 1;
                }
                continue;
            }
            let start = self.at;
            let parsed =
                if self.is_word("struct") && matches!(self.peek_at(2), Some(Kind::Punct("{"))) {
                    self.structure()
                } else if self.eat_word("typedef") {
                    self.typedef()
                } else if self.eat_word("template") {
                    self.template()
                } else {
                    self.top_level()
                };
            if let Err(error) = parsed {
                // The declaration is skipped whole; what uses it fails when
                // it is lowered, and the rest of the library stands.
                self.at = start;
                let (name, entry) = self.declared_name();
                self.skip_declaration();
                self.program.failed.push(FailedItem { name, entry, error });
            }
        }
        Ok(())
    }

    /// The name the declaration at the cursor declares - the identifier
    /// before its first `(`, `=`, `[`, `;` or `{` - and whether it is an entry
    /// point.
    fn declared_name(&self) -> (Option<String>, bool) {
        let mut entry = false;
        let mut at = self.at;
        let mut last = None;
        while let Some(token) = self.tokens.get(at) {
            match &token.kind {
                Kind::Ident(word) if matches!(word.as_str(), "vertex" | "fragment" | "kernel") => {
                    entry = true;
                }
                Kind::Ident(word) if word == "__attribute__" => {
                    // `__attribute__((...))`: past both parentheses.
                    at += 3;
                    continue;
                }
                Kind::Ident(word) => last = Some(word.clone()),
                Kind::Punct("(" | "=" | "[" | ";" | "{") => return (last, entry),
                // A template's parameters name nothing it declares.
                Kind::Punct(">") => last = None,
                _ => {}
            }
            at += 1;
        }
        (None, entry)
    }

    /// Move past the declaration at the cursor: to its `;`, or past the
    /// body its first `{` opens and the `;` which may follow it.
    fn skip_declaration(&mut self) {
        let mut depth = 0usize;
        while let Some(token) = self.tokens.get(self.at) {
            self.at += 1;
            match &token.kind {
                Kind::Punct("{") => depth += 1,
                Kind::Punct("}") if depth > 0 => {
                    depth -= 1;
                    if depth == 0 {
                        self.eat(";");
                        return;
                    }
                }
                Kind::Punct(";") if depth == 0 => return,
                _ => {}
            }
        }
    }

    fn structure(&mut self) -> Result<()> {
        self.at += 1;
        let name = self.ident()?;
        let fields = self.struct_body()?;
        self.expect(";")?;
        self.program.structs.push(StructDef { name, fields });
        Ok(())
    }

    /// `template <typename T, size_t N> struct Name { T member[...]; ... };`
    /// whose other members are only subscript operators returning
    /// `member[index]`: an array of `N` `T`s, at least one. Any other
    /// template is refused.
    /// `template <...>` and what it declares: a function template whose
    /// parameters are all types, or an array wrapper.
    fn template(&mut self) -> Result<()> {
        let line = self.line();
        self.expect("<")?;
        let mut types = Vec::new();
        let mut values = Vec::new();
        loop {
            if self.eat_word("typename") || self.eat_word("class") {
                types.push(self.ident()?);
            } else if self.eat_word("size_t") || self.eat_word("uint") || self.eat_word("int") {
                values.push(self.ident()?);
            } else {
                return Err(self.error("unsupported template parameter"));
            }
            if !self.eat(",") {
                break;
            }
        }
        self.expect(">")?;
        if self.eat_word("struct") {
            return match (types.as_slice(), values.as_slice()) {
                ([element], [_]) => self.array_template(element.clone()),
                _ => Err(syntax_error(
                    line,
                    "a class template other than an array wrapper is not supported",
                )),
            };
        }
        if !values.is_empty() {
            return Err(syntax_error(
                line,
                "a function template with a non-type parameter is not supported",
            ));
        }
        self.type_params = types;
        let parsed = self.top_level();
        self.type_params.clear();
        parsed
    }

    fn array_template(&mut self, element: String) -> Result<()> {
        let line = self.line();
        let unsupported = || {
            syntax_error(
                line,
                "a class template other than an array wrapper is not supported",
            )
        };
        let name = self.ident()?;
        self.expect("{")?;
        if !self.eat_word(&element) {
            return Err(unsupported());
        }
        let member = self.ident()?;
        self.expect("[")?;
        self.skip_balanced("[", "]");
        self.expect(";")?;
        while !self.eat("}") {
            // A member function: qualifiers, `operator []`, its parameter
            // and qualifiers, then `{ return member[parameter]; }`.
            let mut subscript = false;
            while !self.is_punct("{") {
                if self.is_word("operator") && matches!(self.peek_at(1), Some(Kind::Punct("["))) {
                    subscript = true;
                }
                if self.peek().is_none() {
                    return Err(self.error("unterminated template"));
                }
                self.at += 1;
            }
            if !subscript {
                return Err(unsupported());
            }
            self.expect("{")?;
            if !self.eat_word("return") || !self.eat_word(&member) {
                return Err(unsupported());
            }
            self.expect("[")?;
            self.ident()?;
            self.expect("]")?;
            self.expect(";")?;
            self.expect("}")?;
        }
        self.expect(";")?;
        self.array_templates.insert(name);
        Ok(())
    }

    /// Move past the tokens up to and including the `close` which balances
    /// an `open` already consumed.
    fn skip_balanced(&mut self, open: &str, close: &str) {
        let mut depth = 1usize;
        while let Some(token) = self.tokens.get(self.at) {
            self.at += 1;
            match &token.kind {
                Kind::Punct(p) if *p == open => depth += 1,
                Kind::Punct(p) if *p == close => {
                    depth -= 1;
                    if depth == 0 {
                        return;
                    }
                }
                _ => {}
            }
        }
    }

    /// `typedef struct [tag] { ... } Name;`, `typedef enum [tag] [: T] { ... }
    /// Name;` or `typedef T Name;`.
    fn typedef(&mut self) -> Result<()> {
        if self.eat_word("enum") {
            if matches!(self.peek(), Some(Kind::Ident(_))) {
                self.ident()?;
            }
            let scalar = self.enumeration()?;
            let name = self.ident()?;
            self.expect(";")?;
            self.aliases.insert(name, Ty::Scalar(scalar));
            return Ok(());
        }
        if self.eat_word("struct") {
            let tag = match self.peek() {
                Some(Kind::Ident(_)) => Some(self.ident()?),
                _ => None,
            };
            let fields = self.struct_body()?;
            let name = self.ident()?;
            self.expect(";")?;
            if let Some(tag) = tag.filter(|tag| *tag != name) {
                let index = self.program.structs.len();
                self.aliases.insert(name, Ty::Struct(index));
                self.program.structs.push(StructDef { name: tag, fields });
            } else {
                self.program.structs.push(StructDef { name, fields });
            }
            return Ok(());
        }
        let ty = self.ty()?;
        let name = self.ident()?;
        self.expect(";")?;
        self.aliases.insert(name, ty);
        Ok(())
    }

    /// An enumeration's `[: T] { A = 1, B, ... }`, its enumerators recorded,
    /// answering its underlying type.
    fn enumeration(&mut self) -> Result<Scalar> {
        let line = self.line();
        // Without a fixed type, every value here fits the `int` C++ picks.
        let scalar = if self.eat(":") {
            match self.ty()? {
                Ty::Scalar(scalar @ Scalar::Int { .. }) => scalar,
                _ => {
                    return Err(syntax_error(
                        line,
                        "an enumeration's type must be an integer",
                    ));
                }
            }
        } else {
            Scalar::INT
        };
        self.expect("{")?;
        let mut next = 0u64;
        while !self.eat("}") {
            let name = self.ident()?;
            if self.eat("=") {
                let negative = self.eat("-");
                let value = match self.peek() {
                    Some(Kind::Int { value, .. }) => *value,
                    _ => return Err(self.error("an enumerator's value must be an integer literal")),
                };
                self.at += 1;
                next = if negative {
                    value.wrapping_neg()
                } else {
                    value
                };
            }
            self.program.enumerators.insert(name, (next, scalar));
            next = next.wrapping_add(1);
            if !self.eat(",") {
                self.expect("}")?;
                break;
            }
        }
        Ok(scalar)
    }

    /// A structure's `{ fields }`.
    fn struct_body(&mut self) -> Result<Vec<Field>> {
        self.expect("{")?;
        let mut fields = Vec::new();
        while !self.eat("}") {
            let line = self.line();
            self.skip_qualifiers();
            let base = self.ty()?;
            loop {
                let name = self.ident()?;
                let ty = self.array_suffix(base.clone())?.in_memory();
                let attributes = self.attributes()?;
                fields.push(Field {
                    name,
                    ty,
                    attributes,
                    line,
                });
                if !self.eat(",") {
                    break;
                }
            }
            self.expect(";")?;
        }
        Ok(fields)
    }

    /// A global variable or a function definition.
    fn top_level(&mut self) -> Result<()> {
        let line = self.line();
        let leading = self.attributes()?;
        let mut early_fragment_tests = false;
        for attribute in &leading {
            if attribute.name == "early_fragment_tests" && attribute.args.is_empty() {
                early_fragment_tests = true;
            } else {
                return Err(syntax_error(
                    attribute.line,
                    format!("unsupported function attribute [[{}]]", attribute.name),
                ));
            }
        }
        let stage = if self.eat_word("vertex") {
            Some(Stage::Vertex)
        } else if self.eat_word("fragment") {
            Some(Stage::Fragment)
        } else if self.eat_word("kernel") {
            Some(Stage::Kernel)
        } else {
            None
        };
        if early_fragment_tests && stage != Some(Stage::Fragment) {
            return Err(syntax_error(
                line,
                "early_fragment_tests requires a fragment function",
            ));
        }
        let space = self.skip_qualifiers();
        self.inlining_attributes()?;
        let space = self.skip_qualifiers().or(space);
        let ty = self.ty()?;
        let name = self.ident()?;
        if ty == Ty::Sampler && self.is_punct("(") {
            if stage.is_some() {
                return Err(syntax_error(line, "a stage function returning a sampler"));
            }
            let sampler = self.inline_sampler()?;
            self.expect(";")?;
            self.file_samplers.push((name, sampler));
            return Ok(());
        }
        if self.is_punct("(") {
            let mut params = self.parameters()?;
            if self.eat(";") {
                // A prototype; the definition follows.
                return Ok(());
            }
            self.local_samplers.clear();
            let body = self.block()?;
            let locals = std::mem::take(&mut self.local_samplers);
            match stage {
                // The stage takes each sampler it can see as a resource the
                // runtime binds, named as the source names the sampler.
                Some(_) => {
                    let visible = self.file_samplers.iter().map(|(n, i)| (n, *i, line));
                    let locals = locals.iter().map(|(n, i, line)| (n, *i, *line));
                    for (name, index, line) in visible.chain(locals) {
                        params.push(Param {
                            ty: Ty::Sampler,
                            name: name.clone(),
                            space: None,
                            indirection: Indirection::Value,
                            attributes: vec![Attribute {
                                name: INLINE_SAMPLER.to_string(),
                                args: vec![Expr {
                                    kind: ExprKind::Int {
                                        value: index as u64,
                                        unsigned: true,
                                    },
                                    line,
                                }],
                                line,
                            }],
                            line,
                        });
                    }
                }
                // A helper would need the sampler passed in, which nothing
                // here does yet.
                None => {
                    if let Some((_, _, line)) = locals.first() {
                        return Err(syntax_error(
                            *line,
                            "a constexpr sampler outside a stage function",
                        ));
                    }
                }
            }
            self.program.functions.push(FunctionDef {
                stage,
                early_fragment_tests,
                ret: ty,
                name,
                params,
                body,
                line,
            });
            return Ok(());
        }
        if stage.is_some() {
            return Err(syntax_error(
                line,
                "a stage qualifier on something not a function",
            ));
        }
        if space != Some(Space::Constant) {
            return Err(syntax_error(
                line,
                "a file-scope variable outside the constant address space",
            ));
        }
        let ty = self.array_suffix(ty)?;
        self.expect("=")?;
        let init = self.initializer()?;
        self.expect(";")?;
        // `T name[] = {...}` takes its length from the initializer.
        let ty = match (ty, &init.kind) {
            (Ty::Array(element, 0), ExprKind::InitList(items)) => {
                Ty::Array(element, items.len() as u32)
            }
            (ty, _) => ty,
        };
        self.program.globals.push(Global {
            name,
            ty,
            init,
            line,
        });
        Ok(())
    }

    fn initializer(&mut self) -> Result<Expr> {
        if self.is_punct("{") {
            let line = self.line();
            self.at += 1;
            let mut items = Vec::new();
            while !self.eat("}") {
                items.push(self.initializer()?);
                if !self.eat(",") {
                    self.expect("}")?;
                    break;
                }
            }
            return Ok(Expr {
                kind: ExprKind::InitList(items),
                line,
            });
        }
        self.assignment()
    }

    /// `__attribute__((always_inline))`, an inlining hint, which changes
    /// nothing the function computes; any other GNU attribute is refused.
    fn inlining_attributes(&mut self) -> Result<()> {
        while self.eat_word("__attribute__") {
            self.expect("(")?;
            self.expect("(")?;
            loop {
                let line = self.line();
                let name = self.ident()?;
                if name != "always_inline" {
                    return Err(syntax_error(
                        line,
                        format!("unsupported __attribute__(({name}))"),
                    ));
                }
                if !self.eat(",") {
                    break;
                }
            }
            self.expect(")")?;
            self.expect(")")?;
        }
        Ok(())
    }

    /// Skip declaration qualifiers, answering the address space among them.
    fn skip_qualifiers(&mut self) -> Option<Space> {
        let mut space = None;
        loop {
            match self.peek() {
                Some(Kind::Ident(word)) => match word.as_str() {
                    "const" | "constexpr" | "static" | "inline" | "volatile" | "metal" => {}
                    "constant" => space = Some(Space::Constant),
                    "device" => space = Some(Space::Device),
                    "thread" => space = Some(Space::Thread),
                    "threadgroup" => space = Some(Space::Threadgroup),
                    _ => return space,
                },
                Some(Kind::Punct("::")) => {}
                _ => return space,
            }
            self.at += 1;
        }
    }

    fn array_suffix(&mut self, ty: Ty) -> Result<Ty> {
        let mut dimensions = Vec::new();
        while self.is_punct("[") && !matches!(self.peek_at(1), Some(Kind::Punct("["))) {
            self.at += 1;
            if self.eat("]") {
                dimensions.push(0);
                continue;
            }
            let count = match self.peek() {
                Some(Kind::Int { value, .. }) => *value,
                _ => return Err(self.error("an array length must be an integer literal")),
            };
            self.at += 1;
            self.expect("]")?;
            dimensions.push(u32::try_from(count).map_err(|_| self.error("array too long"))?);
        }
        Ok(dimensions
            .into_iter()
            .rev()
            .fold(ty, |ty, count| Ty::Array(Box::new(ty), count)))
    }

    /// Zero or more `[[ ... ]]` groups.
    fn attributes(&mut self) -> Result<Vec<Attribute>> {
        let mut attributes = Vec::new();
        while self.is_punct("[") && matches!(self.peek_at(1), Some(Kind::Punct("["))) {
            self.at += 2;
            loop {
                let line = self.line();
                let name = self.ident()?;
                let mut args = Vec::new();
                if self.eat("(") {
                    while !self.eat(")") {
                        args.push(self.assignment()?);
                        if !self.eat(",") {
                            self.expect(")")?;
                            break;
                        }
                    }
                }
                attributes.push(Attribute { name, args, line });
                if !self.eat(",") {
                    break;
                }
            }
            self.expect("]")?;
            self.expect("]")?;
        }
        Ok(attributes)
    }

    fn parameters(&mut self) -> Result<Vec<Param>> {
        self.expect("(")?;
        let mut params = Vec::new();
        if self.eat(")") {
            return Ok(params);
        }
        loop {
            let line = self.line();
            let space = self.skip_qualifiers();
            let ty = self.ty()?;
            // `const` may follow the type, before the indirection.
            self.skip_qualifiers();
            let indirection = if self.eat("&") {
                Indirection::Reference
            } else if self.eat("*") {
                Indirection::Pointer
            } else {
                Indirection::Value
            };
            self.skip_qualifiers();
            let name = self.ident()?;
            let ty = self.array_suffix(ty)?;
            let attributes = self.attributes()?;
            params.push(Param {
                ty,
                name,
                space,
                indirection,
                attributes,
                line,
            });
            if !self.eat(",") {
                break;
            }
        }
        self.expect(")")?;
        Ok(params)
    }

    /// Whether the tokens at the cursor begin a type.
    fn at_type(&self) -> bool {
        match self.peek() {
            Some(Kind::Ident(word)) => {
                scalar_named(word).is_some()
                    || vector_named(word).is_some()
                    || word
                        .strip_prefix("packed_")
                        .and_then(vector_named)
                        .is_some()
                    || matrix_named(word).is_some()
                    || texture_dimension(word).is_some()
                    || matches!(
                        word.as_str(),
                        "sampler" | "void" | "unsigned" | "vec" | "matrix"
                    )
                    || self.struct_named(word).is_some()
                    || self.aliases.contains_key(word)
                    || self.array_templates.contains(word)
                    || self.type_params.contains(word)
                    || standard_struct_fields(word, 0).is_some()
            }
            _ => false,
        }
    }

    fn struct_named(&self, name: &str) -> Option<usize> {
        self.program
            .structs
            .iter()
            .position(|structure| structure.name == name)
    }

    fn ty(&mut self) -> Result<Ty> {
        let line = self.line();
        let word = self.ident()?;
        if word == "void" {
            return Ok(Ty::Void);
        }
        if word == "unsigned" {
            // `unsigned int`, `unsigned short`, `unsigned char`, or `unsigned`.
            let bits = if self.eat_word("int") {
                32
            } else if self.eat_word("short") {
                16
            } else if self.eat_word("char") {
                8
            } else {
                32
            };
            return Ok(Ty::Scalar(Scalar::Int {
                signed: false,
                bits,
            }));
        }
        if let Some(scalar) = scalar_named(&word) {
            return Ok(Ty::Scalar(scalar));
        }
        if let Some((scalar, lanes)) = vector_named(&word) {
            return Ok(Ty::Vector(scalar, lanes));
        }
        if let Some(matrix) = matrix_named(&word) {
            return Ok(matrix);
        }
        if word == "matrix" {
            // `matrix<T, C, R>`.
            self.expect("<")?;
            let element = self.ty()?;
            let mut dimensions = [0u8; 2];
            for dimension in &mut dimensions {
                self.expect(",")?;
                *dimension = match self.peek() {
                    Some(Kind::Int { value, .. }) if (2..=4).contains(value) => *value as u8,
                    _ => return Err(self.error("a matrix<T, C, R> dimension must be 2, 3 or 4")),
                };
                self.at += 1;
            }
            self.expect(">")?;
            let Ty::Scalar(scalar @ Scalar::Float { .. }) = element else {
                return Err(syntax_error(
                    line,
                    "a matrix<T, C, R> element must be float or half",
                ));
            };
            let [columns, rows] = dimensions;
            return Ok(Ty::Matrix {
                scalar,
                columns,
                rows,
            });
        }
        if word == "sampler" {
            return Ok(Ty::Sampler);
        }
        if word == "vec" {
            self.expect("<")?;
            let element = self.ty()?;
            self.expect(",")?;
            let lanes = match self.peek() {
                Some(Kind::Int { value, .. }) if (2..=4).contains(value) => *value as u8,
                _ => return Err(self.error("a vec<T, N> lane count must be 2, 3 or 4")),
            };
            self.at += 1;
            self.expect(">")?;
            let Ty::Scalar(scalar) = element else {
                return Err(syntax_error(line, "a vec<T, N> element must be a scalar"));
            };
            return Ok(Ty::Vector(scalar, lanes));
        }
        if let Some((dimension, array, multisampled, depth)) = texture_dimension(&word) {
            return self.texture(line, dimension, array, multisampled, depth);
        }
        if let Some(index) = self.struct_named(&word) {
            return Ok(Ty::Struct(index));
        }
        if let Some(ty) = self.aliases.get(&word) {
            return Ok(ty.clone());
        }
        if self.type_params.contains(&word) {
            return Ok(Ty::Param(word));
        }
        if self.array_templates.contains(&word) {
            self.expect("<")?;
            let element = self.ty()?;
            self.expect(",")?;
            let count = match self.peek() {
                Some(Kind::Int { value, .. }) => {
                    u32::try_from(*value).map_err(|_| self.error("an array length out of range"))?
                }
                _ => return Err(self.error("an array template's length must be a literal")),
            };
            self.at += 1;
            self.expect(">")?;
            return Ok(Ty::Array(Box::new(element), count.max(1)));
        }
        if let Some((scalar, lanes)) = word.strip_prefix("packed_").and_then(vector_named) {
            return Ok(Ty::Packed(scalar, lanes));
        }
        if let Some(fields) = standard_struct_fields(&word, line) {
            let index = self.program.structs.len();
            self.program.structs.push(StructDef { name: word, fields });
            return Ok(Ty::Struct(index));
        }
        Err(syntax_error(line, format!("unknown type {word}")))
    }

    fn texture(
        &mut self,
        line: usize,
        dimension: TextureDimension,
        array: bool,
        multisampled: bool,
        depth: bool,
    ) -> Result<Ty> {
        self.expect("<")?;
        let element = self.ty()?;
        // A texture buffer is only ever read or written, and reads by default.
        let mut access = match dimension {
            TextureDimension::Buffer => Access::Read,
            _ => Access::Sample,
        };
        if self.eat(",") {
            self.eat_word("access");
            self.expect("::")?;
            access = match self.ident()?.as_str() {
                "sample" => Access::Sample,
                "read" => Access::Read,
                "write" => Access::Write,
                "read_write" => Access::ReadWrite,
                other => {
                    return Err(syntax_error(
                        line,
                        format!("unknown texture access {other}"),
                    ));
                }
            };
        }
        self.expect(">")?;
        // A `half` or `short` texture keeps its width here; the SPIR-V
        // emitter converts at the image access, since Vulkan's sampled type is
        // always 32-bit.
        let (texel, signed) = match element {
            Ty::Scalar(Scalar::Float {
                bits: bits @ (16 | 32),
            }) => (Type::Float { bits }, false),
            Ty::Scalar(Scalar::Int {
                signed,
                bits: bits @ (16 | 32),
            }) => (
                Type::Int {
                    bits,
                    signed: false,
                },
                signed,
            ),
            _ => return Err(syntax_error(line, "unsupported texture element type")),
        };
        if depth && element != Ty::Scalar(Scalar::FLOAT) {
            return Err(syntax_error(
                line,
                "a depth texture's element must be float",
            ));
        }
        Ok(Ty::Texture(TextureType {
            dimension,
            array,
            multisampled,
            depth,
            texel: Box::new(texel),
            signed,
            access,
        }))
    }

    fn block(&mut self) -> Result<Vec<Stmt>> {
        self.expect("{")?;
        let mut statements = Vec::new();
        while !self.eat("}") {
            if self.peek().is_none() {
                return Err(self.error("unterminated block"));
            }
            self.statement(&mut statements)?;
        }
        Ok(statements)
    }

    /// One statement, or a body: a block or a single statement.
    fn body(&mut self) -> Result<Vec<Stmt>> {
        if self.is_punct("{") {
            return self.block();
        }
        let mut statements = Vec::new();
        self.statement(&mut statements)?;
        Ok(statements)
    }

    fn statement(&mut self, out: &mut Vec<Stmt>) -> Result<()> {
        let line = self.line();
        if self.eat(";") {
            return Ok(());
        }
        if self.is_punct("{") {
            out.push(Stmt::Block(self.block()?));
            return Ok(());
        }
        if self.eat_word("if") {
            self.expect("(")?;
            let condition = self.expression()?;
            self.expect(")")?;
            let then = self.body()?;
            let otherwise = if self.eat_word("else") {
                Some(self.body()?)
            } else {
                None
            };
            out.push(Stmt::If {
                condition,
                then,
                otherwise,
            });
            return Ok(());
        }
        if self.eat_word("while") {
            self.expect("(")?;
            let condition = self.expression()?;
            self.expect(")")?;
            let body = self.body()?;
            out.push(Stmt::While { condition, body });
            return Ok(());
        }
        if self.eat_word("do") {
            let body = self.body()?;
            if !self.eat_word("while") {
                return Err(self.error("expected `while`"));
            }
            self.expect("(")?;
            let condition = self.expression()?;
            self.expect(")")?;
            self.expect(";")?;
            out.push(Stmt::DoWhile { body, condition });
            return Ok(());
        }
        if self.eat_word("for") {
            self.expect("(")?;
            let mut init = Vec::new();
            if !self.eat(";") {
                self.simple_statement(&mut init)?;
                self.expect(";")?;
            }
            let condition = if self.is_punct(";") {
                None
            } else {
                Some(self.expression()?)
            };
            self.expect(";")?;
            let step = if self.is_punct(")") {
                None
            } else {
                Some(self.expression()?)
            };
            self.expect(")")?;
            let body = self.body()?;
            out.push(Stmt::For {
                init,
                condition,
                step,
                body,
            });
            return Ok(());
        }
        if self.eat_word("break") {
            self.expect(";")?;
            out.push(Stmt::Break(line));
            return Ok(());
        }
        if self.eat_word("continue") {
            self.expect(";")?;
            out.push(Stmt::Continue(line));
            return Ok(());
        }
        if self.eat_word("return") {
            let value = if self.is_punct(";") {
                None
            } else {
                Some(self.expression()?)
            };
            self.expect(";")?;
            out.push(Stmt::Return(value, line));
            return Ok(());
        }
        if self.eat_word("switch") {
            self.expect("(")?;
            let selector = self.expression()?;
            self.expect(")")?;
            let cases = self.cases()?;
            out.push(Stmt::Switch {
                selector,
                cases,
                line,
            });
            return Ok(());
        }
        if self.is_word("goto") {
            return Err(self.error("unsupported statement"));
        }
        self.simple_statement(out)?;
        self.expect(";")
    }

    /// A `switch` body: `{`, then statements, each case begun by its label.
    fn cases(&mut self) -> Result<Vec<Case>> {
        self.expect("{")?;
        let mut cases: Vec<Case> = Vec::new();
        while !self.eat("}") {
            if self.eat_word("case") {
                let label = self.ternary()?;
                self.expect(":")?;
                cases.push(Case {
                    label: Some(label),
                    body: Vec::new(),
                });
            } else if self.eat_word("default") {
                self.expect(":")?;
                cases.push(Case {
                    label: None,
                    body: Vec::new(),
                });
            } else {
                if self.peek().is_none() {
                    return Err(self.error("unterminated switch"));
                }
                let Some(case) = cases.last_mut() else {
                    return Err(self.error("a statement before a switch's first label"));
                };
                self.statement(&mut case.body)?;
            }
        }
        Ok(cases)
    }

    /// A declaration or an expression, without its `;`.
    fn simple_statement(&mut self, out: &mut Vec<Stmt>) -> Result<()> {
        let line = self.line();
        let start = self.at;
        let space = self.skip_qualifiers();
        if self.is_word("auto") && matches!(self.peek_at(1), Some(Kind::Punct("&"))) {
            self.at += 2;
            return self.reference(space, None, out);
        }
        // `auto x = e`: a local of the initializer's type.
        if self.is_word("auto") && matches!(self.peek_at(1), Some(Kind::Ident(_))) {
            self.at += 1;
            if space.is_some_and(|space| space != Space::Thread) {
                return Err(syntax_error(line, "a local in a non-thread address space"));
            }
            let name = self.ident()?;
            self.expect("=")?;
            let init = self.assignment()?;
            out.push(Stmt::Auto { name, init, line });
            return Ok(());
        }
        if self.at_type() {
            let mark = self.at;
            let ty = self.ty()?;
            if ty == Ty::Sampler
                && matches!(self.peek(), Some(Kind::Ident(_)))
                && matches!(self.peek_at(1), Some(Kind::Punct("(")))
            {
                let name = self.ident()?;
                let sampler = self.inline_sampler()?;
                self.local_samplers.push((name, sampler, line));
                return Ok(());
            }
            self.skip_qualifiers();
            if self.eat("&") {
                return self.reference(space, Some(ty), out);
            }
            if matches!(self.peek(), Some(Kind::Ident(_))) {
                if space == Some(Space::Threadgroup) {
                    loop {
                        let line = self.line();
                        let name = self.ident()?;
                        let ty = self.array_suffix(ty.clone())?;
                        if self.is_punct("=") || self.is_punct("(") {
                            return Err(syntax_error(line, "an initialized threadgroup variable"));
                        }
                        out.push(Stmt::Threadgroup { ty, name, line });
                        if !self.eat(",") {
                            return Ok(());
                        }
                    }
                }
                if space == Some(Space::Device) {
                    return Err(syntax_error(line, "a local in a non-thread address space"));
                }
                loop {
                    let line = self.line();
                    let name = self.ident()?;
                    let ty = self.array_suffix(ty.clone())?;
                    let init = if self.eat("=") {
                        Some(self.initializer()?)
                    } else if self.is_punct("(") {
                        // `float4 x(a, b)`: direct initialization.
                        let args = self.arguments()?;
                        Some(Expr {
                            kind: ExprKind::Construct(ty.clone(), args),
                            line,
                        })
                    } else {
                        None
                    };
                    out.push(Stmt::Decl {
                        ty,
                        name,
                        init,
                        line,
                    });
                    if !self.eat(",") {
                        return Ok(());
                    }
                }
            }
            self.at = mark;
        }
        self.at = start;
        out.push(Stmt::Expr(self.expression()?));
        Ok(())
    }

    /// The rest of `SPACE T& name = init`, after the `&`.
    fn reference(
        &mut self,
        space: Option<Space>,
        ty: Option<Ty>,
        out: &mut Vec<Stmt>,
    ) -> Result<()> {
        let line = self.line();
        let name = self.ident()?;
        self.expect("=")?;
        let init = self.assignment()?;
        out.push(Stmt::Reference {
            space,
            ty,
            name,
            init,
            line,
        });
        Ok(())
    }

    /// The `(...)` of a `constexpr sampler`, recorded in the program; answers
    /// its index there.
    fn inline_sampler(&mut self) -> Result<usize> {
        let mut sampler = crate::ir::InlineSampler::default();
        self.expect("(")?;
        while !self.eat(")") {
            let line = self.line();
            let key = self.ident()?;
            if self.eat("::") {
                let value = self.ident()?;
                inline_sampler_value(&mut sampler, &key, &value).ok_or_else(|| {
                    syntax_error(
                        line,
                        format!("unsupported constexpr sampler argument {key}::{value}"),
                    )
                })?;
            } else {
                let mut numbers = Vec::new();
                self.expect("(")?;
                while !self.eat(")") {
                    numbers.push(match self.peek() {
                        Some(Kind::Int { value, .. }) => *value as f64,
                        Some(Kind::Float { value, .. }) => *value,
                        _ => return Err(self.error("a constexpr sampler argument must be a literal")),
                    });
                    self.at += 1;
                    if !self.eat(",") {
                        self.expect(")")?;
                        break;
                    }
                }
                match (key.as_str(), numbers.as_slice()) {
                    ("max_anisotropy", [n]) if (1.0..=16.0).contains(n) => {
                        sampler.max_anisotropy = *n as u8;
                    }
                    ("lod_clamp", [min, max]) => {
                        sampler.lod_min_clamp = (*min as f32).to_bits();
                        sampler.lod_max_clamp = (*max as f32).to_bits();
                    }
                    _ => {
                        return Err(syntax_error(
                            line,
                            format!("unsupported constexpr sampler argument {key}(...)"),
                        ));
                    }
                }
            }
            if !self.eat(",") {
                self.expect(")")?;
                break;
            }
        }
        let line = self.line();
        if !sampler.normalized_coordinates {
            // What Vulkan allows unnormalized coordinates with, which is also
            // what Metal documents for `coord::pixel`.
            let clamped = sampler.address.iter().all(|&a| a == 0 || a == 5);
            if !clamped
                || sampler.mip_filter != 0
                || sampler.min_filter != sampler.mag_filter
                || sampler.max_anisotropy != 1
                || sampler.compare_function != 0
            {
                return Err(syntax_error(
                    line,
                    "a coord::pixel sampler with a mode Vulkan's unnormalized coordinates lack",
                ));
            }
        }
        self.program.inline_samplers.push(sampler);
        Ok(self.program.inline_samplers.len() - 1)
    }

    fn arguments(&mut self) -> Result<Vec<Expr>> {
        self.expect("(")?;
        let mut args = Vec::new();
        if self.eat(")") {
            return Ok(args);
        }
        loop {
            args.push(self.assignment()?);
            if !self.eat(",") {
                break;
            }
        }
        self.expect(")")?;
        Ok(args)
    }

    fn expression(&mut self) -> Result<Expr> {
        let first = self.assignment()?;
        if self.is_punct(",") {
            return Err(self.error("the comma operator is not supported"));
        }
        Ok(first)
    }

    fn assignment(&mut self) -> Result<Expr> {
        let line = self.line();
        let lhs = self.ternary()?;
        let op = match self.peek() {
            Some(Kind::Punct("=")) => None,
            Some(Kind::Punct(op))
                if op.len() >= 2
                    && op.ends_with('=')
                    && !matches!(*op, "==" | "!=" | "<=" | ">=") =>
            {
                Some(&op[..op.len() - 1])
            }
            _ => return Ok(lhs),
        };
        self.at += 1;
        let op = op.map(static_op);
        let rhs = self.assignment()?;
        Ok(Expr {
            kind: ExprKind::Assign(op, Box::new(lhs), Box::new(rhs)),
            line,
        })
    }

    fn ternary(&mut self) -> Result<Expr> {
        let line = self.line();
        let condition = self.binary(0)?;
        if !self.eat("?") {
            return Ok(condition);
        }
        let yes = self.assignment()?;
        self.expect(":")?;
        let no = self.assignment()?;
        Ok(Expr {
            kind: ExprKind::Ternary(Box::new(condition), Box::new(yes), Box::new(no)),
            line,
        })
    }

    fn binary(&mut self, minimum: u8) -> Result<Expr> {
        let mut lhs = self.unary()?;
        loop {
            let Some(Kind::Punct(op)) = self.peek() else {
                return Ok(lhs);
            };
            let op = *op;
            let precedence = match op {
                "||" => 1,
                "&&" => 2,
                "|" => 3,
                "^" => 4,
                "&" => 5,
                "==" | "!=" => 6,
                "<" | ">" | "<=" | ">=" => 7,
                "<<" | ">>" => 8,
                "+" | "-" => 9,
                "*" | "/" | "%" => 10,
                _ => return Ok(lhs),
            };
            if precedence < minimum {
                return Ok(lhs);
            }
            let line = self.line();
            self.at += 1;
            let rhs = self.binary(precedence + 1)?;
            lhs = Expr {
                kind: ExprKind::Binary(op, Box::new(lhs), Box::new(rhs)),
                line,
            };
        }
    }

    fn unary(&mut self) -> Result<Expr> {
        let line = self.line();
        for op in ["-", "+", "!", "~", "++", "--", "&", "*"] {
            if self.eat(op) {
                let operand = self.unary()?;
                return Ok(Expr {
                    kind: ExprKind::Unary(static_op(op), Box::new(operand)),
                    line,
                });
            }
        }
        if self.is_punct("(") {
            // A cast or a compound literal: `(` type `)`.
            let start = self.at;
            self.at += 1;
            let space = self.skip_qualifiers();
            if self.at_type() {
                let ty = self.ty()?;
                self.skip_qualifiers();
                if self.eat("*") {
                    self.expect(")")?;
                    let operand = self.unary()?;
                    return Ok(Expr {
                        kind: ExprKind::PointerCast(space, ty, Box::new(operand)),
                        line,
                    });
                }
                if self.eat(")") {
                    if self.is_punct("{") {
                        return self.compound_literal(ty, line);
                    }
                    let operand = self.unary()?;
                    return Ok(Expr {
                        kind: ExprKind::Cast(ty, Box::new(operand)),
                        line,
                    });
                }
            }
            self.at = start;
        }
        self.postfix()
    }

    fn compound_literal(&mut self, ty: Ty, line: usize) -> Result<Expr> {
        self.expect("{")?;
        let mut items = Vec::new();
        while !self.eat("}") {
            let name = if self.eat(".") {
                let name = self.ident()?;
                self.expect("=")?;
                Some(name)
            } else {
                None
            };
            items.push((name, self.initializer()?));
            if !self.eat(",") {
                self.expect("}")?;
                break;
            }
        }
        Ok(Expr {
            kind: ExprKind::CompoundLiteral(ty, items),
            line,
        })
    }

    fn postfix(&mut self) -> Result<Expr> {
        let mut expr = self.primary()?;
        loop {
            let line = self.line();
            if self.eat(".") {
                let name = self.ident()?;
                if self.is_punct("(") {
                    let args = self.arguments()?;
                    expr = Expr {
                        kind: ExprKind::Method {
                            object: Box::new(expr),
                            name,
                            args,
                        },
                        line,
                    };
                } else {
                    expr = Expr {
                        kind: ExprKind::Member(Box::new(expr), name),
                        line,
                    };
                }
            } else if self.is_punct("->") {
                return Err(self.error("`->` is not supported"));
            } else if self.is_punct("[") && !matches!(self.peek_at(1), Some(Kind::Punct("["))) {
                self.at += 1;
                let index = self.expression()?;
                self.expect("]")?;
                expr = Expr {
                    kind: ExprKind::Index(Box::new(expr), Box::new(index)),
                    line,
                };
            } else if self.is_punct("++") || self.is_punct("--") {
                let op = if self.eat("++") {
                    "++"
                } else {
                    self.at += 1;
                    "--"
                };
                expr = Expr {
                    kind: ExprKind::Postfix(op, Box::new(expr)),
                    line,
                };
            } else {
                return Ok(expr);
            }
        }
    }

    fn primary(&mut self) -> Result<Expr> {
        let line = self.line();
        match self.peek().cloned() {
            Some(Kind::Int { value, unsigned }) => {
                self.at += 1;
                Ok(Expr {
                    kind: ExprKind::Int { value, unsigned },
                    line,
                })
            }
            Some(Kind::Float { value, half }) => {
                self.at += 1;
                Ok(Expr {
                    kind: ExprKind::Float { value, half },
                    line,
                })
            }
            Some(Kind::Punct("(")) => {
                self.at += 1;
                let inner = self.expression()?;
                self.expect(")")?;
                Ok(inner)
            }
            Some(Kind::Ident(word)) => {
                if word == "true" || word == "false" {
                    self.at += 1;
                    return Ok(Expr {
                        kind: ExprKind::Bool(word == "true"),
                        line,
                    });
                }

                if self.at_type() && matches!(self.peek_at(1), Some(Kind::Punct("(" | "<"))) {
                    let ty = self.ty()?;
                    // `Array({ a, b })`: an array built from an initializer list.
                    if matches!(ty, Ty::Array(..))
                        && self.is_punct("(")
                        && matches!(self.peek_at(1), Some(Kind::Punct("{")))
                    {
                        self.expect("(")?;
                        let items = self.initializer()?;
                        self.expect(")")?;
                        return Ok(items);
                    }
                    let args = self.arguments()?;
                    return Ok(Expr {
                        kind: ExprKind::Construct(ty, args),
                        line,
                    });
                }
                self.at += 1;
                // `metal::` and `access::` qualify names this frontend knows
                // unqualified.
                if self.eat("::") {
                    if word == "component" {
                        return Ok(Expr {
                            kind: ExprKind::Ident(format!("component::{}", self.ident()?)),
                            line,
                        });
                    }
                    return self.primary();
                }
                if word == "reinterpret_cast" && self.is_punct("<") {
                    let mark = self.at;
                    self.at += 1;
                    self.skip_qualifiers();
                    let scalar = match self.peek() {
                        Some(Kind::Ident(name)) if name == "atomic_uint" => Some(Scalar::UINT),
                        Some(Kind::Ident(name)) if name == "atomic_int" => Some(Scalar::INT),
                        _ => None,
                    };
                    if let Some(scalar) = scalar {
                        self.at += 1;
                        self.expect("*")?;
                        self.expect(">")?;
                        self.expect("(")?;
                        let pointer = self.expression()?;
                        self.expect(")")?;
                        return Ok(Expr {
                            kind: ExprKind::AtomicCast(scalar, Box::new(pointer)),
                            line,
                        });
                    }
                    let space = self.skip_qualifiers();
                    if self.at_type() {
                        let ty = self.ty()?;
                        self.skip_qualifiers();
                        if self.eat("*") {
                            self.expect(">")?;
                            self.expect("(")?;
                            let pointer = self.expression()?;
                            self.expect(")")?;
                            return Ok(Expr {
                                kind: ExprKind::PointerCast(space, ty, Box::new(pointer)),
                                line,
                            });
                        }
                    }
                    self.at = mark;
                }
                if self.is_punct("<")
                    && matches!(
                        word.as_str(),
                        "as_type" | "static_cast" | "reinterpret_cast"
                    )
                {
                    self.at += 1;
                    let template = self.ty()?;
                    self.expect(">")?;
                    let args = self.arguments()?;
                    return Ok(Expr {
                        kind: ExprKind::Call {
                            name: word,
                            template: Some(template),
                            args,
                        },
                        line,
                    });
                }
                if self.is_punct("(") {
                    let args = self.arguments()?;
                    return Ok(Expr {
                        kind: ExprKind::Call {
                            name: word,
                            template: None,
                            args,
                        },
                        line,
                    });
                }
                Ok(Expr {
                    kind: ExprKind::Ident(word),
                    line,
                })
            }
            _ => Err(self.error("expected an expression")),
        }
    }
}

fn static_op(op: &str) -> &'static str {
    const OPS: &[&str] = &[
        "+", "-", "*", "/", "%", "&", "|", "^", "<<", ">>", "!", "~", "++", "--",
    ];
    OPS.iter()
        .copied()
        .find(|known| *known == op)
        .expect("an operator the lexer produced")
}

fn scalar_named(word: &str) -> Option<Scalar> {
    Some(match word {
        "bool" => Scalar::Bool,
        "char" | "int8_t" => Scalar::Int {
            signed: true,
            bits: 8,
        },
        "uchar" | "uint8_t" => Scalar::Int {
            signed: false,
            bits: 8,
        },
        "short" | "int16_t" => Scalar::Int {
            signed: true,
            bits: 16,
        },
        "ushort" | "uint16_t" => Scalar::Int {
            signed: false,
            bits: 16,
        },
        "int" | "int32_t" => Scalar::INT,
        "uint" | "uint32_t" => Scalar::UINT,
        "half" => Scalar::Float { bits: 16 },
        "float" => Scalar::FLOAT,
        _ => return None,
    })
}

fn vector_named(word: &str) -> Option<(Scalar, u8)> {
    let lanes = match word.as_bytes().last()? {
        b'2' => 2,
        b'3' => 3,
        b'4' => 4,
        _ => return None,
    };
    Some((scalar_named(&word[..word.len() - 1])?, lanes))
}

/// `floatCxR` and `halfCxR`: C columns of R rows, each two to four.
fn matrix_named(word: &str) -> Option<Ty> {
    let (base, shape) = word.split_at(word.len().checked_sub(3)?);
    let scalar = match base {
        "float" => Scalar::FLOAT,
        "half" => Scalar::Float { bits: 16 },
        _ => return None,
    };
    let dimension = |byte: u8| (b'2'..=b'4').contains(&byte).then(|| byte - b'0');
    let [columns, b'x', rows] = shape.as_bytes() else {
        return None;
    };
    Some(Ty::Matrix {
        scalar,
        columns: dimension(*columns)?,
        rows: dimension(*rows)?,
    })
}

/// The shape a texture type name spells: dimension, array, multisampled,
/// depth.
fn texture_dimension(word: &str) -> Option<(TextureDimension, bool, bool, bool)> {
    let (depth, body) = if let Some(body) = word.strip_prefix("texture") {
        (false, body)
    } else {
        let body = word.strip_prefix("depth")?;
        (true, body)
    };
    let (body, array) = match body.strip_suffix("_array") {
        Some(body) => (body, true),
        None => (body, false),
    };
    let (body, multisampled) = match body.strip_suffix("_ms") {
        Some(body) => (body, true),
        None => (body, false),
    };
    let dimension = match body {
        "1d" => TextureDimension::D1,
        "2d" => TextureDimension::D2,
        "3d" => TextureDimension::D3,
        "cube" => TextureDimension::Cube,
        "_buffer" if !depth && !array && !multisampled => TextureDimension::Buffer,
        _ => return None,
    };
    Some((dimension, array, multisampled, depth))
}

/// Apply one `key::value` argument of a `constexpr sampler`, in the values of
/// the `MTLSampler*` enumerations. `None` for anything not understood.
fn inline_sampler_value(
    sampler: &mut crate::ir::InlineSampler,
    key: &str,
    value: &str,
) -> Option<()> {
    let filter = || match value {
        "nearest" => Some(0),
        "linear" => Some(1),
        _ => None,
    };
    let address = || match value {
        "clamp_to_edge" => Some(0),
        "repeat" => Some(2),
        "mirrored_repeat" => Some(3),
        "clamp_to_zero" => Some(4),
        "clamp_to_border" => Some(5),
        _ => None,
    };
    match key {
        "coord" => {
            sampler.normalized_coordinates = match value {
                "normalized" => true,
                "pixel" => false,
                _ => return None,
            }
        }
        "filter" => {
            sampler.min_filter = filter()?;
            sampler.mag_filter = sampler.min_filter;
        }
        "min_filter" => sampler.min_filter = filter()?,
        "mag_filter" => sampler.mag_filter = filter()?,
        "mip_filter" => {
            sampler.mip_filter = match value {
                "none" => 0,
                "nearest" => 1,
                "linear" => 2,
                _ => return None,
            }
        }
        "address" => sampler.address = [address()?; 3],
        "s_address" => sampler.address[0] = address()?,
        "t_address" => sampler.address[1] = address()?,
        "r_address" => sampler.address[2] = address()?,
        "border_color" => {
            sampler.border_color = match value {
                "transparent_black" => 0,
                "opaque_black" => 1,
                "opaque_white" => 2,
                _ => return None,
            }
        }
        // `never` is left out: the runtime reads 0 as no comparison, and a
        // comparison that always fails is not that.
        "compare_func" => {
            sampler.compare_function = match value {
                "none" => 0,
                "less" => 1,
                "equal" => 2,
                "less_equal" => 3,
                "greater" => 4,
                "not_equal" => 5,
                "greater_equal" => 6,
                "always" => 7,
                _ => return None,
            }
        }
        _ => return None,
    }
    Some(())
}

/// The indirect argument layouts `<metal_stdlib>` declares, which a library
/// uses without declaring them. Each is defined where it is first named, so a
/// library which declares its own keeps it.
fn standard_struct_fields(name: &str, line: usize) -> Option<Vec<Field>> {
    let uint = || {
        Ty::Scalar(Scalar::Int {
            signed: false,
            bits: 32,
        })
    };
    let int = Ty::Scalar(Scalar::Int {
        signed: true,
        bits: 32,
    });
    let members: Vec<(&str, Ty)> = match name {
        "MTLDrawPrimitivesIndirectArguments" => vec![
            ("vertexCount", uint()),
            ("instanceCount", uint()),
            ("vertexStart", uint()),
            ("baseInstance", uint()),
        ],
        "MTLDrawIndexedPrimitivesIndirectArguments" => vec![
            ("indexCount", uint()),
            ("instanceCount", uint()),
            ("indexStart", uint()),
            ("baseVertex", int),
            ("baseInstance", uint()),
        ],
        "MTLDrawPatchIndirectArguments" => vec![
            ("patchCount", uint()),
            ("instanceCount", uint()),
            ("patchStart", uint()),
            ("baseInstance", uint()),
        ],
        "MTLDispatchThreadgroupsIndirectArguments" => {
            vec![("threadgroupsPerGrid", Ty::Array(Box::new(uint()), 3))]
        }
        _ => return None,
    };
    Some(
        members
            .into_iter()
            .map(|(name, ty)| Field {
                name: name.to_string(),
                ty,
                attributes: Vec::new(),
                line,
            })
            .collect(),
    )
}
