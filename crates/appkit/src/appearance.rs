use std::cell::Cell;
use std::sync::Mutex;

use objc::object::Id;
use objc::sel::Sel;

use crate::define::{class_methods, methods};

pub const AQUA: &str = "NSAppearanceNameAqua";
pub const DARK_AQUA: &str = "NSAppearanceNameDarkAqua";
pub const NAMES: &[&str] = &[
    AQUA,
    DARK_AQUA,
    "NSAppearanceNameLightContent",
    "NSAppearanceNameVibrantLight",
    "NSAppearanceNameVibrantDark",
];
pub const MATCHING_NAMES: &[&str] = &[
    "NSAppearanceNameAccessibilityHighContrastAqua",
    "NSAppearanceNameAccessibilityHighContrastDarkAqua",
    "NSAppearanceNameAccessibilityHighContrastVibrantLight",
    "NSAppearanceNameAccessibilityHighContrastVibrantDark",
];

#[repr(C)]
pub struct Appearance {
    isa: *const std::ffi::c_void,
    name: Id,
}

static NAMED: Mutex<Vec<(&'static str, usize)>> = Mutex::new(Vec::new());
thread_local! {
    static CURRENT: Cell<usize> = const { Cell::new(0) };
}

fn named(text: &str) -> Id {
    let Some(&text) = NAMES.iter().find(|&&name| name == text) else {
        return std::ptr::null_mut();
    };
    let mut cache = NAMED.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((_, object)) = cache.iter().find(|(name, _)| *name == text) {
        return *object as Id;
    }
    let class = objc::class::lookup(c"NSAppearance").expect("declared before its methods");
    let object = objc::object::create_instance(class, 0);
    unsafe {
        (*(object as *mut Appearance)).name = foundation::notification::name_object(text) as Id;
    }
    objc::arc::retain(object);
    cache.push((text, object as usize));
    object
}

pub fn aqua() -> Id {
    named(AQUA)
}

/// What a program that sets no appearance of its own is given: Aqua when it
/// was built before Dark Mode existed, or asks for Aqua in its `Info.plist`,
/// as AppKit decides; the system's otherwise.
pub fn for_program() -> Id {
    match requires_aqua(
        darwin::images::program_sdk(),
        foundation::bundle::main().and_then(|b| b.info.get("NSRequiresAquaSystemAppearance")),
    ) {
        true => aqua(),
        false => system(),
    }
}

/// The 10.14 SDK, the first that knew Dark Mode.
const DARK_MODE_SDK: u32 = 0x000A_0E00;

fn requires_aqua(sdk: Option<u32>, key: Option<&foundation::plist::Value>) -> bool {
    let asked = match key {
        Some(foundation::plist::Value::Bool(value)) => *value,
        Some(other) => other
            .as_i64()
            .map(|value| value != 0)
            .or_else(|| {
                other
                    .as_str()
                    .map(|text| matches!(text, "YES" | "true" | "1"))
            })
            .unwrap_or(false),
        None => false,
    };
    asked || sdk.is_some_and(|sdk| sdk < DARK_MODE_SDK)
}

/// The host preference is live; named appearance objects keep their own names.
pub fn system() -> Id {
    named(
        if corefoundation::preferences::interface_style().is_some() {
            DARK_AQUA
        } else {
            AQUA
        },
    )
}

pub(crate) unsafe extern "C" fn name(receiver: Id, _sel: Sel) -> Id {
    unsafe { (*(receiver as *const Appearance)).name }
}

unsafe extern "C" fn appearance_named(_class: Id, _sel: Sel, name: Id) -> Id {
    let bytes = unsafe { foundation::string::bytes(name) };
    named(&String::from_utf8_lossy(&bytes))
}

unsafe extern "C" fn init_named(receiver: Id, sel: Sel, name: Id, _bundle: Id) -> Id {
    let appearance = unsafe { appearance_named(receiver, sel, name) };
    if !appearance.is_null() {
        unsafe { (*(receiver as *mut Appearance)).name = self::name(appearance, sel) };
        receiver
    } else {
        objc::arc::release(receiver);
        std::ptr::null_mut()
    }
}

unsafe extern "C" fn current(_class: Id, _sel: Sel) -> Id {
    CURRENT.with(|current| {
        let appearance = current.get() as Id;
        if appearance.is_null() {
            effective_of(crate::application::shared())
        } else {
            appearance
        }
    })
}

unsafe extern "C" fn set_current(_class: Id, _sel: Sel, appearance: Id) {
    let appearance = objc::arc::retain(appearance);
    let old = CURRENT.with(|current| current.replace(appearance as usize));
    objc::arc::release(old as Id);
}

unsafe extern "C" fn perform_current(receiver: Id, sel: Sel, block: Id) {
    struct Restore(usize);
    impl Drop for Restore {
        fn drop(&mut self) {
            let old = CURRENT.with(|current| current.replace(self.0));
            objc::arc::release(old as Id);
        }
    }
    let previous = CURRENT.with(Cell::get);
    let _restore = Restore(objc::arc::retain(previous as Id) as usize);
    unsafe { set_current(std::ptr::null_mut(), sel, receiver) };
    if let Some(invoke) = unsafe { crate::animation::invoke(block) } {
        let invoke: unsafe extern "C" fn(Id) = unsafe { std::mem::transmute(invoke) };
        unsafe { invoke(block) };
    }
}

/// Draw this thread in Aqua, so a test's colours do not follow the host theme.
#[cfg(test)]
pub(crate) fn use_aqua_on_this_thread() {
    let sel = objc::sel::get("setCurrentAppearance:");
    // SAFETY: the class side ignores its receiver.
    unsafe { set_current(std::ptr::null_mut(), sel, aqua()) };
}

pub fn is_dark() -> bool {
    let appearance = unsafe {
        current(
            std::ptr::null_mut(),
            objc::sel::get("currentDrawingAppearance"),
        )
    };
    let name = unsafe { name(appearance, objc::sel::get("name")) };
    let name = unsafe { foundation::string::bytes(name) };
    dark(&String::from_utf8_lossy(&name))
}

fn dark(name: &str) -> bool {
    name.contains("Dark")
}

unsafe extern "C" fn allows_vibrancy(receiver: Id, sel: Sel) -> u8 {
    let name = unsafe { foundation::string::bytes(self::name(receiver, sel)) };
    String::from_utf8_lossy(&name).contains("Vibrant") as u8
}

unsafe extern "C" fn best_match(receiver: Id, sel: Sel, names: Id) -> Id {
    let Some(names) = (unsafe { corefoundation::collections::array(names.cast()) }) else {
        return std::ptr::null_mut();
    };
    let own = unsafe { foundation::string::bytes(name(receiver, sel)) };
    let own = String::from_utf8_lossy(&own);
    names
        .values
        .iter()
        .filter_map(|&name| {
            let bytes = unsafe { foundation::string::bytes(name as Id) };
            let candidate = String::from_utf8_lossy(&bytes);
            if !NAMES.contains(&candidate.as_ref()) && !MATCHING_NAMES.contains(&candidate.as_ref())
            {
                return None;
            }
            let score = if candidate == own {
                0
            } else if dark(&candidate) == dark(&own) {
                1
            } else {
                2
            };
            Some((score, name as Id))
        })
        .min_by_key(|(score, _)| *score)
        .map_or(std::ptr::null_mut(), |(_, name)| name)
}

/// Send through dispatch so guest subclasses can customize their appearance.
pub(crate) fn effective_of(object: Id) -> Id {
    if object.is_null() {
        return system();
    }
    let sel = objc::sel::get("effectiveAppearance");
    let class = unsafe { objc::object::class_of(object) }.expect("an appearance source");
    let imp = objc::class::method_implementation(class, sel);
    let call: unsafe extern "C" fn(Id, Sel) -> Id = unsafe { std::mem::transmute(imp) };
    unsafe { call(object, sel) }
}

pub(crate) fn install() {
    let class = objc::class::lookup(c"NSAppearance").expect("declared before its methods");
    class_methods!(class, {
        "appearanceNamed:" => appearance_named, c"@24@0:8@16";
        "currentAppearance" => current, c"@16@0:8";
        "currentDrawingAppearance" => current, c"@16@0:8";
        "setCurrentAppearance:" => set_current, c"v24@0:8@16";
    });
    methods!(class, {
        "name" => name, c"@16@0:8";
        "initWithAppearanceNamed:bundle:" => init_named, c"@32@0:8@16@24";
        "allowsVibrancy" => allows_vibrancy, c"c16@0:8";
        "bestMatchFromAppearancesWithNames:" => best_match, c"@24@0:8@16";
        "performAsCurrentDrawingAppearance:" => perform_current, c"v24@0:8@?16";
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_program_built_before_dark_mode_or_asking_for_aqua_requires_it() {
        use foundation::plist::Value;
        assert!(requires_aqua(Some(0x000A_0D00), None));
        assert!(!requires_aqua(Some(0x000A_0E00), None));
        assert!(!requires_aqua(None, None));
        assert!(requires_aqua(Some(0x000B_0000), Some(&Value::Bool(true))));
        assert!(!requires_aqua(Some(0x000B_0000), Some(&Value::Bool(false))));
    }

    #[test]
    fn named_appearances_are_stable_and_match_independently_of_the_system() {
        crate::bootstrap();
        for &text in NAMES {
            let appearance = named(text);
            assert_eq!(appearance, named(text));
            assert_eq!(
                unsafe { foundation::string::bytes(name(appearance, objc::sel::get("name"))) },
                text.as_bytes()
            );
        }
        assert!(named(MATCHING_NAMES[0]).is_null());
        let aqua = foundation::string::from_str(AQUA);
        let dark = foundation::string::from_str(DARK_AQUA);
        let names = corefoundation::collections::new_array(
            &[aqua.cast(), dark.cast()],
            corefoundation::callbacks::Policy::from_type_callbacks(),
            false,
        );
        let sel = objc::sel::get("bestMatchFromAppearancesWithNames:");
        assert_eq!(
            unsafe { best_match(named(DARK_AQUA), sel, names as Id) },
            dark
        );
        assert_eq!(unsafe { best_match(named(AQUA), sel, names as Id) }, aqua);
    }
}
