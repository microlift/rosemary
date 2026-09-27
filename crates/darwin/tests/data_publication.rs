//! What every `data` and `constants` row publishes, checked against what the
//! loader then does with the number.
//!
//! # The contract, from the three places it is written down
//!
//! `exports!`'s two data sections both pass their right-hand side straight
//! through, unchanged, to one call:
//!
//! * `exports::exports!` - `out.data(concat!("_", $dname), $dvalue)` and
//!   `out.data(concat!("_", $cname), $cvalue)`.
//! * `exports::sink` - `Exports for machish::resolve::SyntheticDylib`:
//!   `fn data(&mut self, symbol, address) { self.define_data(symbol, address) }`.
//! * `machish::fixups::Binder::bind` - and this is the one that settles it:
//!
//!   ```text
//!   let value = r.addr.wrapping_add(addend as u64);
//!   mem.write(slot_va, &value.to_le_bytes())?;
//!   ```
//!
//!   The number a `data` or `constants` row supplies is written **into the
//!   guest's slot**. It is not read as the symbol's value, because there is no
//!   path in the format where a data import yields a value: every relocation
//!   kind that can name an imported data symbol - a chained
//!   `DYLD_CHAINED_IMPORT`, a `BIND_OPCODE_DO_BIND` in a `LC_DYLD_INFO_ONLY`
//!   stream - writes an **address** and adds the addend to the address.
//!
//! And `machish::resolve::Resolved`'s own field says so: `addr` is "the address
//! to store in the slot ... a **host pointer, valid in the host's address
//! space**".
//!
//! So the invariant is: **a `data` or `constants` row publishes the address of
//! storage, and the guest loads through it.** The arithmetic at the row is not
//! the guest's arithmetic; the guest's is one load after the bind.
//!
//! # The defect this guards, and why it is a value/address confusion at all
//!
//! A census agent flagged `xlocale.rs`'s row
//!
//! ```text
//! "__mb_cur_max" => MB_CUR_MAX.as_ptr() as usize as u64,
//! ```
//!
//! on the grounds that it "publishes the address of a 4-byte int under a name
//! that denotes a 4-byte int", so a guest "gets a pointer where it expected a
//! number". It is a natural reading and it is wrong, for two reasons that are
//! both checkable rather than arguable.
//!
//! **One: the name denotes a *variable*, not a value.** A Mach-O data import is
//! bound to the address of the definition, and the guest dereferences it - which
//! is what `crates/rosetta3/src/abi/table.rs` already says in its own words,
//! for this very name:
//!
//! ```text
//! const DATA: &str = "a data import, read through its slot and never called";
//! ...
//! ("__mb_cur_max", DATA),
//! ```
//!
//! **Two: the same tree answers the *function* of that name with a value, and
//! the two are different symbols.** `xlocale.rs` publishes all three spellings:
//!
//! ```text
//! "__mb_cur_max_l" | "___mb_cur_max_l" => mb_cur_max_l,   // functions
//! "___mb_cur_max" => mb_cur_max,                           // functions
//! "__mb_cur_max" => MB_CUR_MAX.as_ptr() as usize as u64,   // constants
//! ```
//!
//! `___mb_cur_max` is a *function* and returns the number in `eax`; `__mb_cur_max`
//! is a *variable* and is read through the slot. Both are right, and
//! `crates/rosetta3/src/abi/table.rs` types them differently for the same reason:
//! `sig!("___mb_cur_max", I32)` against `("__mb_cur_max", DATA)`. Changing the
//! `constants` row to publish the value would make the guest dereference address
//! `1`.
//!
//! # Why the test is stated over the whole table and not over that row
//!
//! A guard that only covers the reported row is worth one row. The class is
//! "a `data`/`constants` row published something the guest cannot load through",
//! and the way to catch the class is to ask the question of **every** published
//! data name - which is only possible where the whole table is visible, and
//! `darwin::provider()` is that place: it is `Module::provide_all(MODULES, …)`
//! over the same list `darwin::symbol()` walks.
//!
//! The expected value is derived, not written down. Nothing here compares an
//! address to a literal, because a literal is what drifts: the row's expression
//! changes, the test keeps passing, and the guard has guarded nothing. The
//! question is instead asked of the **operating system**: is this number
//! somewhere a load would actually land? `machish::resolve::Resolved::addr` says
//! the answer has to be "a host pointer, valid in the host's address space", so
//! `/proc/self/maps` is the authority and the kernel's own view of this process
//! is the oracle.
//!
//! Two controls, because a guard that cannot fail is not a guard:
//! `the_predicate_agrees_with_the_kernel_about_a_value` checks that the
//! predicate **rejects** the values a confused row would publish, so a green
//! run is evidence the predicate works and not just that nothing tripped it.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use machish::resolve::{SymbolKind, SymbolProvider};

/// Every name `darwin` publishes as **data**, with the address it publishes.
///
/// Data only, and that is the point rather than a convenience. `functions` rows
/// publish a *code* address, which is a different question with a different
/// answer, and `machish::resolve::SymbolKind` is what tells the two apart at the
/// loader. Note what this means for the invariant below: **the loader cannot
/// tell a `data` row from a `constants` row** - both arrive as
/// `SymbolKind::Data` - so the guard is necessarily stated over both, and a
/// `constants`-only test could not see a `data` row at all.
fn published_data() -> BTreeMap<String, u64> {
    let mut out = BTreeMap::new();
    darwin::provider().enumerate(&mut |name, resolved| {
        if resolved.kind == SymbolKind::Data {
            out.insert(name.to_string(), resolved.addr);
        }
    });
    assert!(
        !out.is_empty(),
        "darwin published no data at all, so this file is checking nothing"
    );
    out
}

/// This process's readable mappings, from `/proc/self/maps`.
///
/// Read once: it is a syscall and a parse per call, and nothing here mutates the
/// map after the first read. A failure to read it is a **panic**, not a skip -
/// a guard that quietly stops guarding when a file is missing is the failure mode
/// this project treats as worst.
fn readable_mappings() -> &'static [(u64, u64)] {
    static M: OnceLock<Vec<(u64, u64)>> = OnceLock::new();
    M.get_or_init(|| {
        let text = std::fs::read_to_string("/proc/self/maps").unwrap_or_else(|e| {
            panic!(
                "/proc/self/maps is unreadable ({e}), so `is_live_host_storage` \
                 would answer `false` for every address and this file would fail \
                 on all {} rows rather than on the wrong ones. A missing guard is \
                 worse than a missing test.",
                published_data().len()
            )
        });
        let mut out = Vec::new();
        for line in text.lines() {
            let mut fields = line.split_whitespace();
            let Some(range) = fields.next() else { continue };
            let perms = fields.next().unwrap_or("");
            let Some((start, end)) = range.split_once('-') else {
                continue;
            };
            // The guest's *load* is the question, so `r` is the only bit that
            // matters. A write-only or exec-only mapping is not somewhere this
            // runtime puts a data symbol, and requiring `r` is what makes the
            // predicate strict enough to reject a value.
            if !perms.starts_with('r') {
                continue;
            }
            let (Ok(start), Ok(end)) =
                (u64::from_str_radix(start, 16), u64::from_str_radix(end, 16))
            else {
                continue;
            };
            out.push((start, end));
        }
        assert!(!out.is_empty(), "/proc/self/maps held no readable mapping");
        out
    })
}

/// Would a load of eight bytes at `addr` land in this process's memory?
///
/// Eight bytes because that is the widest thing a data symbol can be asked for
/// at one address on x86-64, so a row that passes this passes any narrower read
/// too. A **range** check and never a dereference: a row that published the
/// value `1` must make this test *fail*, and a test that segfaults on the way to
/// reporting the failure has reported nothing.
fn is_live_host_storage(addr: u64) -> bool {
    readable_mappings()
        .iter()
        .any(|&(start, end)| addr >= start && addr.saturating_add(8) <= end)
}

/// Read `N` bytes at an address already known to be live host storage.
///
/// # Safety
///
/// The caller has established with `is_live_host_storage` that eight bytes at
/// `addr` are readable, and reads no more than that.
unsafe fn read_bytes<const N: usize>(addr: u64) -> [u8; N] {
    let mut buf = [0u8; N];
    // SAFETY: `addr` is live readable storage for at least eight bytes and
    // `N <= 8`, so the copy stays inside it.
    unsafe { std::ptr::copy_nonoverlapping(addr as usize as *const u8, buf.as_mut_ptr(), N) };
    buf
}

/// The rows that are not the address of anything.
///
/// Collected rather than asserted inside the loop, so one run names **every**
/// offending row instead of the first. `darwin::no_module_defines_a_name_another_one_does`
/// is the standing example of why: it `panic!`s on the first duplicate, and
/// `sched_yield` is at `MODULES` index 0, so it has been asserting nothing about
/// the other ~40 modules for as long as it has existed.
fn offenders() -> Vec<(String, u64)> {
    published_data()
        .into_iter()
        .filter(|(_, addr)| !is_live_host_storage(*addr))
        .collect()
}

#[test]
fn a_data_row_publishes_the_address_of_storage_and_not_a_value() {
    let published = published_data();
    let bad = offenders();
    assert!(
        bad.is_empty(),
        "{} of {} published data name(s) are not the address of anything in this \
         process, so a guest that loads through the slot lands outside it. A row \
         that publishes a *value* looks like this: the number reaches the slot, \
         the slot holds it, and the guest's next instruction dereferences it. \
         `machish::fixups::Binder::bind` writes `r.addr + addend` into the slot, \
         and `Resolved::addr` is documented as a host pointer valid in the host's \
         address space - so for these names the address of the storage is the \
         only thing the format can carry.\n\
         the rows:\n  {bad:#?}\n\
         all {} published data name(s) are otherwise checked, and this is the \
         list that is wrong, not a sample of it.",
        bad.len(),
        published.len(),
        published.len(),
    );
}

/// The two controls, and they are what make the test above evidence.
///
/// A predicate that accepts everything passes the test above for the wrong
/// reason. So: the numbers a value/address confusion would actually publish must
/// be **rejected**, and a real `static`'s address must be **accepted** - the
/// second with a positive control that the parse in `readable_mappings` is
/// finding this process's data at all.
#[test]
fn the_predicate_agrees_with_the_kernel_about_a_value() {
    // What a confused row publishes. `1` is `MB_CUR_MAX` itself, `0x1000` and
    // `0x2000` are the shapes `crates/exports`' own macro tests use, `4096` is
    // a page size, and `0xdead_beef` is what a hand-written test fixture uses.
    for value in [1u64, 0x1000, 0x2000, 4096, 0xdead_beef, u64::MAX] {
        assert!(
            !is_live_host_storage(value),
            "{value:#x} was accepted as storage, so the test above would pass a \
             row that published a value"
        );
    }
    // Zero cannot appear: `darwin::provider()`'s sink is `exports::NonNull`,
    // which drops a zero address so the import is left unbound and refused *by
    // name* at the call, which is the designed answer and is better than
    // publishing an address the guest would dereference. Asserted so that if
    // that ever changes, the reason is written down here.
    assert!(
        !published_data().values().any(|a| *a == 0),
        "a data row published address 0; `NonNull` is supposed to drop it so the \
         name stays unbound"
    );
    // The positive control: the address of a real `static` in this test binary.
    static HERE: u64 = 0;
    assert!(
        is_live_host_storage((&raw const HERE) as u64),
        "a real static's address was rejected, so the predicate is not finding \
         this process's data and the test above is vacuous"
    );
    assert_eq!(
        HERE, 0,
        "the control static is read by the test above, so it must not be optimised out"
    );
}

/// The instance that was reported, stated as the property it actually has.
///
/// `__mb_cur_max` is Darwin's `int`, so the address published for it is the
/// address of a four-byte cell and a guest's `movl (%rax), %eax` off the bound
/// slot yields a number. That is asserted through the **loader**, and against
/// the cell the row names, so a change to the row's expression changes this test
/// rather than being noticed by nobody.
#[test]
fn a_numeric_constant_is_published_so_that_loads_through_it_yield_the_number() {
    const NAME: &str = "___mb_cur_max";
    let published = published_data();
    let addr = *published
        .get(NAME)
        .unwrap_or_else(|| panic!("{NAME} is published as data, as `xlocale.rs` declares"));

    // The row's contract, in the order it matters: the address is storage, the
    // storage is four bytes wide and aligned, and what is in it is a number a
    // C locale can have.
    assert!(
        is_live_host_storage(addr),
        "{NAME} is published at {addr:#x}, which is not storage in this process"
    );
    assert_eq!(
        addr % 4,
        0,
        "{NAME} is a Darwin `int` and its storage is 4-aligned"
    );

    // SAFETY: the assertion above established eight readable bytes at `addr`.
    let bytes = unsafe { read_bytes::<4>(addr) };
    let value = i32::from_le_bytes(bytes);

    // MB_CUR_MAX is the C locale's 1 and UTF-8's 4 to 6. A page size or a host
    // pointer here would mean the row and the cell have come apart.
    assert!(
        (1..=16).contains(&value),
        "{NAME} is published at {addr:#x} and a 4-byte load there is {value}, \
         which is not an `MB_CUR_MAX`; the row and the cell it names have diverged"
    );

    // The reported defect, restated so it cannot be reintroduced quietly: the
    // published number is the address, and the number the guest loads is a
    // different number. Publishing the value would make these equal, and the
    // address 1, which the test above already rejects.
    assert_ne!(
        value, addr as i32,
        "{NAME} is published at an address equal to the value behind it, which is \
         what publishing the value would look like"
    );
}

/// The rest of the family, by hand, because "the name denotes a number" is a
/// claim about each row rather than a property of the table.
///
/// Six names in this tree are published at the address of a **numeric global**,
/// and every one of them is a place where a value published in place of the
/// address would be invisible: the slot would hold a small integer, the guest
/// would load through it, and nothing would say which symbol it was. So each is
/// checked at the width and the range its own declaration gives it.
///
/// Two of the six are the point of the exercise, and the note in
/// `mach/mod.rs` states the rule in this tree's own words:
///
/// ```text
/// /// The loader binds the address and the guest reads the word through it, so the
/// /// value has to be there before the symbol is published rather than before it is
/// /// read.
/// ```
///
/// which is exactly why those rows take `&raw const` of a global that
/// `publish_page_size()` has already filled, and publish the address rather than
/// the number. The same shape is `_mach_task_self_` and `_bootstrap_port`, whose
/// `mach_port_t` is an `unsigned int`: a name denoting a number, published at the
/// address of one.
#[test]
fn every_numeric_global_is_published_at_storage_a_guest_can_load() {
    // Declared here rather than reached through `darwin::` on purpose: a test
    // that asked the crate for the expected address could not tell a row that
    // publishes storage from one that publishes the same storage for the wrong
    // reason, and the widths and ranges below are the whole of the claim.
    let cases: &[(&str, usize, &str)] = &[
        // mach_port_t, an `unsigned int`; must be a real port, not zero.
        ("_mach_task_self_", 4, "a mach_port_t"),
        ("_bootstrap_port", 4, "a mach_port_t"),
        // vm_size_t, an `unsigned long`; a power of two in the host's range.
        ("_vm_page_size", 8, "a vm_size_t"),
        ("_vm_page_mask", 8, "a vm_size_t"),
        // `int vm_page_shift` in <mach/vm_param.h>.
        ("_vm_page_shift", 4, "an int"),
    ];
    let published = published_data();
    let mut seen = Vec::new();
    for (name, width, what) in cases {
        let addr = *published
            .get(*name)
            .unwrap_or_else(|| panic!("{name} is published as data, as `mach/mod.rs` declares"));
        assert!(
            is_live_host_storage(addr),
            "{name} is published at {addr:#x}, which is not storage in this process"
        );
        assert_eq!(
            addr % *width as u64,
            0,
            "{name} is {what}, so its storage is {width}-byte aligned"
        );
        // SAFETY: the assertion above established eight readable bytes at `addr`,
        // and `width <= 8`.
        let buf = unsafe { read_bytes::<8>(addr) };
        let mut value: u64 = 0;
        for (i, b) in buf.iter().take(*width).enumerate() {
            value |= (*b as u64) << (8 * i);
        }
        assert_ne!(value, 0, "{name} is published at storage holding zero");

        // Read it again. These rows point at a `static`, so the number behind
        // the address cannot change between two reads; a row whose expression
        // *computed* something per call would answer a different number each
        // time `provide()` is asked, and the loader and a `dlsym` would disagree
        // - which is the split-brain `crates/darwin/tests/published_census.rs`
        // exists for, caught here from the other direction.
        // SAFETY: as above.
        let again = unsafe { read_bytes::<8>(addr) };
        assert_eq!(
            buf, again,
            "{name} is published at storage whose contents changed between two \
             reads, so it is not a `static` and the address is not a fixed datum"
        );
        seen.push((*name, value));
    }

    let get = |n: &str| seen.iter().find(|(k, _)| *k == n).map(|(_, v)| *v).unwrap();
    // mask is defined as size - 1, so this is a relation between the two rows
    // rather than two literals: a row that published the *value* would break it.
    assert_eq!(
        get("_vm_page_mask"),
        get("_vm_page_size") - 1,
        "`vm_page_mask` is `vm_page_size - 1` on Darwin; the two published values \
         do not agree, so at least one of them is not the number the guest reads"
    );
    // A page size is a power of two, and the shift is its base-2 log.
    assert!(
        get("_vm_page_size").is_power_of_two(),
        "`vm_page_size` reads as {}, which is not a power of two",
        get("_vm_page_size")
    );
    assert_eq!(
        get("_vm_page_shift"),
        get("_vm_page_size").trailing_zeros() as u64,
        "`vm_page_shift` must be the base-2 log of `vm_page_size`"
    );
}
