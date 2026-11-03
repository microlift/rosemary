//! `+[NSLayoutConstraint constraintsWithVisualFormat:options:metrics:views:]`.
//!
//! The parser works on names and numbers only, so it is tested without any
//! views; [`constraints_with_visual_format`] resolves the names afterwards.
//! The grammar is Apple's, from the Auto Layout Guide:
//!
//! ```text
//! format     ::= (orientation ':')? (superview connection)? view (connection view)* (connection superview)?
//! connection ::= '' | '-' predicates '-' | '-'
//! view       ::= '[' name ('(' predicate (',' predicate)* ')')? ']'
//! predicate  ::= relation? (constant | name) ('@' priority)?
//! ```

use objc::object::Id;
use objc::sel::Sel;

/// `NSLayoutRelation`.
pub const LESS_OR_EQUAL: isize = -1;
pub const EQUAL: isize = 0;
pub const GREATER_OR_EQUAL: isize = 1;

/// AppKit's standard spacing between two sibling views, and between a view
/// and the edge of its superview.
const SIBLING_SPACING: f64 = 8.0;
const SUPERVIEW_SPACING: f64 = 20.0;

/// What a predicate compares against.
#[derive(Clone, Debug, PartialEq)]
pub enum Object {
    Constant(f64),
    /// A metric's value, looked up by name.
    Metric(String),
    /// Another view's same dimension.
    View(String),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Priority {
    Value(f64),
    Metric(String),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Predicate {
    pub relation: isize,
    pub object: Object,
    pub priority: Option<Priority>,
}

/// One end of a connection: a named view, or the superview's edge.
#[derive(Clone, Debug, PartialEq)]
pub enum Item {
    Superview,
    View(String),
}

/// A spacing between two items. `None` is the standard spacing.
#[derive(Clone, Debug, PartialEq)]
pub struct Connection {
    pub before: Item,
    pub after: Item,
    pub predicates: Option<Vec<Predicate>>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Format {
    pub vertical: bool,
    /// Each view in order, with its size predicates.
    pub views: Vec<(String, Vec<Predicate>)>,
    pub connections: Vec<Connection>,
}

struct Parser<'a> {
    text: &'a [u8],
    at: usize,
}

impl Parser<'_> {
    fn peek(&self) -> Option<u8> {
        self.text.get(self.at).copied()
    }

    fn eat(&mut self, byte: u8) -> bool {
        if self.peek() == Some(byte) {
            self.at += 1;
            true
        } else {
            false
        }
    }

    fn skip_spaces(&mut self) {
        while self.peek().is_some_and(|b| b.is_ascii_whitespace()) {
            self.at += 1;
        }
    }

    fn error(&self, what: &str) -> String {
        format!(
            "{what} at offset {} of '{}'",
            self.at,
            String::from_utf8_lossy(self.text)
        )
    }

    fn name(&mut self) -> Option<String> {
        let start = self.at;
        if !self
            .peek()
            .is_some_and(|b| b.is_ascii_alphabetic() || b == b'_')
        {
            return None;
        }
        while self
            .peek()
            .is_some_and(|b| b.is_ascii_alphanumeric() || b == b'_')
        {
            self.at += 1;
        }
        Some(String::from_utf8_lossy(&self.text[start..self.at]).into_owned())
    }

    fn number(&mut self) -> Option<f64> {
        let start = self.at;
        if matches!(self.peek(), Some(b'-' | b'+')) {
            self.at += 1;
        }
        while self.peek().is_some_and(|b| b.is_ascii_digit() || b == b'.') {
            self.at += 1;
        }
        let text = std::str::from_utf8(&self.text[start..self.at]).ok()?;
        match text.parse() {
            Ok(value) => Some(value),
            Err(_) => {
                self.at = start;
                None
            }
        }
    }

    fn relation(&mut self) -> isize {
        for (spelling, relation) in [
            (b"==", EQUAL),
            (b"<=", LESS_OR_EQUAL),
            (b">=", GREATER_OR_EQUAL),
        ] {
            if self.text[self.at..].starts_with(spelling) {
                self.at += 2;
                return relation;
            }
        }
        EQUAL
    }

    /// `relation? (constant | name) ('@' priority)?`. A name is a metric in a
    /// connection and a view in a view's own predicates; the caller says which.
    fn predicate(&mut self, names_are_views: bool) -> Result<Predicate, String> {
        self.skip_spaces();
        let relation = self.relation();
        self.skip_spaces();
        let object = if let Some(value) = self.number() {
            Object::Constant(value)
        } else if let Some(name) = self.name() {
            match names_are_views {
                true => Object::View(name),
                false => Object::Metric(name),
            }
        } else {
            return Err(self.error("expected a constant or a name"));
        };
        self.skip_spaces();
        let priority = if self.eat(b'@') {
            self.skip_spaces();
            Some(if let Some(value) = self.number() {
                Priority::Value(value)
            } else if let Some(name) = self.name() {
                Priority::Metric(name)
            } else {
                return Err(self.error("expected a priority"));
            })
        } else {
            None
        };
        self.skip_spaces();
        Ok(Predicate {
            relation,
            object,
            priority,
        })
    }

    /// `'(' predicate (',' predicate)* ')'`.
    fn predicate_list(&mut self, names_are_views: bool) -> Result<Vec<Predicate>, String> {
        let mut out = vec![self.predicate(names_are_views)?];
        while self.eat(b',') {
            out.push(self.predicate(names_are_views)?);
        }
        if !self.eat(b')') {
            return Err(self.error("expected ')'"));
        }
        Ok(out)
    }

    /// A connection's predicates after its opening `-`, through its closing
    /// one; `None` for a bare `-`, which is the standard spacing.
    fn connection_predicates(&mut self) -> Result<Option<Vec<Predicate>>, String> {
        if matches!(self.peek(), Some(b'[' | b'|')) {
            return Ok(None);
        }
        let predicates = if self.eat(b'(') {
            self.predicate_list(false)?
        } else if let Some(value) = self.number() {
            vec![Predicate {
                relation: EQUAL,
                object: Object::Constant(value),
                priority: None,
            }]
        } else if let Some(name) = self.name() {
            vec![Predicate {
                relation: EQUAL,
                object: Object::Metric(name),
                priority: None,
            }]
        } else {
            return Err(self.error("expected a spacing"));
        };
        if !self.eat(b'-') {
            return Err(self.error("expected '-' after a spacing"));
        }
        Ok(Some(predicates))
    }

    /// A connection: nothing (the items touch), or a `-` spacing. Answers
    /// `None` when there is nothing at all, which is a spacing of zero.
    fn connection(&mut self) -> Result<Option<Option<Vec<Predicate>>>, String> {
        if !self.eat(b'-') {
            return Ok(None);
        }
        Ok(Some(self.connection_predicates()?))
    }

    fn view(&mut self) -> Result<(String, Vec<Predicate>), String> {
        if !self.eat(b'[') {
            return Err(self.error("expected '['"));
        }
        let Some(name) = self.name() else {
            return Err(self.error("expected a view name"));
        };
        let predicates = if self.eat(b'(') {
            self.predicate_list(true)?
        } else {
            Vec::new()
        };
        if !self.eat(b']') {
            return Err(self.error("expected ']'"));
        }
        Ok((name, predicates))
    }
}

/// The spacing a connection with no `-` at all stands for.
fn touching() -> Vec<Predicate> {
    vec![Predicate {
        relation: EQUAL,
        object: Object::Constant(0.0),
        priority: None,
    }]
}

pub fn parse(text: &str) -> Result<Format, String> {
    let compact: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    let mut p = Parser {
        text: compact.as_bytes(),
        at: 0,
    };
    let mut vertical = false;
    if p.text.len() >= 2 && p.text[1] == b':' && matches!(p.text[0], b'H' | b'V') {
        vertical = p.text[0] == b'V';
        p.at = 2;
    }
    let mut views = Vec::new();
    let mut connections = Vec::new();
    let mut previous = None;
    if p.eat(b'|') {
        let spacing = p.connection()?;
        previous = Some((Item::Superview, spacing));
    }
    loop {
        let (name, predicates) = p.view()?;
        if let Some((before, spacing)) = previous.take() {
            connections.push(Connection {
                before,
                after: Item::View(name.clone()),
                predicates: match spacing {
                    None => Some(touching()),
                    Some(predicates) => predicates,
                },
            });
        }
        views.push((name.clone(), predicates));
        let spacing = p.connection()?;
        if p.peek() == Some(b'[') {
            previous = Some((Item::View(name), spacing));
            continue;
        }
        if p.eat(b'|') {
            connections.push(Connection {
                before: Item::View(name),
                after: Item::Superview,
                predicates: match spacing {
                    None => Some(touching()),
                    Some(predicates) => predicates,
                },
            });
        } else if spacing.is_some() {
            return Err(p.error("a spacing must be followed by a view or '|'"));
        }
        break;
    }
    if p.at != p.text.len() {
        return Err(p.error("unexpected text"));
    }
    Ok(Format {
        vertical,
        views,
        connections,
    })
}

/// What `constraints_with_visual_format` needs to look names up.
struct Names {
    views: Id,
    metrics: Id,
}

impl Names {
    fn lookup(dictionary: Id, name: &str) -> Id {
        if dictionary.is_null() {
            return std::ptr::null_mut();
        }
        let key = foundation::string::from_str(name);
        // SAFETY: an NSDictionary, sent `-objectForKey:`.
        let value = unsafe {
            let f: unsafe extern "C" fn(Id, Sel, Id) -> Id =
                std::mem::transmute(objc::msg::entry());
            f(dictionary, objc::sel::get("objectForKey:"), key)
        };
        objc::arc::release(key);
        value
    }

    fn view(&self, name: &str) -> Result<Id, String> {
        match Self::lookup(self.views, name) {
            v if v.is_null() => Err(format!("no view named '{name}'")),
            v => Ok(v),
        }
    }

    fn metric(&self, name: &str) -> Result<f64, String> {
        let value = Self::lookup(self.metrics, name);
        if value.is_null() {
            return Err(format!("no metric named '{name}'"));
        }
        // SAFETY: an NSNumber, sent `-doubleValue`.
        Ok(unsafe {
            let f: unsafe extern "C" fn(Id, Sel) -> f64 = std::mem::transmute(objc::msg::entry());
            f(value, objc::sel::get("doubleValue"))
        })
    }

    fn constant(&self, object: &Object) -> Result<f64, String> {
        match object {
            Object::Constant(value) => Ok(*value),
            Object::Metric(name) => self.metric(name),
            Object::View(name) => Err(format!("'{name}' names a view where a constant goes")),
        }
    }

    fn priority(&self, priority: &Option<Priority>) -> Result<f32, String> {
        Ok(match priority {
            None => 1000.0,
            Some(Priority::Value(value)) => *value as f32,
            Some(Priority::Metric(name)) => self.metric(name)? as f32,
        })
    }
}

/// `NSLayoutFormatOptions`' direction field.
const DIRECTION_MASK: usize = 3 << 16;
const DIRECTION_LEFT_TO_RIGHT: usize = 1 << 16;
const DIRECTION_RIGHT_TO_LEFT: usize = 2 << 16;

/// A constraint to make: `first.attr relation second.attr + constant`.
type Spec = (Id, isize, isize, Id, isize, f64, f32);

fn build(format: &Format, options: usize, names: &Names) -> Result<Vec<Spec>, String> {
    // The leading and trailing edges along the format's axis, and its size.
    let (lead, trail, size) = match (format.vertical, options & DIRECTION_MASK) {
        (true, _) => (3, 4, 8),
        (false, DIRECTION_LEFT_TO_RIGHT) => (1, 2, 7),
        (false, DIRECTION_RIGHT_TO_LEFT) => (2, 1, 7),
        (false, _) => (5, 6, 7),
    };
    let mut out = Vec::new();
    for (name, predicates) in &format.views {
        let view = names.view(name)?;
        for predicate in predicates {
            let priority = names.priority(&predicate.priority)?;
            match &predicate.object {
                Object::View(other) => {
                    let other = names.view(other)?;
                    out.push((view, size, predicate.relation, other, size, 0.0, priority));
                }
                object => {
                    let constant = names.constant(object)?;
                    let none = std::ptr::null_mut();
                    out.push((view, size, predicate.relation, none, 0, constant, priority));
                }
            }
        }
    }
    let superview = || -> Result<Id, String> {
        let (first, _) = format.views.first().ok_or("no views")?;
        let first = names.view(first)?;
        // SAFETY: a view, sent `-superview`.
        let superview = unsafe {
            let f: unsafe extern "C" fn(Id, Sel) -> Id = std::mem::transmute(objc::msg::entry());
            f(first, objc::sel::get("superview"))
        };
        match superview.is_null() {
            true => Err("'|' used for a view with no superview".into()),
            false => Ok(superview),
        }
    };
    for connection in &format.connections {
        let to_edge = matches!(connection.before, Item::Superview)
            || matches!(connection.after, Item::Superview);
        let standard = vec![Predicate {
            relation: EQUAL,
            object: Object::Constant(match to_edge {
                true => SUPERVIEW_SPACING,
                false => SIBLING_SPACING,
            }),
            priority: None,
        }];
        let predicates = connection.predicates.as_ref().unwrap_or(&standard);
        // `after.lead relation before.trail + spacing`; for the superview the
        // edge faced is its own leading or trailing one.
        let (after, after_attribute) = match &connection.after {
            Item::Superview => (superview()?, trail),
            Item::View(name) => (names.view(name)?, lead),
        };
        let (before, before_attribute) = match &connection.before {
            Item::Superview => (superview()?, lead),
            Item::View(name) => (names.view(name)?, trail),
        };
        for predicate in predicates {
            let constant = names.constant(&predicate.object)?;
            let priority = names.priority(&predicate.priority)?;
            out.push((
                after,
                after_attribute,
                predicate.relation,
                before,
                before_attribute,
                constant,
                priority,
            ));
        }
    }
    // The alignment options: every view's attribute equal to the first's.
    for attribute in 1..=12isize {
        if options & (1 << attribute) == 0 {
            continue;
        }
        let mut views = format.views.iter();
        let Some((first, _)) = views.next() else {
            break;
        };
        let first = names.view(first)?;
        for (name, _) in views {
            let view = names.view(name)?;
            out.push((view, attribute, EQUAL, first, attribute, 0.0, 1000.0));
        }
    }
    Ok(out)
}

/// `+constraintsWithVisualFormat:options:metrics:views:`.
///
/// A format that does not parse, or names something the dictionaries do not
/// hold, is Apple's `NSInvalidArgumentException`; it is reported by name and
/// answered with no constraints rather than with a guess at the layout.
pub(crate) unsafe extern "C" fn constraints_with_visual_format(
    _class: Id,
    _sel: Sel,
    format: Id,
    options: usize,
    metrics: Id,
    views: Id,
) -> Id {
    // SAFETY: an NSString.
    let text = String::from_utf8_lossy(&unsafe { foundation::string::bytes(format) }).into_owned();
    let names = Names { views, metrics };
    let specs = parse(&text).and_then(|format| build(&format, options, &names));
    let specs = match specs {
        Ok(specs) => specs,
        Err(error) => {
            eprintln!("rosemary: NSLayoutConstraint: unable to parse '{text}': {error}");
            Vec::new()
        }
    };
    let constraints: Vec<corefoundation::base::CFTypeRef> = specs
        .into_iter()
        .map(|(first, fa, relation, second, sa, constant, priority)| {
            let c =
                crate::layout::new_constraint((first, fa), relation, (second, sa), 1.0, constant);
            crate::layout::set_priority_of(c, priority);
            c as corefoundation::base::CFTypeRef
        })
        .collect();
    let array = corefoundation::collections::new_array(
        &constraints,
        corefoundation::callbacks::Policy::from_type_callbacks(),
        false,
    );
    objc::arc::autorelease(array as Id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn constant(relation: isize, value: f64) -> Predicate {
        Predicate {
            relation,
            object: Object::Constant(value),
            priority: None,
        }
    }

    #[test]
    fn parses_superview_edges_and_standard_spacing() {
        let format = parse("H:|-[a]-[b]-|").unwrap();
        assert!(!format.vertical);
        assert_eq!(format.views.len(), 2);
        assert_eq!(format.connections.len(), 3);
        assert!(format.connections.iter().all(|c| c.predicates.is_none()));
        assert_eq!(format.connections[0].before, Item::Superview);
        assert_eq!(format.connections[2].after, Item::Superview);
    }

    #[test]
    fn touching_views_have_zero_spacing() {
        let format = parse("V:|[a][b]|").unwrap();
        assert!(format.vertical);
        for connection in &format.connections {
            assert_eq!(connection.predicates, Some(vec![constant(EQUAL, 0.0)]));
        }
    }

    #[test]
    fn parses_view_and_connection_predicates() {
        let format = parse("[a(>=50@750,==b)]-(<=pad@low)-[b(100)]").unwrap();
        assert_eq!(
            format.views[0].1,
            vec![
                Predicate {
                    relation: GREATER_OR_EQUAL,
                    object: Object::Constant(50.0),
                    priority: Some(Priority::Value(750.0)),
                },
                Predicate {
                    relation: EQUAL,
                    object: Object::View("b".into()),
                    priority: None,
                },
            ]
        );
        assert_eq!(
            format.connections[0].predicates,
            Some(vec![Predicate {
                relation: LESS_OR_EQUAL,
                object: Object::Metric("pad".into()),
                priority: Some(Priority::Metric("low".into())),
            }])
        );
        assert_eq!(format.views[1].1, vec![constant(EQUAL, 100.0)]);
    }

    #[test]
    fn a_number_spacing_and_negative_constant_parse() {
        let format = parse("H:|-12-[a]-(-1)-[b]|").unwrap();
        assert_eq!(
            format.connections[0].predicates,
            Some(vec![constant(EQUAL, 12.0)])
        );
        assert_eq!(
            format.connections[1].predicates,
            Some(vec![constant(EQUAL, -1.0)])
        );
    }

    #[test]
    fn malformed_formats_are_refused() {
        for bad in ["H:|-[a", "[a]-", "[a]x", "|-(>=)-[a]"] {
            assert!(parse(bad).is_err(), "{bad} parsed");
        }
    }
}
