#[repr(i32)]
enum CGWindowLevelKey {
    _BaseWindow = 0,
}

type CGWindowLevel = i32;

// FIXME: Window levels are not implemented; every key returns zero.
#[allow(non_snake_case)]
extern "C" fn CGWindowLevelForKey(_key: CGWindowLevelKey) -> CGWindowLevel {
    0
}

exports::exports! {
    functions {
        // `CGCursorIsVisible` was here too, re-exported from
        // `appkit::cursor`, and it is not: it is a **CoreGraphics** function
        // that ApplicationServices re-exports, and on a real system the
        // defining image is CoreGraphics, not ApplicationServices. Its two
        // siblings in the same family - the functions that actually *change*
        // what it answers - are `CGDisplayHideCursor` and
        // `CGDisplayShowCursor`, and both are published by `appkit`, which is
        // where this tree keeps the `CG*` C surface (`appkit::screen`,
        // `appkit::cgevent`, `appkit::graphics`, `appkit::cursor`). Three
        // functions over one process-wide counted hide level, split across two
        // publishers, is the shape of the bug PLAN.md §10 item 3 names: the
        // bind path and the `dlsym` path reach different code, and
        // `rosetta3::frameworks::tests::no_framework_defines_a_symbol_another_
        // one_does` is what says so.
        //
        // Nothing else loses the name. `rosetta3::frameworks` publishes every
        // framework into one flat synthetic dylib, and its `symbol` is a
        // `find_map` down the same list, in which AppKit comes first - so a
        // bind of `_CGCursorIsVisible` and a `dlsym` of it both land on
        // `appkit::cursor::CGCursorIsVisible` at the same address, exactly as
        // before.
        "CGWindowLevelForKey" => CGWindowLevelForKey,
    }
}
