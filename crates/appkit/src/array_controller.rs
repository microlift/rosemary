//! `NSArrayController`: an object controller whose content is an array (or a
//! set), arranged by a filter predicate and sort descriptors, with a selection
//! of arranged indexes.

use objc::object::Id;
use objc::sel::Sel;

use crate::define::{class_methods, methods};
use crate::tree_controller::{
    ObjectController, announce, array, class_named, elements, object, object_dealloc, object_init,
    replace, replace_with_copy, send_id, send_id_id, send_word,
};

pub(crate) const INSTANCE_SIZE: usize = size_of::<ArrayController>();

/// `NSNotFound`.
const NOT_FOUND: usize = isize::MAX as usize;

#[repr(C)]
struct ArrayController {
    base: ObjectController,
    /// The arranged objects: a retained array, or nil until next asked for.
    arranged: Id,
    /// Retained, or nil.
    sort_descriptors: Id,
    filter_predicate: Id,
    /// Retained immutable `NSIndexSet`, or nil for no selection.
    selection: Id,
    avoids_empty_selection: u8,
    preserves_selection: u8,
    selects_inserted_objects: u8,
    automatically_rearranges_objects: u8,
    clears_filter_predicate_on_insertion: u8,
    filter_restricts_insertion: u8,
    always_uses_multiple_values_marker: u8,
}

fn controller(receiver: Id) -> &'static mut ArrayController {
    // SAFETY: an `NSArrayController`, laid out as this module allocates it.
    unsafe { &mut *(receiver as *mut ArrayController) }
}

fn is_kind(object: Id, name: &std::ffi::CStr) -> bool {
    // SAFETY: a live object or nil.
    !object.is_null()
        && objc::class::lookup(name)
            .is_some_and(|class| unsafe { corefoundation::base::is_kind_of(object.cast(), class) })
}

/// The content as an array: an array as it is, a set's objects in its order.
fn content_array(receiver: Id) -> Id {
    match object(receiver).content {
        content if content.is_null() => array(&[]),
        content if is_kind(content, c"NSSet") => send_id(content, "allObjects"),
        content => content,
    }
}

fn arranged(receiver: Id) -> Id {
    let state = controller(receiver);
    if state.arranged.is_null() {
        let made = send_id_id(receiver, "arrangeObjects:", content_array(receiver));
        controller(receiver).arranged = objc::arc::retain(match made.is_null() {
            true => array(&[]),
            false => made,
        });
    }
    controller(receiver).arranged
}

fn arranged_elements(receiver: Id) -> Vec<Id> {
    elements(arranged(receiver), &|| {
        String::from("-[NSArrayController arrangedObjects]")
    })
}

fn indexes_of(set: Id) -> Vec<usize> {
    match set.is_null() {
        true => Vec::new(),
        false => foundation::indexset::ranges_of(set)
            .into_iter()
            .flat_map(|(start, end)| start..end)
            .collect(),
    }
}

fn selected(receiver: Id) -> Vec<usize> {
    indexes_of(controller(receiver).selection)
}

/// `-setSelectionIndexes:`, sent so its observers are told.
fn select(receiver: Id, indexes: &[usize]) -> u8 {
    let set = foundation::indexset::from_indexes(indexes);
    // SAFETY: `BOOL (id, SEL, NSIndexSet *)`.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel, Id) -> u8 = std::mem::transmute(objc::msg::entry());
        f(receiver, objc::sel::get("setSelectionIndexes:"), set)
    }
}

/// Arrange the content again, announcing it as a change of
/// `arrangedObjects`, then choose the selection again.
fn rearrange(receiver: Id, change: impl FnOnce()) {
    let before = arranged_elements(receiver);
    let old = selected(receiver);
    let previously: Vec<Id> = old
        .iter()
        .filter_map(|i| before.get(*i))
        .map(|o| objc::arc::retain(*o))
        .collect();
    announce(receiver, "willChangeValueForKey:", "arrangedObjects");
    change();
    let stale = std::mem::replace(&mut controller(receiver).arranged, std::ptr::null_mut());
    objc::arc::release(stale);
    let now = arranged_elements(receiver);
    announce(receiver, "didChangeValueForKey:", "arrangedObjects");

    let state = controller(receiver);
    let mut indexes: Vec<usize> = match state.preserves_selection != 0 {
        true => previously
            .iter()
            .filter_map(|wanted| now.iter().position(|o| o == wanted))
            .collect(),
        // FIXME (ACCURACY): without preserving the selected objects, the
        // indexes still in range are kept; inferred, not observed.
        false => old.iter().copied().filter(|i| *i < now.len()).collect(),
    };
    previously.into_iter().for_each(objc::arc::release);
    indexes.sort_unstable();
    indexes.dedup();
    if indexes.is_empty() && state.avoids_empty_selection != 0 && !now.is_empty() {
        indexes.push(0);
    }
    if indexes != old {
        select(receiver, &indexes);
    }
}

unsafe extern "C" fn init(receiver: Id, sel: Sel) -> Id {
    // SAFETY: the object controller's `-init`, on the same receiver.
    let receiver = unsafe { object_init(receiver, sel) };
    if receiver.is_null() {
        return receiver;
    }
    let state = controller(receiver);
    state.avoids_empty_selection = 1;
    state.preserves_selection = 1;
    state.selects_inserted_objects = 1;
    state.clears_filter_predicate_on_insertion = 1;
    state.filter_restricts_insertion = 1;
    receiver
}

unsafe extern "C" fn dealloc(receiver: Id, sel: Sel) {
    let state = controller(receiver);
    for field in [
        state.arranged,
        state.sort_descriptors,
        state.filter_predicate,
        state.selection,
    ] {
        objc::arc::release(field);
    }
    // SAFETY: the object controller's part, which finishes the chain.
    unsafe { object_dealloc(receiver, sel) };
}

unsafe extern "C" fn set_content(receiver: Id, _sel: Sel, content: Id) {
    rearrange(receiver, || replace(&mut object(receiver).content, content));
}

unsafe extern "C" fn arranged_objects(receiver: Id, _sel: Sel) -> Id {
    arranged(receiver)
}

/// `-arrangeObjects:`: filtered by the predicate, then sorted. A subclass
/// overrides this to arrange its own way.
unsafe extern "C" fn arrange_objects(receiver: Id, _sel: Sel, objects: Id) -> Id {
    let state = controller(receiver);
    let mut arranged = objects;
    if !state.filter_predicate.is_null() {
        arranged = send_id_id(
            arranged,
            "filteredArrayUsingPredicate:",
            state.filter_predicate,
        );
    }
    if !state.sort_descriptors.is_null() && send_word(state.sort_descriptors, "count") != 0 {
        arranged = send_id_id(
            arranged,
            "sortedArrayUsingDescriptors:",
            state.sort_descriptors,
        );
    }
    arranged
}

unsafe extern "C" fn rearrange_objects(receiver: Id, _sel: Sel) {
    rearrange(receiver, || {});
}

unsafe extern "C" fn sort_descriptors(receiver: Id, _sel: Sel) -> Id {
    match controller(receiver).sort_descriptors {
        none if none.is_null() => array(&[]),
        descriptors => descriptors,
    }
}

unsafe extern "C" fn set_sort_descriptors(receiver: Id, _sel: Sel, descriptors: Id) {
    rearrange(receiver, || {
        replace_with_copy(&mut controller(receiver).sort_descriptors, descriptors)
    });
}

unsafe extern "C" fn filter_predicate(receiver: Id, _sel: Sel) -> Id {
    controller(receiver).filter_predicate
}

unsafe extern "C" fn set_filter_predicate(receiver: Id, _sel: Sel, predicate: Id) {
    rearrange(receiver, || {
        replace(&mut controller(receiver).filter_predicate, predicate)
    });
}

unsafe extern "C" fn selection_indexes(receiver: Id, _sel: Sel) -> Id {
    match controller(receiver).selection {
        none if none.is_null() => foundation::indexset::from_indexes(&[]),
        selection => selection,
    }
}

/// `-setSelectionIndexes:`, which answers whether the selection changed to
/// what was asked for; it always does here.
unsafe extern "C" fn set_selection_indexes(receiver: Id, _sel: Sel, indexes: Id) -> u8 {
    replace_with_copy(&mut controller(receiver).selection, indexes);
    1
}

unsafe extern "C" fn selection_index(receiver: Id, _sel: Sel) -> usize {
    selected(receiver).first().copied().unwrap_or(NOT_FOUND)
}

unsafe extern "C" fn set_selection_index(receiver: Id, _sel: Sel, index: usize) -> u8 {
    match index {
        NOT_FOUND => select(receiver, &[]),
        index => select(receiver, &[index]),
    }
}

unsafe extern "C" fn add_selection_indexes(receiver: Id, _sel: Sel, indexes: Id) -> u8 {
    let mut selection = selected(receiver);
    selection.extend(indexes_of(indexes));
    selection.sort_unstable();
    selection.dedup();
    select(receiver, &selection)
}

unsafe extern "C" fn remove_selection_indexes(receiver: Id, _sel: Sel, indexes: Id) -> u8 {
    let gone = indexes_of(indexes);
    let mut selection = selected(receiver);
    selection.retain(|i| !gone.contains(i));
    select(receiver, &selection)
}

unsafe extern "C" fn selected_objects(receiver: Id, _sel: Sel) -> Id {
    let objects = arranged_elements(receiver);
    let picked: Vec<Id> = selected(receiver)
        .into_iter()
        .filter_map(|i| objects.get(i).copied())
        .collect();
    array(&picked)
}

unsafe extern "C" fn set_selected_objects(receiver: Id, _sel: Sel, wanted: Id) -> u8 {
    let objects = arranged_elements(receiver);
    let mut indexes: Vec<usize> = elements(wanted, &|| String::from("selected objects"))
        .into_iter()
        .filter_map(|w| objects.iter().position(|o| *o == w))
        .collect();
    indexes.sort_unstable();
    indexes.dedup();
    select(receiver, &indexes)
}

unsafe extern "C" fn can_select_next(receiver: Id, _sel: Sel) -> u8 {
    let count = arranged_elements(receiver).len();
    selected(receiver).last().is_some_and(|i| i + 1 < count) as u8
}

unsafe extern "C" fn can_select_previous(receiver: Id, _sel: Sel) -> u8 {
    selected(receiver).first().is_some_and(|i| *i > 0) as u8
}

unsafe extern "C" fn select_next(receiver: Id, sel: Sel, _sender: Id) {
    // SAFETY: the same receiver.
    if unsafe { can_select_next(receiver, sel) } != 0 {
        let last = *selected(receiver).last().expect("checked above");
        select(receiver, &[last + 1]);
    }
}

unsafe extern "C" fn select_previous(receiver: Id, sel: Sel, _sender: Id) {
    // SAFETY: the same receiver.
    if unsafe { can_select_previous(receiver, sel) } != 0 {
        let first = selected(receiver)[0];
        select(receiver, &[first - 1]);
    }
}

/// `+keyPathsForValuesAffectingSelectionIndex`.
unsafe extern "C" fn affected_by_selection(_class: Id, _sel: Sel) -> Id {
    let key = foundation::string::from_str("selectionIndexes");
    let set = send_id_id(class_named(c"NSSet"), "setWithObject:", key);
    objc::arc::release(key);
    set
}

/// `+keyPathsForValuesAffectingSelectedObjects`: the selection, and what it
/// indexes into.
unsafe extern "C" fn affected_by_selection_and_arrangement(_class: Id, _sel: Sel) -> Id {
    let keys = [
        foundation::string::from_str("selectionIndexes"),
        foundation::string::from_str("arrangedObjects"),
    ];
    // SAFETY: `id (Class, SEL, const id *, NSUInteger)`.
    let set = unsafe {
        let f: unsafe extern "C" fn(Id, Sel, *const Id, usize) -> Id =
            std::mem::transmute(objc::msg::entry());
        f(
            class_named(c"NSSet"),
            objc::sel::get("setWithObjects:count:"),
            keys.as_ptr(),
            keys.len(),
        )
    };
    keys.into_iter().for_each(objc::arc::release);
    set
}

macro_rules! flag_accessors {
    ($($field:ident, $getter:ident, $setter:ident;)*) => {$(
        unsafe extern "C" fn $getter(receiver: Id, _sel: Sel) -> u8 {
            controller(receiver).$field
        }
        unsafe extern "C" fn $setter(receiver: Id, _sel: Sel, flag: u8) {
            controller(receiver).$field = (flag != 0) as u8;
        }
    )*};
}

flag_accessors! {
    avoids_empty_selection, avoids_empty_selection, set_avoids_empty_selection;
    preserves_selection, preserves_selection, set_preserves_selection;
    selects_inserted_objects, selects_inserted_objects, set_selects_inserted_objects;
    clears_filter_predicate_on_insertion, clears_filter_predicate_on_insertion,
    set_clears_filter_predicate_on_insertion;
    filter_restricts_insertion, filter_restricts_insertion, set_filter_restricts_insertion;
    always_uses_multiple_values_marker, always_uses_multiple_values_marker,
    set_always_uses_multiple_values_marker;
}

unsafe extern "C" fn automatically_rearranges_objects(receiver: Id, _sel: Sel) -> u8 {
    controller(receiver).automatically_rearranges_objects
}

/// FIXME (ACCURACY): recorded only; the content's objects are not watched
/// for changes to the keys the predicate and sort descriptors read.
unsafe extern "C" fn set_automatically_rearranges_objects(receiver: Id, _sel: Sel, flag: u8) {
    controller(receiver).automatically_rearranges_objects = (flag != 0) as u8;
}

pub(crate) fn install() {
    let class = objc::class::lookup(c"NSArrayController").expect("declared before its methods");
    methods!(class, {
        "init" => init, c"@16@0:8";
        "dealloc" => dealloc, c"v16@0:8";
        "setContent:" => set_content, c"v24@0:8@16";
        "arrangedObjects" => arranged_objects, c"@16@0:8";
        "arrangeObjects:" => arrange_objects, c"@24@0:8@16";
        "rearrangeObjects" => rearrange_objects, c"v16@0:8";
        "sortDescriptors" => sort_descriptors, c"@16@0:8";
        "setSortDescriptors:" => set_sort_descriptors, c"v24@0:8@16";
        "filterPredicate" => filter_predicate, c"@16@0:8";
        "setFilterPredicate:" => set_filter_predicate, c"v24@0:8@16";
        "selectionIndexes" => selection_indexes, c"@16@0:8";
        "setSelectionIndexes:" => set_selection_indexes, c"c24@0:8@16";
        "selectionIndex" => selection_index, c"Q16@0:8";
        "setSelectionIndex:" => set_selection_index, c"c24@0:8Q16";
        "addSelectionIndexes:" => add_selection_indexes, c"c24@0:8@16";
        "removeSelectionIndexes:" => remove_selection_indexes, c"c24@0:8@16";
        "selectedObjects" => selected_objects, c"@16@0:8";
        "setSelectedObjects:" => set_selected_objects, c"c24@0:8@16";
        "canSelectNext" => can_select_next, c"c16@0:8";
        "canSelectPrevious" => can_select_previous, c"c16@0:8";
        "selectNext:" => select_next, c"v24@0:8@16";
        "selectPrevious:" => select_previous, c"v24@0:8@16";
        "avoidsEmptySelection" => avoids_empty_selection, c"c16@0:8";
        "setAvoidsEmptySelection:" => set_avoids_empty_selection, c"v20@0:8c16";
        "preservesSelection" => preserves_selection, c"c16@0:8";
        "setPreservesSelection:" => set_preserves_selection, c"v20@0:8c16";
        "selectsInsertedObjects" => selects_inserted_objects, c"c16@0:8";
        "setSelectsInsertedObjects:" => set_selects_inserted_objects, c"v20@0:8c16";
        "clearsFilterPredicateOnInsertion" => clears_filter_predicate_on_insertion, c"c16@0:8";
        "setClearsFilterPredicateOnInsertion:" => set_clears_filter_predicate_on_insertion,
        c"v20@0:8c16";
        "filterRestrictsInsertion" => filter_restricts_insertion, c"c16@0:8";
        "setFilterRestrictsInsertion:" => set_filter_restricts_insertion, c"v20@0:8c16";
        "alwaysUsesMultipleValuesMarker" => always_uses_multiple_values_marker, c"c16@0:8";
        "setAlwaysUsesMultipleValuesMarker:" => set_always_uses_multiple_values_marker,
        c"v20@0:8c16";
        "automaticallyRearrangesObjects" => automatically_rearranges_objects, c"c16@0:8";
        "setAutomaticallyRearrangesObjects:" => set_automatically_rearranges_objects,
        c"v20@0:8c16";
    });
    class_methods!(class, {
        "keyPathsForValuesAffectingSelectionIndex" => affected_by_selection, c"@16@0:8";
        "keyPathsForValuesAffectingSelectedObjects" => affected_by_selection_and_arrangement,
        c"@16@0:8";
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn string(text: &str) -> Id {
        objc::arc::autorelease(foundation::string::from_str(text))
    }

    fn controller_of(content: &[Id]) -> Id {
        crate::bootstrap();
        let controller = send_id(send_id(class_named(c"NSArrayController"), "alloc"), "init");
        send_id_id(controller, "setContent:", array(content));
        controller
    }

    fn arranged_texts(controller: Id) -> Vec<String> {
        arranged_elements(controller)
            .into_iter()
            // SAFETY: strings.
            .map(|s| String::from_utf8_lossy(&unsafe { foundation::string::bytes(s) }).into_owned())
            .collect()
    }

    #[test]
    fn sort_descriptors_arrange_the_content() {
        crate::bootstrap();
        let controller = controller_of(&[string("b"), string("c"), string("a")]);
        // SAFETY: `+sortDescriptorWithKey:ascending:` is `id (Class, SEL, NSString *, BOOL)`.
        let descriptor = unsafe {
            let f: unsafe extern "C" fn(Id, Sel, Id, u8) -> Id =
                std::mem::transmute(objc::msg::entry());
            f(
                class_named(c"NSSortDescriptor"),
                objc::sel::get("sortDescriptorWithKey:ascending:"),
                string("self"),
                1,
            )
        };
        send_id_id(controller, "setSortDescriptors:", array(&[descriptor]));
        assert_eq!(arranged_texts(controller), ["a", "b", "c"]);
        objc::arc::release(controller);
    }

    #[test]
    fn the_selection_follows_its_object_through_a_rearrangement() {
        let (a, b) = (string("a"), string("b"));
        let controller = controller_of(&[a, b]);
        assert_eq!(selected(controller), [0], "avoids an empty selection");
        select(controller, &[1]);
        send_id_id(controller, "setContent:", array(&[b, a]));
        assert_eq!(selected(controller), [0]);
        assert_eq!(
            send_word(send_id(controller, "selectedObjects"), "count"),
            1
        );
        objc::arc::release(controller);
    }
}
