use super::*;

use corefoundation::callbacks::Policy;
use corefoundation::collections::{Number, new_dictionary, new_number};

fn properties(pairs: &[(&str, i64)]) -> CFTypeRef {
    corefoundation::bootstrap();
    let pairs: Vec<(CFTypeRef, CFTypeRef)> = pairs
        .iter()
        .map(|(name, value)| (key(name), new_number(Number::Integer(*value))))
        .collect();
    new_dictionary(
        &pairs,
        Policy::from_key_callbacks(),
        Policy::from_type_callbacks(),
        false,
    )
}

/// What Qt's `QIOSurfaceGraphicsBuffer` asks for.
fn bgra(width: i64, height: i64) -> CFTypeRef {
    // SAFETY: keys this crate made.
    let row = unsafe { IOSurfaceAlignProperty(key(BYTES_PER_ROW), width as usize * 4) };
    let size = unsafe { IOSurfaceAlignProperty(key(ALLOC_SIZE), row * height as usize) };
    let options = properties(&[
        (WIDTH, width),
        (HEIGHT, height),
        (PIXEL_FORMAT, i64::from(PIXEL_FORMAT_BGRA)),
        (BYTES_PER_ELEMENT, 4),
        (BYTES_PER_ROW, row as i64),
        (ALLOC_SIZE, size as i64),
    ]);
    // SAFETY: a dictionary of numbers.
    unsafe { IOSurfaceCreate(options) }
}

#[test]
fn a_surface_has_the_layout_it_was_aligned_for() {
    let surface = bgra(600, 300);
    assert!(!surface.is_null());
    // SAFETY: a surface this test made.
    unsafe {
        let row = IOSurfaceAlignProperty(key(BYTES_PER_ROW), 600 * 4);
        assert_eq!(row % ROW_ALIGNMENT, 0);
        assert!(row >= 2400);
        assert_eq!(IOSurfaceGetBytesPerRow(surface), row);
        assert_eq!(IOSurfaceGetWidth(surface), 600);
        assert_eq!(IOSurfaceGetHeight(surface), 300);
        assert_eq!(IOSurfaceGetBytesPerElement(surface), 4);
        assert_eq!(IOSurfaceGetPixelFormat(surface), PIXEL_FORMAT_BGRA);
        assert!(IOSurfaceGetAllocSize(surface) >= row * 300);
        assert_eq!(IOSurfaceGetAllocSize(surface) % PAGE, 0);
        let base = IOSurfaceGetBaseAddress(surface) as *mut u8;
        assert!(!base.is_null());
        // The whole of the last row is writable.
        base.add(row * 299 + 2399).write(7);
        assert_eq!(
            IOSurfaceGetTypeID(),
            corefoundation::api::CFGetTypeID(surface)
        );
        corefoundation::api::CFRelease(surface);
    }
}

#[test]
fn a_surface_without_a_size_is_not_made() {
    // SAFETY: a dictionary of numbers.
    assert!(unsafe { IOSurfaceCreate(properties(&[(WIDTH, 4)])) }.is_null());
    assert!(unsafe { IOSurfaceCreate(properties(&[(WIDTH, 4), (HEIGHT, -1)])) }.is_null());
}

#[test]
fn a_planar_surface_is_refused() {
    corefoundation::bootstrap();
    let planes = corefoundation::collections::new_array(&[], Policy::from_type_callbacks(), false);
    let options = new_dictionary(
        &[
            (key(WIDTH), new_number(Number::Integer(4))),
            (key(HEIGHT), new_number(Number::Integer(4))),
            (key(PLANE_INFO), planes),
        ],
        Policy::from_key_callbacks(),
        Policy::from_type_callbacks(),
        false,
    );
    // SAFETY: a dictionary.
    assert!(unsafe { IOSurfaceCreate(options) }.is_null());
}

#[test]
fn defaults_are_one_byte_per_element() {
    // SAFETY: a dictionary of numbers.
    let surface = unsafe { IOSurfaceCreate(properties(&[(WIDTH, 3), (HEIGHT, 2)])) };
    // SAFETY: a surface this test made.
    unsafe {
        assert_eq!(IOSurfaceGetBytesPerElement(surface), 1);
        assert_eq!(IOSurfaceGetElementWidth(surface), 1);
        assert_eq!(IOSurfaceGetBytesPerRow(surface), ROW_ALIGNMENT);
        corefoundation::api::CFRelease(surface);
    }
}

#[test]
fn a_write_unlock_moves_the_seed_and_a_read_unlock_does_not() {
    let surface = bgra(2, 2);
    let mut seed = u32::MAX;
    // SAFETY: a surface this test made.
    unsafe {
        assert_eq!(IOSurfaceUnlock(surface, 0, &mut seed), NOT_LOCKED);
        assert_eq!(IOSurfaceLock(surface, 0, &mut seed), SUCCESS);
        assert_eq!(seed, 0);
        assert_eq!(IOSurfaceUnlock(surface, 0, &mut seed), SUCCESS);
        assert_eq!(seed, 1);
        IOSurfaceLock(surface, LOCK_READ_ONLY, std::ptr::null_mut());
        IOSurfaceUnlock(surface, LOCK_READ_ONLY, &mut seed);
        assert_eq!(seed, 1);
        assert_eq!(IOSurfaceGetSeed(surface), 1);
        corefoundation::api::CFRelease(surface);
    }
}

#[test]
fn the_use_count_is_what_in_use_reports() {
    let surface = bgra(2, 2);
    // SAFETY: a surface this test made.
    unsafe {
        assert_eq!(IOSurfaceIsInUse(surface), 0);
        IOSurfaceIncrementUseCount(surface);
        assert_eq!(IOSurfaceIsInUse(surface), 1);
        assert_eq!(IOSurfaceGetUseCount(surface), 1);
        IOSurfaceDecrementUseCount(surface);
        IOSurfaceDecrementUseCount(surface);
        assert_eq!(IOSurfaceGetUseCount(surface), 0, "never below zero");
        corefoundation::api::CFRelease(surface);
    }
}

#[test]
fn values_are_set_replaced_copied_and_removed() {
    let surface = bgra(2, 2);
    let name = key("IOSurfaceColorSpace");
    let first = new_number(Number::Integer(1));
    let second = new_number(Number::Integer(2));
    // SAFETY: a surface and CF objects this test made.
    unsafe {
        IOSurfaceSetValue(surface, name, first);
        IOSurfaceSetValue(surface, name, second);
        let copied = IOSurfaceCopyValue(surface, name);
        assert_eq!(copied, second);
        corefoundation::api::CFRelease(copied);
        let all = IOSurfaceCopyAllValues(surface);
        assert_eq!(corefoundation::api::CFDictionaryGetCount(all), 1);
        corefoundation::api::CFRelease(all);
        IOSurfaceRemoveValue(surface, name);
        assert!(IOSurfaceCopyValue(surface, name).is_null());
        corefoundation::api::CFRelease(surface);
    }
}

#[test]
fn a_surface_is_found_by_its_id_until_it_goes() {
    let surface = bgra(2, 2);
    // SAFETY: a surface this test made.
    unsafe {
        let id = IOSurfaceGetID(surface);
        assert_ne!(id, 0);
        let found = IOSurfaceLookup(id);
        assert_eq!(found, surface);
        corefoundation::api::CFRelease(found);
        corefoundation::api::CFRelease(surface);
        assert!(IOSurfaceLookup(id).is_null());
    }
}

#[test]
fn every_key_is_published_as_a_cell_holding_its_string() {
    let symbols = data_symbols();
    let (_, cell) = symbols
        .iter()
        .find(|(name, _)| name == "_kIOSurfaceBytesPerRow")
        .expect("published");
    // SAFETY: a cell `global_cell` leaked, holding a string.
    let string = unsafe { *(*cell as *const u64) } as CFTypeRef;
    // SAFETY: two CF strings.
    assert_ne!(
        unsafe { corefoundation::api::CFEqual(string, key(BYTES_PER_ROW)) },
        0
    );
}

fn publish_pixels(surface: CFTypeRef, value: u8) {
    // SAFETY: the test owns the surface and writes only its visible rows.
    unsafe {
        assert_eq!(IOSurfaceLock(surface, 0, std::ptr::null_mut()), SUCCESS);
        let p = pixels(surface).unwrap();
        for y in 0..p.height {
            std::ptr::write_bytes(
                p.data.add(y * p.bytes_per_row) as *mut u8,
                value,
                p.width * 4,
            );
        }
        assert_eq!(IOSurfaceUnlock(surface, 0, std::ptr::null_mut()), SUCCESS);
    }
}

#[test]
fn snapshots_own_visible_bytes_and_detect_reused_surfaces() {
    let a = bgra(2, 2);
    let b = bgra(2, 2);
    publish_pixels(a, 11);
    publish_pixels(b, 11);
    // SAFETY: retained test surfaces with lock-bracketed writes.
    unsafe {
        let first = snapshot(a, None).unwrap();
        let equal = snapshot(b, Some(&first)).unwrap();
        assert!(first.same_pixels(&equal));
        // Padding changes are not visible content.
        IOSurfaceLock(b, 0, std::ptr::null_mut());
        (IOSurfaceGetBaseAddress(b) as *mut u8).add(8).write(99);
        IOSurfaceUnlock(b, 0, std::ptr::null_mut());
        let padded = snapshot(b, Some(&equal)).unwrap();
        assert!(padded.same_pixels(&equal));
        publish_pixels(b, 22);
        assert!(!snapshot_is_current(b, &padded));
        let changed = snapshot(b, Some(&padded)).unwrap();
        assert!(!changed.same_pixels(&padded));
        assert_eq!(first.bytes(), &[11; 16]);
        assert_eq!(changed.bytes(), &[22; 16]);
        corefoundation::api::CFRelease(a);
        corefoundation::api::CFRelease(b);
        assert_eq!(first.bytes(), &[11; 16]);
    }
}

#[test]
fn explicit_snapshot_revalidation_detects_lockless_changes() {
    let image = bgra(2, 2);
    publish_pixels(image, 1);
    let before = unsafe { snapshot(image, None).unwrap() };
    // A guest is allowed to hold a base address even if it does not honor
    // the CPU lock protocol. The setter must not misclassify such bytes.
    unsafe { std::ptr::write_bytes(IOSurfaceGetBaseAddress(image) as *mut u8, 3, 8) };
    let cached = unsafe { snapshot(image, Some(&before)).unwrap() };
    assert!(cached.same_pixels(&before));
    let updated = unsafe { snapshot_revalidated(image, Some(&before)).unwrap() };
    assert!(!updated.same_pixels(&before));
    assert_eq!(&updated.bytes()[..8], &[3; 8]);
    assert!(snapshot_is_current(image, &updated));
    let same = unsafe { snapshot_revalidated(image, Some(&updated)).unwrap() };
    assert!(same.same_pixels(&updated));
    unsafe { corefoundation::api::CFRelease(image) };
}

#[test]
fn active_producers_cannot_be_observed_halfway_through_a_publication() {
    let surface = bgra(64, 2);
    publish_pixels(surface, 3);
    let before = unsafe { snapshot(surface, None).unwrap() };
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let (finish_tx, finish_rx) = std::sync::mpsc::channel();
    let address = surface as usize;
    let producer = std::thread::spawn(move || unsafe {
        let surface = address as CFTypeRef;
        IOSurfaceLock(surface, 0, std::ptr::null_mut());
        let p = pixels(surface).unwrap();
        std::ptr::write_bytes(p.data as *mut u8, 9, 256);
        ready_tx.send(()).unwrap();
        finish_rx.recv().unwrap();
        std::ptr::write_bytes(p.data.add(p.bytes_per_row) as *mut u8, 9, 256);
        IOSurfaceUnlock(surface, 0, std::ptr::null_mut());
    });
    ready_rx.recv().unwrap();
    assert_eq!(
        unsafe { snapshot(surface, Some(&before)) }.err(),
        Some(SnapshotError::Busy)
    );
    assert!(!snapshot_is_current(surface, &before));
    assert_eq!(before.bytes(), &[3; 512]);
    finish_tx.send(()).unwrap();
    producer.join().unwrap();
    let after = unsafe { snapshot(surface, Some(&before)).unwrap() };
    assert_eq!(after.bytes(), &[9; 512]);
    assert!(snapshot_is_current(surface, &after));
    unsafe { corefoundation::api::CFRelease(surface) };
}

#[test]
fn seed_wrap_does_not_reuse_an_old_snapshot() {
    let surface = bgra(2, 1);
    publish_pixels(surface, 1);
    let before = unsafe { snapshot(surface, None).unwrap() };
    state(surface)
        .unwrap()
        .seed
        .store(u32::MAX, Ordering::Release);
    publish_pixels(surface, 2);
    assert_eq!(unsafe { IOSurfaceGetSeed(surface) }, 0);
    assert!(!snapshot_is_current(surface, &before));
    let after = unsafe { snapshot(surface, Some(&before)).unwrap() };
    assert_eq!(after.bytes(), &[2; 8]);
    state(surface)
        .unwrap()
        .cpu_access
        .lock()
        .unwrap()
        .generation = Some(u64::MAX);
    publish_pixels(surface, 3);
    assert_eq!(
        unsafe { snapshot(surface, Some(&after)) }.err(),
        Some(SnapshotError::Unsupported)
    );
    unsafe { corefoundation::api::CFRelease(surface) };
}

#[test]
fn untracked_initial_memory_is_not_eligible_for_deduplication() {
    let surface = bgra(2, 1);
    unsafe {
        let before = snapshot(surface, None).unwrap();
        assert!(!before.is_published());
        // Compatibility fallback: initialization before any lock publication.
        (IOSurfaceGetBaseAddress(surface) as *mut u8).write(17);
        let after = snapshot(surface, Some(&before)).unwrap();
        assert!(!before.same_pixels(&after));
        assert_eq!(after.bytes()[0], 17);
        assert_eq!(before.bytes()[0], 0);
        corefoundation::api::CFRelease(surface);
    }
}

#[test]
fn first_cpu_publication_advances_an_untracked_snapshot() {
    let surface = bgra(2, 1);
    let other = bgra(2, 1);
    let first = unsafe { snapshot(surface, None).unwrap() };
    assert!(!first.is_published());
    assert!(snapshot_is_current(surface, &first));
    assert!(!snapshot_is_current(other, &first));

    // An in-progress writer cannot expose a partial first publication.
    unsafe { assert_eq!(IOSurfaceLock(surface, 0, std::ptr::null_mut()), SUCCESS) };
    assert!(!snapshot_is_current(surface, &first));
    unsafe { assert_eq!(IOSurfaceUnlock(surface, 0, std::ptr::null_mut()), SUCCESS) };
    assert!(!snapshot_is_current(surface, &first));

    // No visible bytes changed, yet the initial snapshot must be retired.
    let published = unsafe { snapshot(surface, Some(&first)).unwrap() };
    assert!(published.is_published());
    assert!(!first.same_pixels(&published));
    assert_eq!(first.bytes(), published.bytes());
    assert!(snapshot_is_current(surface, &published));
    publish_pixels(surface, 13);
    assert!(!snapshot_is_current(surface, &published));
    unsafe {
        corefoundation::api::CFRelease(surface);
        corefoundation::api::CFRelease(other);
    }
}

#[test]
fn one_immutable_snapshot_is_shared_across_layers() {
    let surface = bgra(512, 512);
    publish_pixels(surface, 7);
    let first = unsafe { snapshot(surface, None).unwrap() };
    let others: Vec<_> = (0..6)
        .map(|_| unsafe { snapshot(surface, None).unwrap() })
        .collect();
    for item in &others {
        assert!(Arc::ptr_eq(&first.bytes, &item.bytes));
    }
    // Revalidation of a second layer must share rather than allocate a copy.
    let explicit = unsafe { snapshot_revalidated(surface, None).unwrap() };
    assert!(Arc::ptr_eq(&first.bytes, &explicit.bytes));
    publish_pixels(surface, 9);
    let next = unsafe { snapshot(surface, Some(&first)).unwrap() };
    assert!(!Arc::ptr_eq(&first.bytes, &next.bytes));
    let later = unsafe { snapshot(surface, Some(&first)).unwrap() };
    assert!(Arc::ptr_eq(&next.bytes, &later.bytes));
    assert_eq!(first.bytes()[..4], [7; 4]);
    assert_eq!(later.bytes()[..4], [9; 4]);
    unsafe { corefoundation::api::CFRelease(surface) };
}

#[test]
fn oversized_surface_still_tracks_publications_without_owned_bytes() {
    let surface = bgra(4096, 2160);
    assert_eq!(
        unsafe { snapshot(surface, None) }.err(),
        Some(SnapshotError::Unsupported)
    );
    let first = publication(surface).unwrap();
    assert!(publication_is_current(surface, &first));
    publish_pixels(surface, 4);
    assert!(!publication_is_current(surface, &first));
    let second = publication(surface).unwrap();
    assert!(publication_is_current(surface, &second));
    unsafe { corefoundation::api::CFRelease(surface) };
}

#[test]
fn read_only_locks_do_not_hold_back_a_snapshot() {
    let surface = bgra(2, 1);
    publish_pixels(surface, 5);
    unsafe {
        assert_eq!(
            IOSurfaceLock(surface, LOCK_READ_ONLY, std::ptr::null_mut()),
            SUCCESS
        );
        let held = snapshot(surface, None).unwrap();
        assert_eq!(held.bytes(), &[5; 8]);
        assert!(snapshot_is_current(surface, &held));
        assert!(!cpu_access_active(surface));
        assert_eq!(
            IOSurfaceUnlock(surface, LOCK_READ_ONLY, std::ptr::null_mut()),
            SUCCESS
        );
        assert!(snapshot_is_current(surface, &held));
        corefoundation::api::CFRelease(surface);
    }
}

#[test]
fn mismatched_unlock_options_do_not_strand_a_writer() {
    let surface = bgra(2, 1);
    unsafe {
        assert_eq!(IOSurfaceLock(surface, 0, std::ptr::null_mut()), SUCCESS);
        assert_eq!(
            IOSurfaceUnlock(surface, LOCK_READ_ONLY, std::ptr::null_mut()),
            SUCCESS
        );
        assert!(!cpu_access_active(surface));
        assert!(snapshot(surface, None).is_ok());
        corefoundation::api::CFRelease(surface);
    }
}

#[test]
fn a_writer_locking_mid_copy_discards_the_copy_without_waiting() {
    let surface = bgra(2, 1);
    publish_pixels(surface, 1);
    let s = state(surface).unwrap();
    let gate = open_gate(s).unwrap();
    // The copy runs outside the mutex: a writer's lock does not block on it.
    publish_pixels(surface, 2);
    assert_eq!(gate.close(s).err(), Some(SnapshotError::Busy));
    let after = unsafe { snapshot(surface, None).unwrap() };
    assert_eq!(after.bytes(), &[2; 8]);
    unsafe { corefoundation::api::CFRelease(surface) };
}

#[test]
fn the_cached_image_goes_with_its_last_holder() {
    let surface = bgra(64, 64);
    publish_pixels(surface, 8);
    let first = unsafe { snapshot(surface, None).unwrap() };
    let bytes = Arc::downgrade(&first.bytes);
    drop(first);
    assert!(bytes.upgrade().is_none());
    let again = unsafe { snapshot(surface, None).unwrap() };
    assert_eq!(again.bytes()[..4], [8; 4]);
    unsafe { corefoundation::api::CFRelease(surface) };
}
