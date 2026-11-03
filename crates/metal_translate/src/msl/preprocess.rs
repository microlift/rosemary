//! The C preprocessor, as much of it as Metal source uses: object and
//! function-like macros, conditional compilation, and `#include` of Metal's
//! own headers, which declare nothing this frontend needs from a file.
//!
//! Output keeps every line where it was, so a diagnostic names the line of the
//! source the guest handed over.

use std::collections::{HashMap, HashSet};

use super::syntax_error;
use crate::TranslateError;

#[derive(Clone, Debug)]
struct Macro {
    /// `None` for an object-like macro.
    parameters: Option<Vec<String>>,
    body: String,
}

/// Macros Metal's compiler defines for every library this frontend compiles.
///
/// `__HAVE_FMA__` says `fma` is a fused operation on the device, which it is
/// on every Vulkan adapter this runtime accepts.
///
/// The limits and constants after them are `<metal_stdlib>`'s, which defines
/// them as macros; MojoShader's output uses `FLT_MAX` for a guarded `rsqrt`.
const PREDEFINED: &[(&str, &str)] = &[
    ("__METAL_VERSION__", "220"),
    ("__METAL_MACOS__", "1"),
    ("__HAVE_FMA__", "1"),
    ("FLT_MAX", "3.40282347e+38f"),
    ("FLT_MIN", "1.17549435e-38f"),
    ("FLT_EPSILON", "1.19209290e-07f"),
    ("HALF_MAX", "65504.0h"),
    ("HALF_MIN", "6.10351562e-05h"),
    ("HALF_EPSILON", "9.765625e-04h"),
    ("INT_MAX", "2147483647"),
    ("UINT_MAX", "4294967295u"),
    ("M_PI_F", "3.14159265f"),
    ("M_PI_2_F", "1.57079633f"),
    ("M_PI_4_F", "0.785398163f"),
    ("M_1_PI_F", "0.318309886f"),
    ("M_2_PI_F", "0.636619772f"),
    ("M_E_F", "2.71828183f"),
    ("M_LN2_F", "0.693147181f"),
    ("M_LN10_F", "2.30258509f"),
    ("M_SQRT2_F", "1.41421356f"),
    ("M_SQRT1_2_F", "0.707106781f"),
];

/// Run the preprocessor over `source`, with `macros` defined first, as
/// `MTLCompileOptions.preprocessorMacros` defines them.
pub(super) fn preprocess(
    source: &str,
    macros: &[(String, String)],
) -> Result<String, TranslateError> {
    let mut table: HashMap<String, Macro> = HashMap::new();
    for (name, body) in PREDEFINED {
        table.insert(
            (*name).to_string(),
            Macro {
                parameters: None,
                body: (*body).to_string(),
            },
        );
    }
    for (name, body) in macros {
        table.insert(
            name.clone(),
            Macro {
                parameters: None,
                body: body.clone(),
            },
        );
    }
    let text = strip_comments(source);
    let lines = join_continuations(&text);
    // Each entry: whether this region is taken, whether any branch of the
    // conditional has been taken, and whether the enclosing region is.
    let mut conditions: Vec<(bool, bool, bool)> = Vec::new();
    let mut output = String::with_capacity(source.len());
    for (number, (line, extra)) in lines.iter().enumerate() {
        let line_number = number + 1;
        let active = conditions.last().is_none_or(|top| top.0);
        let trimmed = line.trim_start();
        if let Some(directive) = trimmed.strip_prefix('#') {
            let directive = directive.trim_start();
            let (name, rest) = split_word(directive);
            match name {
                "if" | "ifdef" | "ifndef" => {
                    let taken = active
                        && match name {
                            "if" => evaluate(rest, &table, line_number)? != 0,
                            "ifdef" => table.contains_key(split_word(rest).0),
                            _ => !table.contains_key(split_word(rest).0),
                        };
                    conditions.push((taken, taken, active));
                }
                "elif" => {
                    let Some(top) = conditions.last_mut() else {
                        return Err(syntax_error(line_number, "#elif without #if"));
                    };
                    let taken = top.2 && !top.1 && evaluate(rest, &table, line_number)? != 0;
                    top.0 = taken;
                    top.1 |= taken;
                }
                "else" => {
                    let Some(top) = conditions.last_mut() else {
                        return Err(syntax_error(line_number, "#else without #if"));
                    };
                    top.0 = top.2 && !top.1;
                    top.1 = true;
                }
                "endif" => {
                    if conditions.pop().is_none() {
                        return Err(syntax_error(line_number, "#endif without #if"));
                    }
                }
                _ if !active => {}
                "define" => define(rest, &mut table, line_number)?,
                "undef" => {
                    table.remove(split_word(rest).0);
                }
                // Metal's own headers declare the language this frontend
                // implements natively; a guest's own header is not available.
                // `<simd/simd.h>` only adds `simd_` and `vector_` names for
                // vector types, which are rejected as unknown if used.
                "include" => {
                    let target = rest.trim();
                    if !(target.starts_with("<metal")
                        || target.starts_with("\"metal")
                        || target == "<simd/simd.h>")
                    {
                        return Err(syntax_error(
                            line_number,
                            format!("#include {target} names a file which is not available"),
                        ));
                    }
                }
                "pragma" | "line" | "" => {}
                "error" => {
                    return Err(syntax_error(line_number, format!("#error {}", rest.trim())));
                }
                other => {
                    return Err(syntax_error(
                        line_number,
                        format!("unsupported preprocessor directive #{other}"),
                    ));
                }
            }
        } else if active {
            output.push_str(&expand(line, &table, &HashSet::new(), line_number)?);
        }
        output.push('\n');
        // A line joined with a continuation still accounts for its lines.
        for _ in 0..*extra {
            output.push('\n');
        }
    }
    if !conditions.is_empty() {
        return Err(syntax_error(lines.len(), "#if without #endif"));
    }
    Ok(output)
}

/// Replace every comment with a space, keeping newlines where they were.
fn strip_comments(source: &str) -> String {
    let bytes = source.as_bytes();
    let mut out = String::with_capacity(source.len());
    let mut at = 0;
    while at < bytes.len() {
        match bytes[at] {
            b'/' if bytes.get(at + 1) == Some(&b'/') => {
                while at < bytes.len() && bytes[at] != b'\n' {
                    at += 1;
                }
                out.push(' ');
            }
            b'/' if bytes.get(at + 1) == Some(&b'*') => {
                at += 2;
                out.push(' ');
                while at < bytes.len() && !(bytes[at] == b'*' && bytes.get(at + 1) == Some(&b'/')) {
                    if bytes[at] == b'\n' {
                        out.push('\n');
                    }
                    at += 1;
                }
                at += 2;
            }
            b'"' => {
                let start = at;
                at += 1;
                while at < bytes.len() && bytes[at] != b'"' && bytes[at] != b'\n' {
                    if bytes[at] == b'\\' {
                        at += 1;
                    }
                    at += 1;
                }
                at = (at + 1).min(bytes.len());
                out.push_str(&source[start..at]);
            }
            _ => {
                let start = at;
                let width = source[at..].chars().next().map_or(1, char::len_utf8);
                at += width;
                out.push_str(&source[start..at]);
            }
        }
    }
    out
}

/// Logical lines, with how many physical lines each continuation swallowed.
fn join_continuations(text: &str) -> Vec<(String, usize)> {
    let mut lines = Vec::new();
    let mut pending = String::new();
    let mut joined = 0;
    for line in text.split('\n') {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if let Some(head) = line.strip_suffix('\\') {
            pending.push_str(head);
            pending.push(' ');
            joined += 1;
            continue;
        }
        pending.push_str(line);
        lines.push((std::mem::take(&mut pending), joined));
        joined = 0;
    }
    if !pending.is_empty() {
        lines.push((pending, joined));
    }
    lines
}

fn split_word(text: &str) -> (&str, &str) {
    let text = text.trim_start();
    let end = text
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .unwrap_or(text.len());
    (&text[..end], &text[end..])
}

fn define(
    rest: &str,
    table: &mut HashMap<String, Macro>,
    line: usize,
) -> Result<(), TranslateError> {
    let (name, after) = split_word(rest);
    if name.is_empty() {
        return Err(syntax_error(line, "#define has no name"));
    }
    // A function-like macro's parenthesis follows its name immediately.
    let (parameters, body) = if let Some(list) = after.strip_prefix('(') {
        let close = list
            .find(')')
            .ok_or_else(|| syntax_error(line, "#define has an unterminated parameter list"))?;
        let parameters = list[..close]
            .split(',')
            .map(|parameter| parameter.trim().to_string())
            .filter(|parameter| !parameter.is_empty())
            .collect::<Vec<_>>();
        (Some(parameters), &list[close + 1..])
    } else {
        (None, after)
    };
    if body.contains('#') {
        return Err(syntax_error(
            line,
            "a macro using # or ## is not supported by this preprocessor",
        ));
    }
    table.insert(
        name.to_string(),
        Macro {
            parameters,
            body: body.trim().to_string(),
        },
    );
    Ok(())
}

/// Expand every macro in `text`, never expanding one inside its own
/// expansion.
fn expand(
    text: &str,
    table: &HashMap<String, Macro>,
    disabled: &HashSet<String>,
    line: usize,
) -> Result<String, TranslateError> {
    if disabled.len() > 64 {
        return Err(syntax_error(line, "macro expansion nests too deeply"));
    }
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut at = 0;
    while at < bytes.len() {
        let c = bytes[at];
        if c.is_ascii_digit() {
            // A number's suffix is not an identifier: `1u` stays whole.
            let start = at;
            while at < bytes.len()
                && (bytes[at].is_ascii_alphanumeric() || bytes[at] == b'.' || bytes[at] == b'_')
            {
                // An exponent's sign belongs to the number.
                if (bytes[at] == b'e' || bytes[at] == b'E')
                    && matches!(bytes.get(at + 1), Some(b'+' | b'-'))
                    && !text[start..at].starts_with("0x")
                {
                    at += 1;
                }
                at += 1;
            }
            out.push_str(&text[start..at]);
            continue;
        }
        if !(c.is_ascii_alphabetic() || c == b'_') {
            let width = text[at..].chars().next().map_or(1, char::len_utf8);
            out.push_str(&text[at..at + width]);
            at += width;
            continue;
        }
        let start = at;
        while at < bytes.len() && (bytes[at].is_ascii_alphanumeric() || bytes[at] == b'_') {
            at += 1;
        }
        let name = &text[start..at];
        let Some(definition) = table.get(name).filter(|_| !disabled.contains(name)) else {
            out.push_str(name);
            continue;
        };
        let mut inner = disabled.clone();
        inner.insert(name.to_string());
        match &definition.parameters {
            None => out.push_str(&expand(&definition.body, table, &inner, line)?),
            Some(parameters) => {
                let after = &text[at..];
                let skipped = after.len() - after.trim_start().len();
                if !after.trim_start().starts_with('(') {
                    // A function-like macro's name without arguments is just
                    // the name.
                    out.push_str(name);
                    continue;
                }
                let open = at + skipped;
                let (arguments, end) = macro_arguments(text, open).ok_or_else(|| {
                    syntax_error(line, format!("unterminated arguments to {name}"))
                })?;
                let arguments = if parameters.is_empty()
                    && arguments.len() == 1
                    && arguments[0].trim().is_empty()
                {
                    Vec::new()
                } else {
                    arguments
                };
                if arguments.len() != parameters.len() {
                    return Err(syntax_error(
                        line,
                        format!(
                            "macro {name} takes {} argument(s), given {}",
                            parameters.len(),
                            arguments.len()
                        ),
                    ));
                }
                let expanded = arguments
                    .iter()
                    .map(|argument| expand(argument, table, disabled, line))
                    .collect::<Result<Vec<_>, _>>()?;
                let substituted = substitute(&definition.body, parameters, &expanded);
                out.push_str(&expand(&substituted, table, &inner, line)?);
                at = end;
            }
        }
    }
    Ok(out)
}

/// The comma-separated arguments of a macro call whose `(` is at `open`, and
/// the offset just past its `)`.
fn macro_arguments(text: &str, open: usize) -> Option<(Vec<String>, usize)> {
    let mut depth = 0usize;
    let mut arguments = Vec::new();
    let mut current = String::new();
    for (offset, c) in text[open..].char_indices() {
        match c {
            '(' => {
                depth += 1;
                if depth > 1 {
                    current.push(c);
                }
            }
            ')' => {
                depth -= 1;
                if depth == 0 {
                    arguments.push(current);
                    return Some((arguments, open + offset + 1));
                }
                current.push(c);
            }
            ',' if depth == 1 => arguments.push(std::mem::take(&mut current)),
            _ => current.push(c),
        }
    }
    None
}

/// Put each argument where its parameter stands, by whole identifiers.
fn substitute(body: &str, parameters: &[String], arguments: &[String]) -> String {
    let bytes = body.as_bytes();
    let mut out = String::with_capacity(body.len());
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at].is_ascii_alphabetic() || bytes[at] == b'_' {
            let start = at;
            while at < bytes.len() && (bytes[at].is_ascii_alphanumeric() || bytes[at] == b'_') {
                at += 1;
            }
            let word = &body[start..at];
            match parameters.iter().position(|parameter| parameter == word) {
                Some(index) => out.push_str(&arguments[index]),
                None => out.push_str(word),
            }
        } else {
            let width = body[at..].chars().next().map_or(1, char::len_utf8);
            out.push_str(&body[at..at + width]);
            at += width;
        }
    }
    out
}

/// Evaluate a `#if` condition.
fn evaluate(
    text: &str,
    table: &HashMap<String, Macro>,
    line: usize,
) -> Result<i64, TranslateError> {
    // `defined` is answered before expansion, as it must be.
    let mut resolved = String::new();
    let mut rest = text;
    while let Some(at) = rest.find("defined") {
        let before_ok = at == 0
            || !rest.as_bytes()[at - 1].is_ascii_alphanumeric() && rest.as_bytes()[at - 1] != b'_';
        let after = &rest[at + "defined".len()..];
        let after_ok = after
            .chars()
            .next()
            .is_none_or(|c| !(c.is_ascii_alphanumeric() || c == '_'));
        if !(before_ok && after_ok) {
            resolved.push_str(&rest[..at + "defined".len()]);
            rest = after;
            continue;
        }
        resolved.push_str(&rest[..at]);
        let trimmed = after.trim_start();
        let (name, remainder) = if let Some(inner) = trimmed.strip_prefix('(') {
            let close = inner
                .find(')')
                .ok_or_else(|| syntax_error(line, "unterminated defined()"))?;
            (inner[..close].trim(), &inner[close + 1..])
        } else {
            let (name, remainder) = split_word(trimmed);
            (name, remainder)
        };
        resolved.push_str(if table.contains_key(name) {
            " 1 "
        } else {
            " 0 "
        });
        rest = remainder;
    }
    resolved.push_str(rest);
    let expanded = expand(&resolved, table, &HashSet::new(), line)?;
    let tokens = super::lex::lex(&expanded, line)?;
    let mut parser = Condition {
        tokens: &tokens,
        at: 0,
        line,
    };
    let value = parser.ternary()?;
    if parser.at != tokens.len() {
        return Err(syntax_error(line, "unexpected tokens in #if"));
    }
    Ok(value)
}

struct Condition<'a> {
    tokens: &'a [super::lex::Token],
    at: usize,
    line: usize,
}

impl Condition<'_> {
    fn peek(&self) -> Option<&super::lex::Kind> {
        self.tokens.get(self.at).map(|token| &token.kind)
    }

    fn eat(&mut self, punct: &str) -> bool {
        if matches!(self.peek(), Some(super::lex::Kind::Punct(p)) if *p == punct) {
            self.at += 1;
            true
        } else {
            false
        }
    }

    fn ternary(&mut self) -> Result<i64, TranslateError> {
        let condition = self.binary(0)?;
        if self.eat("?") {
            let yes = self.ternary()?;
            if !self.eat(":") {
                return Err(syntax_error(self.line, "#if ?: without :"));
            }
            let no = self.ternary()?;
            return Ok(if condition != 0 { yes } else { no });
        }
        Ok(condition)
    }

    fn binary(&mut self, minimum: u8) -> Result<i64, TranslateError> {
        let mut lhs = self.unary()?;
        loop {
            let Some(super::lex::Kind::Punct(op)) = self.peek() else {
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
            self.at += 1;
            let rhs = self.binary(precedence + 1)?;
            lhs = match op {
                "||" => i64::from(lhs != 0 || rhs != 0),
                "&&" => i64::from(lhs != 0 && rhs != 0),
                "|" => lhs | rhs,
                "^" => lhs ^ rhs,
                "&" => lhs & rhs,
                "==" => i64::from(lhs == rhs),
                "!=" => i64::from(lhs != rhs),
                "<" => i64::from(lhs < rhs),
                ">" => i64::from(lhs > rhs),
                "<=" => i64::from(lhs <= rhs),
                ">=" => i64::from(lhs >= rhs),
                "<<" => lhs.wrapping_shl(rhs as u32),
                ">>" => lhs.wrapping_shr(rhs as u32),
                "+" => lhs.wrapping_add(rhs),
                "-" => lhs.wrapping_sub(rhs),
                "*" => lhs.wrapping_mul(rhs),
                "/" | "%" if rhs == 0 => {
                    return Err(syntax_error(self.line, "division by zero in #if"));
                }
                "/" => lhs.wrapping_div(rhs),
                _ => lhs.wrapping_rem(rhs),
            };
        }
    }

    fn unary(&mut self) -> Result<i64, TranslateError> {
        if self.eat("!") {
            return Ok(i64::from(self.unary()? == 0));
        }
        if self.eat("-") {
            return Ok(self.unary()?.wrapping_neg());
        }
        if self.eat("+") {
            return self.unary();
        }
        if self.eat("~") {
            return Ok(!self.unary()?);
        }
        if self.eat("(") {
            let value = self.ternary()?;
            if !self.eat(")") {
                return Err(syntax_error(self.line, "unbalanced parentheses in #if"));
            }
            return Ok(value);
        }
        let value = match self.peek() {
            Some(super::lex::Kind::Int { value, .. }) => *value as i64,
            // An identifier left after expansion is zero, as in C.
            Some(super::lex::Kind::Ident(_)) => 0,
            _ => return Err(syntax_error(self.line, "malformed #if expression")),
        };
        self.at += 1;
        Ok(value)
    }
}
