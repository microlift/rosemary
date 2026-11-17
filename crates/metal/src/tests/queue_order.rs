use super::*;

unsafe fn copy_command(queue: Id, source: Id, target: Id) -> Id {
    // SAFETY: the fixture owns the queue and both four-byte buffers.
    unsafe {
        let command = send(queue, "commandBuffer");
        let blit = send(command, "blitCommandEncoder");
        copy_buffer(blit, source, target, 4);
        send_void(blit, "endEncoding");
        command
    }
}

#[test]
fn enqueued_buffers_keep_order_when_workers_commit_in_reverse() {
    isolate_metal_fixture!();
    let Some(device) = gpu_device() else {
        return;
    };
    // SAFETY: test-owned buffers and queues, through their registered methods.
    unsafe {
        let queue = new_command_queue(device, std::ptr::null());
        let independent = new_command_queue(device, std::ptr::null());
        let first_value = new_buffer(device, std::ptr::null(), 4, 0);
        let second_value = new_buffer(device, std::ptr::null(), 4, 0);
        let target = new_buffer(device, std::ptr::null(), 4, 0);
        object::body::<resource::Buffer>(first_value)
            .inner
            .contents
            .cast::<u32>()
            .write(11);
        object::body::<resource::Buffer>(second_value)
            .inner
            .contents
            .cast::<u32>()
            .write(22);
        let first = copy_command(queue, first_value, target);
        let second = copy_command(queue, second_value, target);
        assert_eq!(send_number_result(first, "status"), 0);
        send_void(first, "enqueue");
        send_void(first, "enqueue");
        send_void(second, "enqueue");
        assert_eq!(send_number_result(first, "status"), 1);
        assert_eq!(send_number_result(second, "status"), 1);

        let address = second as usize;
        std::thread::spawn(move || {
            crate::enter_darwin_thread();
            send_void(address as Id, "commit");
        })
        .join()
        .unwrap();

        // This queue must progress while the first queue has an empty reservation.
        let probe = send(independent, "commandBuffer");
        send_void(probe, "commit");
        send_void(probe, "waitUntilCompleted");
        assert_eq!(send_number_result(probe, "status"), 4);
        assert_eq!(send_number_result(second, "status"), 2);
        assert_eq!(
            object::body::<resource::Buffer>(target)
                .inner
                .contents
                .cast::<u32>()
                .read(),
            0
        );

        send_void(first, "commit");
        send_void(second, "waitUntilCompleted");
        assert_eq!(send_number_result(first, "status"), 4);
        assert_eq!(send_number_result(second, "status"), 4);
        assert_eq!(
            object::body::<resource::Buffer>(target)
                .inner
                .contents
                .cast::<u32>()
                .read(),
            22
        );
        for object in [
            first_value,
            second_value,
            target,
            queue,
            independent,
            device,
        ] {
            objc::arc::release(object);
        }
    }
}

#[test]
fn implicit_enqueue_reserves_at_commit_instead_of_creation() {
    isolate_metal_fixture!();
    let Some(device) = gpu_device() else {
        return;
    };
    // SAFETY: test-owned buffers and queue, through their registered methods.
    unsafe {
        let queue = new_command_queue(device, std::ptr::null());
        let source = new_buffer(device, std::ptr::null(), 4, 0);
        let target = new_buffer(device, std::ptr::null(), 4, 0);
        let old = send(queue, "commandBuffer");
        object::body::<resource::Buffer>(source)
            .inner
            .contents
            .cast::<u32>()
            .write(42);
        let newer = copy_command(queue, source, target);
        send_void(newer, "commit");
        send_void(newer, "waitUntilCompleted");
        assert_eq!(send_number_result(old, "status"), 0);
        assert_eq!(send_number_result(newer, "status"), 4);
        assert_eq!(
            object::body::<resource::Buffer>(target)
                .inner
                .contents
                .cast::<u32>()
                .read(),
            42
        );
        send_void(old, "commit");
        send_void(old, "waitUntilCompleted");
        send_void(old, "enqueue");
        assert_eq!(send_number_result(old, "status"), 4);
        for object in [source, target, queue, device] {
            objc::arc::release(object);
        }
    }
}
