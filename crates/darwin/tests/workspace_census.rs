//! Every `exports!` row in the workspace, counted, and every name two of them
//! define.
//!
//! # Why this is a test and not a review
//!
//! Two modules in this tree published `_copyfile` with **different behaviour**,
//! and nobody saw it for two reasons that a green suite cannot fix:
//!
//! 1. A duplicate is invisible at runtime by construction.
//!    `darwin::provider()` is a `BTreeMap`, so the second definition silently
//!    replaces the first; `darwin::symbol()` is a `find_map`, so it answers with
//!    the first. Both are "correct" and they are different functions, and
//!    nothing in the process can see that a replacement happened. This test
//!    reads the *rows* instead, which is the only place a duplicate still
//!    exists as two things.
//! 2. The two guards that should have caught it both stop early.
//!    `darwin::tests::no_module_defines_a_name_another_one_does` `panic!`s on
//!    the **first** duplicate in `MODULES` order, and `sched_yield` (`idle` at
//!    index 0) has been there since before `copyfile` (index 56) and
//!    `media_unavailable` (index 122) were written - so it has been reporting
//!    the same known failure and nothing else, indefinitely.
//!    `rosetta3::frameworks::tests::no_framework_defines_a_symbol_another_one_
//!    does` does collect every duplicate, but it only walks the seventeen
//!    frameworks, so a name defined once in a framework and once in `darwin` -
//!    `_CGRectEqualToRect` - is outside what it can see. That is the whole of
//!    the appkit/darwin coverage gap, and it had no test at all.
//!
//! So this is the census, workspace-wide, and it names **every** duplicate
//! rather than the first.
//!
//! # What it can and cannot see
//!
//! It parses `exports!` invocations, so it sees `functions`, `data` and
//! `constants` rows, and it sees `cfg`-disabled ones - which is the point, since
//! a row that is off on this host and on on another is two different trees.
//!
//! It does **not** see:
//!
//! * names published outside an `exports!` block - `rosetta3::frameworks`'s
//!   `MEDIA_CONSTANTS` and `shimmed()`, and every `abi::table` row;
//! * `groups { }` membership, which is resolved by the macro rather than named
//!   in the block;
//! * a name that is both an `exports!` row and a `shim_for` arm. That is a real
//!   and larger population - 45 such names on this host - and most are the
//!   intended pattern, a shim existing because the ABI needs marshalling. It
//!   needs a semantic judgement per name that no census can make, so it is
//!   reported by hand rather than asserted here.
//!
//! Those three limits are the reason this is a *census* with a written
//! allowlist rather than a proof. Claiming more than it checks is how a green
//! suite comes to mean nothing.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// A name two `exports!` rows define, with where they are.
#[derive(Debug)]
struct Duplicate {
    name: String,
    at: Vec<String>,
}

/// Names defined more than once, with no `cfg` making the two exclusive.
///
/// Allowlisted, and every entry says why. The allowlist is a written record of
/// the known-bad set rather than a line inside somebody's panic message, and it
/// is checked: an entry that no longer duplicates fails here, so the list cannot
/// rot into a blanket suppression.
///
/// Each is a **real** split-brain, not a harmless one - a guest import and a
/// guest `dlsym` of the name reach different functions:
///
/// * `_sched_yield` - `darwin::idle` and `darwin::pthread`. `pthread`'s row is
///   `"sched_yield" => libc::sched_yield`; `idle`'s is its own. Documented in
///   `test/PLAN.md` §8. `idle`'s row is the one that should not exist.
///
/// `_CGCursorIsVisible` **was** here, and is not any more. It was defined by
/// `appkit::cursor` and re-exported by `applicationservices`, and the re-export
/// is gone: `_CGCursorIsVisible` is a CoreGraphics function, the defining image
/// is CoreGraphics, and in this tree the CoreGraphics C surface **is** `appkit`
/// - which already publishes the two functions that change what it answers
/// (`CGDisplayHideCursor`/`CGDisplayShowCursor`: three functions over one
/// counted hide level). Leaving the entry here would make it a blanket
/// suppression that hides the next duplicate, which is the failure mode the
/// other test in this file exists to prevent.
const KNOWN: &[(&str, &str)] = &[(
    "_sched_yield",
    "darwin::idle and darwin::pthread; pre-existing, PLAN.md §8. `idle`'s row should go",
)];

// ---------------------------------------------------------------- the parser

/// One `"name" | "alias" => target,` row.
struct Row {
    name: String,
    /// The `cfg` that makes this row exclusive with another, if any.
    cfg: Option<String>,
    line: usize,
    section: &'static str,
}

/// A quoted arm: `"a" | "b" =>`, with its attributes and the line it starts on.
struct Arm {
    names: Vec<String>,
    cfg: Option<String>,
    line: usize,
}

fn is_macro_line(line: &str) -> bool {
    let line = line.trim_start();
    let line = line.strip_prefix("pub ").unwrap_or(line);
    // `exports!`, `exports::exports!`, `group!`, `crate::group!`, ...
    let rest = match line.find("::") {
        Some(at) => &line[at + 2..],
        None => line,
    };
    rest.starts_with("exports!") || rest.starts_with("group!")
}

/// Line index one past the `}` closing the block whose `{` is on `lines[start]`.
fn block_end(lines: &[String], start: usize) -> usize {
    let mut depth = 0i32;
    for (i, line) in lines.iter().enumerate().skip(start) {
        depth += line.matches('{').count() as i32 - line.matches('}').count() as i32;
        if depth <= 0 {
            return i;
        }
    }
    lines.len() - 1
}

/// The `cfg` on the run of attribute lines immediately above `at`.
fn cfg_above(lines: &[String], at: usize) -> Option<String> {
    let mut found = None;
    let mut i = at;
    while i > 0 {
        i -= 1;
        let line = lines[i].trim();
        if line.starts_with("#[") {
            found = line
                .strip_prefix("#[cfg(")
                .and_then(|rest| rest.strip_suffix(")]"))
                .map(|body| body.trim().to_string())
                .or(found);
            continue;
        }
        if line.is_empty() || line.starts_with("//") {
            continue;
        }
        break;
    }
    found
}

/// The arms of a `name { ... }` subsection, and where it starts.
fn arms_in(lines: &[String], from: usize, to: usize) -> Vec<Arm> {
    let mut out = Vec::new();
    let mut pending: Option<(Vec<String>, usize)> = None;
    for (i, line) in lines.iter().enumerate().take(to).skip(from + 1) {
        let line = line.trim();
        // A wrapped arm: the previous line ended in `|` and this one continues.
        if let Some((names, start)) = pending.take() {
            if let Some(rest) = line.strip_prefix('|') {
                let mut names = names;
                names.extend(quoted(rest));
                if rest.contains("=>") {
                    out.push(Arm {
                        names,
                        cfg: cfg_above(lines, start),
                        line: start + 1,
                    });
                } else {
                    pending = Some((names, start));
                }
                continue;
            }
        }
        let Some(head) = line.strip_prefix('"') else {
            continue;
        };
        let Some((first, rest)) = head.split_once('"') else {
            continue;
        };
        if !rest.contains("=>") {
            // A `|` list that wraps onto the next line: remember it and wait.
            if rest.contains('|') {
                let mut names = vec![first.to_string()];
                names.extend(quoted(rest));
                pending = Some((names, i));
            }
            continue;
        }
        let mut names = vec![first.to_string()];
        names.extend(quoted(rest));
        out.push(Arm {
            names,
            cfg: cfg_above(lines, i),
            line: i + 1,
        });
    }
    out
}

/// Every `"..."` in `text` that is a bare arm member - a `|` list, not a value.
fn quoted(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for part in text.split('|') {
        let Some(rest) = part.trim().strip_prefix('"') else {
            continue;
        };
        if let Some((name, _)) = rest.split_once('"') {
            out.push(name.to_string());
        }
    }
    out
}

fn rows_of(path: &Path) -> Vec<Row> {
    let Ok(raw) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let lines: Vec<String> = raw
        .lines()
        .map(|l| {
            // Line comments only: a `//` inside a string literal would be
            // misread, but no `exports!` row here has one, and the arms this
            // reads are names rather than text.
            let mut out = String::with_capacity(l.len());
            let mut in_str = false;
            let mut chars = l.chars();
            while let Some(c) = chars.next() {
                match c {
                    '"' => {
                        in_str = !in_str;
                        out.push(c);
                    }
                    '\\' if in_str => {
                        out.push(c);
                        if let Some(next) = chars.next() {
                            out.push(next);
                        }
                    }
                    '/' if !in_str && chars.as_str().starts_with('/') => break,
                    _ => out.push(c),
                }
            }
            out
        })
        .collect();

    let mut out = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        if !is_macro_line(line) {
            continue;
        }
        // The block's own `cfg` is the fallback for a row that carries none.
        let block_cfg = cfg_above(&lines, i);
        let end = block_end(&lines, i);
        for section in ["functions", "data", "constants"] {
            let Some(open) = (i..=end).find(|j| {
                // `functions {`, `data {`, `constants {` - the brace is its own
                // token, so this cannot be a whitespace-split equality.
                let mut words = lines[*j].split_whitespace();
                words.next() == Some(section) && words.next() == Some("{")
            }) else {
                continue;
            };
            for arm in arms_in(&lines, open, block_end(&lines, open)) {
                for name in arm.names {
                    out.push(Row {
                        // `functions` and `constants` publish `"_" + row`;
                        // `data` publishes the row verbatim, because
                        // `_OBJC_CLASS_$_NSView` is not a C identifier.
                        name: if section == "data" {
                            name
                        } else {
                            format!("_{name}")
                        },
                        cfg: arm.cfg.clone().or_else(|| block_cfg.clone()),
                        line: arm.line,
                        section: match section {
                            "functions" => "functions",
                            "data" => "data",
                            _ => "constants",
                        },
                    });
                }
            }
        }
    }
    out
}

/// Whether no two of `cfgs` can be true at the same time.
///
/// Only claims exclusivity for the one shape this tree actually uses - a
/// `target_os = "macos"` and its `not(..)` - and says "no" for anything else, so
/// a novel `cfg` is reported as a duplicate rather than assumed harmless.
fn exclusive(cfgs: &[Option<String>]) -> bool {
    if cfgs.len() < 2 || cfgs.iter().any(Option::is_none) {
        return false;
    }
    let the_one_shape =
        |c: &str| c == "target_os = \"macos\"" || c == "not(target_os = \"macos\")";
    cfgs.len() == 2
        && cfgs[0] != cfgs[1]
        && cfgs.iter().all(|c| the_one_shape(c.as_deref().unwrap_or("")))
}

// ------------------------------------------------------------------ the test

fn workspace() -> PathBuf {
    // CARGO_MANIFEST_DIR is `<repo>/crates/darwin`; the workspace is its parent.
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crates/darwin has a parent")
        .to_path_buf()
}

fn every_source_file() -> Vec<PathBuf> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        let mut entries: Vec<_> = entries.flatten().map(|e| e.path()).collect();
        entries.sort();
        for path in entries {
            if path.is_dir() {
                if path.file_name().is_some_and(|n| n == "target" || n == ".git") {
                    continue;
                }
                walk(&path, out);
            } else if path.extension().is_some_and(|e| e == "rs") {
                out.push(path);
            }
        }
    }
    let mut out = Vec::new();
    walk(&workspace(), &mut out);
    assert!(
        out.len() > 100,
        "only found {} .rs files under {}",
        out.len(),
        workspace().display(),
    );
    // This file, last: the parser's own test fixture is an `exports!` block in a
    // raw string, and counting it would put `_plain` and `_wrapped` in the
    // census. A census that reads its own test data is not a census. The path
    // is built from `CARGO_MANIFEST_DIR` rather than `file!()` because `file!()`
    // is relative to the workspace root while a test's working directory is the
    // package root, so canonicalising one against the other silently fails.
    let me = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/workspace_census.rs");
    out.retain(|p| p != &me);
    out
}

#[test]
fn no_exports_row_is_defined_twice_in_the_workspace() {
    let root = workspace();
    let mut by_name: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut total = 0usize;
    for file in every_source_file() {
        for row in rows_of(&file) {
            total += 1;
            let at = format!(
                "{}:{} ({})",
                file.strip_prefix(&root).unwrap_or(&file).display(),
                row.line,
                row.section,
            );
            by_name.entry(row.name).or_default().push(at);
        }
    }
    assert!(total > 2000, "the parser found only {total} rows; it is broken");

    let mut duplicates = Vec::new();
    let mut cfg_exclusive: Vec<String> = Vec::new();
    for (name, at) in &by_name {
        if at.len() < 2 {
            continue;
        }
        // Re-read the cfgs for this name, to tell a real duplicate from a pair
        // of mutually exclusive rows.
        let cfgs: Vec<Option<String>> = at
            .iter()
            .filter_map(|where_| {
                let (path, rest) = where_.split_once(':')?;
                let line: usize = rest.split_whitespace().next()?.parse().ok()?;
                rows_of(&root.join(path))
                    .into_iter()
                    .find(|r| r.line == line && r.name == *name)
                    .map(|r| r.cfg)
            })
            .collect();
        if exclusive(&cfgs) {
            cfg_exclusive.push(name.clone());
        } else {
            duplicates.push(Duplicate {
                name: name.clone(),
                at: at.clone(),
            });
        }
    }

    // A `cfg`-exclusive pair is not a duplicate: exactly one of the two rows is
    // compiled. `_pthread_create` and the two `pthread_get_stack*_np` are
    // `libc::` on Darwin and this tree's own off it.
    assert!(
        cfg_exclusive.len() >= 3,
        "the `cfg` classification stopped recognising mutually exclusive rows; \
         it found {} where there are 3 (`_pthread_create` and the two \
         `pthread_get_stack*_np`)",
        cfg_exclusive.len(),
    );

    let fresh: Vec<&Duplicate> = duplicates
        .iter()
        .filter(|d| !KNOWN.iter().any(|(name, _)| *name == d.name))
        .collect();
    let mut report = String::new();
    for d in &fresh {
        report.push_str(&format!("\n  {}\n", d.name));
        for at in &d.at {
            report.push_str(&format!("      {at}\n"));
        }
    }
    assert!(
        fresh.is_empty(),
        "{} name(s) are defined by more than one `exports!` row. `provider()` \
         takes the LAST (a `BTreeMap` insert) and `symbol()` takes the FIRST (a \
         `find_map`), so a guest import and a guest `dlsym` of the same name \
         reach different functions, and which one a guest gets is decided by how \
         the symbol was resolved rather than by what it means:{report}\n\
         all duplicates, `KNOWN` included: {:?}",
        fresh.len(),
        duplicates.iter().map(|d| d.name.as_str()).collect::<Vec<_>>(),
    );
}

#[test]
fn the_known_duplicates_are_still_duplicates() {
    // The other half of the allowlist: an entry that has been fixed must not sit
    // there suppressing the next one silently. Move it out of `KNOWN` when this
    // fails.
    let mut by_name: BTreeMap<String, usize> = BTreeMap::new();
    for file in every_source_file() {
        for row in rows_of(&file) {
            *by_name.entry(row.name).or_default() += 1;
        }
    }
    for (name, why) in KNOWN {
        assert_eq!(
            by_name.get(*name),
            Some(&2),
            "{name} is allowlisted as a duplicate ({why}) but is defined {} \
             time(s) now - drop it from `KNOWN`",
            by_name.get(*name).copied().unwrap_or(0),
        );
    }
}

// ------------------------------------------------------------- the parser

#[cfg(test)]
mod parser {
    use super::*;

    /// The parser is the load-bearing part of this file, so it is checked
    /// against text that has every shape the tree actually uses.
    #[test]
    fn the_parser_reads_the_shapes_this_tree_uses() {
        let src = r#"
            /// A doc comment mentioning "exports!" and `groups { }` by name.
            exports::exports! {
                functions {
                    "plain" => a,
                    "with_alias" | "second_alias" => b,
                    #[cfg(not(target_os = "macos"))]
                    "guarded" => c,
                    #[cfg(target_os = "macos")]
                    "guarded" => libc::guarded,
                    "wrapped" | "wrapped2"
                    | "wrapped3" => d,
                }
                data {
                    "_OBJC_CLASS_$_NSView" => view_cell(),
                }
                constants {
                    "in6addr_any" => ADDR,
                }
                groups {
                    inner,
                }
            }
        "#;
        let file = std::env::temp_dir().join(format!(
            "darwin-workspace-census-{}-{:p}.rs",
            std::process::id(),
            src
        ));
        std::fs::write(&file, src).unwrap();
        let rows = rows_of(&file);
        let _ = std::fs::remove_file(&file);

        let names: Vec<&str> = rows.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "_plain",
                "_with_alias",
                "_second_alias",
                "_guarded",
                "_guarded",
                "_wrapped",
                "_wrapped2",
                "_wrapped3",
                "_OBJC_CLASS_$_NSView",
                "_in6addr_any",
            ],
            "every row, in order, with the `functions`/`constants` underscore \
             added and the `data` row verbatim"
        );

        // The `cfg` has to be read from the line *above* the row, which is where
        // it is, and matched into a `not(..)`/`target_os` pair.
        let guarded: Vec<Option<String>> = rows
            .iter()
            .filter(|r| r.name == "_guarded")
            .map(|r| r.cfg.clone())
            .collect();
        assert_eq!(
            guarded,
            [
                Some("not(target_os = \"macos\")".to_string()),
                Some("target_os = \"macos\"".to_string()),
            ],
        );
        assert!(exclusive(&guarded), "a `not(..)`/`target_os` pair is exclusive");
        assert!(!exclusive(&[None, None]), "two unguarded rows are not");
        assert!(
            !exclusive(&[Some("target_os = \"macos\")".into()), Some("target_os = \"macos\")".into())]),
            "two identical cfgs are exclusive on no build"
        );
    }
}
