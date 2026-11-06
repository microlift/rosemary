//! `NSFilePromiseReceiver`, the receiving half of a promised-files drag.
//!
//! FIXME (ACCURACY): dragging a file promise *out of* this runtime is not
//! implemented (`crate::pasteboard` writes no promise types), so nothing a
//! guest drags ever carries one in. A receiver therefore always reports an
//! empty promise rather than running the one it was never given - the
//! correct answer for the promise this process can actually see, not an
//! approximation of a real drop.

use objc::object::Id;
use objc::sel::Sel;

use crate::define::methods;

fn empty_array() -> Id {
    corefoundation::collections::new_array(
        &[],
        corefoundation::callbacks::Policy::from_type_callbacks(),
        false,
    ) as Id
}

unsafe extern "C" fn file_types(_receiver: Id, _sel: Sel) -> Id {
    objc::arc::autorelease(empty_array())
}

unsafe extern "C" fn file_names(_receiver: Id, _sel: Sel) -> Id {
    objc::arc::autorelease(empty_array())
}

/// `-receivePromisedFilesAtDestination:options:operationQueue:reader:`: the
/// reader block is called once per promised file, and there are none - see
/// the module FIXME.
unsafe extern "C" fn receive_promised_files(
    _receiver: Id,
    _sel: Sel,
    _destination: Id,
    _options: Id,
    _queue: Id,
    _reader: Id,
) {
}

pub(crate) fn install() {
    let class = objc::class::lookup(c"NSFilePromiseReceiver").expect("declared before its methods");
    methods!(class, {
        "fileTypes" => file_types, c"@16@0:8";
        "fileNames" => file_names, c"@16@0:8";
        "receivePromisedFilesAtDestination:options:operationQueue:reader:" =>
            receive_promised_files, c"v48@0:8@16@24@32@40";
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_receiver_always_promises_nothing() {
        crate::bootstrap();
        let class = objc::class::lookup(c"NSFilePromiseReceiver").expect("declared");
        let receiver = objc::object::create_instance(class, 0);
        let types = unsafe { file_types(receiver, objc::sel::get("fileTypes")) };
        let count = unsafe { corefoundation::collections::array(types as *const std::ffi::c_void) }
            .map_or(usize::MAX, |a| a.values.len());
        assert_eq!(count, 0);
        objc::arc::release(receiver);
    }
}
