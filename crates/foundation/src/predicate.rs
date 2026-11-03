//! `NSPredicate`: block predicates, and predicates parsed from the format
//! language - comparisons, string operators, `IN`, `BETWEEN`, aggregate
//! modifiers and the `AND`/`OR`/`NOT` connectives - and `NSCompoundPredicate`.
//!
//! The variadic `+predicateWithFormat:` reaches `+predicateWithFormat:argumentArray:`
//! through the runtime's variadic shims, which read the arguments the format's
//! conversions name.

use std::ffi::c_void;

use corefoundation::base::CFTypeRef;
use objc::object::Id;
use objc::sel::Sel;

use crate::define::{class_methods, methods};

#[repr(C)]
struct Predicate {
    isa: *const objc::class::Class,
    /// A `Box<State>`.
    state: *mut State,
}

struct State {
    node: Node,
    /// The source text, for `-predicateFormat`.
    format: String,
    /// Every object the tree holds, retained.
    owned: Vec<Id>,
}

#[derive(Clone)]
enum Node {
    Constant(bool),
    And(Vec<Node>),
    Or(Vec<Node>),
    Not(Box<Node>),
    Compare(Comparison),
    /// A block predicate, copied.
    Block(Id),
    /// Another predicate object, asked with `-evaluateWithObject:`.
    Object(Id),
}

#[derive(Clone)]
struct Comparison {
    left: Expr,
    op: Op,
    right: Expr,
    modifier: Modifier,
    case_insensitive: bool,
    diacritic_insensitive: bool,
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Op {
    Equal,
    NotEqual,
    Less,
    LessOrEqual,
    Greater,
    GreaterOrEqual,
    BeginsWith,
    EndsWith,
    Contains,
    Like,
    Matches,
    In,
    Between,
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Modifier {
    Direct,
    Any,
    All,
    None,
}

#[derive(Clone, Debug)]
enum Expr {
    /// Retained by the predicate, or nil.
    Constant(Id),
    KeyPath(String),
    Evaluated,
    Variable(String),
    Aggregate(Vec<Expr>),
}

// Parsing.

#[derive(Clone, Debug, PartialEq)]
enum Token {
    Word(String),
    Text(String),
    Number(f64, bool),
    Punct(&'static str),
    /// A format conversion: `@`, `K`, or a numeric one.
    Conversion(char),
    Variable(String),
}

fn tokenize(format: &str) -> Result<Vec<Token>, String> {
    let chars: Vec<char> = format.chars().collect();
    let mut out = Vec::new();
    let mut at = 0;
    const PUNCTS: &[&str] = &[
        "==", "!=", "<>", "<=", "=<", ">=", "=>", "&&", "||", "(", ")", "{", "}", "[", "]", ",",
        "=", "<", ">", "!",
    ];
    'outer: while at < chars.len() {
        let c = chars[at];
        if c.is_whitespace() {
            at += 1;
            continue;
        }
        if c == '"' || c == '\'' {
            let mut text = String::new();
            at += 1;
            while at < chars.len() && chars[at] != c {
                if chars[at] == '\\' && at + 1 < chars.len() {
                    at += 1;
                    text.push(match chars[at] {
                        'n' => '\n',
                        't' => '\t',
                        other => other,
                    });
                } else {
                    text.push(chars[at]);
                }
                at += 1;
            }
            if at >= chars.len() {
                return Err("an unterminated string".into());
            }
            at += 1;
            out.push(Token::Text(text));
            continue;
        }
        if c == '%' {
            // Length modifiers, then the conversion itself.
            let mut end = at + 1;
            while end < chars.len() && matches!(chars[end], 'l' | 'q' | 'h' | 'z' | 'L') {
                end += 1;
            }
            let Some(&conversion) = chars.get(end) else {
                return Err("a format that ends in %".into());
            };
            match conversion {
                '@' | 'K' | 'd' | 'i' | 'u' | 'x' | 'X' | 'o' | 'c' | 'f' | 'g' | 'e' | 'G'
                | 'E' => {
                    out.push(Token::Conversion(match conversion {
                        'f' | 'g' | 'e' | 'G' | 'E' => 'f',
                        '@' | 'K' => conversion,
                        _ => 'd',
                    }));
                    at = end + 1;
                    continue;
                }
                other => return Err(format!("the conversion %{other}")),
            }
        }
        if c == '$' {
            let start = at + 1;
            let mut end = start;
            while end < chars.len() && (chars[end].is_alphanumeric() || chars[end] == '_') {
                end += 1;
            }
            out.push(Token::Variable(chars[start..end].iter().collect()));
            at = end;
            continue;
        }
        let negative_number = c == '-' && chars.get(at + 1).is_some_and(|n| n.is_ascii_digit());
        if c.is_ascii_digit() || negative_number {
            let start = at;
            at += 1;
            let mut real = false;
            while at < chars.len() && (chars[at].is_ascii_digit() || chars[at] == '.') {
                real |= chars[at] == '.';
                at += 1;
            }
            let text: String = chars[start..at].iter().collect();
            let value = text
                .parse::<f64>()
                .map_err(|_| format!("the number {text}"))?;
            out.push(Token::Number(value, real));
            continue;
        }
        if c.is_alphabetic() || matches!(c, '_' | '@' | '#' | '.') {
            let start = at;
            at += 1;
            while at < chars.len()
                && (chars[at].is_alphanumeric() || matches!(chars[at], '_' | '.' | '@'))
            {
                at += 1;
            }
            let word: String = chars[start..at].iter().collect();
            out.push(Token::Word(word));
            continue;
        }
        for punct in PUNCTS {
            let len = punct.chars().count();
            if chars[at..].iter().take(len).copied().eq(punct.chars()) {
                out.push(Token::Punct(punct));
                at += len;
                continue 'outer;
            }
        }
        return Err(format!("the character {c:?}"));
    }
    Ok(out)
}

struct Parser<'a> {
    tokens: Vec<Token>,
    at: usize,
    arguments: &'a [Id],
    next_argument: usize,
    owned: Vec<Id>,
}

fn keyword(token: Option<&Token>, name: &str) -> bool {
    matches!(token, Some(Token::Word(w)) if w.eq_ignore_ascii_case(name))
}

impl Parser<'_> {
    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.at)
    }

    fn take(&mut self) -> Option<Token> {
        let token = self.tokens.get(self.at).cloned();
        self.at += 1;
        token
    }

    fn eat_punct(&mut self, punct: &str) -> bool {
        if matches!(self.peek(), Some(Token::Punct(p)) if *p == punct) {
            self.at += 1;
            return true;
        }
        false
    }

    fn eat_keyword(&mut self, name: &str) -> bool {
        if keyword(self.peek(), name) {
            self.at += 1;
            return true;
        }
        false
    }

    fn argument(&mut self) -> Result<Id, String> {
        let value = *self
            .arguments
            .get(self.next_argument)
            .ok_or("more conversions than arguments")?;
        self.next_argument += 1;
        Ok(value)
    }

    fn constant(&mut self, value: Id) -> Expr {
        if !value.is_null() {
            self.owned.push(objc::arc::retain(value));
        }
        Expr::Constant(value)
    }

    fn predicate(&mut self) -> Result<Node, String> {
        let mut terms = vec![self.conjunction()?];
        while self.eat_keyword("OR") || self.eat_punct("||") {
            terms.push(self.conjunction()?);
        }
        Ok(match terms.len() {
            1 => terms.pop().expect("one"),
            _ => Node::Or(terms),
        })
    }

    fn conjunction(&mut self) -> Result<Node, String> {
        let mut terms = vec![self.negation()?];
        while self.eat_keyword("AND") || self.eat_punct("&&") {
            terms.push(self.negation()?);
        }
        Ok(match terms.len() {
            1 => terms.pop().expect("one"),
            _ => Node::And(terms),
        })
    }

    fn negation(&mut self) -> Result<Node, String> {
        if self.eat_keyword("NOT") || self.eat_punct("!") {
            return Ok(Node::Not(Box::new(self.negation()?)));
        }
        if self.eat_punct("(") {
            let inner = self.predicate()?;
            if !self.eat_punct(")") {
                return Err("a missing )".into());
            }
            return Ok(inner);
        }
        if self.eat_keyword("TRUEPREDICATE") {
            return Ok(Node::Constant(true));
        }
        if self.eat_keyword("FALSEPREDICATE") {
            return Ok(Node::Constant(false));
        }
        self.comparison()
    }

    fn comparison(&mut self) -> Result<Node, String> {
        let modifier = if self.eat_keyword("ANY") || self.eat_keyword("SOME") {
            Modifier::Any
        } else if self.eat_keyword("ALL") {
            Modifier::All
        } else if self.eat_keyword("NONE") {
            Modifier::None
        } else {
            Modifier::Direct
        };
        let left = self.expression()?;
        let op = match self.take() {
            Some(Token::Punct("==" | "=")) => Op::Equal,
            Some(Token::Punct("!=" | "<>")) => Op::NotEqual,
            Some(Token::Punct("<")) => Op::Less,
            Some(Token::Punct("<=" | "=<")) => Op::LessOrEqual,
            Some(Token::Punct(">")) => Op::Greater,
            Some(Token::Punct(">=" | "=>")) => Op::GreaterOrEqual,
            Some(Token::Word(w)) => match w.to_ascii_uppercase().as_str() {
                "BEGINSWITH" => Op::BeginsWith,
                "ENDSWITH" => Op::EndsWith,
                "CONTAINS" => Op::Contains,
                "LIKE" => Op::Like,
                "MATCHES" => Op::Matches,
                "IN" => Op::In,
                "BETWEEN" => Op::Between,
                _ => return Err(format!("the operator {w}")),
            },
            other => return Err(format!("{other:?} where an operator belongs")),
        };
        let (mut case_insensitive, mut diacritic_insensitive) = (false, false);
        if self.eat_punct("[") {
            match self.take() {
                Some(Token::Word(flags)) => {
                    for flag in flags.chars() {
                        match flag.to_ascii_lowercase() {
                            'c' => case_insensitive = true,
                            'd' => diacritic_insensitive = true,
                            // Normalization: this runtime's strings compare
                            // by code point, which is the same here.
                            'n' | 'l' => {}
                            other => return Err(format!("the option [{other}]")),
                        }
                    }
                }
                other => return Err(format!("{other:?} as an option")),
            }
            if !self.eat_punct("]") {
                return Err("a missing ]".into());
            }
        }
        let right = self.expression()?;
        Ok(Node::Compare(Comparison {
            left,
            op,
            right,
            modifier,
            case_insensitive,
            diacritic_insensitive,
        }))
    }

    fn expression(&mut self) -> Result<Expr, String> {
        match self.take() {
            Some(Token::Conversion('@')) => {
                let value = self.argument()?;
                Ok(self.constant(value))
            }
            Some(Token::Conversion('K')) => {
                let name = self.argument()?;
                // SAFETY: `%K`'s argument is an `NSString`.
                let mut path =
                    String::from_utf8_lossy(&unsafe { crate::string::bytes(name) }).into_owned();
                // A key path may go on past the substituted key.
                while let Some(Token::Word(rest)) = self.peek()
                    && rest.starts_with('.')
                {
                    path.push_str(rest);
                    self.at += 1;
                }
                Ok(Expr::KeyPath(path))
            }
            Some(Token::Conversion(_)) => {
                let value = self.argument()?;
                Ok(self.constant(value))
            }
            Some(Token::Variable(name)) => Ok(Expr::Variable(name)),
            Some(Token::Text(text)) => {
                let value = crate::string::from_str(&text);
                let expr = self.constant(value);
                // `constant` took its own reference.
                // SAFETY: a string this function made.
                unsafe { corefoundation::api::CFRelease(value as CFTypeRef) };
                Ok(expr)
            }
            Some(Token::Number(value, real)) => {
                let number = match real {
                    true => crate::real(value),
                    false => crate::number(value as i64),
                };
                Ok(self.constant(number))
            }
            Some(Token::Punct("{")) => {
                let mut items = Vec::new();
                if !self.eat_punct("}") {
                    loop {
                        items.push(self.expression()?);
                        if self.eat_punct("}") {
                            break;
                        }
                        if !self.eat_punct(",") {
                            return Err("a missing , or }".into());
                        }
                    }
                }
                Ok(Expr::Aggregate(items))
            }
            Some(Token::Word(word)) => match word.to_ascii_uppercase().as_str() {
                "YES" | "TRUE" => Ok(self.constant(crate::number(1))),
                "NO" | "FALSE" => Ok(self.constant(crate::number(0))),
                "NULL" | "NIL" => Ok(Expr::Constant(std::ptr::null_mut())),
                "SELF" => Ok(Expr::Evaluated),
                _ => match word
                    .strip_prefix("SELF.")
                    .or_else(|| word.strip_prefix("self."))
                {
                    Some(path) => Ok(Expr::KeyPath(path.to_string())),
                    // `#` escapes a reserved word used as a key.
                    None => Ok(Expr::KeyPath(word.trim_start_matches('#').to_string())),
                },
            },
            other => Err(format!("{other:?} where an expression belongs")),
        }
    }
}

/// Parse `format`, taking `%@` and `%K` (and numeric conversions) from
/// `arguments` in order.
fn parse(format: &str, arguments: &[Id]) -> Result<State, String> {
    let mut parser = Parser {
        tokens: tokenize(format)?,
        at: 0,
        arguments,
        next_argument: 0,
        owned: Vec::new(),
    };
    let node = parser.predicate();
    let node = match node {
        Ok(node) if parser.at == parser.tokens.len() => Ok(node),
        Ok(_) => Err(format!(
            "{:?} after a complete predicate",
            parser.tokens[parser.at]
        )),
        Err(why) => Err(why),
    };
    match node {
        Ok(node) => Ok(State {
            node,
            format: format.to_string(),
            owned: parser.owned,
        }),
        Err(why) => {
            for object in parser.owned {
                objc::arc::release(object);
            }
            Err(why)
        }
    }
}

// Evaluation.

fn send_id(receiver: Id, sel: &str, argument: Id) -> Id {
    // SAFETY: each selector here is `id (id, SEL, id)`.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel, Id) -> Id = std::mem::transmute(objc::msg::entry());
        f(receiver, objc::sel::get(sel), argument)
    }
}

fn send_bool(receiver: Id, sel: &str, argument: Id) -> bool {
    // SAFETY: each selector here is `BOOL (id, SEL, id)`.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel, Id) -> u8 = std::mem::transmute(objc::msg::entry());
        f(receiver, objc::sel::get(sel), argument) != 0
    }
}

fn is_kind(object: Id, name: &std::ffi::CStr) -> bool {
    !object.is_null()
        && objc::class::lookup(name).is_some_and(|class| {
            // SAFETY: a live object.
            unsafe { corefoundation::base::is_kind_of(object.cast(), class) }
        })
}

fn as_text(value: Id) -> Option<String> {
    if value.is_null() {
        return None;
    }
    // SAFETY: a live object.
    unsafe { corefoundation::string::as_string(value as CFTypeRef) }
        .map(|s| String::from_utf8_lossy(&s.utf8()).into_owned())
}

fn as_number(value: Id) -> Option<f64> {
    if !is_kind(value, c"NSNumber") {
        return None;
    }
    // SAFETY: a number.
    unsafe { crate::real_of(value) }
}

/// The elements of a collection, or `None` for anything else.
fn elements(value: Id) -> Option<Vec<Id>> {
    let array = if is_kind(value, c"NSArray") {
        value
    } else if is_kind(value, c"NSSet") {
        send_id(value, "allObjects", std::ptr::null_mut())
    } else if is_kind(value, c"NSDictionary") {
        send_id(value, "allValues", std::ptr::null_mut())
    } else {
        return None;
    };
    // SAFETY: an array.
    unsafe { corefoundation::collections::array(array as CFTypeRef) }
        .map(|a| a.values.iter().map(|v| *v as Id).collect())
}

fn is_nil(value: Id) -> bool {
    value.is_null() || is_kind(value, c"NSNull")
}

/// A value at evaluation time: an object, or the list an aggregate makes.
enum Value {
    Object(Id),
    List(Vec<Id>),
}

impl Value {
    fn object(&self) -> Id {
        match self {
            Value::Object(object) => *object,
            Value::List(_) => std::ptr::null_mut(),
        }
    }

    fn items(&self) -> Option<Vec<Id>> {
        match self {
            Value::Object(object) => elements(*object),
            Value::List(items) => Some(items.clone()),
        }
    }
}

fn value_of(expr: &Expr, object: Id, variables: Id) -> Value {
    match expr {
        Expr::Constant(value) => Value::Object(*value),
        Expr::Evaluated => Value::Object(object),
        Expr::KeyPath(path) => Value::Object(match object.is_null() {
            true => object,
            false => {
                let path = crate::string::from_str(path);
                let value = send_id(object, "valueForKeyPath:", path);
                // SAFETY: a string this function made.
                unsafe { corefoundation::api::CFRelease(path as CFTypeRef) };
                value
            }
        }),
        Expr::Variable(name) => Value::Object(match variables.is_null() {
            true => variables,
            false => {
                let name = crate::string::from_str(name);
                let value = send_id(variables, "objectForKey:", name);
                // SAFETY: a string this function made.
                unsafe { corefoundation::api::CFRelease(name as CFTypeRef) };
                value
            }
        }),
        Expr::Aggregate(items) => Value::List(
            items
                .iter()
                .map(|item| value_of(item, object, variables).object())
                .collect(),
        ),
    }
}

/// `text` folded for the comparison's options.
fn fold(text: &str, comparison: &Comparison) -> String {
    let text = match comparison.diacritic_insensitive {
        true => corefoundation::api::strip_marks(text),
        false => text.to_string(),
    };
    match comparison.case_insensitive {
        true => text.to_lowercase(),
        false => text,
    }
}

/// `LIKE`'s wildcards: `*` any run, `?` any one character.
fn like(text: &[char], pattern: &[char]) -> bool {
    match pattern.split_first() {
        None => text.is_empty(),
        Some(('*', rest)) => (0..=text.len()).any(|skip| like(&text[skip..], rest)),
        Some(('?', rest)) => !text.is_empty() && like(&text[1..], rest),
        Some((c, rest)) => text.first() == Some(c) && like(&text[1..], rest),
    }
}

fn equal(left: Id, right: Id, comparison: &Comparison) -> bool {
    if is_nil(left) || is_nil(right) {
        return is_nil(left) && is_nil(right);
    }
    if let (Some(a), Some(b)) = (as_number(left), as_number(right)) {
        return a == b;
    }
    if let (Some(a), Some(b)) = (as_text(left), as_text(right)) {
        return fold(&a, comparison) == fold(&b, comparison);
    }
    send_bool(left, "isEqual:", right)
}

fn order(left: Id, right: Id, comparison: &Comparison) -> Option<std::cmp::Ordering> {
    if is_nil(left) || is_nil(right) {
        return None;
    }
    if let (Some(a), Some(b)) = (as_number(left), as_number(right)) {
        return a.partial_cmp(&b);
    }
    if let (Some(a), Some(b)) = (as_text(left), as_text(right)) {
        return Some(fold(&a, comparison).cmp(&fold(&b, comparison)));
    }
    // SAFETY: `NSComparisonResult (id, SEL, id)`.
    let result = unsafe {
        let f: unsafe extern "C" fn(Id, Sel, Id) -> isize = std::mem::transmute(objc::msg::entry());
        f(left, objc::sel::get("compare:"), right)
    };
    Some(result.cmp(&0))
}

/// One comparison of two plain values.
fn compare_values(left: Id, right: &Value, comparison: &Comparison) -> bool {
    use std::cmp::Ordering::*;
    let right_object = right.object();
    let strings = || {
        Some((
            fold(&as_text(left)?, comparison),
            fold(&as_text(right_object)?, comparison),
        ))
    };
    match comparison.op {
        Op::Equal => equal(left, right_object, comparison),
        Op::NotEqual => !equal(left, right_object, comparison),
        Op::Less => order(left, right_object, comparison) == Some(Less),
        Op::LessOrEqual => matches!(order(left, right_object, comparison), Some(Less | Equal)),
        Op::Greater => order(left, right_object, comparison) == Some(Greater),
        Op::GreaterOrEqual => {
            matches!(order(left, right_object, comparison), Some(Greater | Equal))
        }
        Op::BeginsWith => strings().is_some_and(|(a, b)| a.starts_with(&b)),
        Op::EndsWith => strings().is_some_and(|(a, b)| a.ends_with(&b)),
        Op::Contains => match elements(left) {
            Some(items) => items.iter().any(|i| equal(*i, right_object, comparison)),
            None => strings().is_some_and(|(a, b)| a.contains(&b)),
        },
        Op::Like => strings().is_some_and(|(a, b)| {
            like(
                &a.chars().collect::<Vec<_>>(),
                &b.chars().collect::<Vec<_>>(),
            )
        }),
        Op::Matches => strings().is_some_and(|(a, b)| {
            fancy_regex::Regex::new(&format!("^(?:{b})$"))
                .ok()
                .and_then(|r| r.is_match(&a).ok())
                .unwrap_or(false)
        }),
        Op::In => match right.items() {
            Some(items) => items.iter().any(|i| equal(left, *i, comparison)),
            None => match (as_text(left), as_text(right_object)) {
                (Some(a), Some(b)) => fold(&b, comparison).contains(&fold(&a, comparison)),
                _ => false,
            },
        },
        Op::Between => match right.items().as_deref() {
            Some([low, high]) => {
                matches!(order(left, *low, comparison), Some(Greater | Equal))
                    && matches!(order(left, *high, comparison), Some(Less | Equal))
            }
            _ => false,
        },
    }
}

/// Whether `node` holds for `object`, with `variables` (an `NSDictionary` or
/// nil) giving the values of `$NAME`s.
fn evaluate(node: &Node, object: Id, variables: Id) -> bool {
    match node {
        Node::Constant(value) => *value,
        Node::And(terms) => terms.iter().all(|t| evaluate(t, object, variables)),
        Node::Or(terms) => terms.iter().any(|t| evaluate(t, object, variables)),
        Node::Not(term) => !evaluate(term, object, variables),
        Node::Block(block) => {
            // SAFETY: a copied block.
            let Some(invoke) = (unsafe { objc::block::invoke(*block) }) else {
                return false;
            };
            // SAFETY: a predicate block is `BOOL (^)(id, NSDictionary *)`.
            unsafe {
                let call: unsafe extern "C" fn(Id, Id, Id) -> u8 = std::mem::transmute(invoke);
                call(*block, object, variables) != 0
            }
        }
        // SAFETY: `BOOL (id, SEL, id, NSDictionary *)`.
        Node::Object(predicate) => unsafe {
            let f: unsafe extern "C" fn(Id, Sel, Id, Id) -> u8 =
                std::mem::transmute(objc::msg::entry());
            f(
                *predicate,
                objc::sel::get("evaluateWithObject:substitutionVariables:"),
                object,
                variables,
            ) != 0
        },
        Node::Compare(comparison) => {
            let left = value_of(&comparison.left, object, variables);
            let right = value_of(&comparison.right, object, variables);
            match comparison.modifier {
                Modifier::Direct => compare_values(left.object(), &right, comparison),
                modifier => {
                    let items = left.items().unwrap_or_default();
                    let mut each = items.iter().map(|i| compare_values(*i, &right, comparison));
                    match modifier {
                        Modifier::Any => each.any(|b| b),
                        Modifier::All => each.all(|b| b),
                        _ => !each.any(|b| b),
                    }
                }
            }
        }
    }
}

// The classes.

fn state_of<'a>(predicate: Id) -> Option<&'a State> {
    // SAFETY: an instance of a class laid out as `Predicate`.
    let state = unsafe { (*(predicate as *const Predicate)).state };
    // SAFETY: a `Box<State>` this module made, alive as long as the predicate.
    (!state.is_null()).then(|| unsafe { &*state })
}

/// A fresh, autoreleased predicate of `class` holding `state`.
fn make(class: &std::ffi::CStr, state: State) -> Id {
    let class = objc::class::lookup(class).expect("installed with the predicates");
    // SAFETY: the registered layout of both predicate classes.
    let predicate = unsafe { corefoundation::base::allocate::<Predicate>(class) };
    if predicate.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: a fresh instance.
    unsafe { (*predicate).state = Box::into_raw(Box::new(state)) };
    // SAFETY: a fresh object follows the convenience-constructor rule.
    unsafe { corefoundation::api::CFAutorelease(predicate as CFTypeRef) as Id }
}

/// `+predicateWithFormat:argumentArray:`.
unsafe extern "C" fn with_format_arguments(_class: Id, _sel: Sel, format: Id, array: Id) -> Id {
    // SAFETY: an `NSString`.
    let text = String::from_utf8_lossy(&unsafe { crate::string::bytes(format) }).into_owned();
    let arguments: Vec<Id> = match array.is_null() {
        true => Vec::new(),
        // SAFETY: an array or nil.
        false => unsafe { corefoundation::collections::array(array as CFTypeRef) }
            .map(|a| a.values.iter().map(|v| *v as Id).collect())
            .unwrap_or_default(),
    };
    with_format(&text, &arguments)
}

/// A variadic argument as the runtime's shims read it off a call.
pub enum Argument {
    Object(Id),
    Integer(i64),
    Real(f64),
}

/// The conversions in `format`, in order, as the shims must read their
/// arguments: `@` and `K` for objects, `d` for integers, `f` for doubles.
pub fn conversions(format: &str) -> Vec<char> {
    tokenize(format)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|t| match t {
            Token::Conversion(c) => Some(c),
            _ => None,
        })
        .collect()
}

/// [`with_format`] for arguments read off a variadic call.
pub fn with_arguments(format: &str, arguments: &[Argument]) -> Id {
    let boxed: Vec<Id> = arguments
        .iter()
        .map(|a| match a {
            Argument::Object(object) => *object,
            Argument::Integer(value) => crate::number(*value),
            Argument::Real(value) => crate::real(*value),
        })
        .collect();
    with_format(format, &boxed)
}

/// A predicate parsed from `format` and its arguments, autoreleased. A format
/// that does not parse is the exception Apple raises.
pub fn with_format(format: &str, arguments: &[Id]) -> Id {
    match parse(format, arguments) {
        Ok(state) => make(c"NSPredicate", state),
        Err(why) => objc::fail!("NSPredicate: unable to parse the format {format:?}: {why}"),
    }
}

unsafe extern "C" fn with_value(_class: Id, _sel: Sel, value: u8) -> Id {
    make(
        c"NSPredicate",
        State {
            node: Node::Constant(value != 0),
            format: match value != 0 {
                true => "TRUEPREDICATE".into(),
                false => "FALSEPREDICATE".into(),
            },
            owned: Vec::new(),
        },
    )
}

unsafe extern "C" fn with_block(_class: Id, _sel: Sel, block: Id) -> Id {
    let block = objc::block::copy(block);
    make(
        c"NSPredicate",
        State {
            node: Node::Block(block),
            format: "BLOCKPREDICATE".into(),
            owned: Vec::new(),
        },
    )
}

unsafe extern "C" fn evaluate_with_object(receiver: Id, _sel: Sel, object: Id) -> u8 {
    state_of(receiver).is_some_and(|s| evaluate(&s.node, object, std::ptr::null_mut())) as u8
}

unsafe extern "C" fn evaluate_with_variables(
    receiver: Id,
    _sel: Sel,
    object: Id,
    variables: Id,
) -> u8 {
    state_of(receiver).is_some_and(|s| evaluate(&s.node, object, variables)) as u8
}

/// `-predicateWithSubstitutionVariables:`: the same tree with each `$NAME`
/// replaced by its value.
unsafe extern "C" fn with_substitution_variables(receiver: Id, _sel: Sel, variables: Id) -> Id {
    let Some(state) = state_of(receiver) else {
        return receiver;
    };
    let mut owned: Vec<Id> = state.owned.iter().map(|o| objc::arc::retain(*o)).collect();
    let node = substitute(&state.node, variables, &mut owned);
    make(
        c"NSPredicate",
        State {
            node,
            format: state.format.clone(),
            owned,
        },
    )
}

fn substitute(node: &Node, variables: Id, owned: &mut Vec<Id>) -> Node {
    fn expr(e: &Expr, variables: Id, owned: &mut Vec<Id>) -> Expr {
        match e {
            Expr::Variable(_) => {
                let value = value_of(e, std::ptr::null_mut(), variables).object();
                if !value.is_null() {
                    owned.push(objc::arc::retain(value));
                }
                Expr::Constant(value)
            }
            Expr::Aggregate(items) => {
                Expr::Aggregate(items.iter().map(|i| expr(i, variables, owned)).collect())
            }
            other => other.clone(),
        }
    }
    match node {
        Node::And(terms) => Node::And(
            terms
                .iter()
                .map(|t| substitute(t, variables, owned))
                .collect(),
        ),
        Node::Or(terms) => Node::Or(
            terms
                .iter()
                .map(|t| substitute(t, variables, owned))
                .collect(),
        ),
        Node::Not(term) => Node::Not(Box::new(substitute(term, variables, owned))),
        Node::Compare(c) => Node::Compare(Comparison {
            left: expr(&c.left, variables, owned),
            right: expr(&c.right, variables, owned),
            ..c.clone()
        }),
        // A block owns nothing here to share; the copy holds its own.
        Node::Block(block) => Node::Block(objc::block::copy(*block)),
        Node::Object(predicate) => {
            owned.push(objc::arc::retain(*predicate));
            Node::Object(*predicate)
        }
        Node::Constant(value) => Node::Constant(*value),
    }
}

unsafe extern "C" fn predicate_format(receiver: Id, _sel: Sel) -> Id {
    let format = state_of(receiver).map_or("", |s| s.format.as_str());
    let string = crate::string::from_str(format);
    // SAFETY: a fresh string.
    unsafe { corefoundation::api::CFAutorelease(string as CFTypeRef) as Id }
}

unsafe extern "C" fn copy(receiver: Id, _sel: Sel) -> Id {
    objc::arc::retain(receiver)
}

unsafe extern "C" fn copy_with_zone(receiver: Id, _sel: Sel, _zone: *const c_void) -> Id {
    objc::arc::retain(receiver)
}

unsafe extern "C" fn dealloc(receiver: Id, _sel: Sel) {
    // SAFETY: the predicate's own state, taken once.
    let state = unsafe {
        std::mem::replace(
            &mut (*(receiver as *mut Predicate)).state,
            std::ptr::null_mut(),
        )
    };
    if !state.is_null() {
        // SAFETY: a `Box<State>` this module made.
        let state = unsafe { Box::from_raw(state) };
        for object in &state.owned {
            objc::arc::release(*object);
        }
        if let Node::Block(block) = state.node {
            objc::block::release(block);
        }
    }
    objc::arc::finalize(receiver);
}

/// `NSCompoundPredicateType`.
const NOT: usize = 0;
const AND: usize = 1;
const OR: usize = 2;

/// An `NSCompoundPredicate` of `kind` over the predicates in `array`.
fn compound(kind: usize, array: Id) -> Id {
    let predicates: Vec<Id> = elements(array).unwrap_or_default();
    let owned: Vec<Id> = predicates.iter().map(|p| objc::arc::retain(*p)).collect();
    let terms: Vec<Node> = predicates.iter().map(|p| Node::Object(*p)).collect();
    let formats: Vec<String> = predicates
        .iter()
        .map(|p| as_text(send_id(*p, "predicateFormat", std::ptr::null_mut())).unwrap_or_default())
        .collect();
    let (node, format) = match kind {
        AND => (Node::And(terms), formats.join(" AND ")),
        OR => (Node::Or(terms), formats.join(" OR ")),
        _ => (
            Node::Not(Box::new(Node::And(terms))),
            format!("NOT {}", formats.join(" AND ")),
        ),
    };
    let predicate = make(
        c"NSCompoundPredicate",
        State {
            node,
            format,
            owned,
        },
    );
    objc::arc::set_associated_word(predicate, &KIND as *const u8 as *const c_void, kind);
    objc::arc::set_associated_with_policy(
        predicate,
        &SUBPREDICATES as *const u8 as *const c_void,
        array,
        3,
    );
    predicate
}

/// Association keys for `-compoundPredicateType` and `-subpredicates`.
static KIND: u8 = 0;
static SUBPREDICATES: u8 = 0;

unsafe extern "C" fn and_with(_class: Id, _sel: Sel, array: Id) -> Id {
    compound(AND, array)
}

unsafe extern "C" fn or_with(_class: Id, _sel: Sel, array: Id) -> Id {
    compound(OR, array)
}

unsafe extern "C" fn not_with(_class: Id, _sel: Sel, predicate: Id) -> Id {
    let class = objc::class::lookup(c"NSArray").expect("NSArray");
    let array = send_id(objc::class::ptr(class) as Id, "arrayWithObject:", predicate);
    compound(NOT, array)
}

unsafe extern "C" fn init_with_type(receiver: Id, _sel: Sel, kind: usize, array: Id) -> Id {
    objc::arc::release(receiver);
    objc::arc::retain(compound(kind, array))
}

unsafe extern "C" fn compound_type(receiver: Id, _sel: Sel) -> usize {
    objc::arc::associated(receiver, &KIND as *const u8 as *const c_void) as usize
}

unsafe extern "C" fn subpredicates(receiver: Id, _sel: Sel) -> Id {
    objc::arc::associated(receiver, &SUBPREDICATES as *const u8 as *const c_void)
}

// What the collections do with one.

fn filtered(values: &[CFTypeRef], predicate: Id) -> Vec<CFTypeRef> {
    values
        .iter()
        .copied()
        .filter(|v| send_bool(predicate, "evaluateWithObject:", *v as Id))
        .collect()
}

unsafe extern "C" fn filtered_array(receiver: Id, _sel: Sel, predicate: Id) -> Id {
    // SAFETY: an array.
    let values = unsafe { corefoundation::collections::array(receiver as CFTypeRef) }
        .map(|a| a.values.clone())
        .unwrap_or_default();
    let kept = filtered(&values, predicate);
    // SAFETY: a fresh array follows the convenience-constructor rule.
    unsafe {
        corefoundation::api::CFAutorelease(corefoundation::collections::new_array(
            &kept,
            corefoundation::callbacks::Policy::from_type_callbacks(),
            false,
        )) as Id
    }
}

unsafe extern "C" fn filter_array(receiver: Id, _sel: Sel, predicate: Id) {
    // SAFETY: a mutable array.
    let Some(array) = (unsafe { corefoundation::collections::array(receiver as CFTypeRef) }) else {
        return;
    };
    let values = array.values.clone();
    let kept = filtered(&values, predicate);
    for value in values.iter().filter(|v| !kept.contains(v)) {
        array.policy.release(*value);
    }
    array.values = kept;
}

unsafe extern "C" fn filtered_set(receiver: Id, _sel: Sel, predicate: Id) -> Id {
    // SAFETY: a set.
    let values = unsafe { corefoundation::collections::set(receiver as CFTypeRef) }
        .map(|s| s.values.clone())
        .unwrap_or_default();
    let kept = filtered(&values, predicate);
    // SAFETY: a fresh set follows the convenience-constructor rule.
    unsafe {
        corefoundation::api::CFAutorelease(corefoundation::collections::new_set(
            &kept,
            corefoundation::callbacks::Policy::from_type_callbacks(),
        )) as Id
    }
}

pub(crate) fn install() {
    let predicate =
        corefoundation::base::register(c"NSPredicate", None, size_of::<Predicate>(), false);
    methods!(predicate, {
        "dealloc" => dealloc, c"v16@0:8";
        "evaluateWithObject:" => evaluate_with_object, c"c24@0:8@16";
        "evaluateWithObject:substitutionVariables:" => evaluate_with_variables, c"c32@0:8@16@24";
        "predicateWithSubstitutionVariables:" => with_substitution_variables, c"@24@0:8@16";
        "predicateFormat" => predicate_format, c"@16@0:8";
        "description" => predicate_format, c"@16@0:8";
        "copy" => copy, c"@16@0:8";
        "copyWithZone:" => copy_with_zone, c"@24@0:8^v16";
    });
    class_methods!(predicate, {
        "predicateWithBlock:" => with_block, c"@24@0:8@?16";
        "predicateWithValue:" => with_value, c"@20@0:8c16";
        "predicateWithFormat:argumentArray:" => with_format_arguments, c"@32@0:8@16@24";
    });

    let compound = corefoundation::base::register(
        c"NSCompoundPredicate",
        Some(predicate),
        size_of::<Predicate>(),
        false,
    );
    methods!(compound, {
        "initWithType:subpredicates:" => init_with_type, c"@32@0:8Q16@24";
        "compoundPredicateType" => compound_type, c"Q16@0:8";
        "subpredicates" => subpredicates, c"@16@0:8";
    });
    class_methods!(compound, {
        "andPredicateWithSubpredicates:" => and_with, c"@24@0:8@16";
        "orPredicateWithSubpredicates:" => or_with, c"@24@0:8@16";
        "notPredicateWithSubpredicate:" => not_with, c"@24@0:8@16";
    });

    let array = objc::class::lookup(c"NSArray").expect("NSArray");
    methods!(array, {
        "filteredArrayUsingPredicate:" => filtered_array, c"@24@0:8@16";
    });
    let mutable_array = objc::class::lookup(c"NSMutableArray").expect("NSMutableArray");
    methods!(mutable_array, {
        "filterUsingPredicate:" => filter_array, c"v24@0:8@16";
    });
    let set = objc::class::lookup(c"NSSet").expect("NSSet");
    methods!(set, {
        "filteredSetUsingPredicate:" => filtered_set, c"@24@0:8@16";
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dictionary(pairs: &[(&str, Id)]) -> Id {
        let keys: Vec<CFTypeRef> = pairs
            .iter()
            .map(|(k, _)| crate::string::from_str(k) as CFTypeRef)
            .collect();
        let entries: Vec<(CFTypeRef, CFTypeRef)> = keys
            .iter()
            .zip(pairs)
            .map(|(k, (_, v))| (*k, *v as CFTypeRef))
            .collect();
        corefoundation::collections::new_dictionary(
            &entries,
            corefoundation::callbacks::Policy::from_key_callbacks(),
            corefoundation::callbacks::Policy::from_type_callbacks(),
            false,
        ) as Id
    }

    fn holds(format: &str, arguments: &[Id], object: Id) -> bool {
        crate::bootstrap();
        let predicate = with_format(format, arguments);
        send_bool(predicate, "evaluateWithObject:", object)
    }

    fn text(value: &str) -> Id {
        crate::string::from_str(value)
    }

    #[test]
    fn comparisons_against_literals_and_arguments() {
        crate::bootstrap();
        let object = dictionary(&[
            ("isInstalled", crate::number(0)),
            ("path", text("/tmp/x")),
            ("name", text("Ruby")),
        ]);
        assert!(holds("isInstalled == NO", &[], object));
        assert!(!holds("isInstalled == YES", &[], object));
        assert!(holds("path != NULL", &[], object));
        assert!(holds("missing == nil", &[], object));
        assert!(holds("name == %@", &[text("Ruby")], object));
        assert!(holds("%K == 'Ruby'", &[text("name")], object));
        assert!(holds("name BEGINSWITH[c] 'ru'", &[], object));
        assert!(!holds("name BEGINSWITH 'ru'", &[], object));
        assert!(holds("name LIKE 'R*y'", &[], object));
        assert!(holds("name MATCHES 'R.b.'", &[], object));
    }

    #[test]
    fn connectives_and_in() {
        crate::bootstrap();
        let object = dictionary(&[
            ("hasUpdate", crate::number(1)),
            ("isCompatible", crate::number(1)),
            ("isInstalled", crate::number(1)),
        ]);
        let empty = send_id(
            objc::class::ptr(objc::class::lookup(c"NSArray").expect("NSArray")) as Id,
            "array",
            std::ptr::null_mut(),
        );
        assert!(holds(
            "(hasUpdate == YES AND isCompatible == YES) OR (isInstalled == NO AND NOT (SELF IN %@))",
            &[empty],
            object
        ));
        assert!(holds("NOT (SELF IN %@)", &[empty], object));
        assert!(holds("hasUpdate IN {0, 1}", &[], object));
        assert!(holds("hasUpdate BETWEEN {0, 2}", &[], object));
        assert!(!holds(
            "hasUpdate == YES AND isInstalled == NO",
            &[],
            object
        ));
    }

    #[test]
    fn a_bad_format_is_reported() {
        assert!(parse("a ==", &[]).is_err());
        assert!(parse("a == 1 b", &[]).is_err());
        assert!(parse("a == %@", &[]).is_err());
    }
}
