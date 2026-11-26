//! Keyed archives for tests, written from a list of objects rather than by
//! hand as XML.

use std::fmt::Write;

/// One field of an archived object.
pub(crate) enum Field<'a> {
    Int(i64),
    Real(f64),
    Bool(bool),
    /// A string object of its own.
    Text(&'a str),
    /// Another object of the archive, by its place in the list (from 0).
    Ref(usize),
    /// An array's elements, as `NS.objects`.
    Refs(Vec<usize>),
}

/// An object: its class and its fields.
pub(crate) type Object<'a> = (&'a str, Vec<(&'a str, Field<'a>)>);

/// The XML spelling of a keyed archive of `objects`, whose top entry `top`
/// names object 0.
pub(crate) fn archive(top: &str, objects: &[Object]) -> String {
    let uid = |index: usize| format!("<dict><key>CF$UID</key><integer>{index}</integer></dict>");
    // `$objects[0]` is `$null`; the listed objects follow it, then the strings
    // their fields hold, then the classes.
    let mut strings: Vec<&str> = Vec::new();
    let mut classes: Vec<&str> = Vec::new();
    for (class, fields) in objects {
        if !classes.contains(class) {
            classes.push(class);
        }
        for (_, field) in fields {
            if let Field::Text(text) = field {
                strings.push(text);
            }
        }
    }
    let string_base = 1 + objects.len();
    let class_base = string_base + strings.len();
    let mut out = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<plist version=\"1.0\"><dict>\
         <key>$archiver</key><string>NSKeyedArchiver</string><key>$objects</key><array>\
         <string>$null</string>",
    );
    let mut next_string = string_base;
    for (class, fields) in objects {
        let class_uid = class_base + classes.iter().position(|c| c == class).expect("listed");
        write!(out, "<dict><key>$class</key>{}", uid(class_uid)).expect("a string");
        for (key, field) in fields {
            write!(out, "<key>{key}</key>").expect("a string");
            match field {
                Field::Int(value) => write!(out, "<integer>{value}</integer>"),
                Field::Real(value) => write!(out, "<real>{value}</real>"),
                Field::Bool(true) => write!(out, "<true/>"),
                Field::Bool(false) => write!(out, "<false/>"),
                Field::Text(_) => {
                    next_string += 1;
                    write!(out, "{}", uid(next_string - 1))
                }
                Field::Ref(index) => write!(out, "{}", uid(index + 1)),
                Field::Refs(indexes) => {
                    let elements: String = indexes.iter().map(|i| uid(i + 1)).collect();
                    write!(out, "<array>{elements}</array>")
                }
            }
            .expect("a string");
        }
        out.push_str("</dict>");
    }
    for text in &strings {
        write!(out, "<string>{text}</string>").expect("a string");
    }
    for class in &classes {
        write!(
            out,
            "<dict><key>$classes</key><array><string>{class}</string></array>\
             <key>$classname</key><string>{class}</string></dict>"
        )
        .expect("a string");
    }
    write!(
        out,
        "</array><key>$top</key><dict><key>{top}</key>{}</dict>\
         <key>$version</key><integer>100000</integer></dict></plist>",
        uid(1)
    )
    .expect("a string");
    out
}
