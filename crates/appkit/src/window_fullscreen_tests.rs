use super::*;

thread_local! {
    static RESIZES: std::cell::RefCell<Vec<(NSRect, NSSize)>> = const { std::cell::RefCell::new(Vec::new()) };
    static TRANSITION_REQUEST: std::cell::Cell<Option<(bool, NSRect)>> = const { std::cell::Cell::new(None) };
    static TRANSITION_ORIGIN: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static TRANSITION_SEEN: std::cell::Cell<Option<(bool, NSRect, NSRect)>> = const { std::cell::Cell::new(None) };
}

unsafe extern "C" fn resized(receiver: Id, _sel: Sel, _notification: Id) {
    let actual = unsafe { frame(receiver, objc::sel::get("frame")) };
    let content = crate::view::frame_of(window_of(receiver).content_view).size;
    RESIZES.with_borrow_mut(|seen| seen.push((actual, content)));
}

unsafe extern "C" fn transition(receiver: Id, sel: Sel, _notification: Id) {
    let entering = sel == objc::sel::get("windowWillEnterFullScreen:");
    if let Some((wanted, requested)) = TRANSITION_REQUEST.get()
        && wanted == entering
    {
        TRANSITION_REQUEST.set(None);
        let was_fullscreen =
            unsafe { style_mask(receiver, objc::sel::get("styleMask")) } & style::FULL_SCREEN != 0;
        let before = unsafe { frame(receiver, objc::sel::get("frame")) };
        if TRANSITION_ORIGIN.get() {
            unsafe {
                set_frame_origin(
                    receiver,
                    objc::sel::get("setFrameOrigin:"),
                    requested.origin,
                )
            };
        } else {
            unsafe { set_frame(receiver, objc::sel::get("setFrame:"), requested) };
        }
        let after = unsafe { frame(receiver, objc::sel::get("frame")) };
        TRANSITION_SEEN.set(Some((was_fullscreen, before, after)));
    }
}

fn oracle_rows() -> impl Iterator<Item = Vec<&'static str>> {
    include_str!("../tests/fixtures/fullscreen-macos-26.5.tsv")
        .lines()
        .filter(|line| !line.starts_with('#'))
        .map(|line| line.split('\t').collect())
}

#[test]
fn native_fullscreen_frame_constraints_match_the_mac_capture() {
    let host = NSRect {
        origin: NSPoint::default(),
        size: NSSize {
            width: 1440.0,
            height: 900.0,
        },
    };
    let mut count = 0;
    for row in oracle_rows().filter(|row| !matches!(row[2], "origin" | "content-size")) {
        let value = |index: usize| row[index].parse::<f64>().unwrap();
        let requested = NSRect {
            origin: NSPoint {
                x: value(4),
                y: value(5),
            },
            size: NSSize {
                width: value(6),
                height: value(7),
            },
        };
        let expected = NSRect {
            origin: NSPoint {
                x: value(8),
                y: value(9),
            },
            size: NSSize {
                width: value(10),
                height: value(11),
            },
        };
        assert_eq!(
            constrain_fullscreen_frame(requested, host),
            expected,
            "native case {} step {}",
            row[0],
            row[3]
        );
        count += 1;
    }
    assert_eq!(count, 18);
}

/// Run explicitly under a window manager, in its own process so SDL's
/// video thread and event queue are owned by this test throughout.
#[test]
#[ignore = "requires an X11/Wayland window manager; see tests/fixtures/fullscreen-native.md"]
fn native_fullscreen_oracle() {
    crate::bootstrap();
    let pool = objc::arc::pool_push();
    let class = objc::class::allocate_pair(
        objc::class::lookup(c"NSWindow"),
        c"NativeFullscreenOracleWindow",
        0,
    )
    .unwrap();
    class.add_method(
        objc::sel::get("windowDidResize:"),
        resized as objc::class::Imp,
        c"v24@0:8@16",
    );
    class.add_method(
        objc::sel::get("windowWillEnterFullScreen:"),
        transition as objc::class::Imp,
        c"v24@0:8@16",
    );
    class.add_method(
        objc::sel::get("windowWillExitFullScreen:"),
        transition as objc::class::Imp,
        c"v24@0:8@16",
    );
    objc::class::register_pair(class);
    let pump = || {
        while let Some(event) = sdl3::video::poll() {
            crate::event::deliver(event);
        }
    };
    let settle = |predicate: &dyn Fn() -> bool| {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            pump();
            if predicate() {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "window manager did not settle"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    };
    let mut previous = "";
    let mut object: Id = std::ptr::null_mut();
    let mut restore = NSRect::default();
    let mut host = NSRect::default();
    let mut tested = 0;
    for row in oracle_rows() {
        if row[0] != previous {
            if !object.is_null() {
                unsafe {
                    toggle_full_screen(
                        object,
                        objc::sel::get("toggleFullScreen:"),
                        std::ptr::null_mut(),
                    )
                };
                settle(&|| {
                    !is_fullscreen(window_of(object).sdl) && host_frame(object) == Some(restore)
                });
                assert_eq!(unsafe { frame(object, objc::sel::get("frame")) }, restore);
                unsafe {
                    set_content_size(
                        object,
                        objc::sel::get("setContentSize:"),
                        NSSize {
                            width: 640.0,
                            height: 420.0,
                        },
                    )
                };
                settle(&|| {
                    sdl3::video::window_size(window_of(object).sdl)
                        == Some((640, 420 + content_inset(object) as i32))
                });
                assert_eq!(
                    crate::view::frame_of(window_of(object).content_view).size,
                    NSSize {
                        width: 640.0,
                        height: 420.0
                    }
                );
                objc::arc::release(object);
                pump();
            }
            object = objc::object::create_instance(class, 0);
            window_of(object).delegate = object;
            let content = NSRect {
                origin: NSPoint { x: 80.0, y: 100.0 },
                size: NSSize {
                    width: 720.0,
                    height: 460.0,
                },
            };
            assert!(unsafe { init_with_content_rect_for_tests(object, content) });
            let mask = style::TITLED
                | style::CLOSABLE
                | style::MINIATURIZABLE
                | style::RESIZABLE
                | if row[1] == "full-size-content" {
                    1 << 15
                } else {
                    0
                };
            unsafe { set_style_mask(object, objc::sel::get("setStyleMask:"), mask) };
            let sdl = window_of(object).sdl;
            sdl3::video::show_window(sdl);
            window_of(object).shown = 1;
            settle(&|| host_frame(object).is_some());
            restore = unsafe { frame(object, objc::sel::get("frame")) };
            unsafe {
                toggle_full_screen(
                    object,
                    objc::sel::get("toggleFullScreen:"),
                    std::ptr::null_mut(),
                )
            };
            let bounds = sdl3::video::display_bounds(sdl3::video::primary_display()).unwrap();
            settle(&|| {
                is_fullscreen(sdl)
                    && window_of(object).reported_fullscreen != 0
                    && sdl3::video::window_size(sdl) == Some((bounds.w, bounds.h))
            });
            host = host_frame(object).unwrap();
            previous = row[0];
        }
        let value = |index: usize| row[index].parse::<f64>().unwrap();
        // The capture's display was 1440x900. Keep the measured point
        // deltas when exercising a different host display size.
        let width = |v: f64| if v == 1440.0 { host.size.width } else { v };
        let height = |v: f64| {
            if v == 450.0 {
                host.size.height / 2.0
            } else {
                v + host.size.height - 900.0
            }
        };
        let args = NSRect {
            origin: NSPoint {
                x: value(4) + host.origin.x,
                y: value(5) + host.origin.y,
            },
            size: NSSize {
                width: width(value(6)),
                height: height(value(7)),
            },
        };
        let before = unsafe { frame(object, objc::sel::get("frame")) };
        RESIZES.with_borrow_mut(Vec::clear);
        unsafe {
            match row[2] {
                "origin" => {
                    set_frame_origin(object, objc::sel::get("setFrameOrigin:"), args.origin)
                }
                "content-size" => {
                    set_content_size(object, objc::sel::get("setContentSize:"), args.size)
                }
                "frame-display" => {
                    set_frame_display(object, objc::sel::get("setFrame:display:"), args, 1)
                }
                "frame-animate" => set_frame_display_animate(
                    object,
                    objc::sel::get("setFrame:display:animate:"),
                    args,
                    1,
                    1,
                ),
                "roblox-transition" => {
                    set_style_mask(
                        object,
                        objc::sel::get("setStyleMask:"),
                        window_of(object).style_mask & !(1 << 15),
                    );
                    set_frame(object, objc::sel::get("setFrame:"), args);
                }
                _ => set_frame(object, objc::sel::get("setFrame:"), args),
            }
        }
        let expected = NSRect {
            origin: NSPoint {
                x: value(8) + host.origin.x,
                y: value(9) + host.origin.y,
            },
            size: NSSize {
                width: width(value(10)),
                height: height(value(11)),
            },
        };
        assert_eq!(
            unsafe { frame(object, objc::sel::get("frame")) },
            expected,
            "native case {} step {}",
            row[0],
            row[3]
        );
        let view = window_of(object).content_view;
        assert_eq!(crate::view::frame_of(view).size, expected.size);
        let observations = RESIZES.with_borrow(Clone::clone);
        assert_eq!(
            observations.len(),
            usize::from(before.size != expected.size),
            "synchronous resize in native case {}",
            row[0]
        );
        if let Some((notified_frame, notified_size)) = observations.first() {
            assert_eq!(*notified_frame, expected);
            assert_eq!(*notified_size, expected.size);
        }
        let mapped = rect_to_surface(
            object,
            NSRect {
                origin: NSPoint::default(),
                size: expected.size,
            },
        )
        .unwrap();
        let density = sdl3::video::window_pixel_density(window_of(object).sdl) as f64;
        let presented = unsafe { crate::view::presentation_frame(view) }.unwrap();
        // Expected physical position comes from the native frame, not the
        // mapping helper under test. In the 31-point case the top is zero.
        let expected_x = expected.origin.x - host.origin.x;
        let expected_y =
            host.origin.y + host.size.height - expected.origin.y - expected.size.height;
        assert_eq!(
            (presented.x, presented.y),
            (
                (expected_x * density).round() as i32,
                (expected_y * density).round() as i32
            ),
            "native placement case {} step {}",
            row[0],
            row[3]
        );
        assert_eq!(
            (presented.x, presented.y),
            (
                (mapped.origin.x * density).round() as i32,
                (mapped.origin.y * density).round() as i32
            )
        );
        let backing = crate::backing::placed_rect_for_tests(object, view).unwrap();
        assert_eq!(
            (backing.x, backing.y, backing.width, backing.height),
            (
                i64::from(presented.x),
                i64::from(presented.y),
                i64::from(presented.width),
                i64::from(presented.height)
            )
        );
        let (_, ime) = unsafe { crate::view::input_frame(view) }.unwrap();
        assert_eq!(
            (ime.x, ime.y),
            (
                mapped.origin.x.round() as i32,
                mapped.origin.y.round() as i32
            )
        );
        let point =
            point_from_surface(object, mapped.origin.x + 17.0, mapped.origin.y + 23.0).unwrap();
        assert_eq!(
            point,
            NSPoint {
                x: 17.0,
                y: expected.size.height - 23.0
            }
        );
        assert_eq!(window_of(object).size_pending, 0);
        tested += 1;
    }
    assert_eq!(tested, 22);
    unsafe {
        toggle_full_screen(
            object,
            objc::sel::get("toggleFullScreen:"),
            std::ptr::null_mut(),
        )
    };
    settle(&|| !is_fullscreen(window_of(object).sdl) && host_frame(object) == Some(restore));
    unsafe {
        set_content_size(
            object,
            objc::sel::get("setContentSize:"),
            NSSize {
                width: 640.0,
                height: 420.0,
            },
        )
    };
    settle(&|| {
        sdl3::video::window_size(window_of(object).sdl)
            == Some((640, 420 + content_inset(object) as i32))
    });
    assert_eq!(
        crate::view::frame_of(window_of(object).content_view).size,
        NSSize {
            width: 640.0,
            height: 420.0
        }
    );
    objc::arc::release(object);

    // The native capture restores the pre-transition frame even when a
    // will-enter callback resizes the window, and will-exit still sees the
    // current fullscreen frame. Exercise those callback boundaries directly.
    for (entering, origin_only, oversized) in [
        (true, false, false),
        (false, false, false),
        (false, true, false),
        (false, false, true),
    ] {
        TRANSITION_ORIGIN.set(origin_only);
        let object = objc::object::create_instance(class, 0);
        window_of(object).delegate = object;
        let content = NSRect {
            origin: NSPoint { x: 80.0, y: 100.0 },
            size: NSSize {
                width: 720.0,
                height: 460.0,
            },
        };
        assert!(unsafe { init_with_content_rect_for_tests(object, content) });
        let sdl = window_of(object).sdl;
        sdl3::video::show_window(sdl);
        settle(&|| host_frame(object).is_some());
        let restore = unsafe { frame(object, objc::sel::get("frame")) };
        let requested = NSRect {
            origin: NSPoint {
                x: restore.origin.x,
                y: restore.origin.y + 31.0,
            },
            size: NSSize {
                width: restore.size.width,
                height: restore.size.height - 31.0,
            },
        };
        if entering {
            TRANSITION_REQUEST.set(Some((true, requested)));
        }
        unsafe {
            toggle_full_screen(
                object,
                objc::sel::get("toggleFullScreen:"),
                std::ptr::null_mut(),
            )
        };
        settle(&|| is_fullscreen(sdl) && window_of(object).reported_fullscreen != 0);
        if entering {
            let (was_fullscreen, before, after) = TRANSITION_SEEN.take().unwrap();
            assert!(!was_fullscreen);
            assert_eq!(before, restore);
            assert_eq!(after, requested);
        } else {
            let full = host_frame(object).unwrap();
            TRANSITION_REQUEST.set(Some((
                false,
                NSRect {
                    origin: NSPoint {
                        x: full.origin.x + if origin_only { 23.0 } else { 0.0 },
                        y: full.origin.y + 31.0,
                    },
                    size: NSSize {
                        width: full.size.width,
                        height: full.size.height + if oversized { 71.0 } else { -31.0 },
                    },
                },
            )));
        }
        unsafe {
            toggle_full_screen(
                object,
                objc::sel::get("toggleFullScreen:"),
                std::ptr::null_mut(),
            )
        };
        settle(&|| {
            !is_fullscreen(sdl)
                && window_of(object).reported_fullscreen == 0
                && host_frame(object) == Some(restore)
        });
        if !entering {
            let (was_fullscreen, before, after) = TRANSITION_SEEN.take().unwrap();
            assert!(was_fullscreen);
            if origin_only {
                assert_eq!(after.size, before.size);
                assert_eq!(after.origin.x - before.origin.x, 23.0);
                assert_eq!(after.origin.y - before.origin.y, 31.0);
            } else {
                assert_eq!(
                    after.size.height - before.size.height,
                    if oversized { 71.0 } else { -31.0 }
                );
                assert_eq!(after.origin.y - before.origin.y, 31.0);
            }
        }
        assert_eq!(unsafe { frame(object, objc::sel::get("frame")) }, restore);
        objc::arc::release(object);
        pump();
    }
    eprintln!(
        "native comparison: 22 observations, 14 settled cases and post-exit resizes, 4 transition callback cases"
    );
    objc::arc::pool_pop(pool);
}

thread_local! {
    static FAILED_TRANSITIONS: std::cell::Cell<(usize, usize)> = const { std::cell::Cell::new((0, 0)) };
    static RECREATE_ON_NEXT_ENTER: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static RELEASE_CALLBACK_SEEN: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

unsafe extern "C" fn recreate_on_will_enter(receiver: Id, _sel: Sel, _notification: Id) {
    if RECREATE_ON_NEXT_ENTER.with(|recreate| recreate.replace(false)) {
        assert!(rebuild_for_opengl(receiver));
    }
}

unsafe extern "C" fn release_on_will_enter(receiver: Id, _sel: Sel, _notification: Id) {
    // The callback consumes the guest-owned reference while the toggle runs.
    RELEASE_CALLBACK_SEEN.set(true);
    objc::arc::release(receiver);
}

unsafe extern "C" fn did_fail_enter(receiver: Id, _sel: Sel, window: Id) {
    assert_eq!(receiver, window);
    FAILED_TRANSITIONS.with(|count| count.update(|(enter, exit)| (enter + 1, exit)));
}

unsafe extern "C" fn did_fail_exit(receiver: Id, _sel: Sel, window: Id) {
    assert_eq!(receiver, window);
    FAILED_TRANSITIONS.with(|count| count.update(|(enter, exit)| (enter, exit + 1)));
}

#[test]
#[ignore = "requires an X11/Wayland window manager; see tests/fixtures/fullscreen-native.md"]
fn fullscreen_window_replacement_confinement_and_failures() {
    crate::bootstrap();
    let pool = objc::arc::pool_push();
    let class = objc::class::allocate_pair(
        objc::class::lookup(c"NSWindow"),
        c"NativeFullscreenLifecycleRegressionWindow",
        0,
    )
    .unwrap();
    class.add_method(
        objc::sel::get("windowWillEnterFullScreen:"),
        recreate_on_will_enter as objc::class::Imp,
        c"v24@0:8@16",
    );
    class.add_method(
        objc::sel::get("windowDidFailToEnterFullScreen:"),
        did_fail_enter as objc::class::Imp,
        c"v24@0:8@16",
    );
    class.add_method(
        objc::sel::get("windowDidFailToExitFullScreen:"),
        did_fail_exit as objc::class::Imp,
        c"v24@0:8@16",
    );
    objc::class::register_pair(class);
    let object = objc::object::create_instance(class, 0);
    window_of(object).delegate = object;
    assert!(unsafe {
        init_with_content_rect_for_tests(
            object,
            NSRect {
                origin: NSPoint { x: 90.0, y: 110.0 },
                size: NSSize {
                    width: 700.0,
                    height: 480.0,
                },
            },
        )
    });
    unsafe {
        set_style_mask(
            object,
            objc::sel::get("setStyleMask:"),
            style::TITLED | style::CLOSABLE | style::RESIZABLE,
        )
    };
    let original = window_of(object).sdl;
    sdl3::video::show_window(original);
    window_of(object).shown = 1;
    let restore = unsafe { frame(object, objc::sel::get("frame")) };
    let pump = || {
        while let Some(event) = sdl3::video::poll() {
            crate::event::deliver(event);
        }
    };
    let settle = |predicate: &dyn Fn() -> bool| {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            pump();
            if predicate() {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "fullscreen did not settle"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    };

    RECREATE_ON_NEXT_ENTER.with(|flag| flag.set(true));
    unsafe {
        toggle_full_screen(
            object,
            objc::sel::get("toggleFullScreen:"),
            std::ptr::null_mut(),
        )
    };
    let replacement = window_of(object).sdl;
    assert_ne!(replacement, original, "will-enter must rebuild SDL surface");
    assert_eq!(window_of(object).fullscreen, 1);
    settle(&|| window_of(object).reported_fullscreen != 0);
    assert!(is_fullscreen(replacement));
    assert_eq!(FAILED_TRANSITIONS.get(), (0, 0));
    let host = host_frame(object).unwrap();

    let confinement = NSRect {
        origin: NSPoint { x: 19.0, y: 12.0 },
        size: NSSize {
            width: 80.0,
            height: 100.0,
        },
    };
    unsafe {
        set_mouse_confinement_rect(
            object,
            objc::sel::get("setMouseConfinementRect:"),
            confinement,
        )
    };
    let before = mouse_confinement_in_surface(object).unwrap();
    let logical = NSRect {
        origin: NSPoint {
            x: host.origin.x,
            y: host.origin.y + 31.0,
        },
        size: NSSize {
            width: host.size.width,
            height: host.size.height - 31.0,
        },
    };
    unsafe { set_frame(object, objc::sel::get("setFrame:"), logical) };
    let after = mouse_confinement_in_surface(object).unwrap();
    assert_eq!(after.x, before.x);
    assert_eq!(after.y, before.y - 31);
    assert_eq!((after.w, after.h), (80, 100));
    assert_eq!(window_of(object).mouse_confinement, confinement);

    REJECT_NEXT_FULLSCREEN_REQUEST.with(|flag| flag.set(true));
    unsafe {
        toggle_full_screen(
            object,
            objc::sel::get("toggleFullScreen:"),
            std::ptr::null_mut(),
        )
    };
    assert!(is_fullscreen(window_of(object).sdl));
    assert_eq!(window_of(object).fullscreen_transition, 0);
    assert_eq!(FAILED_TRANSITIONS.get(), (0, 1));
    assert_eq!(unsafe { frame(object, objc::sel::get("frame")) }, logical);

    unsafe {
        toggle_full_screen(
            object,
            objc::sel::get("toggleFullScreen:"),
            std::ptr::null_mut(),
        )
    };
    settle(&|| window_of(object).reported_fullscreen == 0 && host_frame(object) == Some(restore));
    assert_eq!(window_of(object).mouse_confinement, confinement);
    let restored = mouse_confinement_in_surface(object).unwrap();
    assert_eq!(restored.y, restore.size.height.round() as i32 - 112);
    assert_eq!(unsafe { frame(object, objc::sel::get("frame")) }, restore);

    REJECT_NEXT_FULLSCREEN_REQUEST.with(|flag| flag.set(true));
    unsafe {
        toggle_full_screen(
            object,
            objc::sel::get("toggleFullScreen:"),
            std::ptr::null_mut(),
        )
    };
    assert!(!is_fullscreen(window_of(object).sdl));
    assert_eq!(window_of(object).fullscreen_transition, 0);
    assert_eq!(window_of(object).fullscreen_restore, NSRect::default());
    assert_eq!(FAILED_TRANSITIONS.get(), (1, 1));

    unsafe {
        toggle_full_screen(
            object,
            objc::sel::get("toggleFullScreen:"),
            std::ptr::null_mut(),
        )
    };
    settle(&|| window_of(object).reported_fullscreen != 0);
    unsafe {
        toggle_full_screen(
            object,
            objc::sel::get("toggleFullScreen:"),
            std::ptr::null_mut(),
        )
    };
    settle(&|| window_of(object).reported_fullscreen == 0 && host_frame(object) == Some(restore));
    objc::arc::release(object);
    pump();

    // A will-enter callback can also drop the caller's last NSWindow reference.
    // toggleFullScreen: must not dereference the object after it is finalized.
    let releasing_class = objc::class::allocate_pair(
        objc::class::lookup(c"NSWindow"),
        c"NativeFullscreenReleaseRegressionWindow",
        0,
    )
    .unwrap();
    releasing_class.add_method(
        objc::sel::get("windowWillEnterFullScreen:"),
        release_on_will_enter as objc::class::Imp,
        c"v24@0:8@16",
    );
    objc::class::register_pair(releasing_class);
    let released = objc::object::create_instance(releasing_class, 0);
    window_of(released).delegate = released;
    assert!(unsafe {
        init_with_content_rect_for_tests(
            released,
            NSRect {
                origin: NSPoint { x: 110.0, y: 120.0 },
                size: NSSize {
                    width: 500.0,
                    height: 360.0,
                },
            },
        )
    });
    sdl3::video::show_window(window_of(released).sdl);
    window_of(released).shown = 1;
    unsafe {
        toggle_full_screen(
            released,
            objc::sel::get("toggleFullScreen:"),
            std::ptr::null_mut(),
        )
    };
    assert!(RELEASE_CALLBACK_SEEN.get());
    pump();
    objc::arc::pool_pop(pool);
}

/// SDL takes a hidden window's request as a pending flag and reports no state
/// change until the window is shown, and a window manager may never answer.
/// Neither may leave `-toggleFullScreen:` ignoring every later request.
#[test]
#[ignore = "requires an X11/Wayland window manager; see tests/fixtures/fullscreen-native.md"]
fn unanswered_fullscreen_requests_do_not_wedge_later_toggles() {
    crate::bootstrap();
    let pool = objc::arc::pool_push();
    let class = objc::class::allocate_pair(
        objc::class::lookup(c"NSWindow"),
        c"UnansweredFullscreenRegressionWindow",
        0,
    )
    .unwrap();
    class.add_method(
        objc::sel::get("windowDidFailToEnterFullScreen:"),
        did_fail_enter as objc::class::Imp,
        c"v24@0:8@16",
    );
    objc::class::register_pair(class);
    let object = objc::object::create_instance(class, 0);
    window_of(object).delegate = object;
    assert!(unsafe {
        init_with_content_rect_for_tests(
            object,
            NSRect {
                origin: NSPoint { x: 90.0, y: 110.0 },
                size: NSSize {
                    width: 700.0,
                    height: 480.0,
                },
            },
        )
    });
    let toggle = || unsafe {
        toggle_full_screen(
            object,
            objc::sel::get("toggleFullScreen:"),
            std::ptr::null_mut(),
        )
    };
    let pump = || {
        while let Some(event) = sdl3::video::poll() {
            crate::event::deliver(event);
        }
    };

    // Hidden: SDL records the request and answers nothing until shown.
    toggle();
    assert_eq!(window_of(object).fullscreen, 1);
    toggle();
    assert_eq!(
        window_of(object).fullscreen,
        0,
        "second toggle on a hidden window was ignored"
    );
    assert!(!is_fullscreen(window_of(object).sdl));

    // Shown, with the host's answer never delivered: a later toggle is a
    // fresh request once the first has had longer than any transition takes.
    sdl3::video::show_window(window_of(object).sdl);
    window_of(object).shown = 1;
    pump();
    toggle();
    assert_eq!(window_of(object).fullscreen, 1);
    std::thread::sleep(FULLSCREEN_TRANSITION_TIMEOUT + std::time::Duration::from_millis(100));
    toggle();
    assert_eq!(
        FAILED_TRANSITIONS.get(),
        (1, 0),
        "stale entry was not failed"
    );
    assert_eq!(window_of(object).fullscreen, 1, "new request was ignored");
    objc::arc::release(object);
    pump();
    objc::arc::pool_pop(pool);
}

thread_local! {
    static FULLSCREEN_LIFECYCLE: std::cell::RefCell<Vec<(&'static str, NSRect, NSSize)>> = const { std::cell::RefCell::new(Vec::new()) };
    static SHORTEN_INSIDE_DID_ENTER: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

thread_local! {
    static SENT_EVENTS: std::cell::RefCell<Vec<(u64, NSPoint, NSPoint)>> = const { std::cell::RefCell::new(Vec::new()) };
}

unsafe extern "C" fn record_sent_event(_receiver: Id, _sel: Sel, event: Id) {
    let fields = crate::nsevent::fields_of(event).unwrap();
    SENT_EVENTS.with_borrow_mut(|sent| sent.push((fields.kind, fields.delta, fields.location)));
}

/// A disassociated pointer's frozen position may sit outside a shortened
/// fullscreen frame; its motion deltas and clicks still belong to the window.
#[test]
#[ignore = "requires an X11/Wayland window manager; see tests/fixtures/fullscreen-native.md"]
fn disassociated_pointer_input_ignores_the_logical_frame_edge() {
    crate::bootstrap();
    let pool = objc::arc::pool_push();
    let class = objc::class::allocate_pair(
        objc::class::lookup(c"NSWindow"),
        c"DisassociatedPointerRegressionWindow",
        0,
    )
    .unwrap();
    class.add_method(
        objc::sel::get("sendEvent:"),
        record_sent_event as objc::class::Imp,
        c"v24@0:8@16",
    );
    objc::class::register_pair(class);
    let object = objc::object::create_instance(class, 0);
    assert!(unsafe {
        init_with_content_rect_for_tests(
            object,
            NSRect {
                origin: NSPoint { x: 90.0, y: 110.0 },
                size: NSSize {
                    width: 700.0,
                    height: 480.0,
                },
            },
        )
    });
    window_of(object).accepts_mouse_moved = 1;
    sdl3::video::show_window(window_of(object).sdl);
    window_of(object).shown = 1;
    let pump = || {
        while let Some(event) = sdl3::video::poll() {
            crate::event::deliver(event);
        }
    };
    unsafe {
        toggle_full_screen(
            object,
            objc::sel::get("toggleFullScreen:"),
            std::ptr::null_mut(),
        )
    };
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while window_of(object).reported_fullscreen == 0 {
        assert!(
            std::time::Instant::now() < deadline,
            "fullscreen did not settle"
        );
        pump();
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    let host = host_frame(object).unwrap();
    let logical = NSRect {
        origin: host.origin,
        size: NSSize {
            width: host.size.width,
            height: host.size.height - 31.0,
        },
    };
    unsafe { set_frame(object, objc::sel::get("setFrame:"), logical) };
    pump();
    SENT_EVENTS.with_borrow_mut(Vec::clear);

    let id = sdl3::video::window_id(window_of(object).sdl);
    unsafe { crate::screen::CGAssociateMouseAndMouseCursorPosition(0) };
    // Frozen in the 31-point band above the logical frame.
    crate::event::warp_pointer(40.0, 10.0);
    let click = |down| sdl3::video::Event::MouseButton {
        window: id,
        button: sdl3::mouse::LEFT,
        down,
        clicks: 1,
        x: 400.0,
        y: 300.0,
    };
    crate::event::deliver(sdl3::video::Event::MouseMotion {
        window: id,
        x: 400.0,
        y: 300.0,
        dx: 5.0,
        dy: 3.0,
        buttons: 0,
    });
    crate::event::deliver(click(true));
    crate::event::deliver(click(false));
    unsafe { crate::screen::CGAssociateMouseAndMouseCursorPosition(1) };
    let sent = SENT_EVENTS.take();
    let kinds: Vec<u64> = sent.iter().map(|(kind, _, _)| *kind).collect();
    // NSEventTypeMouseMoved, LeftMouseDown, LeftMouseUp.
    assert_eq!(
        kinds,
        [5, 1, 2],
        "input of a disassociated pointer was dropped"
    );
    assert_eq!(sent[0].1, NSPoint { x: 5.0, y: 3.0 });
    objc::arc::release(object);
    pump();
    objc::arc::pool_pop(pool);
}

/// A windowed `-setFrame:` is answered at once by `-frame` and the content
/// view, before SDL resizes; a click in between is placed in that frame.
#[test]
#[ignore = "requires an X11/Wayland window manager; see tests/fixtures/fullscreen-native.md"]
fn click_location_follows_a_pending_windowed_resize() {
    crate::bootstrap();
    let pool = objc::arc::pool_push();
    let class = objc::class::allocate_pair(
        objc::class::lookup(c"NSWindow"),
        c"PendingResizeLocationRegressionWindow",
        0,
    )
    .unwrap();
    class.add_method(
        objc::sel::get("sendEvent:"),
        record_sent_event as objc::class::Imp,
        c"v24@0:8@16",
    );
    objc::class::register_pair(class);
    let object = objc::object::create_instance(class, 0);
    let first = NSRect {
        origin: NSPoint { x: 90.0, y: 110.0 },
        size: NSSize {
            width: 700.0,
            height: 480.0,
        },
    };
    assert!(unsafe { init_with_content_rect_for_tests(object, first) });
    sdl3::video::show_window(window_of(object).sdl);
    window_of(object).shown = 1;
    let pump = || {
        while let Some(event) = sdl3::video::poll() {
            crate::event::deliver(event);
        }
    };
    std::thread::sleep(std::time::Duration::from_millis(200));
    pump();
    let before = unsafe { frame(object, objc::sel::get("frame")) };
    let taller = NSRect {
        origin: before.origin,
        size: NSSize {
            width: before.size.width,
            height: before.size.height + 120.0,
        },
    };
    unsafe { set_frame(object, objc::sel::get("setFrame:"), taller) };
    assert_ne!(
        window_of(object).size_pending,
        0,
        "SDL resized synchronously"
    );
    SENT_EVENTS.with_borrow_mut(Vec::clear);
    let id = sdl3::video::window_id(window_of(object).sdl);
    crate::event::deliver(sdl3::video::Event::MouseButton {
        window: id,
        button: sdl3::mouse::LEFT,
        down: true,
        clicks: 1,
        x: 100.0,
        y: 50.0,
    });
    let height = unsafe { frame(object, objc::sel::get("frame")) }
        .size
        .height;
    let sent = SENT_EVENTS.take();
    assert_eq!(sent.len(), 1);
    assert_eq!(
        sent[0].2,
        NSPoint {
            x: 100.0,
            y: height - 50.0
        }
    );
    objc::arc::release(object);
    pump();
    objc::arc::pool_pop(pool);
}

unsafe extern "C" fn record_fullscreen_lifecycle(receiver: Id, selector: Sel, _note: Id) {
    let name = if selector == objc::sel::get("windowWillEnterFullScreen:") {
        "willEnter"
    } else if selector == objc::sel::get("windowDidEnterFullScreen:") {
        "didEnter"
    } else if selector == objc::sel::get("windowWillExitFullScreen:") {
        "willExit"
    } else if selector == objc::sel::get("windowDidExitFullScreen:") {
        "didExit"
    } else if selector == objc::sel::get("windowDidResize:") {
        "didResize"
    } else if selector == objc::sel::get("windowDidMove:") {
        "didMove"
    } else {
        panic!("unexpected fullscreen callback selector");
    };
    let frame = unsafe { frame(receiver, objc::sel::get("frame")) };
    let content = crate::view::frame_of(window_of(receiver).content_view).size;
    FULLSCREEN_LIFECYCLE.with_borrow_mut(|events| events.push((name, frame, content)));
    if name == "didEnter" && SHORTEN_INSIDE_DID_ENTER.get() {
        let host = host_frame(receiver).unwrap();
        unsafe {
            set_frame(
                receiver,
                objc::sel::get("setFrame:"),
                NSRect {
                    origin: NSPoint {
                        x: host.origin.x,
                        y: host.origin.y + 31.0,
                    },
                    size: NSSize {
                        width: host.size.width,
                        height: host.size.height - 31.0,
                    },
                },
            );
        }
    }
}

/// The host may queue resize/move after the fullscreen state event. AppKit
/// nevertheless sends the settled geometry callbacks before did-enter/did-exit.
#[test]
#[ignore = "requires an X11/Wayland window manager; see tests/fixtures/fullscreen-native.md"]
fn fullscreen_transition_posts_geometry_before_did_notifications() {
    crate::bootstrap();
    let pool = objc::arc::pool_push();
    let class = objc::class::allocate_pair(
        objc::class::lookup(c"NSWindow"),
        c"NativeFullscreenNotificationOrderWindow",
        0,
    )
    .unwrap();
    for selector in [
        "windowWillEnterFullScreen:",
        "windowDidEnterFullScreen:",
        "windowWillExitFullScreen:",
        "windowDidExitFullScreen:",
        "windowDidResize:",
        "windowDidMove:",
    ] {
        class.add_method(
            objc::sel::get(selector),
            record_fullscreen_lifecycle as objc::class::Imp,
            c"v24@0:8@16",
        );
    }
    objc::class::register_pair(class);
    for shorten_in_did_enter in [false, true] {
        SHORTEN_INSIDE_DID_ENTER.set(shorten_in_did_enter);
        let object = objc::object::create_instance(class, 0);
        window_of(object).delegate = object;
        assert!(unsafe {
            init_with_content_rect_for_tests(
                object,
                NSRect {
                    origin: NSPoint { x: 80.0, y: 100.0 },
                    size: NSSize {
                        width: 720.0,
                        height: 460.0,
                    },
                },
            )
        });
        unsafe {
            set_style_mask(
                object,
                objc::sel::get("setStyleMask:"),
                style::TITLED | style::CLOSABLE | style::RESIZABLE,
            )
        };
        let sdl = window_of(object).sdl;
        sdl3::video::show_window(sdl);
        window_of(object).shown = 1;
        let pump = || {
            while let Some(event) = sdl3::video::poll() {
                crate::event::deliver(event);
            }
        };
        let settle = |wanted: bool| {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            loop {
                pump();
                if is_fullscreen(sdl) == wanted
                    && (window_of(object).reported_fullscreen != 0) == wanted
                {
                    break;
                }
                assert!(
                    std::time::Instant::now() < deadline,
                    "fullscreen did not settle"
                );
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            // Catch trailing host geometry events, not just the first state event.
            std::thread::sleep(std::time::Duration::from_millis(50));
            pump();
        };
        pump();
        let restored = unsafe { frame(object, objc::sel::get("frame")) };
        FULLSCREEN_LIFECYCLE.with_borrow_mut(Vec::clear);
        unsafe {
            toggle_full_screen(
                object,
                objc::sel::get("toggleFullScreen:"),
                std::ptr::null_mut(),
            )
        };
        settle(true);
        let entered = unsafe { frame(object, objc::sel::get("frame")) };
        let seq = FULLSCREEN_LIFECYCLE.with_borrow(|events| events.clone());
        let names = seq.iter().map(|(name, ..)| *name).collect::<Vec<_>>();
        if shorten_in_did_enter {
            assert_eq!(
                names,
                ["willEnter", "didResize", "didEnter", "didResize"],
                "enter: {seq:?}"
            );
            assert_eq!(seq[1].1, seq[2].1);
            assert_eq!(seq[3].1, entered);
        } else {
            assert_eq!(
                names,
                ["willEnter", "didResize", "didEnter"],
                "enter: {seq:?}"
            );
            assert_eq!(seq[2].1, entered);
        }
        assert_eq!(seq[0].1, restored);
        for (_, frame, content) in &seq[1..] {
            assert_eq!(content.width, frame.size.width);
            assert_eq!(content.height, frame.size.height - content_inset(object));
        }
        FULLSCREEN_LIFECYCLE.with_borrow_mut(Vec::clear);
        unsafe {
            toggle_full_screen(
                object,
                objc::sel::get("toggleFullScreen:"),
                std::ptr::null_mut(),
            )
        };
        settle(false);
        let seq = FULLSCREEN_LIFECYCLE.with_borrow(|events| events.clone());
        assert_eq!(
            seq.iter().map(|(name, ..)| *name).collect::<Vec<_>>(),
            ["willExit", "didResize", "didMove", "didExit"],
            "exit: {seq:?}"
        );
        assert_eq!(seq[0].1, entered);
        for (_, frame, content) in &seq[1..] {
            assert_eq!(*frame, restored);
            assert_eq!(content.width, frame.size.width);
            assert_eq!(content.height, frame.size.height - content_inset(object));
        }
        objc::arc::release(object);
        pump();
    }
    SHORTEN_INSIDE_DID_ENTER.set(false);
    objc::arc::pool_pop(pool);
}

#[test]
#[ignore = "requires Xvfb/xfwm4"]
fn fractional_fullscreen_frame_matches_content_view() {
    crate::bootstrap();
    let pool = objc::arc::pool_push();
    let class = objc::class::lookup(c"NSWindow").unwrap();
    let object = objc::object::create_instance(class, 0);
    assert!(unsafe {
        init_with_content_rect_for_tests(
            object,
            NSRect {
                origin: NSPoint { x: 80.0, y: 100.0 },
                size: NSSize {
                    width: 720.0,
                    height: 460.0,
                },
            },
        )
    });
    unsafe {
        set_style_mask(
            object,
            objc::sel::get("setStyleMask:"),
            style::TITLED | style::RESIZABLE,
        );
    }
    sdl3::video::show_window(window_of(object).sdl);
    window_of(object).shown = 1;
    unsafe {
        toggle_full_screen(
            object,
            objc::sel::get("toggleFullScreen:"),
            std::ptr::null_mut(),
        )
    };
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while window_of(object).reported_fullscreen == 0 {
        while let Some(event) = sdl3::video::poll() {
            crate::event::deliver(event);
        }
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    let host = host_frame(object).unwrap();
    let logical = NSRect {
        origin: NSPoint {
            x: host.origin.x + 0.5,
            y: host.origin.y + 0.5,
        },
        size: NSSize {
            width: host.size.width - 0.5,
            height: host.size.height - 0.5,
        },
    };
    unsafe { set_frame(object, objc::sel::get("setFrame:"), logical) };
    assert_eq!(unsafe { frame(object, objc::sel::get("frame")) }, logical);
    let view_size = crate::view::frame_of(window_of(object).content_view).size;
    eprintln!(
        "REVIEWER_FRACTIONAL logical={:?} content_view={:?}",
        logical.size, view_size
    );
    assert_eq!(
        view_size, logical.size,
        "NSWindow frame and NSView frame disagree in fullscreen"
    );
    let surface = surface_frame(object).unwrap();
    let sdl = window_of(object).sdl;
    let clip = presentation_clip(sdl).unwrap();
    let physical = sdl3::video::window_size_in_pixels(sdl).unwrap();
    let view = window_of(object).content_view;
    let metal_frame = unsafe { crate::view::presentation_frame(view) }.unwrap();
    let backing_frame = crate::backing::placed_rect_for_tests(object, view).unwrap();
    // Compare actual view/Metal and software placement with the authoritative
    // logical window rectangle, not with a second copy of the rounding math.
    assert_eq!((metal_frame.x, metal_frame.width), (clip.x, clip.w as u32));
    assert_eq!((metal_frame.y, metal_frame.height), (clip.y, clip.h as u32));
    assert_eq!(
        (
            backing_frame.x,
            backing_frame.y,
            backing_frame.width,
            backing_frame.height
        ),
        (
            i64::from(metal_frame.x),
            i64::from(metal_frame.y),
            i64::from(metal_frame.width),
            i64::from(metal_frame.height)
        )
    );
    assert!(clip.x >= 0 && clip.y >= 0);
    assert!(clip.x + clip.w <= physical.0 && clip.y + clip.h <= physical.1);
    assert!(
        clip.w < physical.0 || clip.h < physical.1,
        "fractional NSWindow remained the whole host surface"
    );
    let inside = (
        (surface.origin.x + 0.5) as f32,
        (surface.origin.y + 0.5) as f32,
    );
    let outside = (
        (surface.origin.x + surface.size.width + 0.5) as f32,
        inside.1,
    );
    assert!(accepts_surface_point(object, inside.0, inside.1));
    assert!(!accepts_surface_point(object, outside.0, outside.1));
    assert!(!accepts_surface_point(
        object,
        inside.0,
        (surface.origin.y - 0.5) as f32
    ));
    objc::arc::release(object);
    objc::arc::pool_pop(pool);
}

/// The front buffer can outlive the guest's next render. Changing the logical
/// fullscreen frame must clip an *already swapped* full-display image with no
/// GL calls on AppKit's event thread and without another guest swap.
#[test]
#[ignore = "requires Xvfb/xfwm4, ImageMagick and X11 Shape"]
fn opengl_idle_front_buffer_is_clipped_without_guest_swap() {
    crate::bootstrap();
    assert!(sdl3::video::init_video());
    assert!(
        sdl3::video::is_x11(),
        "the X11 idle-presentation regression requires SDL_VIDEODRIVER=x11"
    );
    let pool = objc::arc::pool_push();
    let class = objc::class::lookup(c"NSWindow").unwrap();
    let object = objc::object::create_instance(class, 0);
    assert!(unsafe {
        init_with_content_rect_for_tests(
            object,
            NSRect {
                origin: NSPoint { x: 80.0, y: 100.0 },
                size: NSSize {
                    width: 720.0,
                    height: 460.0,
                },
            },
        )
    });
    unsafe {
        set_style_mask(
            object,
            objc::sel::get("setStyleMask:"),
            style::TITLED | style::RESIZABLE,
        )
    };
    assert!(rebuild_for_opengl(object));
    let sdl = window_of(object).sdl;
    sdl3::video::show_window(sdl);
    window_of(object).shown = 1;
    let gl = sdl3::video::gl_create_context(sdl).unwrap();
    assert!(sdl3::video::gl_make_current(sdl, gl));
    unsafe {
        toggle_full_screen(
            object,
            objc::sel::get("toggleFullScreen:"),
            std::ptr::null_mut(),
        )
    };
    let until = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while window_of(object).reported_fullscreen == 0 {
        while let Some(event) = sdl3::video::poll() {
            crate::event::deliver(event);
        }
        assert!(
            std::time::Instant::now() < until,
            "fullscreen did not settle"
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    // SDL reports the fullscreen extent before Xfwm4 has always processed
    // its X11 ConfigureNotify. Do not capture a still-windowed front buffer.
    std::thread::sleep(std::time::Duration::from_millis(180));
    while let Some(event) = sdl3::video::poll() {
        crate::event::deliver(event);
    }
    let host = host_frame(object).unwrap();
    let (width, height) = sdl3::video::window_size_in_pixels(sdl).unwrap();
    assert!(
        sdl3::video::gl_make_current(sdl, gl),
        "GL context unavailable after fullscreen"
    );
    let clear_color = sdl3::video::gl_proc_address(c"glClearColor").unwrap();
    let clear_color: unsafe extern "C" fn(f32, f32, f32, f32) =
        unsafe { std::mem::transmute(clear_color) };
    let clear = sdl3::video::gl_proc_address(c"glClear").unwrap();
    let clear: unsafe extern "C" fn(u32) = unsafe { std::mem::transmute(clear) };
    let pixels = || {
        let filename =
            std::env::temp_dir().join(format!("rosemary-gl-idle-{}.png", std::process::id()));
        let screenshot = std::process::Command::new("import")
            .args(["-window", "root"])
            .arg(&filename)
            .status()
            .expect("ImageMagick import");
        assert!(screenshot.success());
        let bytes = std::process::Command::new("magick")
            .arg(&filename)
            .args(["-depth", "8", "RGB:-"])
            .output()
            .expect("ImageMagick magick");
        let _ = std::fs::remove_file(filename);
        assert!(bytes.status.success());
        assert_eq!(bytes.stdout.len(), width as usize * height as usize * 3);
        bytes.stdout
    };
    let sample = |bytes: &[u8], x: i32, y: i32| {
        let at = (y as usize * width as usize + x as usize) * 3;
        bytes[at..at + 3].to_vec()
    };
    // Make sure the GLX drawable actually caught up with the earlier X11
    // window resize. Repeated *pre-resize* guest swaps are fine; the point of
    // the test is that there are absolutely no guest swaps *after* shrinking.
    let mut full = Vec::new();
    for try_at in 0..12 {
        assert!(sdl3::video::gl_make_current(sdl, gl));
        unsafe {
            clear_color(1.0, 0.0, 0.0, 1.0);
            clear(0x4000);
        }
        sdl3::video::swap_window(sdl);
        std::thread::sleep(std::time::Duration::from_millis(60));
        while let Some(event) = sdl3::video::poll() {
            crate::event::deliver(event);
        }
        full = pixels();
        if sample(&full, width / 2, height - 5) == [255, 0, 0] {
            break;
        }
        if try_at == 11 {
            eprintln!("OpenGL host drawable never settled after fullscreen");
        }
    }
    assert_eq!(
        sample(&full, width / 2, height - 5),
        [255, 0, 0],
        "the GLX front buffer never filled the SDL fullscreen window"
    );
    let logical = NSRect {
        origin: NSPoint {
            x: host.origin.x + 0.5,
            y: host.origin.y + 31.5,
        },
        size: NSSize {
            width: host.size.width - 1.5,
            height: host.size.height - 31.5,
        },
    };
    unsafe { set_frame(object, objc::sel::get("setFrame:"), logical) };
    let clip = presentation_clip(sdl).unwrap();
    assert!(clip.w < width && clip.h < height);
    // No glClear, no -flushBuffer, no SDL_GL_SwapWindow after logical resize.
    let after = pixels();
    assert_eq!(sample(&after, width / 2, height / 2), [255, 0, 0]);
    assert_eq!(
        sample(&after, width / 2, height - 5),
        [0, 0, 0],
        "stale GL bottom edge escaped"
    );
    assert_eq!(
        sample(&after, width - 1, height / 2),
        [0, 0, 0],
        "stale GL right edge escaped"
    );
    assert_eq!(
        sample(&after, 0, height / 2),
        [0, 0, 0],
        "fractional left edge escaped"
    );
    unsafe { set_frame(object, objc::sel::get("setFrame:"), host) };
    // X11 does not promise that a window retains obscured front-buffer
    // contents. The guest can redraw on expansion; make sure the overlay is
    // gone and the entire restored drawable is visible after that new swap.
    assert!(sdl3::video::gl_make_current(sdl, gl));
    unsafe {
        clear_color(1.0, 0.0, 0.0, 1.0);
        clear(0x4000);
    }
    sdl3::video::swap_window(sdl);
    std::thread::sleep(std::time::Duration::from_millis(80));
    let restored = pixels();
    assert_eq!(
        sample(&restored, width / 2, height - 5),
        [255, 0, 0],
        "blackout remained after restored guest presentation"
    );
    assert_eq!(sample(&restored, width - 1, height / 2), [255, 0, 0]);
    // Ensure the host child mask did not steal input focus.
    assert!(sdl3::video::window_flags(sdl) & sdl3::video::flags::FULLSCREEN != 0);
    sdl3::video::gl_make_current(sdl, std::ptr::null_mut());
    sdl3::video::gl_destroy_context(gl);
    objc::arc::release(object);
    objc::arc::pool_pop(pool);
}

/// Exercise the AppKit-level resize path under a real Wayland compositor.
/// The guest deliberately performs no GL operation after the logical resize;
/// the compositor must hide the stale exterior from the *old* front buffer.
#[test]
#[ignore = "requires nested Weston on Xvfb, Mesa EGL and ImageMagick"]
fn wayland_idle_fullscreen_opengl_frame_is_masked_from_nswindow_resize() {
    crate::bootstrap();
    assert!(sdl3::video::init_video());
    assert!(
        sdl3::video::is_wayland(),
        "requires SDL_VIDEODRIVER=wayland"
    );
    let pool = objc::arc::pool_push();
    let class = objc::class::lookup(c"NSWindow").unwrap();
    let object = objc::object::create_instance(class, 0);
    assert!(unsafe {
        init_with_content_rect_for_tests(
            object,
            NSRect {
                origin: NSPoint { x: 80.0, y: 100.0 },
                size: NSSize {
                    width: 720.0,
                    height: 460.0,
                },
            },
        )
    });
    unsafe {
        set_style_mask(
            object,
            objc::sel::get("setStyleMask:"),
            style::TITLED | style::RESIZABLE,
        )
    };
    assert!(rebuild_for_opengl(object));
    let sdl = window_of(object).sdl;
    sdl3::video::show_window(sdl);
    window_of(object).shown = 1;
    let gl = sdl3::video::gl_create_context(sdl).expect("EGL context");
    assert!(sdl3::video::gl_make_current(sdl, gl));
    unsafe {
        toggle_full_screen(
            object,
            objc::sel::get("toggleFullScreen:"),
            std::ptr::null_mut(),
        )
    };
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while window_of(object).reported_fullscreen == 0 {
        while let Some(event) = sdl3::video::poll() {
            crate::event::deliver(event);
        }
        assert!(
            std::time::Instant::now() < deadline,
            "Weston did not settle fullscreen"
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    let host = host_frame(object).unwrap();
    let (w, h) = sdl3::video::window_size_in_pixels(sdl).unwrap();
    assert_eq!(
        (w, h),
        (1440, 900),
        "fixture expects a 1440×900 Weston output"
    );
    let color: unsafe extern "C" fn(f32, f32, f32, f32) =
        unsafe { std::mem::transmute(sdl3::video::gl_proc_address(c"glClearColor").unwrap()) };
    let clear: unsafe extern "C" fn(u32) =
        unsafe { std::mem::transmute(sdl3::video::gl_proc_address(c"glClear").unwrap()) };
    let capture = || {
        let image = std::env::temp_dir().join(format!(
            "rosemary-wayland-appkit-{}.png",
            std::process::id()
        ));
        assert!(
            std::process::Command::new("import")
                .args(["-window", "root"])
                .arg(&image)
                .status()
                .unwrap()
                .success()
        );
        let rgb = std::process::Command::new("magick")
            .arg(&image)
            .args(["-depth", "8", "RGB:-"])
            .output()
            .unwrap();
        let _ = std::fs::remove_file(image);
        assert!(rgb.status.success());
        assert_eq!(rgb.stdout.len(), (w * h * 3) as usize);
        rgb.stdout
    };
    let sample = |pixels: &[u8], x: usize, y: usize| {
        let at = (y * w as usize + x) * 3;
        [pixels[at], pixels[at + 1], pixels[at + 2]]
    };
    let mut full = Vec::new();
    let mut bottom = 0usize;
    for _ in 0..10 {
        assert!(sdl3::video::gl_make_current(sdl, gl));
        unsafe {
            color(1.0, 0.0, 0.0, 1.0);
            clear(0x4000)
        };
        sdl3::video::swap_window(sdl);
        while let Some(event) = sdl3::video::poll() {
            crate::event::deliver(event);
        }
        std::thread::sleep(std::time::Duration::from_millis(65));
        full = capture();
        bottom = (0..h as usize)
            .rev()
            .find(|y| sample(&full, 450, *y) == [255, 0, 0])
            .unwrap_or(0);
        if bottom >= h as usize - 4 {
            break;
        }
    }
    assert!(
        bottom >= h as usize - 4,
        "guest never presented full-height red front buffer"
    );
    assert_eq!(sample(&full, 450, bottom - 5), [255, 0, 0]);
    let shorter = NSRect {
        origin: NSPoint {
            x: host.origin.x + 1.0,
            y: host.origin.y + 31.0,
        },
        size: NSSize {
            width: host.size.width - 32.0,
            height: host.size.height - 31.0,
        },
    };
    unsafe { set_frame(object, objc::sel::get("setFrame:"), shorter) };
    let logical = presentation_clip(sdl).unwrap();
    assert!(logical.w < w && logical.h < h);
    // No GL draw and no guest swap between setFrame: and the screenshot.
    std::thread::sleep(std::time::Duration::from_millis(85));
    let masked = capture();
    assert_eq!(
        sample(&masked, 450, 450),
        [255, 0, 0],
        "Wayland overlay damaged guest interior"
    );
    assert_eq!(
        sample(&masked, 450, bottom - 5),
        [0, 0, 0],
        "idle guest's bottom edge escaped"
    );
    assert_eq!(
        sample(&masked, w as usize - 8, 450),
        [0, 0, 0],
        "idle guest's right edge escaped"
    );
    unsafe { set_frame(object, objc::sel::get("setFrame:"), host) };
    assert!(sdl3::video::gl_make_current(sdl, gl));
    unsafe {
        color(1.0, 0.0, 0.0, 1.0);
        clear(0x4000)
    };
    sdl3::video::swap_window(sdl);
    std::thread::sleep(std::time::Duration::from_millis(90));
    let restored = capture();
    assert_eq!(
        sample(&restored, 450, bottom - 5),
        [255, 0, 0],
        "restoration retained black mask"
    );
    sdl3::video::gl_make_current(sdl, std::ptr::null_mut());
    sdl3::video::gl_destroy_context(gl);
    objc::arc::release(object);
    objc::arc::pool_pop(pool);
}

/// A context's viewport is not changed by resizing its window on macOS: only
/// a titlebar inset or a logical fullscreen frame calls for a fit here.
#[test]
#[ignore = "requires Xvfb/xfwm4 and Mesa OpenGL"]
fn opengl_viewport_is_left_alone_by_a_plain_windowed_resize() {
    crate::bootstrap();
    let pool = objc::arc::pool_push();
    let class = objc::class::lookup(c"NSWindow").unwrap();
    let object = objc::object::create_instance(class, 0);
    assert!(unsafe {
        init_with_content_rect_for_tests(
            object,
            NSRect {
                origin: NSPoint { x: 80.0, y: 100.0 },
                size: NSSize {
                    width: 720.0,
                    height: 460.0,
                },
            },
        )
    });
    unsafe { set_style_mask(object, objc::sel::get("setStyleMask:"), 0) };
    assert!(rebuild_for_opengl(object));
    let sdl = window_of(object).sdl;
    sdl3::video::show_window(sdl);
    window_of(object).shown = 1;
    let pump = || {
        while let Some(event) = sdl3::video::poll() {
            crate::event::deliver(event);
        }
    };
    let gl = sdl3::video::gl_create_context(sdl).expect("OpenGL context");
    assert!(sdl3::video::gl_make_current(sdl, gl));
    let get = sdl3::video::gl_proc_address(c"glGetIntegerv").unwrap();
    let get: unsafe extern "C" fn(u32, *mut i32) = unsafe { std::mem::transmute(get) };
    let viewport = || {
        let mut actual = [0; 4];
        unsafe { get(0x0BA2, actual.as_mut_ptr()) };
        actual
    };
    assert_eq!(crate::window::content_inset(object), 0.0);
    crate::opengl::fit_viewport_for_test(object, sdl, gl);
    let first = viewport();
    let mut grown = unsafe { frame(object, objc::sel::get("frame")) };
    grown.size.width += 100.0;
    grown.size.height += 60.0;
    unsafe { set_frame(object, objc::sel::get("setFrame:"), grown) };
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while window_of(object).size_pending != 0 {
        assert!(std::time::Instant::now() < deadline, "resize timed out");
        pump();
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    crate::opengl::fit_viewport_for_test(object, sdl, gl);
    assert_eq!(
        viewport(),
        first,
        "a windowed resize changed the GL viewport"
    );
    assert!(sdl3::video::gl_make_current(
        std::ptr::null_mut(),
        std::ptr::null_mut()
    ));
    sdl3::video::gl_destroy_context(gl);
    objc::arc::release(object);
    pump();
    objc::arc::pool_pop(pool);
}

#[test]
#[ignore = "requires Xvfb/xfwm4 and Mesa OpenGL"]
fn opengl_viewport_tracks_fullscreen_logical_frame_and_respects_guest_viewport() {
    crate::bootstrap();
    let pool = objc::arc::pool_push();
    let class = objc::class::lookup(c"NSWindow").unwrap();
    let object = objc::object::create_instance(class, 0);
    assert!(unsafe {
        init_with_content_rect_for_tests(
            object,
            NSRect {
                origin: NSPoint { x: 80.0, y: 100.0 },
                size: NSSize {
                    width: 720.0,
                    height: 460.0,
                },
            },
        )
    });
    unsafe {
        set_style_mask(
            object,
            objc::sel::get("setStyleMask:"),
            style::TITLED | style::RESIZABLE,
        )
    };
    assert!(rebuild_for_opengl(object));
    let sdl = window_of(object).sdl;
    sdl3::video::show_window(sdl);
    window_of(object).shown = 1;
    let gl = sdl3::video::gl_create_context(sdl).expect("OpenGL context");
    assert!(sdl3::video::gl_make_current(sdl, gl));
    let get = sdl3::video::gl_proc_address(c"glGetIntegerv").unwrap();
    let get: unsafe extern "C" fn(u32, *mut i32) = unsafe { std::mem::transmute(get) };
    let set = sdl3::video::gl_proc_address(c"glViewport").unwrap();
    let set: unsafe extern "C" fn(i32, i32, i32, i32) = unsafe { std::mem::transmute(set) };
    let viewport = || {
        let mut actual = [0; 4];
        unsafe { get(0x0BA2, actual.as_mut_ptr()) };
        actual
    };
    // Normal NSOpenGLContext setup fits the windowed content first.
    crate::opengl::fit_viewport_for_test(object, sdl, gl);
    unsafe {
        toggle_full_screen(
            object,
            objc::sel::get("toggleFullScreen:"),
            std::ptr::null_mut(),
        )
    };
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while window_of(object).reported_fullscreen == 0 {
        while let Some(event) = sdl3::video::poll() {
            crate::event::deliver(event);
        }
        assert!(std::time::Instant::now() < deadline, "fullscreen timed out");
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    crate::opengl::fit_viewport_for_test(object, sdl, gl);
    let previous = viewport();
    let host = host_frame(object).unwrap();
    let fractional = NSRect {
        origin: NSPoint {
            x: host.origin.x + 0.5,
            y: host.origin.y + 0.5,
        },
        size: NSSize {
            width: host.size.width - 0.5,
            height: host.size.height - 0.5,
        },
    };
    unsafe { set_frame(object, objc::sel::get("setFrame:"), fractional) };
    crate::opengl::fit_viewport_for_test(object, sdl, gl);
    let updated = viewport();
    assert_ne!(
        updated, previous,
        "GL viewport was not updated on fullscreen frame change"
    );
    let (pixel_width, pixel_height) = sdl3::video::window_size_in_pixels(sdl).unwrap();
    assert!(updated[0] >= 0 && updated[1] >= 0);
    assert!(updated[0] + updated[2] <= pixel_width);
    assert!(updated[1] + updated[3] <= pixel_height);

    // SDL preserves glViewport when an existing context is rebound to B.
    // This must be recognized as our previous fit, not as a guest override.
    let other = objc::object::create_instance(class, 0);
    assert!(unsafe {
        init_with_content_rect_for_tests(
            other,
            NSRect {
                origin: NSPoint { x: 100.0, y: 130.0 },
                size: NSSize {
                    width: 420.0,
                    height: 300.0,
                },
            },
        )
    });
    unsafe {
        set_style_mask(
            other,
            objc::sel::get("setStyleMask:"),
            style::TITLED | style::RESIZABLE,
        )
    };
    assert!(rebuild_for_opengl(other));
    let other_sdl = window_of(other).sdl;
    sdl3::video::show_window(other_sdl);
    assert!(sdl3::video::gl_make_current(other_sdl, gl));
    assert_eq!(
        viewport(),
        updated,
        "the driver changed context viewport on rebind"
    );
    crate::opengl::fit_viewport_for_test(other, other_sdl, gl);
    let on_other = viewport();
    assert_ne!(
        on_other, updated,
        "AppKit failed to update viewport when context changed window"
    );
    assert!(sdl3::video::gl_make_current(sdl, gl));
    crate::opengl::fit_viewport_for_test(object, sdl, gl);
    assert_eq!(
        viewport(),
        updated,
        "AppKit did not refit on return to previous window"
    );

    unsafe { set(7, 11, 17, 19) };
    assert!(sdl3::video::gl_make_current(other_sdl, gl));
    crate::opengl::fit_viewport_for_test(other, other_sdl, gl);
    assert_eq!(
        viewport(),
        [7, 11, 17, 19],
        "rebind overwrote guest-owned viewport"
    );
    assert!(sdl3::video::gl_make_current(sdl, gl));
    crate::opengl::fit_viewport_for_test(object, sdl, gl);
    assert_eq!(viewport(), [7, 11, 17, 19]);
    let shorter = NSRect {
        origin: NSPoint {
            y: fractional.origin.y + 31.0,
            ..fractional.origin
        },
        size: NSSize {
            height: fractional.size.height - 31.0,
            ..fractional.size
        },
    };
    unsafe { set_frame(object, objc::sel::get("setFrame:"), shorter) };
    crate::opengl::fit_viewport_for_test(object, sdl, gl);
    assert_eq!(
        viewport(),
        [7, 11, 17, 19],
        "overwrote the guest's viewport"
    );
    // The default framebuffer must be clipped at presentation, not by
    // glViewport: glClear disregards a viewport and guest code can set its own.
    let gl_clear_color = sdl3::video::gl_proc_address(c"glClearColor").unwrap();
    let gl_clear_color: unsafe extern "C" fn(f32, f32, f32, f32) =
        unsafe { std::mem::transmute(gl_clear_color) };
    let gl_clear = sdl3::video::gl_proc_address(c"glClear").unwrap();
    let gl_clear: unsafe extern "C" fn(u32) = unsafe { std::mem::transmute(gl_clear) };
    let gl_read = sdl3::video::gl_proc_address(c"glReadPixels").unwrap();
    let gl_read: unsafe extern "C" fn(i32, i32, i32, i32, u32, u32, *mut std::ffi::c_void) =
        unsafe { std::mem::transmute(gl_read) };
    let gl_scissor = sdl3::video::gl_proc_address(c"glScissor").unwrap();
    let gl_scissor: unsafe extern "C" fn(i32, i32, i32, i32) =
        unsafe { std::mem::transmute(gl_scissor) };
    let gl_enable = sdl3::video::gl_proc_address(c"glEnable").unwrap();
    let gl_enable: unsafe extern "C" fn(u32) = unsafe { std::mem::transmute(gl_enable) };
    let gl_disable = sdl3::video::gl_proc_address(c"glDisable").unwrap();
    let gl_disable: unsafe extern "C" fn(u32) = unsafe { std::mem::transmute(gl_disable) };
    let read = |x, y| {
        let mut pixel = [0u8; 4];
        unsafe { gl_read(x, y, 1, 1, 0x1908, 0x1401, pixel.as_mut_ptr().cast()) };
        pixel
    };
    unsafe {
        gl_clear_color(1.0, 0.0, 0.0, 1.0);
        gl_clear(0x4000)
    };
    assert_eq!(
        read(5, 5),
        [255, 0, 0, 255],
        "glClear ignores a narrow guest viewport"
    );
    crate::opengl::mask_logical_frame_for_test(object, sdl);
    assert_eq!(
        read(5, 5),
        [0, 0, 0, 255],
        "logical-frame exterior was not masked"
    );
    assert_eq!(
        read(5, 100),
        [255, 0, 0, 255],
        "mask damaged interior pixels"
    );
    assert_eq!(viewport(), [7, 11, 17, 19], "mask changed guest viewport");
    // Draw another color using a guest-defined scissor, then mask again.
    unsafe {
        gl_enable(0x0C11);
        gl_scissor(0, 100, 200, 100);
        gl_clear_color(0.0, 1.0, 0.0, 1.0);
        gl_clear(0x4000);
    }
    let mut guest_scissor = [0; 4];
    unsafe { get(0x0C10, guest_scissor.as_mut_ptr()) };
    crate::opengl::mask_logical_frame_for_test(object, sdl);
    let mut after_mask_scissor = [0; 4];
    unsafe { get(0x0C10, after_mask_scissor.as_mut_ptr()) };
    assert_eq!(
        after_mask_scissor, guest_scissor,
        "mask changed guest scissor"
    );
    assert_eq!(read(5, 5), [0, 0, 0, 255]);
    assert_eq!(
        read(5, 140),
        [0, 255, 0, 255],
        "guest drawing inside logical frame was lost"
    );
    unsafe { gl_disable(0x0C11) };
    // Restoration lifts the logical mask; formerly excluded pixels are usable.
    unsafe { set_frame(object, objc::sel::get("setFrame:"), host) };
    unsafe {
        gl_clear_color(1.0, 0.0, 0.0, 1.0);
        gl_clear(0x4000)
    };
    crate::opengl::mask_logical_frame_for_test(object, sdl);
    assert_eq!(
        read(5, 5),
        [255, 0, 0, 255],
        "restored full frame remained masked"
    );

    assert!(sdl3::video::gl_make_current(
        std::ptr::null_mut(),
        std::ptr::null_mut()
    ));
    sdl3::video::gl_destroy_context(gl);
    objc::arc::release(other);
    objc::arc::release(object);
    objc::arc::pool_pop(pool);
}

/// Run with a window manager reserving part of the desktop for a dock/panel.
#[test]
#[ignore = "requires a 1440x900 display, window manager and reserved desktop work area"]
fn fullscreen_screen_visible_frame_ignores_desktop_panel() {
    crate::bootstrap();
    let pool = objc::arc::pool_push();
    let object = objc::object::create_instance(objc::class::lookup(c"NSWindow").unwrap(), 0);
    assert!(unsafe {
        init_with_content_rect_for_tests(
            object,
            NSRect {
                origin: NSPoint { x: 80.0, y: 100.0 },
                size: NSSize {
                    width: 720.0,
                    height: 460.0,
                },
            },
        )
    });
    let sdl = window_of(object).sdl;
    let display = sdl3::video::display_for_window(sdl).unwrap();
    let bounds = sdl3::video::display_bounds(display).unwrap();
    let usable = sdl3::video::display_usable_bounds(display).unwrap();
    let desktop = crate::geometry::rect_from_sdl(usable.x, usable.y, usable.w, usable.h);
    let whole = crate::geometry::rect_from_sdl(bounds.x, bounds.y, bounds.w, bounds.h);
    assert_ne!(desktop, whole, "run with a reserved panel work area");
    let screen = crate::screen::screen_object(c"NSScreen", display);
    let visible = |screen| {
        let method = objc::class::lookup(c"NSScreen")
            .unwrap()
            .method(objc::sel::get("visibleFrame"))
            .unwrap();
        // SAFETY: the registered NSScreen method returns NSRect.
        let call: unsafe extern "C" fn(Id, Sel) -> NSRect =
            unsafe { std::mem::transmute(method.implementation()) };
        unsafe { call(screen, objc::sel::get("visibleFrame")) }
    };
    assert_eq!(visible(screen), desktop);
    order_front(object, true);
    let wait_until = |done: &dyn Fn() -> bool, what: &str| {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            while let Some(event) = sdl3::video::poll() {
                crate::event::deliver(event);
            }
            if done() {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "{what} did not settle"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    };
    let minimized = || sdl3::video::window_flags(sdl) & sdl3::video::flags::MINIMIZED != 0;
    let settle = |wanted| {
        wait_until(
            &|| {
                is_fullscreen(sdl) == wanted
                    && (window_of(object).reported_fullscreen != 0) == wanted
            },
            "fullscreen",
        )
    };
    unsafe {
        toggle_full_screen(
            object,
            objc::sel::get("toggleFullScreen:"),
            std::ptr::null_mut(),
        )
    };
    settle(true);
    assert_eq!(visible(screen), whole);
    assert_eq!(
        whole.size,
        NSSize {
            width: 1440.0,
            height: 900.0
        },
        "run tools/fullscreen-oracle/check-linux.sh for the native fixture extent"
    );
    let pixels = sdl3::video::window_size_in_pixels(sdl).unwrap();
    let mut observations = 0;
    for line in include_str!("../tests/fixtures/fullscreen-workarea-pixels.tsv")
        .lines()
        .filter(|line| !line.starts_with('#'))
    {
        let fields: Vec<_> = line.split('\t').collect();
        let value = |i: usize| fields[i].parse::<f64>().unwrap();
        let requested = NSRect {
            origin: NSPoint {
                x: whole.origin.x + value(2),
                y: whole.origin.y + value(3),
            },
            size: NSSize {
                width: value(4),
                height: value(5),
            },
        };
        unsafe { set_frame(object, objc::sel::get("setFrame:"), requested) };
        assert_eq!(unsafe { frame(object, objc::sel::get("frame")) }, requested);
        assert_eq!(
            crate::view::frame_of(window_of(object).content_view).size,
            requested.size
        );
        let top = value(6);
        let bottom = value(7);
        assert_eq!(top + requested.size.height + bottom, whole.size.height);
        let expected = sdl3::Rect {
            x: 0,
            y: (top * f64::from(pixels.1) / whole.size.height).round() as i32,
            w: pixels.0,
            h: (requested.size.height * f64::from(pixels.1) / whole.size.height).round() as i32,
        };
        assert_eq!(
            presentation_clip(sdl),
            Some(expected),
            "native {} {}",
            fields[0],
            fields[1]
        );
        assert_eq!(
            visible(screen),
            whole,
            "a smaller window must not change the Space's work area"
        );
        observations += 1;
    }
    assert_eq!(observations, 12);
    // The screen's usable area must not depend on a guest's logical frame.
    let shortened = NSRect {
        size: NSSize {
            width: whole.size.width,
            height: whole.size.height - 50.0,
        },
        ..whole
    };
    unsafe { set_frame(object, objc::sel::get("setFrame:"), shortened) };
    assert_eq!(visible(screen), whole);
    // A fullscreen window on this display must not change other displays.
    for other in sdl3::video::displays()
        .into_iter()
        .filter(|id| *id != display)
    {
        let usable = sdl3::video::display_usable_bounds(other).unwrap();
        assert_eq!(
            visible(crate::screen::screen_object(c"NSScreen", other)),
            crate::geometry::rect_from_sdl(usable.x, usable.y, usable.w, usable.h)
        );
    }
    assert!(sdl3::video::minimize_window(sdl));
    wait_until(&minimized, "minimize");
    assert_eq!(
        visible(screen),
        desktop,
        "minimized windows do not cover the desktop"
    );
    assert!(sdl3::video::restore_window(sdl));
    wait_until(&|| !minimized(), "restore");
    settle(true);
    assert_eq!(visible(screen), whole);
    unsafe { order_out(object, objc::sel::get("orderOut:"), std::ptr::null_mut()) };
    assert_eq!(
        visible(screen),
        desktop,
        "hidden windows do not cover the desktop"
    );
    order_front(object, true);
    unsafe {
        toggle_full_screen(
            object,
            objc::sel::get("toggleFullScreen:"),
            std::ptr::null_mut(),
        )
    };
    settle(false);
    assert_eq!(visible(screen), desktop);
    unsafe { close(object, objc::sel::get("close")) };
    objc::arc::release(object);
    objc::arc::pool_pop(pool);
}
