use super::*;

unsafe fn signal(command: Id, event: Id, value: u64) {
    // SAFETY: the objects implement the native signal selector.
    unsafe {
        let (send, sel) =
            method::<unsafe extern "C" fn(Id, Sel, Id, u64)>(command, "encodeSignalEvent:value:");
        send(command, sel, event, value);
    }
}

unsafe fn counter(device: Id, event: Id) -> u64 {
    // SAFETY: the fixture owns the device and the event's live semaphore.
    unsafe {
        let gpu = object::body_ref::<Device>(device).gpu.unwrap();
        let semaphore = object::body_ref::<crate::event::Event>(event).semaphore;
        gpu.device.get_semaphore_counter_value(semaphore).unwrap()
    }
}

#[test]
fn nonshared_signals_ignore_zero_equal_and_decreasing_values() {
    isolate_metal_fixture!();
    let Some(device) = gpu_device() else {
        return;
    };
    // SAFETY: fixture-owned objects, with native method prototypes.
    unsafe {
        let event = send(device, "newEvent");
        let queue = new_command_queue(device, std::ptr::null());
        for (values, expected) in [
            (&[0, 4, 4, 2, 5][..], 5),
            (&[3, 5, 0][..], 5),
            (&[7][..], 7),
        ] {
            let command = send(queue, "commandBuffer");
            for &value in values {
                signal(command, event, value);
            }
            send_void(command, "commit");
            send_void(command, "waitUntilCompleted");
            assert_eq!(send_number_result(command, "status"), 4);
            assert_eq!(counter(device, event), expected);
        }
        for object in [event, queue, device] {
            objc::arc::release(object);
        }
    }
}

#[test]
fn nonshared_admission_follows_submission_instead_of_recording_order() {
    isolate_metal_fixture!();
    let Some(device) = gpu_device() else {
        return;
    };
    // SAFETY: fixture-owned objects, with native method prototypes.
    unsafe {
        let event = send(device, "newEvent");
        let queue = new_command_queue(device, std::ptr::null());
        let higher = send(queue, "commandBuffer");
        signal(higher, event, 8);
        let lower = send(queue, "commandBuffer");
        signal(lower, event, 2);
        send_void(lower, "commit");
        send_void(lower, "waitUntilCompleted");
        assert_eq!(counter(device, event), 2);
        send_void(higher, "commit");
        send_void(higher, "waitUntilCompleted");
        assert_eq!(counter(device, event), 8);
        for object in [event, queue, device] {
            objc::arc::release(object);
        }
    }
}

#[test]
fn concurrent_nonshared_submissions_preserve_the_largest_value() {
    isolate_metal_fixture!();
    let Some(device) = gpu_device() else {
        return;
    };
    // SAFETY: handles stay owned until all workers and GPU operations complete.
    unsafe {
        let event = send(device, "newEvent");
        let queue = new_command_queue(device, std::ptr::null());
        let workers: Vec<_> = (0..4)
            .map(|worker| {
                let (event, queue) = (event as usize, queue as usize);
                std::thread::spawn(move || {
                    crate::enter_darwin_thread();
                    let pool = objc::arc::pool_push();
                    for value in (1..=16).rev() {
                        let command = send(queue as Id, "commandBuffer");
                        signal(command, event as Id, worker * 16 + value);
                        send_void(command, "commit");
                        send_void(command, "waitUntilCompleted");
                        assert_eq!(send_number_result(command, "status"), 4);
                    }
                    objc::arc::pool_pop(pool);
                })
            })
            .collect();
        for worker in workers {
            worker.join().unwrap();
        }
        assert_eq!(counter(device, event), 64);
        for object in [event, queue, device] {
            objc::arc::release(object);
        }
    }
}
