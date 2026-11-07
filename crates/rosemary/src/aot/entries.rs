//! Where an image's code is entered, most urgent first: what the offline compiler
//! compiles, in the order a time-boxed build reaches it.

use std::collections::HashSet;

use machish::image::lsda::Lsda;
use machish::image::{MachObject, UnwindInfo};
use machish::memory::AddressSpace;
use machish::report::ImageReport;
use machish::spec::arch::X86_64;

/// The selector a class's `+load` is sent as.
const LOAD_SELECTOR: &str = "load";

/// One image's entry points.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Entries {
    /// Slid addresses in priority order, each once.
    pub order: Vec<u64>,
    /// Every `LC_FUNCTION_STARTS` entry and landing pad, slid and ascending: the
    /// addresses a region declares as resume points when it holds them.
    pub declared: Vec<u64>,
}

/// What the file says about an image's code, independent of where it was placed.
pub struct Layout {
    /// Unslid `LC_FUNCTION_STARTS`, ascending.
    pub function_starts: Vec<u64>,
    pub unwind: UnwindInfo,
}

impl Layout {
    /// Reads `bytes`, the image's file, as placed at `slide`.
    pub fn parse(bytes: &[u8], slide: u64) -> machish::Result<Self> {
        let obj = MachObject::parse_any(bytes, X86_64)?;
        let function_starts = match obj.section("__TEXT", "__text") {
            Some(text) => machish::image::function_ranges(&obj, text.addr, text.addr + text.size)?
                .into_iter()
                .map(|range| range.start)
                .collect(),
            None => Vec::new(),
        };
        Ok(Layout {
            function_starts,
            unwind: UnwindInfo::parse(&obj, slide)?,
        })
    }
}

/// `report`'s entries, read from `memory` (its exception tables) and `layout`, with
/// `profiled` the unslid addresses earlier runs had to JIT.
///
/// The order is the roadmap's: Objective-C `IMP`s with every `+load` first, which
/// realization reaches before `main`; initializers; functions with an exception
/// table, then their landing pads; the entry point; what earlier runs executed; and
/// every other function start.
pub fn of<M: AddressSpace + ?Sized>(
    report: &ImageReport,
    memory: &M,
    layout: &Layout,
    profiled: &[u64],
) -> Entries {
    let slide = report.slide;
    let mut order = Ordered::new(report);

    if let Some(objc) = &report.objc {
        let class_loads = objc
            .classes
            .iter()
            .filter_map(|class| class.meta.as_ref())
            .flat_map(|meta| meta.ro.methods.iter());
        let category_loads = objc.categories.iter().flat_map(|c| c.class_methods.iter());
        for method in class_loads.chain(category_loads) {
            if method.name == LOAD_SELECTOR {
                order.push(method.imp);
            }
        }
        for imp in objc.implementations() {
            order.push(imp);
        }
    }

    for &initializer in &report.initializers {
        order.push(initializer);
    }

    let mut pads = Vec::new();
    for entry in layout.unwind.entries() {
        let Some(lsda) = entry.lsda else {
            continue;
        };
        order.push(entry.function);
        // A table that will not parse leaves only its function to compile; the
        // unwinder refuses the same table at run time.
        let Ok(table) = Lsda::parse(lsda, entry.function, memory) else {
            continue;
        };
        for pad in table.landing_pads(memory).unwrap_or_default() {
            order.push(pad);
            pads.push(pad);
        }
    }

    if let Some(entry) = report.entry {
        order.push(entry);
    }

    for &address in profiled {
        order.push(address.wrapping_add(slide));
    }

    let starts: Vec<u64> = layout
        .function_starts
        .iter()
        .map(|start| start.wrapping_add(slide))
        .collect();
    for &start in &starts {
        order.push(start);
    }

    let mut declared = starts;
    declared.extend(pads);
    declared.sort_unstable();
    declared.dedup();
    Entries {
        order: order.addresses,
        declared,
    }
}

/// Addresses in first-seen order, keeping only the image's own code.
struct Ordered<'a> {
    report: &'a ImageReport,
    seen: HashSet<u64>,
    addresses: Vec<u64>,
}

impl<'a> Ordered<'a> {
    fn new(report: &'a ImageReport) -> Self {
        Ordered {
            report,
            seen: HashSet::new(),
            addresses: Vec::new(),
        }
    }

    fn push(&mut self, address: u64) {
        let in_image = crate::program::code_image_in(self.report, address).is_some();
        if in_image && self.seen.insert(address) {
            self.addresses.push(address);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use machish::extensions::objc::{ClassRo, ObjcClass, ObjcImage, ObjcMetaclass, ObjcMethod};
    use machish::report::SegmentReport;

    const SLIDE: u64 = 0x1000_0000;
    const TEXT: u64 = 0x1_0000_0000 + SLIDE;

    fn method(name: &str, imp: u64) -> ObjcMethod {
        ObjcMethod {
            name: name.into(),
            types: "v16@0:8".into(),
            imp,
        }
    }

    fn ro(methods: Vec<ObjcMethod>) -> ClassRo {
        ClassRo {
            va: 0,
            flags: 0,
            instance_start: 0,
            instance_size: 0,
            name: "C".into(),
            methods,
            ivars: Vec::new(),
            properties: Vec::new(),
            protocols: Vec::new(),
            swift_initializer: 0,
        }
    }

    fn report() -> ImageReport {
        let mut report = ImageReport {
            slide: SLIDE,
            entry: Some(TEXT + 0x50),
            initializers: vec![TEXT + 0x40],
            ..ImageReport::default()
        };
        report.segments.push(SegmentReport {
            name: "__TEXT".into(),
            vmaddr: TEXT,
            vmsize: 0x1000,
            file_range: None,
            executable: true,
            writable: false,
        });
        report.objc = Some(ObjcImage {
            classes: vec![ObjcClass {
                va: 0,
                isa: 0,
                superclass: 0,
                bits: 0,
                ro: ro(vec![method("run", TEXT + 0x30)]),
                meta: Some(ObjcMetaclass {
                    va: 0,
                    isa: 0,
                    superclass: 0,
                    ro: ro(vec![
                        method("shared", TEXT + 0x20),
                        method(LOAD_SELECTOR, TEXT + 0x10),
                    ]),
                }),
            }],
            ..ObjcImage::default()
        });
        report
    }

    fn layout() -> Layout {
        Layout {
            function_starts: vec![
                TEXT - SLIDE + 0x10,
                TEXT - SLIDE + 0x60,
                TEXT - SLIDE + 0x70,
            ],
            unwind: UnwindInfo::default(),
        }
    }

    #[test]
    fn entries_come_in_priority_order_each_once() {
        let memory = machish::memory::BufferSpace::new(0);
        let entries = of(&report(), &memory, &layout(), &[0x1_0000_0070]);
        assert_eq!(
            entries.order,
            [
                TEXT + 0x10, // +load
                TEXT + 0x30, // instance method
                TEXT + 0x20, // class method
                TEXT + 0x40, // initializer
                TEXT + 0x50, // entry point
                TEXT + 0x70, // profiled
                TEXT + 0x60, // remaining function start
            ]
        );
    }

    #[test]
    fn addresses_outside_the_image_are_not_entries() {
        let mut report = report();
        report.initializers.push(0x42);
        let memory = machish::memory::BufferSpace::new(0);
        let entries = of(&report, &memory, &layout(), &[]);
        assert!(!entries.order.contains(&0x42));
    }
}
