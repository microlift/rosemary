/// 64-bit Mach-O, host-endian (little). The only form this crate accepts.
pub const MH_MAGIC_64: u32 = 0xfeed_facf;
/// 64-bit Mach-O, byte-swapped. Recognized only so the error can be specific.
pub const MH_CIGAM_64: u32 = 0xcffa_edfe;
/// 32-bit Mach-O, host-endian. Rejected.
pub const MH_MAGIC: u32 = 0xfeed_face;
/// 32-bit Mach-O, byte-swapped. Rejected.
pub const MH_CIGAM: u32 = 0xcefa_edfe;

/// Fat/universal archive header, 32-bit offsets. Always big-endian on disk.
pub const FAT_MAGIC: u32 = 0xcafe_babe;
/// Fat/universal archive header, 64-bit offsets.
pub const FAT_MAGIC_64: u32 = 0xcafe_babf;

pub const CPU_ARCH_ABI64: i32 = 0x0100_0000;
pub const CPU_TYPE_X86: i32 = 7;
pub const CPU_TYPE_X86_64: i32 = CPU_TYPE_X86 | CPU_ARCH_ABI64;
pub const CPU_TYPE_ARM: i32 = 12;
pub const CPU_TYPE_ARM64: i32 = CPU_TYPE_ARM | CPU_ARCH_ABI64;

/// The capability/feature bits of a `cpusubtype`, occupying the top byte.
/// `CPU_SUBTYPE_LIB64` is `0x8000_0000`, and `arm64e` slices carry `0x80` in
/// that byte to mark the pointer-authentication ABI version - so `/bin/ls`'s
/// arm64e slice reports `cpusubtype` as `0x8000_0002`, not `2`.
pub const CPU_SUBTYPE_FEATURE_MASK: u32 = 0xff00_0000;

/// Strip the capability bits, leaving the architecture identity.
pub const fn subtype_of(cpusubtype: i32) -> i32 {
    (cpusubtype as u32 & !CPU_SUBTYPE_FEATURE_MASK) as i32
}

pub const CPU_SUBTYPE_X86_64_ALL: i32 = 3;
/// Haswell baseline: assumes AVX2/BMI, which the lifter may not implement.
pub const CPU_SUBTYPE_X86_64_H: i32 = 8;
pub const CPU_SUBTYPE_ARM64_ALL: i32 = 0;
pub const CPU_SUBTYPE_ARM64E: i32 = 2;

pub const MH_OBJECT: u32 = 0x1;
pub const MH_EXECUTE: u32 = 0x2;
pub const MH_DYLIB: u32 = 0x6;
pub const MH_DYLINKER: u32 = 0x7;
pub const MH_BUNDLE: u32 = 0x8;

/// Two-level namespace: imports are keyed by (library ordinal, name).
pub const MH_TWOLEVEL: u32 = 0x0000_0080;
/// Flat namespace lookup, the pre-10.1 behaviour.
pub const MH_FORCE_FLAT: u32 = 0x0000_0100;
/// Position-independent: the image tolerates being slid.
pub const MH_PIE: u32 = 0x0020_0000;
/// The image's fixups are `LC_DYLD_CHAINED_FIXUPS` style.
pub const MH_DYLDLINK: u32 = 0x0000_0004;

/// Set on commands the loader must understand or refuse to run the image.
pub const LC_REQ_DYLD: u32 = 0x8000_0000;

pub const LC_SYMTAB: u32 = 0x02;
pub const LC_DYSYMTAB: u32 = 0x0b;
pub const LC_LOAD_DYLIB: u32 = 0x0c;
pub const LC_ID_DYLIB: u32 = 0x0d;
pub const LC_LOAD_DYLINKER: u32 = 0x0e;
pub const LC_ROUTINES_64: u32 = 0x1a;
pub const LC_SEGMENT_64: u32 = 0x19;
pub const LC_UUID: u32 = 0x1b;
pub const LC_CODE_SIGNATURE: u32 = 0x1d;
pub const LC_ENCRYPTION_INFO_64: u32 = 0x2c;
pub const LC_UNIXTHREAD: u32 = 0x05;
pub const LC_FUNCTION_STARTS: u32 = 0x26;
pub const LC_DATA_IN_CODE: u32 = 0x29;
pub const LC_SOURCE_VERSION: u32 = 0x2a;
pub const LC_BUILD_VERSION: u32 = 0x32;
pub const LC_VERSION_MIN_MACOSX: u32 = 0x24;

pub const LC_RPATH: u32 = 0x1c | LC_REQ_DYLD;
pub const LC_LOAD_WEAK_DYLIB: u32 = 0x18 | LC_REQ_DYLD;
pub const LC_REEXPORT_DYLIB: u32 = 0x1f | LC_REQ_DYLD;
pub const LC_LOAD_UPWARD_DYLIB: u32 = 0x23 | LC_REQ_DYLD;
pub const LC_MAIN: u32 = 0x28 | LC_REQ_DYLD;
pub const LC_DYLD_INFO: u32 = 0x22;
pub const LC_DYLD_INFO_ONLY: u32 = 0x22 | LC_REQ_DYLD;
pub const LC_DYLD_EXPORTS_TRIE: u32 = 0x33 | LC_REQ_DYLD;
pub const LC_DYLD_CHAINED_FIXUPS: u32 = 0x34 | LC_REQ_DYLD;

pub const VM_PROT_READ: i32 = 0x1;
pub const VM_PROT_WRITE: i32 = 0x2;
pub const VM_PROT_EXECUTE: i32 = 0x4;

/// `flags & SECTION_TYPE` isolates the type from the attribute bits.
pub const SECTION_TYPE: u32 = 0x0000_00ff;

pub const S_REGULAR: u8 = 0x0;
pub const S_ZEROFILL: u8 = 0x1;
pub const S_NON_LAZY_SYMBOL_POINTERS: u8 = 0x6;
pub const S_LAZY_SYMBOL_POINTERS: u8 = 0x7;
/// `reserved1` indexes the indirect symbol table; `reserved2` is the stub size.
pub const S_SYMBOL_STUBS: u8 = 0x8;
pub const S_MOD_INIT_FUNC_POINTERS: u8 = 0x9;
/// `__TEXT,__init_offsets`: initializers as 32-bit offsets from the image
/// base rather than as pointers. What a current `ld64` emits in place of
/// `__mod_init_func`, and the reason is worth knowing: an offset table needs no
/// rebasing, so it can live in read-only `__TEXT` instead of in writable
/// `__DATA` where a function-pointer array is an attractive target to overwrite.
pub const S_INIT_FUNC_OFFSETS: u8 = 0x16;
pub const S_MOD_TERM_FUNC_POINTERS: u8 = 0xa;
pub const S_THREAD_LOCAL_REGULAR: u8 = 0x11;
pub const S_THREAD_LOCAL_ZEROFILL: u8 = 0x12;
pub const S_THREAD_LOCAL_VARIABLES: u8 = 0x13;
pub const S_THREAD_LOCAL_VARIABLE_POINTERS: u8 = 0x14;
pub const S_THREAD_LOCAL_INIT_FUNCTION_POINTERS: u8 = 0x15;

/// Every opcode byte splits into a 4-bit opcode and a 4-bit immediate.
pub const REBASE_OPCODE_MASK: u8 = 0xf0;
pub const REBASE_IMMEDIATE_MASK: u8 = 0x0f;

pub const REBASE_OPCODE_DONE: u8 = 0x00;
pub const REBASE_OPCODE_SET_TYPE_IMM: u8 = 0x10;
pub const REBASE_OPCODE_SET_SEGMENT_AND_OFFSET_ULEB: u8 = 0x20;
pub const REBASE_OPCODE_ADD_ADDR_ULEB: u8 = 0x30;
pub const REBASE_OPCODE_ADD_ADDR_IMM_SCALED: u8 = 0x40;
pub const REBASE_OPCODE_DO_REBASE_IMM_TIMES: u8 = 0x50;
pub const REBASE_OPCODE_DO_REBASE_ULEB_TIMES: u8 = 0x60;
pub const REBASE_OPCODE_DO_REBASE_ADD_ADDR_ULEB: u8 = 0x70;
pub const REBASE_OPCODE_DO_REBASE_ULEB_TIMES_SKIPPING_ULEB: u8 = 0x80;

/// The only rebase/bind type a 64-bit image uses: an 8-byte pointer slot.
/// `TEXT_ABSOLUTE32` (2) and `TEXT_PCREL32` (3) exist for 32-bit images only.
pub const REBASE_TYPE_POINTER: u8 = 1;

pub const BIND_OPCODE_MASK: u8 = 0xf0;
pub const BIND_IMMEDIATE_MASK: u8 = 0x0f;

pub const BIND_OPCODE_DONE: u8 = 0x00;
pub const BIND_OPCODE_SET_DYLIB_ORDINAL_IMM: u8 = 0x10;
pub const BIND_OPCODE_SET_DYLIB_ORDINAL_ULEB: u8 = 0x20;
pub const BIND_OPCODE_SET_DYLIB_SPECIAL_IMM: u8 = 0x30;
pub const BIND_OPCODE_SET_SYMBOL_TRAILING_FLAGS_IMM: u8 = 0x40;
pub const BIND_OPCODE_SET_TYPE_IMM: u8 = 0x50;
pub const BIND_OPCODE_SET_ADDEND_SLEB: u8 = 0x60;
pub const BIND_OPCODE_SET_SEGMENT_AND_OFFSET_ULEB: u8 = 0x70;
pub const BIND_OPCODE_ADD_ADDR_ULEB: u8 = 0x80;
pub const BIND_OPCODE_DO_BIND: u8 = 0x90;
pub const BIND_OPCODE_DO_BIND_ADD_ADDR_ULEB: u8 = 0xa0;
pub const BIND_OPCODE_DO_BIND_ADD_ADDR_IMM_SCALED: u8 = 0xb0;
pub const BIND_OPCODE_DO_BIND_ULEB_TIMES_SKIPPING_ULEB: u8 = 0xc0;
/// arm64e threaded rebase/bind. Out of scope with pointer authentication.
pub const BIND_OPCODE_THREADED: u8 = 0xd0;

pub const BIND_TYPE_POINTER: u8 = 1;

/// Special dylib "ordinals", encoded as sign-extended 4-bit immediates.
pub const BIND_SPECIAL_DYLIB_SELF: i32 = 0;
pub const BIND_SPECIAL_DYLIB_MAIN_EXECUTABLE: i32 = -1;
pub const BIND_SPECIAL_DYLIB_FLAT_LOOKUP: i32 = -2;
/// Bind only if some *other* image already defines the symbol weakly.
pub const BIND_SPECIAL_DYLIB_WEAK_LOOKUP: i32 = -3;

/// The import may go unresolved; the slot is then left as zero.
pub const BIND_SYMBOL_FLAGS_WEAK_IMPORT: u8 = 0x1;
/// Appears in `weak_bind`: this image's own definition is the strong one.
pub const BIND_SYMBOL_FLAGS_NON_WEAK_DEFINITION: u8 = 0x8;

/// Size of a pointer slot in a 64-bit image.
pub const POINTER_SIZE: u64 = 8;

/// `dyld_chained_ptr_format` values.
pub const DYLD_CHAINED_PTR_ARM64E: u16 = 1;
/// 64-bit, target is an unslid virtual address.
pub const DYLD_CHAINED_PTR_64: u16 = 2;
pub const DYLD_CHAINED_PTR_32: u16 = 3;
pub const DYLD_CHAINED_PTR_32_CACHE: u16 = 4;
pub const DYLD_CHAINED_PTR_32_FIRMWARE: u16 = 5;
/// 64-bit, target is an offset from the image base. What a modern x86-64
/// `MH_EXECUTE` actually uses.
pub const DYLD_CHAINED_PTR_64_OFFSET: u16 = 6;
pub const DYLD_CHAINED_PTR_ARM64E_KERNEL: u16 = 7;
pub const DYLD_CHAINED_PTR_64_KERNEL_CACHE: u16 = 8;
pub const DYLD_CHAINED_PTR_ARM64E_USERLAND: u16 = 9;
pub const DYLD_CHAINED_PTR_ARM64E_FIRMWARE: u16 = 10;
pub const DYLD_CHAINED_PTR_X86_64_KERNEL_CACHE: u16 = 11;
pub const DYLD_CHAINED_PTR_ARM64E_USERLAND24: u16 = 12;

/// Whether a pointer format is one of the pointer-authentication encodings.
pub const fn is_arm64e_pointer_format(format: u16) -> bool {
    matches!(
        format,
        DYLD_CHAINED_PTR_ARM64E
            | DYLD_CHAINED_PTR_ARM64E_KERNEL
            | DYLD_CHAINED_PTR_ARM64E_USERLAND
            | DYLD_CHAINED_PTR_ARM64E_FIRMWARE
            | DYLD_CHAINED_PTR_ARM64E_USERLAND24
    )
}

/// The arm64e formats an application image uses. The kernel, firmware and
/// kernel-cache formats are not Mach-O userland and stay refused.
pub const fn is_arm64e_userland_pointer_format(format: u16) -> bool {
    matches!(
        format,
        DYLD_CHAINED_PTR_ARM64E
            | DYLD_CHAINED_PTR_ARM64E_USERLAND
            | DYLD_CHAINED_PTR_ARM64E_USERLAND24
    )
}

/// `page_start` sentinel meaning the page holds no fixups.
pub const DYLD_CHAINED_PTR_START_NONE: u16 = 0xffff;
/// `page_start` flag: the value is an index into an overflow list of starts
/// rather than a page offset. Emitted when one page begins several chains.
pub const DYLD_CHAINED_PTR_START_MULTI: u16 = 0x8000;
/// In an overflow list, marks the final entry.
pub const DYLD_CHAINED_PTR_START_LAST: u16 = 0x8000;

/// `imports_format` values, which differ only in how much addend they can hold.
pub const DYLD_CHAINED_IMPORT: u32 = 1;
pub const DYLD_CHAINED_IMPORT_ADDEND: u32 = 2;
pub const DYLD_CHAINED_IMPORT_ADDEND64: u32 = 3;

/// Chain links are counted in 4-byte units for every 64-bit format.
pub const CHAINED_NEXT_STRIDE: u64 = 4;
/// The arm64e userland formats count links in 8-byte units, in an 11-bit field.
pub const CHAINED_NEXT_STRIDE_ARM64E: u64 = 8;

/// `flags & EXPORT_SYMBOL_FLAGS_KIND_MASK` isolates what sort of thing the
/// symbol names, which changes what its address *means*.
pub const EXPORT_SYMBOL_FLAGS_KIND_MASK: u32 = 0x03;
pub const EXPORT_SYMBOL_FLAGS_KIND_REGULAR: u32 = 0x00;
pub const EXPORT_SYMBOL_FLAGS_KIND_THREAD_LOCAL: u32 = 0x01;
/// The "address" is the value itself, not an offset from the image base.
pub const EXPORT_SYMBOL_FLAGS_KIND_ABSOLUTE: u32 = 0x02;
/// A weak definition: another image's definition of the same name may win.
pub const EXPORT_SYMBOL_FLAGS_WEAK_DEFINITION: u32 = 0x04;
/// The node carries a dylib ordinal and an optional renamed symbol instead of
/// an address; the real definition lives in another image.
pub const EXPORT_SYMBOL_FLAGS_REEXPORT: u32 = 0x08;
/// The node carries two addresses: the stub callers branch to, and the
/// resolver that picks the implementation on first call (`ifunc`-alikes).
pub const EXPORT_SYMBOL_FLAGS_STUB_AND_RESOLVER: u32 = 0x10;
/// A static resolver, emitted by newer toolchains alongside the stub form.
pub const EXPORT_SYMBOL_FLAGS_STATIC_RESOLVER: u32 = 0x20;

/// Debug-symbol entries, which a loader ignores entirely.
pub const N_STAB: u8 = 0xe0;
/// Private external: visible to the linker, not to other images.
pub const N_PEXT: u8 = 0x10;
/// The type field proper.
pub const N_TYPE: u8 = 0x0e;
/// External: a candidate for export.
pub const N_EXT: u8 = 0x01;

pub const N_UNDF: u8 = 0x0;
pub const N_ABS: u8 = 0x2;
/// An indirect symbol: `n_value` is a string-table offset naming an alias.
pub const N_INDR: u8 = 0xa;
pub const N_SECT: u8 = 0xe;

/// `n_desc` bit marking a weak definition.
pub const N_WEAK_DEF: u16 = 0x0080;
/// `n_desc` bit marking a weak *reference*: an unresolved one binds to zero.
pub const N_WEAK_REF: u16 = 0x0040;

/// Sentinels appearing in the indirect symbol table in place of a real index.
pub const INDIRECT_SYMBOL_LOCAL: u32 = 0x8000_0000;
pub const INDIRECT_SYMBOL_ABS: u32 = 0x4000_0000;

/// `sizeof(struct relocation_info)`.
pub const RELOCATION_INFO_SIZE: usize = 8;
/// `r_address` bit marking a `scattered_relocation_info`, which only 32-bit
/// images use.
pub const R_SCATTERED: u32 = 0x8000_0000;
/// `X86_64_RELOC_UNSIGNED` and `ARM64_RELOC_UNSIGNED`: an absolute pointer.
pub const RELOC_UNSIGNED: u8 = 0;
/// `r_length` of a 64-bit pointer (log2 of its size).
pub const RELOC_LENGTH_POINTER: u8 = 3;

pub const MACH_HEADER_64_SIZE: usize = 32;
pub const LOAD_COMMAND_SIZE: usize = 8;
pub const SEGMENT_COMMAND_64_SIZE: usize = 72;
pub const SECTION_64_SIZE: usize = 80;
pub const FAT_HEADER_SIZE: usize = 8;
pub const FAT_ARCH_SIZE: usize = 20;
pub const FAT_ARCH_64_SIZE: usize = 32;
pub const NLIST_64_SIZE: usize = 16;
