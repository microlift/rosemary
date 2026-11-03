use alloc::string::String;
use core::fmt;

pub type Result<T> = core::result::Result<T, Error>;

#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Error {
    /// A structure ran past the end of the input.
    Truncated {
        /// What was being read (e.g. `"mach_header"`, `"load_command"`).
        what: &'static str,
    },
    /// The magic was not any Mach-O or fat magic - this is not a Mach-O file.
    BadMagic(u32),
    /// The file *is* a Mach-O, but of a form this crate does not load: 32-bit
    /// or byte-swapped. Both died with PowerPC and are deliberately out of
    /// scope. Reported separately from [`Error::BadMagic`] so the caller can
    /// tell "wrong file" from "right file, unsupported flavor".
    UnsupportedMachOForm(u32),
    /// The fat/universal header was present but contained no slice matching
    /// the requested [`crate::spec::MachSpec`].
    NoMatchingSlice {
        /// The `cputype` that was being looked for.
        wanted: i32,
    },
    /// A fat slice's offset/size ran past the end of the file, or two slices
    /// overlapped.
    BadFatSlice,
    /// `cputype` did not match the selected [`crate::spec::MachSpec`].
    WrongCpuType {
        /// Value the spec expects.
        expected: i32,
        /// Value found in the header.
        found: i32,
    },
    /// `filetype` is not something this loader knows how to map. Only
    /// `MH_EXECUTE`, `MH_DYLIB`, and `MH_BUNDLE` are accepted.
    UnsupportedFiletype(u32),
    /// A load command was malformed: `cmdsize` of zero (which would loop
    /// forever), not 8-byte aligned, or extending past `sizeofcmds`.
    BadLoadCommand {
        /// What was wrong.
        why: &'static str,
    },
    /// The image had no mappable segments (everything was `__PAGEZERO`, or
    /// there were no `LC_SEGMENT_64` commands at all).
    NoLoadableSegments,
    /// A LEB128 value did not terminate, or overflowed 64 bits.
    BadLeb,
    /// A string in the image was not NUL-terminated within its bounds, or was
    /// not valid UTF-8.
    BadString,
    /// A fixup opcode/format was not understood.
    UnknownFixup {
        /// Which decoder rejected it.
        format: &'static str,
        /// The offending opcode or pointer-format value.
        value: u32,
    },
    /// A bind ordinal did not correspond to any load command.
    BadLibraryOrdinal(i32),
    /// A required (non-weak) import could not be resolved by any provider.
    UnresolvedSymbol(String),
    /// An address-space operation (reserve/map/protect) failed.
    Memory(&'static str),
    /// A read or write fell outside the mapped image.
    OutOfBounds {
        /// Offending address.
        addr: u64,
        /// Length requested.
        len: usize,
    },
    /// The image is encrypted (`LC_ENCRYPTION_INFO_64` with `cryptid != 0`).
    /// Decryption requires kernel cooperation and is out of scope.
    Encrypted,
    /// A structure nested or chained deeper than the loader's guard allows,
    /// which for untrusted input means a cycle or a decompression bomb.
    TooDeep(&'static str),
    /// A `__gcc_except_tab` entry could not be read, or uses an encoding this
    /// crate does not implement.
    BadExceptionTable,
    /// Catch-all for spec/extension-defined failures.
    Custom(&'static str),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Truncated { what } => write!(f, "truncated Mach-O while reading {what}"),
            Error::BadMagic(m) => write!(f, "not a Mach-O file (magic {m:#010x})"),
            Error::UnsupportedMachOForm(m) => {
                write!(
                    f,
                    "unsupported Mach-O form (magic {m:#010x}); only 64-bit little-endian is loaded"
                )
            }
            Error::NoMatchingSlice { wanted } => {
                write!(f, "no fat slice for cputype {wanted:#x}")
            }
            Error::BadFatSlice => write!(f, "fat slice out of bounds"),
            Error::WrongCpuType { expected, found } => {
                write!(f, "wrong cputype: expected {expected:#x}, found {found:#x}")
            }
            Error::UnsupportedFiletype(t) => write!(f, "unsupported filetype {t:#x}"),
            Error::BadLoadCommand { why } => write!(f, "bad load command: {why}"),
            Error::NoLoadableSegments => write!(f, "no mappable segments"),
            Error::BadLeb => write!(f, "malformed LEB128"),
            Error::BadString => write!(f, "malformed string"),
            Error::UnknownFixup { format, value } => {
                write!(f, "unknown {format} fixup value {value:#x}")
            }
            Error::BadLibraryOrdinal(o) => write!(f, "bad library ordinal {o}"),
            Error::UnresolvedSymbol(name) => write!(f, "unresolved symbol: {name}"),
            Error::Memory(m) => write!(f, "address space error: {m}"),
            Error::OutOfBounds { addr, len } => {
                write!(f, "access out of bounds: addr={addr:#x} len={len}")
            }
            Error::Encrypted => write!(f, "image is encrypted"),
            Error::TooDeep(what) => write!(f, "{what} nested too deeply"),
            Error::BadExceptionTable => write!(f, "malformed exception table"),
            Error::Custom(m) => write!(f, "{m}"),
        }
    }
}

#[cfg(feature = "std")]
impl std::error::Error for Error {}
