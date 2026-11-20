use std::ffi::{CStr, CString, c_char, c_int, c_void};

#[cfg(target_os = "macos")]
type GuestDirent = libc::dirent;
#[cfg(not(target_os = "macos"))]
type GuestDirent = crate::dirent::Dirent;
type GuestStat = crate::stat::Stat;

type ErrorFn = unsafe extern "C" fn(*const c_char, c_int) -> c_int;
type OpenDirFn = unsafe extern "C" fn(*const c_char) -> *mut c_void;
type ReadDirFn = unsafe extern "C" fn(*mut c_void) -> *mut GuestDirent;
type CloseDirFn = unsafe extern "C" fn(*mut c_void);
type StatFn = unsafe extern "C" fn(*const c_char, *mut GuestStat) -> c_int;

/// Darwin's public `glob_t`.
#[repr(C)]
struct Glob {
    pub gl_pathc: usize,
    pub gl_matchc: c_int,
    pub gl_offs: usize,
    pub gl_flags: c_int,
    pub gl_pathv: *mut *mut c_char,
    /// Darwin keeps the caller's `errfunc` in the structure. Written on entry
    /// like the real one, because a guest is entitled to read it back.
    pub gl_errfunc: Option<ErrorFn>,
    pub gl_closedir: Option<CloseDirFn>,
    pub gl_readdir: Option<ReadDirFn>,
    pub gl_opendir: Option<OpenDirFn>,
    pub gl_lstat: Option<StatFn>,
    pub gl_stat: Option<StatFn>,
}

#[cfg(target_pointer_width = "64")]
const _: () = {
    use std::mem::offset_of;

    assert!(size_of::<Glob>() == 88);
    assert!(offset_of!(Glob, gl_offs) == 16);
    assert!(offset_of!(Glob, gl_pathv) == 32);
    assert!(offset_of!(Glob, gl_errfunc) == 40);
    assert!(offset_of!(Glob, gl_stat) == 80);
};

mod darwin {
    #![allow(dead_code)]

    use std::ffi::c_int;

    pub const APPEND: c_int = 0x0001;
    pub const DOOFFS: c_int = 0x0002;
    pub const ERR: c_int = 0x0004;
    pub const MARK: c_int = 0x0008;
    pub const NOCHECK: c_int = 0x0010;
    pub const NOSORT: c_int = 0x0020;
    pub const ALTDIRFUNC: c_int = 0x0040;
    pub const BRACE: c_int = 0x0080;
    pub const MAGCHAR: c_int = 0x0100;
    pub const NOMAGIC: c_int = 0x0200;
    /// Historical, and a no-op on Darwin as well: escaping is on unless
    /// `GLOB_NOESCAPE` turns it off.
    pub const QUOTE: c_int = 0x0400;
    pub const LIMIT: c_int = 0x1000;
    pub const TILDE: c_int = 0x0800;
    pub const NOESCAPE: c_int = 0x2000;

    pub const NOSPACE: c_int = -1;
    pub const ABORTED: c_int = -2;
    pub const NOMATCH: c_int = -3;
    pub const NOSYS: c_int = -4;
}

/// What `GLOB_LIMIT` limits, with Darwin's own numbers.
///
/// Only consulted when the caller asks for the flag - without it both Darwin
/// and this walk run until the filesystem stops answering.
mod limit {
    pub const PATHS: usize = 65536;
    pub const READDIR: usize = 16384;
    pub const STAT: usize = 1024;
    /// `ARG_MAX`, which is what Darwin caps the total path bytes at.
    pub const BYTES: usize = 256 * 1024;
}

/// A ceiling on brace expansion, applied whether or not `GLOB_LIMIT` is set.
///
/// `{a,b}` nested ten deep is a thousand patterns and twenty deep is a million;
/// Darwin's limit is opt-in, but a guest that asks for one of those inside this
/// runtime takes the whole process down with it. Generous enough that no
/// hand-written pattern reaches it.
const EXPANSION_CEILING: usize = 16384;

/// One byte of the pattern, and whether a backslash protected it.
///
/// The first pass over the pattern does nothing but resolve escapes, which is
/// the order Darwin's `glob` works in too: a `\{` is therefore not a brace and
/// a `\*` not a wildcard, because brace expansion and pattern parsing both run
/// after this and both look at `quoted`.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Raw {
    byte: u8,
    quoted: bool,
}

impl Raw {
    /// `byte`, unescaped - the spelling that carries syntactic meaning.
    fn is(self, byte: u8) -> bool {
        !self.quoted && self.byte == byte
    }
}

/// One parsed pattern element.
#[derive(Clone)]
enum Token {
    /// Matches this byte and nothing else, escaped or not.
    Literal(u8),
    /// `?`
    Any,
    /// `*`
    Star,
    /// `[...]`
    Set { negated: bool, items: Vec<SetItem> },
    /// An unescaped `/`, which no wildcard ever matches.
    Separator,
}

#[derive(Clone, Copy)]
enum SetItem {
    One(u8),
    Range(u8, u8),
}

impl Token {
    fn is_magic(&self) -> bool {
        matches!(self, Token::Any | Token::Star | Token::Set { .. })
    }
}

/// Resolve the backslashes, once, before anything else looks at the pattern.
fn protect(pattern: &[u8], escaping: bool) -> Vec<Raw> {
    let mut out = Vec::with_capacity(pattern.len());
    let mut rest = pattern.iter().copied();
    while let Some(byte) = rest.next() {
        match byte == b'\\' && escaping {
            // A trailing backslash stands for itself, which is what Darwin does
            // with it rather than reading past the terminator.
            true => out.push(match rest.next() {
                Some(next) => Raw {
                    byte: next,
                    quoted: true,
                },
                None => Raw {
                    byte: b'\\',
                    quoted: true,
                },
            }),
            false => out.push(Raw {
                byte,
                quoted: false,
            }),
        }
    }
    out
}

/// The `}` that closes the `{` at `open`, skipping nested braces and bracket
/// expressions.
fn closing_brace(pattern: &[Raw], open: usize) -> Option<usize> {
    let mut depth = 0usize;
    let mut at = open + 1;
    while at < pattern.len() {
        let c = pattern[at];
        if c.is(b'[') {
            // Everything between brackets is literal, commas and braces
            // included - but only if the bracket expression is closed at all.
            if let Some(end) = pattern[at + 1..].iter().position(|c| c.is(b']')) {
                at += end + 2;
                continue;
            }
        } else if c.is(b'{') {
            depth += 1;
        } else if c.is(b'}') {
            if depth == 0 {
                return Some(at);
            }
            depth -= 1;
        }
        at += 1;
    }
    None
}

/// Every csh-style brace expansion of `pattern`, left to right.
///
/// Darwin globs each expansion as it produces it; the list is built first here
/// because the two are the same walk in the same order, and a list is what lets
/// the caller tell "no expansion matched" from "one did".
fn expansions(pattern: &[Raw], out: &mut Vec<Vec<Raw>>) -> Result<(), c_int> {
    if out.len() >= EXPANSION_CEILING {
        return Err(darwin::NOSPACE);
    }
    let Some(open) = pattern.iter().position(|c| c.is(b'{')) else {
        out.push(pattern.to_vec());
        return Ok(());
    };
    // An unbalanced `{` is a literal one. Darwin globs only the part before it,
    // which drops the rest of the pattern on the floor; the whole pattern is
    // the answer the caller asked for.
    let Some(close) = closing_brace(pattern, open) else {
        out.push(pattern.to_vec());
        return Ok(());
    };

    let mut depth = 0usize;
    let mut piece = open + 1;
    let mut at = open + 1;
    while at <= close {
        let c = pattern[at];
        if c.is(b'[') {
            if let Some(end) = pattern[at + 1..].iter().position(|c| c.is(b']')) {
                at += end + 2;
                continue;
            }
        } else if c.is(b'{') {
            depth += 1;
        } else if depth > 0 && c.is(b'}') {
            depth -= 1;
        } else if depth == 0 && (at == close || c.is(b',')) {
            let mut built = pattern[..open].to_vec();
            built.extend_from_slice(&pattern[piece..at]);
            built.extend_from_slice(&pattern[close + 1..]);
            // Nested braces in either the piece or the tail are expanded by the
            // same walk over what this one produced.
            expansions(&built, out)?;
            piece = at + 1;
        }
        at += 1;
    }
    Ok(())
}

/// `~` and `~user` as the directory they name.
///
/// The home directory is substituted *protected*, so a metacharacter in it is
/// part of the path rather than part of the pattern: the tilde stands for a
/// directory that exists, not for something to match against.
fn tilde(pattern: &[Raw], flags: c_int) -> Vec<Raw> {
    if flags & darwin::TILDE == 0 || !pattern.first().is_some_and(|c| c.is(b'~')) {
        return pattern.to_vec();
    }
    let end = pattern
        .iter()
        .position(|c| c.is(b'/'))
        .unwrap_or(pattern.len());
    let name: Vec<u8> = pattern[1..end].iter().map(|c| c.byte).collect();
    let user = match name.is_empty() {
        true => None,
        false => match CString::new(name) {
            Ok(name) => Some(name),
            Err(_) => return pattern.to_vec(),
        },
    };
    // An unknown user leaves the tilde alone, which is what a shell does too.
    let Some(home) = home(user.as_deref()) else {
        return pattern.to_vec();
    };

    let mut out: Vec<Raw> = home
        .as_bytes()
        .iter()
        .map(|byte| Raw {
            byte: *byte,
            quoted: true,
        })
        .collect();
    out.extend_from_slice(&pattern[end..]);
    out
}

fn home(user: Option<&CStr>) -> Option<CString> {
    crate::passwd::home(user)
}

/// The pattern as tokens, and whether it held a metacharacter.
///
/// A `[` that opens nothing is one of the literal ones, and does not count as
/// magic - same as Darwin, and the difference matters to `GLOB_NOMAGIC`.
fn parse(pattern: &[Raw]) -> (Vec<Token>, bool) {
    let mut out = Vec::with_capacity(pattern.len());
    let mut magic = false;
    let mut at = 0;
    while at < pattern.len() {
        let c = pattern[at];
        if c.quoted {
            out.push(Token::Literal(c.byte));
            at += 1;
            continue;
        }
        match c.byte {
            b'/' => {
                out.push(Token::Separator);
                at += 1;
            }
            b'?' => {
                out.push(Token::Any);
                magic = true;
                at += 1;
            }
            b'*' => {
                out.push(Token::Star);
                magic = true;
                // Collapsed here so that `***` cannot cost anything at match
                // time.
                while pattern.get(at).is_some_and(|c| c.is(b'*')) {
                    at += 1;
                }
            }
            b'[' => match set(pattern, at) {
                Some((token, next)) => {
                    out.push(token);
                    magic = true;
                    at = next;
                }
                None => {
                    out.push(Token::Literal(b'['));
                    at += 1;
                }
            },
            byte => {
                out.push(Token::Literal(byte));
                at += 1;
            }
        }
    }
    (out, magic)
}

/// The bracket expression opening at `at`, and the index just past it.
///
/// `None` for a `[` that never closes, which is then a literal one. Only `!`
/// negates: Darwin's `glob` has no `^` spelling, and reading one as negation
/// would answer the complement of what the guest asked for.
fn set(pattern: &[Raw], at: usize) -> Option<(Token, usize)> {
    let mut first = at + 1;
    let negated = pattern.get(first).is_some_and(|c| c.is(b'!'));
    if negated {
        first += 1;
    }
    // The member right after the bracket is taken literally, which is how a
    // `]` becomes a member: the search for the closing one starts past it.
    let close = first + 1 + pattern.get(first + 1..)?.iter().position(|c| c.is(b']'))?;

    let mut items = Vec::new();
    let mut scan = first;
    while scan < close {
        let low = pattern[scan].byte;
        scan += 1;
        // `a-]` is not a range: the `-` before the closing bracket is a member.
        match pattern.get(scan).is_some_and(|c| c.is(b'-')) && scan + 1 < close {
            true => {
                items.push(SetItem::Range(low, pattern[scan + 1].byte));
                scan += 2;
            }
            false => items.push(SetItem::One(low)),
        }
    }
    Some((Token::Set { negated, items }, close + 1))
}

/// The tokens as the plain text they came from - escapes resolved, wildcards
/// spelled the way they were parsed.
///
/// What `GLOB_NOCHECK` hands back when nothing matched.
fn literal(pattern: &[Token]) -> Vec<u8> {
    let mut out = Vec::with_capacity(pattern.len());
    for token in pattern {
        match token {
            Token::Literal(byte) => out.push(*byte),
            Token::Any => out.push(b'?'),
            Token::Star => out.push(b'*'),
            Token::Separator => out.push(b'/'),
            Token::Set { negated, items } => {
                out.push(b'[');
                if *negated {
                    out.push(b'!');
                }
                for item in items {
                    match item {
                        SetItem::One(byte) => out.push(*byte),
                        SetItem::Range(low, high) => out.extend_from_slice(&[*low, b'-', *high]),
                    }
                }
                out.push(b']');
            }
        }
    }
    out
}

/// Does one name match one segment's tokens?
///
/// Backtracking through a single remembered `*` rather than recursing, so a
/// pattern like `*a*a*a*b` costs the product of the two lengths instead of
/// growing exponentially in the number of stars.
fn matches(name: &[u8], pattern: &[Token]) -> bool {
    let (mut n, mut p) = (0usize, 0usize);
    let mut star: Option<(usize, usize)> = None;
    loop {
        let advanced = match pattern.get(p) {
            Some(Token::Star) => {
                star = Some((p, n));
                p += 1;
                true
            }
            Some(token) if n < name.len() && matches_byte(token, name[n]) => {
                p += 1;
                n += 1;
                true
            }
            Some(_) => false,
            None => {
                if n == name.len() {
                    return true;
                }
                false
            }
        };
        if advanced {
            continue;
        }
        // The last `*` swallows one more byte and the tail is tried again.
        let Some((at, consumed)) = star else {
            return false;
        };
        if consumed >= name.len() {
            return false;
        }
        star = Some((at, consumed + 1));
        p = at + 1;
        n = consumed + 1;
    }
}

fn matches_byte(token: &Token, byte: u8) -> bool {
    match token {
        Token::Literal(want) => *want == byte,
        Token::Any => true,
        Token::Set { negated, items } => {
            let inside = items.iter().any(|item| match item {
                SetItem::One(one) => *one == byte,
                SetItem::Range(low, high) => *low <= byte && byte <= *high,
            });
            inside != *negated
        }
        // A separator is a segment boundary and never reaches a name, and a
        // star is handled before any byte is looked at.
        Token::Separator | Token::Star => false,
    }
}

/// An open directory, whoever opened it.
enum Handle {
    Host(*mut libc::DIR),
    Guest(*mut c_void),
}

unsafe fn open_host(path: &CStr) -> *mut libc::DIR {
    unsafe { crate::dirent::opendir(path.as_ptr()) }
}

/// The next name in a host handle, as the guest lists the directory: with the
/// prefix's mount points, which this crate's `readdir` adds.
unsafe fn read_host(dir: *mut libc::DIR) -> Option<Vec<u8>> {
    let entry = unsafe { crate::dirent::next_listed(dir) }.ok()??;
    Some(
        unsafe { CStr::from_ptr(entry.name.as_ptr()) }
            .to_bytes()
            .to_vec(),
    )
}

unsafe fn close_host(dir: *mut libc::DIR) {
    unsafe { crate::dirent::closedir(dir) };
}

#[cfg(not(target_os = "macos"))]
unsafe fn guest_entry_name(entry: *mut GuestDirent) -> Vec<u8> {
    unsafe { CStr::from_ptr((&raw const (*entry).name).cast()) }
        .to_bytes()
        .to_vec()
}

#[cfg(target_os = "macos")]
unsafe fn guest_entry_name(entry: *mut GuestDirent) -> Vec<u8> {
    unsafe { CStr::from_ptr((&raw const (*entry).d_name).cast()) }
        .to_bytes()
        .to_vec()
}

fn guest_mode(stat: &GuestStat) -> u32 {
    u32::from(stat.mode)
}

/// The caller's `GLOB_ALTDIRFUNC` hooks, all five or none.
struct Hooks {
    opendir: OpenDirFn,
    readdir: ReadDirFn,
    closedir: CloseDirFn,
    lstat: StatFn,
    stat: StatFn,
}

/// One `glob` call in progress.
struct Walk {
    flags: c_int,
    errfunc: Option<ErrorFn>,
    hooks: Option<Hooks>,
    /// Whether any expansion held a metacharacter, for `GLOB_MAGCHAR`.
    magic: bool,
    pathv: *mut *mut c_char,
    pathc: usize,
    offs: usize,
    matchc: c_int,
    reads: usize,
    stats: usize,
    bytes: usize,
}

impl Walk {
    fn has(&self, flag: c_int) -> bool {
        self.flags & flag != 0
    }

    /// `GLOB_NOSPACE` with `errno` cleared, which is how Darwin reports a limit
    /// rather than an allocation failure.
    fn exhausted(&self) -> c_int {
        crate::errno::set(0);
        darwin::NOSPACE
    }

    /// Charge one directory read, or refuse if `GLOB_LIMIT` says enough.
    fn charge_read(&mut self) -> Result<(), c_int> {
        self.reads += 1;
        match self.has(darwin::LIMIT) && self.reads > limit::READDIR {
            true => Err(self.exhausted()),
            false => Ok(()),
        }
    }

    fn charge_stat(&mut self) -> Result<(), c_int> {
        self.stats += 1;
        match self.has(darwin::LIMIT) && self.stats > limit::STAT {
            true => Err(self.exhausted()),
            false => Ok(()),
        }
    }

    /// Hand `path` and its `errno` to the caller's `gl_errfunc`.
    ///
    /// A non-zero answer from it, or `GLOB_ERR`, ends the whole walk. The
    /// number the callback sees is Darwin's, not this host's.
    fn report(&self, path: &CStr, error: i32) -> Result<(), c_int> {
        if let Some(errfunc) = self.errfunc {
            // The callback is guest code entered during the call, so it needs
            // the guest's thread pointer - see `crate::callback`.
            let stop = unsafe {
                crate::call_as_darwin!(errfunc(path.as_ptr(), crate::errno::from_host(error)))
            };
            if stop != 0 {
                return Err(darwin::ABORTED);
            }
        }
        match self.has(darwin::ERR) {
            true => Err(darwin::ABORTED),
            false => Ok(()),
        }
    }

    fn open(&mut self, path: &CStr) -> Option<Handle> {
        match &self.hooks {
            // SAFETY: the guest's own hook, called with a NUL-terminated path.
            Some(hooks) => {
                let dir = unsafe { crate::call_as_darwin!((hooks.opendir)(path.as_ptr())) };
                (!dir.is_null()).then_some(Handle::Guest(dir))
            }
            None => {
                // This crate's `opendir` rather than libc's: a Mac filesystem
                // is case-insensitive, and the literal part of a pattern is
                // spelled by the guest.
                // SAFETY: a NUL-terminated path.
                let dir = unsafe { open_host(path) };
                (!dir.is_null()).then_some(Handle::Host(dir))
            }
        }
    }

    /// The next entry's name, or `None` at the end of the directory.
    fn read(&mut self, dir: &Handle) -> Result<Option<Vec<u8>>, c_int> {
        self.charge_read()?;
        let name = match (dir, &self.hooks) {
            (Handle::Guest(dir), Some(hooks)) => {
                // SAFETY: the guest's hook, handed the handle its own `opendir`
                // returned. What it answers is a Darwin `struct dirent`, read
                // at Darwin's offsets.
                let entry = unsafe { crate::call_as_darwin!((hooks.readdir)(*dir)) };
                if entry.is_null() {
                    return Ok(None);
                }
                unsafe { guest_entry_name(entry) }
            }
            (Handle::Host(dir), _) => {
                // The end of a directory and a failure are both a null return,
                // told apart by `errno` - which is only meaningful if it was
                // cleared first.
                crate::errno::set(0);
                // SAFETY: a handle this walk opened and has not closed.
                match unsafe { read_host(*dir) } {
                    Some(name) => name,
                    None => return Ok(None),
                }
            }
            // A guest handle with no hooks cannot be built: `hooks` is fixed
            // for the whole call.
            (Handle::Guest(_), None) => return Ok(None),
        };
        Ok(Some(name))
    }

    fn close(&self, dir: Handle) {
        match (dir, &self.hooks) {
            // SAFETY: each handle is closed once, by whoever opened it.
            (Handle::Guest(dir), Some(hooks)) => unsafe {
                crate::call_as_darwin!((hooks.closedir)(dir))
            },
            (Handle::Host(dir), _) => unsafe { close_host(dir) },
            (Handle::Guest(_), None) => {}
        }
    }

    /// `path`'s mode bits, or `None` if it cannot be stat'ed.
    fn mode(&mut self, path: &CStr, follow: bool) -> Result<Option<u32>, c_int> {
        self.charge_stat()?;
        if let Some(hooks) = &self.hooks {
            let ask = match follow {
                true => hooks.stat,
                false => hooks.lstat,
            };
            let mut out: GuestStat = unsafe { std::mem::zeroed() };
            // SAFETY: the guest's hook, filling a Darwin `struct stat` it is
            // handed a pointer to.
            let status = unsafe { crate::call_as_darwin!(ask(path.as_ptr(), &raw mut out)) };
            return Ok((status == 0).then_some(guest_mode(&out)));
        }

        let mut out: libc::stat = unsafe { std::mem::zeroed() };
        // SAFETY: a NUL-terminated path and a structure this frame owns.
        let ask = |path: *const c_char, out: *mut libc::stat| unsafe {
            match follow {
                true => libc::stat(path, out),
                false => libc::lstat(path, out),
            }
        };
        let host = prefix::ns::to_host(path);
        if ask(host.as_ptr(), &raw mut out) == 0 {
            return Ok(Some(out.st_mode as u32));
        }
        // As `open`: the guest may have spelled a literal component in the
        // wrong case. Only a missing file is worth a second look - a permission
        // failure is about this file.
        if crate::errno::get() != libc::ENOENT {
            return Ok(None);
        }
        let Some(found) = prefix::ns::fold(&host) else {
            return Ok(None);
        };
        Ok((ask(found.as_ptr(), &raw mut out) == 0).then_some(out.st_mode as u32))
    }

    /// Walk `pattern`, with `path` holding what has been matched so far.
    ///
    /// Darwin's `glob2`: literal segments are appended without touching the
    /// filesystem, and the first segment with a metacharacter in it opens the
    /// directory built so far.
    fn segments(&mut self, path: &mut Vec<u8>, pattern: &[Token]) -> Result<(), c_int> {
        let mut pattern = pattern;
        loop {
            if pattern.is_empty() {
                return self.found(path);
            }
            let end = pattern
                .iter()
                .position(|token| matches!(token, Token::Separator))
                .unwrap_or(pattern.len());
            let segment = &pattern[..end];
            if segment.iter().any(Token::is_magic) {
                return self.expand(path, segment, &pattern[end..]);
            }
            path.extend_from_slice(&literal(segment));
            pattern = &pattern[end..];
            // Every separator, so that `a//b` stays `a//b`.
            while matches!(pattern.first(), Some(Token::Separator)) {
                path.push(b'/');
                pattern = &pattern[1..];
            }
        }
    }

    /// The pattern ran out: `path` is a match if it exists.
    fn found(&mut self, path: &mut Vec<u8>) -> Result<(), c_int> {
        let Ok(spelled) = CString::new(path.clone()) else {
            return Ok(());
        };
        let Some(mode) = self.mode(&spelled, false)? else {
            return Ok(());
        };
        if self.has(darwin::MARK) && path.last() != Some(&b'/') {
            // A symlink to a directory is marked as one, which costs the second
            // stat Darwin also pays here.
            let directory = is_directory(mode)
                || (is_link(mode) && self.mode(&spelled, true)?.is_some_and(is_directory));
            if directory {
                path.push(b'/');
            }
        }
        self.matchc = self.matchc.saturating_add(1);
        self.extend(path)
    }

    /// Read the directory `path` names and try `segment` against each entry.
    ///
    /// Darwin's `glob3`.
    fn expand(
        &mut self,
        path: &mut Vec<u8>,
        segment: &[Token],
        rest: &[Token],
    ) -> Result<(), c_int> {
        let base = path.len();
        // An empty path is the current directory, which is what the guest
        // asked about when the pattern has no leading component.
        let Ok(directory) = CString::new(match path.is_empty() {
            true => b".".to_vec(),
            false => path.clone(),
        }) else {
            return Ok(());
        };
        let Some(dir) = self.open(&directory) else {
            // Unreadable is not a failure of the glob, only of this directory -
            // unless the caller says otherwise.
            return self.report(&directory, crate::errno::get());
        };

        // A leading `.` is matched only by a pattern that spells one. Darwin
        // requires a *bare* dot and so lets `\.*` match nothing at all, which
        // is a trap rather than a rule worth keeping.
        let hidden_ok = matches!(segment.first(), Some(Token::Literal(b'.')));
        let mut result = Ok(());
        loop {
            let name = match self.read(&dir) {
                Ok(Some(name)) => name,
                Ok(None) => break,
                Err(error) => {
                    result = Err(error);
                    break;
                }
            };
            if name.first() == Some(&b'.') && !hidden_ok {
                continue;
            }
            if !matches(&name, segment) {
                continue;
            }
            path.truncate(base);
            path.extend_from_slice(&name);
            result = self.segments(path, rest);
            if result.is_err() {
                break;
            }
        }
        self.close(dir);
        path.truncate(base);
        result
    }

    /// Append one path to `gl_pathv`, growing it by one slot and its
    /// terminator.
    ///
    /// The vector and the strings come from the host's `malloc`, which is the
    /// allocator `free` and [`globfree`] hand them back to.
    fn extend(&mut self, path: &[u8]) -> Result<(), c_int> {
        self.bytes += path.len() + 1;
        if self.has(darwin::LIMIT) && (self.pathc >= limit::PATHS || self.bytes > limit::BYTES) {
            self.discard();
            return Err(self.exhausted());
        }

        let slots = self.pathc + self.offs + 2;
        let Some(size) = slots.checked_mul(size_of::<*mut c_char>()) else {
            self.discard();
            return Err(darwin::NOSPACE);
        };
        let first = self.pathv.is_null();
        // SAFETY: `pathv` is null or this walk's own allocation, and `size`
        // covers every slot written below.
        let pathv = unsafe {
            match first {
                true => libc::malloc(size),
                false => libc::realloc(self.pathv.cast(), size),
            }
        }
        .cast::<*mut c_char>();
        if pathv.is_null() {
            self.discard();
            return Err(darwin::NOSPACE);
        }
        self.pathv = pathv;
        if first {
            // The slots `GLOB_DOOFFS` reserves are the caller's to fill, and
            // are null until then.
            for slot in 0..self.offs {
                // SAFETY: inside the allocation, which has `offs + 2` slots.
                unsafe { *pathv.add(slot) = std::ptr::null_mut() };
            }
        }

        // SAFETY: `path.len() + 1` bytes, filled immediately below.
        let copy = unsafe { libc::malloc(path.len() + 1) }.cast::<c_char>();
        if copy.is_null() {
            self.discard();
            return Err(darwin::NOSPACE);
        }
        // SAFETY: the allocation is one byte longer than `path`, and the two do
        // not overlap.
        unsafe {
            std::ptr::copy_nonoverlapping(path.as_ptr().cast::<c_char>(), copy, path.len());
            *copy.add(path.len()) = 0;
            *pathv.add(self.offs + self.pathc) = copy;
            *pathv.add(self.offs + self.pathc + 1) = std::ptr::null_mut();
        }
        self.pathc += 1;
        Ok(())
    }

    /// Free everything this call has collected.
    ///
    /// Only on an allocation failure, and the whole vector goes - including
    /// what a previous `GLOB_APPEND` call put there, which Darwin also frees.
    /// Leaving a half-grown vector behind is what turns one failure into a
    /// double free later.
    fn discard(&mut self) {
        // SAFETY: every pointer here came from this walk's own `malloc`.
        unsafe {
            for at in 0..self.pathc {
                let path = *self.pathv.add(self.offs + at);
                if !path.is_null() {
                    libc::free(path.cast());
                }
            }
            if !self.pathv.is_null() {
                libc::free(self.pathv.cast());
            }
        }
        self.pathv = std::ptr::null_mut();
        self.pathc = 0;
    }

    /// One expansion of the pattern, walked and sorted.
    ///
    /// Darwin's `glob0`.
    fn one(&mut self, pattern: &[Raw]) -> Result<(), c_int> {
        let (tokens, magic) = parse(&tilde(pattern, self.flags));
        self.magic |= magic;

        let before = self.pathc;
        self.segments(&mut Vec::new(), &tokens)?;
        if self.pathc == before {
            // `GLOB_NOCHECK` answers with the pattern itself, and
            // `GLOB_NOMAGIC` does the same only for a pattern that could not
            // have matched anything else.
            if self.has(darwin::NOCHECK) || (self.has(darwin::NOMAGIC) && !magic) {
                return self.extend(&literal(&tokens));
            }
            return Err(darwin::NOMATCH);
        }
        if !self.has(darwin::NOSORT) {
            // This expansion's own matches, not the whole vector: an appended
            // chunk is sorted against itself on Darwin too. Bytewise, because
            // Darwin's comparator is `strcmp` - a locale-aware order would put
            // the paths somewhere the guest does not expect.
            // SAFETY: `pathc - before` live slots starting past the reserved
            // ones, all filled by `extend`.
            let chunk = unsafe {
                std::slice::from_raw_parts_mut(
                    self.pathv.add(self.offs + before),
                    self.pathc - before,
                )
            };
            chunk.sort_unstable_by(|a, b| unsafe { CStr::from_ptr(*a).cmp(CStr::from_ptr(*b)) });
        }
        Ok(())
    }
}

/// The file-type bits, which are the same numbers on both platforms - a mode
/// read out of a guest's `struct stat` and one read out of the host's are
/// compared against the same constants. Asserted rather than assumed.
mod kind {
    pub const MASK: u32 = 0o170000;
    pub const DIRECTORY: u32 = 0o040000;
    pub const LINK: u32 = 0o120000;

    const _: () = {
        assert!(MASK == libc::S_IFMT as u32);
        assert!(DIRECTORY == libc::S_IFDIR as u32);
        assert!(LINK == libc::S_IFLNK as u32);
    };
}

fn is_directory(mode: u32) -> bool {
    mode & kind::MASK == kind::DIRECTORY
}

fn is_link(mode: u32) -> bool {
    mode & kind::MASK == kind::LINK
}

/// `int glob(const char *pattern, int flags, int (*errfunc)(const char *, int), glob_t *pglob)`
///
/// # Safety
///
/// `pattern` is a C string, `out` points at a writable Darwin `glob_t` - already
/// filled by a previous call when `GLOB_APPEND` is set - and `err`, if given, is
/// callable. That is `glob`'s own contract.
unsafe extern "C" fn glob(
    pattern: *const c_char,
    flags: c_int,
    err: Option<ErrorFn>,
    out: *mut Glob,
) -> c_int {
    if pattern.is_null() || out.is_null() {
        // Darwin faults here. Inside this runtime a fault is the emulator's
        // crash rather than the guest's, so it is reported instead.
        crate::errno::set(libc::EFAULT);
        return darwin::ABORTED;
    }

    // SAFETY: the caller's structure, whose three result fields are theirs to
    // have filled only when `GLOB_APPEND` says so.
    let (pathv, pathc, offs) = unsafe {
        (*out).gl_flags = flags & !darwin::MAGCHAR;
        (*out).gl_errfunc = err;
        (*out).gl_matchc = 0;
        if flags & darwin::APPEND == 0 {
            (*out).gl_pathc = 0;
            (*out).gl_pathv = std::ptr::null_mut();
            if flags & darwin::DOOFFS == 0 {
                (*out).gl_offs = 0;
            }
        }
        ((*out).gl_pathv, (*out).gl_pathc, (*out).gl_offs)
    };

    let hooks = match flags & darwin::ALTDIRFUNC != 0 {
        // SAFETY: as above; the five hooks are the caller's own pointers.
        true => unsafe {
            match (
                (*out).gl_opendir,
                (*out).gl_readdir,
                (*out).gl_closedir,
                (*out).gl_lstat,
                (*out).gl_stat,
            ) {
                (Some(opendir), Some(readdir), Some(closedir), Some(lstat), Some(stat)) => {
                    Some(Hooks {
                        opendir,
                        readdir,
                        closedir,
                        lstat,
                        stat,
                    })
                }
                // Walking the real filesystem instead would silently ignore the
                // hooks the caller asked to be used.
                _ => return darwin::NOSYS,
            }
        },
        false => None,
    };

    let mut walk = Walk {
        flags,
        errfunc: err,
        hooks,
        magic: false,
        pathv,
        pathc,
        offs,
        matchc: 0,
        reads: 0,
        stats: 0,
        bytes: 0,
    };

    // SAFETY: the caller's NUL-terminated pattern.
    let text = unsafe { CStr::from_ptr(pattern) }.to_bytes().to_vec();
    let raw = protect(&text, flags & darwin::NOESCAPE == 0);

    let mut patterns = Vec::new();
    let mut status = match flags & darwin::BRACE != 0 {
        true => expansions(&raw, &mut patterns),
        false => {
            patterns.push(raw);
            Ok(())
        }
    };

    // An expansion that matches nothing is not a failure as long as another one
    // matched - which is the answer both glibc and a shell give, and the only
    // one that makes `{found,missing}` useful.
    let mut matched = false;
    if status.is_ok() {
        for pattern in &patterns {
            match walk.one(pattern) {
                Ok(()) => matched = true,
                Err(darwin::NOMATCH) => {}
                Err(error) => {
                    status = Err(error);
                    break;
                }
            }
        }
    }

    unsafe {
        (*out).gl_pathc = walk.pathc;
        (*out).gl_pathv = walk.pathv;
        (*out).gl_offs = walk.offs;
        // Darwin counts the matches *this* call made, not the paths in the
        // vector - which are two different numbers under `GLOB_APPEND`.
        (*out).gl_matchc = walk.matchc;
        if walk.magic {
            (*out).gl_flags |= darwin::MAGCHAR;
        }
    }

    match status {
        Err(error) => error,
        Ok(()) if matched => 0,
        Ok(()) => darwin::NOMATCH,
    }
}

/// `void globfree(glob_t *pglob)`
///
/// # Safety
///
/// `out` is a `glob_t` a previous [`glob`] filled, or a zeroed one.
unsafe extern "C" fn globfree(out: *mut Glob) {
    if out.is_null() {
        return;
    }
    // SAFETY: the caller's structure; a null `gl_pathv` is the documented
    // "nothing to free", so a never-globbed, zeroed one is safe to pass.
    unsafe {
        if (*out).gl_pathv.is_null() {
            return;
        }
        for at in 0..(*out).gl_pathc {
            let path = *(*out).gl_pathv.add((*out).gl_offs + at);
            if !path.is_null() {
                libc::free(path.cast());
            }
        }
        libc::free((*out).gl_pathv.cast());
        // Darwin nulls this. Leaving the caller's copy pointing at freed memory
        // is how a second `globfree` becomes a double free.
        (*out).gl_pathv = std::ptr::null_mut();
    }
}

exports::exports! {
    functions {
        "glob" => glob,
        "globfree" => globfree,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::CString;

    /// A directory of three files, named per test so the suite can run in
    /// parallel.
    fn fixture(test: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("rosemary-glob-{test}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("a temporary directory");
        for name in ["beta", "alpha", "gamma"] {
            std::fs::write(dir.join(name), b"x").expect("a file");
        }
        dir
    }

    fn pattern(dir: &std::path::Path, tail: &str) -> CString {
        CString::new(format!("{}/{tail}", dir.display())).expect("no interior NUL")
    }

    fn zeroed() -> Glob {
        Glob {
            gl_pathc: 0,
            gl_matchc: 0,
            gl_offs: 0,
            gl_flags: 0,
            gl_pathv: std::ptr::null_mut(),
            gl_errfunc: None,
            gl_closedir: None,
            gl_readdir: None,
            gl_opendir: None,
            gl_lstat: None,
            gl_stat: None,
        }
    }

    /// The matched paths, as basenames - the slots `gl_offs` reserves are not
    /// matches and are checked separately.
    fn matched(found: &Glob) -> Vec<String> {
        (0..found.gl_pathc)
            .map(|i| {
                // SAFETY: `glob` reported `gl_pathc` paths from `gl_offs` on.
                let path = unsafe { *found.gl_pathv.add(found.gl_offs + i) };
                assert!(!path.is_null());
                let full = unsafe { CStr::from_ptr(path) }.to_string_lossy();
                full.rsplit('/').next().expect("a basename").to_owned()
            })
            .collect()
    }

    /// The matched paths, whole, for the tests where the directory part is the
    /// point.
    fn full(found: &Glob) -> Vec<String> {
        (0..found.gl_pathc)
            .map(|i| {
                // SAFETY: as `matched`.
                let path = unsafe { *found.gl_pathv.add(found.gl_offs + i) };
                unsafe { CStr::from_ptr(path) }
                    .to_string_lossy()
                    .into_owned()
            })
            .collect()
    }

    /// Glob `tail` inside `dir` and answer the basenames, asserting success.
    fn names(dir: &std::path::Path, tail: &str, flags: c_int) -> Vec<String> {
        let mut found = zeroed();
        // SAFETY: a live pattern and structure, freed before returning.
        unsafe {
            let status = glob(pattern(dir, tail).as_ptr(), flags, None, &raw mut found);
            assert_eq!(status, 0, "{tail}");
            let names = matched(&found);
            globfree(&raw mut found);
            names
        }
    }

    #[test]
    fn matches_every_file_and_frees_the_vector() {
        let dir = fixture("all");
        let mut found = zeroed();
        // SAFETY: a live pattern and structure, freed before the test ends.
        unsafe {
            assert_eq!(
                glob(pattern(&dir, "*").as_ptr(), 0, None, &raw mut found),
                0
            );
            assert_eq!(found.gl_pathc, 3);
            assert_eq!(found.gl_matchc, 3);
            // Sorted by default, which is the difference `GLOB_NOSORT` makes.
            assert_eq!(matched(&found), ["alpha", "beta", "gamma"]);
            globfree(&raw mut found);
        }
        // Nulled on the way out, so a second `globfree` is not a double free.
        assert!(found.gl_pathv.is_null());
        // SAFETY: a structure whose `gl_pathv` is null, which is the documented
        // "nothing to free".
        unsafe { globfree(&raw mut found) };
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn append_counts_only_this_calls_matches() {
        let dir = fixture("append");
        let mut found = zeroed();
        // SAFETY: as above; the second call is told the first one's result.
        unsafe {
            assert_eq!(
                glob(pattern(&dir, "alpha").as_ptr(), 0, None, &raw mut found),
                0
            );
            assert_eq!((found.gl_pathc, found.gl_matchc), (1, 1));
            assert_eq!(
                glob(
                    pattern(&dir, "[bg]*").as_ptr(),
                    darwin::APPEND,
                    None,
                    &raw mut found
                ),
                0
            );
            // Three paths in the vector, two of them added by this call - which
            // is the whole difference between Darwin's two counters.
            assert_eq!(found.gl_pathc, 3);
            assert_eq!(found.gl_matchc, 2);
            assert_eq!(matched(&found), ["alpha", "beta", "gamma"]);
            globfree(&raw mut found);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_unmatched_pattern_is_darwins_negative_code() {
        let dir = fixture("nomatch");
        let mut found = zeroed();
        // SAFETY: a live pattern and structure; nothing is allocated on failure.
        let status = unsafe { glob(pattern(&dir, "delta*").as_ptr(), 0, None, &raw mut found) };
        // A guest that only tests for zero cannot tell the sign, and one that
        // switches on the code is sent somewhere else entirely by glibc's 3.
        assert_eq!(status, darwin::NOMATCH);
        assert_eq!(status, -3);
        assert!(darwin::NOSPACE == -1 && darwin::ABORTED == -2);
        assert!(found.gl_pathv.is_null() && found.gl_pathc == 0);
        // SAFETY: nothing was allocated, so `gl_pathv` is null.
        unsafe { globfree(&raw mut found) };
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn nocheck_answers_with_the_pattern_itself() {
        let dir = fixture("nocheck");
        assert_eq!(names(&dir, "delta*", darwin::NOCHECK), ["delta*"]);
        // `GLOB_NOMAGIC` is `GLOB_NOCHECK` for a pattern with no wildcard in
        // it, and plain `GLOB_NOMATCH` for one that has.
        assert_eq!(names(&dir, "delta", darwin::NOMAGIC), ["delta"]);
        let mut found = zeroed();
        // SAFETY: a live pattern and structure.
        let status = unsafe {
            glob(
                pattern(&dir, "delta*").as_ptr(),
                darwin::NOMAGIC,
                None,
                &raw mut found,
            )
        };
        assert_eq!(status, darwin::NOMATCH);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn dooffs_reserves_leading_slots() {
        let dir = fixture("dooffs");
        let mut found = zeroed();
        found.gl_offs = 2;
        unsafe {
            assert_eq!(
                glob(
                    pattern(&dir, "*").as_ptr(),
                    darwin::DOOFFS,
                    None,
                    &raw mut found
                ),
                0
            );
            // `gl_offs` is not folded into `gl_pathc` on either side: the count is
            // of matches, and the reserved slots sit in front of them.
            assert_eq!(found.gl_pathc, 3);
            assert_eq!(found.gl_offs, 2);
            assert!((*found.gl_pathv).is_null() && (*found.gl_pathv.add(1)).is_null());
            // And the vector is NULL-terminated past the last match.
            assert!((*found.gl_pathv.add(5)).is_null());
            assert_eq!(matched(&found), ["alpha", "beta", "gamma"]);
            globfree(&raw mut found);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn braces_expand_in_order_and_nest() {
        let dir = fixture("brace");
        let mut found = zeroed();
        unsafe {
            let braced = pattern(&dir, "{alpha,gamma}");
            // Without the flag the braces are literal, and no such file exists.
            assert_eq!(
                glob(braced.as_ptr(), 0, None, &raw mut found),
                darwin::NOMATCH
            );
            globfree(&raw mut found);
        }
        // Each expansion is globbed and sorted on its own, so the expansions
        // stay in the order they were written.
        assert_eq!(
            names(&dir, "{gamma,alpha}", darwin::BRACE),
            ["gamma", "alpha"]
        );
        assert_eq!(
            names(&dir, "{alpha,{beta,gamma}}", darwin::BRACE),
            ["alpha", "beta", "gamma"]
        );
        // One expansion matching is enough for the call to have succeeded.
        assert_eq!(names(&dir, "{alpha,delta}", darwin::BRACE), ["alpha"]);
        // An unbalanced brace is a literal one, and the rest of the pattern is
        // still part of it.
        assert_eq!(
            names(&dir, "{alpha", darwin::BRACE | darwin::NOCHECK),
            ["{alpha"]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn tilde_expands_to_the_home_directory() {
        let Ok(home) = std::env::var("HOME") else {
            return;
        };
        let mut found = zeroed();
        // SAFETY: a static pattern and a live structure.
        unsafe {
            assert_eq!(glob(c"~".as_ptr(), darwin::TILDE, None, &raw mut found), 0);
            assert_eq!(found.gl_pathc, 1);
            let first = CStr::from_ptr(*found.gl_pathv)
                .to_string_lossy()
                .into_owned();
            assert_eq!(first, home);
            globfree(&raw mut found);

            // Without the flag the tilde is a filename, and there is no file
            // called `~` in the current directory.
            assert_eq!(
                glob(c"~".as_ptr(), 0, None, &raw mut found),
                darwin::NOMATCH
            );
            globfree(&raw mut found);
        }
    }

    /// A wildcard never crosses a `/`, and a trailing one keeps only the
    /// directories.
    #[test]
    fn a_wildcard_stays_inside_one_component() {
        let dir = fixture("components");
        std::fs::create_dir(dir.join("sub")).expect("a directory");
        std::fs::write(dir.join("sub/alpha"), b"x").expect("a file");

        assert_eq!(names(&dir, "*/alpha", 0), ["alpha"]);
        assert_eq!(names(&dir, "*", 0), ["alpha", "beta", "gamma", "sub"]);
        // A trailing separator keeps only what is a directory, and keeps the
        // separator with it.
        let mut found = zeroed();
        // SAFETY: a live pattern and structure, freed below.
        unsafe {
            assert_eq!(
                glob(pattern(&dir, "*/").as_ptr(), 0, None, &raw mut found),
                0
            );
            assert_eq!(full(&found), [format!("{}/sub/", dir.display())]);
            globfree(&raw mut found);

            // Which is also what `GLOB_MARK` does to a match that happens to
            // be a directory.
            assert_eq!(
                glob(
                    pattern(&dir, "s*").as_ptr(),
                    darwin::MARK,
                    None,
                    &raw mut found
                ),
                0
            );
            assert_eq!(full(&found), [format!("{}/sub/", dir.display())]);
            globfree(&raw mut found);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_leading_dot_is_matched_only_by_a_pattern_that_spells_one() {
        let dir = fixture("hidden");
        std::fs::write(dir.join(".hidden"), b"x").expect("a file");

        assert_eq!(names(&dir, "*", 0), ["alpha", "beta", "gamma"]);
        // `.` and `..` are entries like any other, and a pattern that starts
        // with a dot matches them - which is what a shell shows too.
        assert_eq!(names(&dir, ".*", 0), [".", "..", ".hidden"]);
        assert_eq!(names(&dir, ".h*", 0), [".hidden"]);
        // Darwin's own `glob` refuses this one - its escaped dot is not the
        // bare `.` the check compares against - which is a trap rather than a
        // rule, so the escape is honoured here.
        assert_eq!(names(&dir, "\\.h*", 0), [".hidden"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The four wildcard forms, against names the host's own libc disagrees
    /// about: musl and glibc differ over `[!...]`, and neither sorts the way
    /// Darwin does.
    #[test]
    fn the_pattern_language_is_darwins() {
        let dir = fixture("language");
        std::fs::write(dir.join("a-b"), b"x").expect("a file");
        std::fs::write(dir.join("a]b"), b"x").expect("a file");
        std::fs::write(dir.join("a*b"), b"x").expect("a file");

        // Five characters is `alpha`; six is nothing here.
        assert_eq!(names(&dir, "?????", 0), ["alpha", "gamma"]);
        assert_eq!(names(&dir, "??????", darwin::NOCHECK), ["??????"]);
        assert_eq!(names(&dir, "a?b", 0), ["a*b", "a-b", "a]b"]);
        // A `]` first in the set is a member, and a `-` last in it is too.
        assert_eq!(names(&dir, "a[]-]b", 0), ["a-b", "a]b"]);
        assert_eq!(names(&dir, "a[!]-]b", 0), ["a*b"]);
        // A range, and a set that negates one.
        assert_eq!(names(&dir, "[a-c]lpha", 0), ["alpha"]);
        assert_eq!(names(&dir, "[!a-c]amma", 0), ["gamma"]);
        // An escaped wildcard is the character itself.
        assert_eq!(names(&dir, "a\\*b", 0), ["a*b"]);
        // The pattern `GLOB_NOCHECK` hands back has its escapes resolved, which
        // is Darwin's answer and not glibc's - glibc returns the pattern
        // verbatim, backslashes and all.
        assert_eq!(names(&dir, "a\\?c", darwin::NOCHECK), ["a?c"]);
        // And with escaping off, the backslash is a character like any other.
        assert_eq!(
            names(&dir, "a\\*b", darwin::NOESCAPE | darwin::NOCHECK),
            ["a\\*b"]
        );
        // An unclosed `[` is a literal bracket rather than an error.
        assert_eq!(names(&dir, "[alpha", darwin::NOCHECK), ["[alpha"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn nosort_leaves_the_directory_order_alone() {
        let dir = fixture("nosort");
        let mut unsorted = names(&dir, "*", darwin::NOSORT);
        // The order a directory reads back in is the filesystem's, so only the
        // set is assertable - the point is that the walk did not sort it.
        unsorted.sort();
        assert_eq!(unsorted, ["alpha", "beta", "gamma"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The callback is the guest's, and what it is told is Darwin's `errno`.
    #[test]
    fn errfunc_sees_an_unreadable_directory() {
        use std::sync::atomic::{AtomicI32, Ordering};

        static SAW: AtomicI32 = AtomicI32::new(-1);

        unsafe extern "C" fn note(_path: *const c_char, error: c_int) -> c_int {
            SAW.store(error, Ordering::Relaxed);
            0
        }

        let dir = fixture("errfunc");
        let mut found = zeroed();
        // A directory that is not there at all is not an error - `glob` walks
        // past it - so the unreadable one is a *file* used as a directory.
        let pattern = pattern(&dir, "alpha/*");
        // SAFETY: a live pattern, structure and callback.
        let status = unsafe { glob(pattern.as_ptr(), 0, Some(note), &raw mut found) };
        assert_eq!(status, darwin::NOMATCH);
        // Darwin's `ENOTDIR` is 20, the same number this host spells it with.
        assert_eq!(SAW.load(Ordering::Relaxed), 20);
        // And the caller's callback is readable back out of the structure.
        assert!(found.gl_errfunc.is_some());

        // `GLOB_ERR` turns the same failure into an aborted walk.
        SAW.store(-1, Ordering::Relaxed);
        let status = unsafe { glob(pattern.as_ptr(), darwin::ERR, Some(note), &raw mut found) };
        assert_eq!(status, darwin::ABORTED);
        // SAFETY: nothing matched, so nothing was allocated.
        unsafe { globfree(&raw mut found) };
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn gl_flags_reports_the_input_flags_and_magchar() {
        let dir = fixture("flags");
        let mut found = zeroed();
        unsafe {
            assert_eq!(
                glob(
                    pattern(&dir, "*").as_ptr(),
                    darwin::NOSORT,
                    None,
                    &raw mut found
                ),
                0
            );
            // The caller's own flags come back in Darwin's spelling, not the
            // host's - `GLOB_NOSORT` is 0x20 here and 4 in glibc.
            assert_eq!(found.gl_flags & darwin::NOSORT, darwin::NOSORT);
            assert_eq!(found.gl_flags & darwin::MARK, 0);
            // And the one bit the call adds: the pattern held a metacharacter.
            assert_eq!(found.gl_flags & darwin::MAGCHAR, darwin::MAGCHAR);
            globfree(&raw mut found);

            let mut plain = zeroed();
            assert_eq!(
                glob(pattern(&dir, "alpha").as_ptr(), 0, None, &raw mut plain),
                0
            );
            assert_eq!(plain.gl_flags & darwin::MAGCHAR, 0);
            globfree(&raw mut plain);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `GLOB_ALTDIRFUNC` is the whole reason this walk is not the host's: the
    /// hooks are the guest's, and everything they hand back is at Darwin's
    /// offsets. They are answered here out of a table rather than a
    /// filesystem, which is what a guest uses them for.
    mod alt {
        use super::*;

        /// The directory the hooks below describe, and how far each open
        /// handle has read.
        const ENTRIES: [&str; 3] = ["one", "two", "three"];
        static POSITION: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        static OPENED: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

        pub unsafe extern "C" fn opendir(_path: *const c_char) -> *mut c_void {
            POSITION.store(0, std::sync::atomic::Ordering::Relaxed);
            OPENED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            // Any non-null token: nothing but these hooks ever looks at it.
            std::ptr::dangling_mut()
        }

        pub unsafe extern "C" fn readdir(_dir: *mut c_void) -> *mut GuestDirent {
            thread_local! {
                static ENTRY: std::cell::RefCell<GuestDirent> =
                    std::cell::RefCell::new(unsafe { std::mem::zeroed() });
            }
            let at = POSITION.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let Some(name) = ENTRIES.get(at) else {
                return std::ptr::null_mut();
            };
            ENTRY.with(|entry| {
                let mut entry = entry.borrow_mut();
                #[cfg(not(target_os = "macos"))]
                {
                    entry.namlen = name.len() as u16;
                }
                #[cfg(target_os = "macos")]
                {
                    entry.d_namlen = name.len() as u16;
                }
                #[cfg(not(target_os = "macos"))]
                let slots = &mut entry.name;
                #[cfg(target_os = "macos")]
                let slots = &mut entry.d_name;
                slots.fill(0);
                for (slot, byte) in slots.iter_mut().zip(name.bytes()) {
                    *slot = byte as c_char;
                }
                &raw mut *entry
            })
        }

        pub unsafe extern "C" fn closedir(_dir: *mut c_void) {}

        /// Every path the walk asks about exists and is a regular file.
        pub unsafe extern "C" fn stat(_path: *const c_char, out: *mut GuestStat) -> c_int {
            // SAFETY: the walk's own structure.
            unsafe { (*out).mode = libc::S_IFREG as _ };
            0
        }

        /// Held for as long as a test's hooks are in use, as the counters
        /// above are shared and another test's `hooks` would reset them.
        static IN_USE: std::sync::Mutex<()> = std::sync::Mutex::new(());

        pub fn hooks(found: &mut Glob) -> std::sync::MutexGuard<'static, ()> {
            let held = IN_USE.lock().unwrap_or_else(|e| e.into_inner());
            found.gl_opendir = Some(opendir);
            found.gl_readdir = Some(readdir);
            found.gl_closedir = Some(closedir);
            found.gl_lstat = Some(stat);
            found.gl_stat = Some(stat);
            OPENED.store(0, std::sync::atomic::Ordering::Relaxed);
            held
        }

        pub fn opens() -> usize {
            OPENED.load(std::sync::atomic::Ordering::Relaxed)
        }
    }

    #[test]
    fn altdirfunc_reads_the_callers_directory() {
        let mut found = zeroed();
        let _hooks = alt::hooks(&mut found);
        // SAFETY: a static pattern, a live structure and five live hooks.
        unsafe {
            assert_eq!(
                glob(
                    c"/anywhere/t*".as_ptr(),
                    darwin::ALTDIRFUNC,
                    None,
                    &raw mut found
                ),
                0
            );
            // The names came from the hook, not from `/anywhere`, which does
            // not exist.
            assert_eq!(full(&found), ["/anywhere/three", "/anywhere/two"]);
            assert_eq!(alt::opens(), 1);
            globfree(&raw mut found);
        }
    }

    #[test]
    fn altdirfunc_without_every_hook_is_refused() {
        let mut found = zeroed();
        let _hooks = alt::hooks(&mut found);
        found.gl_stat = None;
        // SAFETY: as above, with one hook missing.
        let status = unsafe {
            glob(
                c"/anywhere/*".as_ptr(),
                darwin::ALTDIRFUNC,
                None,
                &raw mut found,
            )
        };
        // Walking the real filesystem instead would ignore the caller's hooks
        // without saying so.
        assert_eq!(status, darwin::NOSYS);
        assert!(found.gl_pathv.is_null());
    }

    #[test]
    fn limit_stops_a_walk_that_reads_too_much() {
        let dir = fixture("limit");
        let mut found = zeroed();
        // Nothing here is near the limits, so the flag changes nothing.
        assert_eq!(names(&dir, "*", darwin::LIMIT), ["alpha", "beta", "gamma"]);

        // A pattern that expands combinatorially is refused rather than run,
        // whether or not the caller asked for the limit.
        let deep = "{a,b}".repeat(20);
        let pattern = CString::new(deep).expect("no interior NUL");
        // SAFETY: a live pattern and structure.
        let status = unsafe {
            glob(
                pattern.as_ptr(),
                darwin::BRACE | darwin::NOCHECK,
                None,
                &raw mut found,
            )
        };
        assert_eq!(status, darwin::NOSPACE);
        // SAFETY: the structure, whose vector this frees.
        unsafe { globfree(&raw mut found) };
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn both_entries_are_published() {
        for name in ["glob", "globfree"] {
            assert!(symbol(name).is_some(), "{name}");
        }
    }
}
