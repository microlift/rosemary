//! `MTLEvent` and `MTLSharedEvent`, both Vulkan timeline semaphores.
//!
//! A command buffer waits for or signals an event between its encoders; see
//! `-encodeWaitForEvent:value:`, which splits its Vulkan recording there. A
//! shared event can also be read, signalled and waited on from the CPU, and
//! can call a block once it reaches a value, which one worker thread watches
//! for on behalf of every listener.

use std::sync::{Mutex, Once};

use ash::vk;

use super::*;

pub(crate) struct Event {
    gpu: &'static Gpu,
    pub(crate) semaphore: vk::Semaphore,
    device: Id,
}

/// `-newEvent` and `-newSharedEvent`.
pub(super) fn new_event(gpu: &'static Gpu, device: Id, class: &std::ffi::CStr) -> Id {
    if !gpu.timeline_semaphores {
        gpu::trace("MTLEvent needs Vulkan timeline semaphores, which this device lacks");
        return std::ptr::null_mut();
    }
    let mut kind = vk::SemaphoreTypeCreateInfo::default()
        .semaphore_type(vk::SemaphoreType::TIMELINE)
        .initial_value(0);
    let info = vk::SemaphoreCreateInfo::default().push_next(&mut kind);
    // SAFETY: a well-formed create info on a device with timeline semaphores.
    match unsafe { gpu.device.create_semaphore(&info, None) } {
        Ok(semaphore) => object::make(
            class,
            Event {
                gpu,
                semaphore,
                device: objc::arc::retain(device),
            },
        ),
        Err(error) => {
            gpu::trace(format!(
                "vkCreateSemaphore for an MTLEvent failed with {error:?}"
            ));
            std::ptr::null_mut()
        }
    }
}

unsafe extern "C" fn event_device(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: an event of this crate's.
    unsafe { object::body::<Event>(receiver) }.device
}

unsafe extern "C" fn event_dealloc(receiver: Id, _sel: Sel) {
    // SAFETY: an event of this crate's. Every command buffer and listener
    // which uses it holds a reference until its work is done, so the
    // semaphore is no longer in use.
    let event = unsafe { object::body::<Event>(receiver) };
    unsafe { event.gpu.device.destroy_semaphore(event.semaphore, None) };
    objc::arc::release(event.device);
    objc::arc::finalize(receiver);
}

unsafe extern "C" fn signaled_value(receiver: Id, _sel: Sel) -> u64 {
    // SAFETY: an event of this crate's.
    let event = unsafe { object::body::<Event>(receiver) };
    // SAFETY: a timeline semaphore of this device's.
    unsafe {
        event
            .gpu
            .device
            .get_semaphore_counter_value(event.semaphore)
    }
    .unwrap_or_else(|error| {
        gpu::trace(format!("vkGetSemaphoreCounterValue failed with {error:?}"));
        0
    })
}

unsafe extern "C" fn set_signaled_value(receiver: Id, sel: Sel, value: u64) {
    // SAFETY: an event of this crate's.
    let event = unsafe { object::body::<Event>(receiver) };
    // SAFETY: as above.
    if value <= unsafe { signaled_value(receiver, sel) } {
        // A timeline only moves forward, so a value at or below the current
        // one cannot be written.
        gpu::trace(format!(
            "MTLSharedEvent setSignaledValue:{value} does not raise the value; ignored"
        ));
        return;
    }
    let info = vk::SemaphoreSignalInfo::default()
        .semaphore(event.semaphore)
        .value(value);
    // SAFETY: a timeline semaphore of this device's, raised.
    if let Err(error) = unsafe { event.gpu.device.signal_semaphore(&info) } {
        gpu::trace(format!("vkSignalSemaphore failed with {error:?}"));
    }
}

/// `-waitUntilSignaledValue:timeoutMS:`: whether the value was reached.
unsafe extern "C" fn wait_until_signaled(
    receiver: Id,
    _sel: Sel,
    value: u64,
    milliseconds: u64,
) -> bool {
    // SAFETY: an event of this crate's.
    let event = unsafe { object::body::<Event>(receiver) };
    let semaphores = [event.semaphore];
    let values = [value];
    let info = vk::SemaphoreWaitInfo::default()
        .semaphores(&semaphores)
        .values(&values);
    let timeout = milliseconds.saturating_mul(1_000_000);
    // SAFETY: a timeline semaphore of this device's.
    unsafe { event.gpu.device.wait_semaphores(&info, timeout) }.is_ok()
}

unsafe extern "C" fn new_shared_event_handle(_receiver: Id, _sel: Sel) -> Id {
    gpu::trace("MTLSharedEvent newSharedEventHandle: events are not shared between processes");
    std::ptr::null_mut()
}

/// `MTLSharedEventListener`: the dispatch queue notifications run on.
struct Listener {
    queue: u64,
}

unsafe extern "C" fn listener_init(receiver: Id, _sel: Sel) -> Id {
    let queue = darwin::dispatch::queue::create_serial("com.apple.Metal.MTLSharedEventListener");
    // SAFETY: a listener straight from `+alloc`.
    unsafe { object::init(receiver, Listener { queue }) };
    receiver
}

unsafe extern "C" fn listener_init_with_queue(receiver: Id, _sel: Sel, queue: u64) -> Id {
    darwin::dispatch::object::retain(queue);
    // SAFETY: a listener straight from `+alloc`.
    unsafe { object::init(receiver, Listener { queue }) };
    receiver
}

unsafe extern "C" fn listener_queue(receiver: Id, _sel: Sel) -> u64 {
    // SAFETY: a listener of this crate's.
    unsafe { object::body::<Listener>(receiver) }.queue
}

unsafe extern "C" fn listener_dealloc(receiver: Id, _sel: Sel) {
    // SAFETY: a listener `-init` established, holding one queue reference.
    unsafe { darwin::dispatch::object::release(object::body::<Listener>(receiver).queue) };
    objc::arc::finalize(receiver);
}

/// A block waiting for an event to reach a value. Addresses rather than
/// `Id`s, which are not `Send`; one reference to each moves with the entry.
struct Notification {
    semaphore: vk::Semaphore,
    value: u64,
    event: usize,
    block: usize,
    queue: u64,
}

struct Watch {
    gpu: &'static Gpu,
    /// Raised by every registration, so the worker's wait returns and picks
    /// up the new entry.
    wake: vk::Semaphore,
    woken: u64,
    pending: Vec<Notification>,
}

static WATCH: Mutex<Option<Watch>> = Mutex::new(None);
static WORKER: Once = Once::new();

/// `-notifyListener:atValue:block:`.
unsafe extern "C" fn notify_listener(receiver: Id, _sel: Sel, listener: Id, value: u64, block: Id) {
    if listener.is_null() || block.is_null() {
        return;
    }
    // SAFETY: an event of this crate's, and the guest passed a block.
    let event = unsafe { object::body::<Event>(receiver) };
    let block = unsafe { objc::shorthand::objc_retainBlock(block) };
    if block.is_null() {
        return;
    }
    // SAFETY: a listener of this crate's.
    let queue = unsafe { object::body::<Listener>(listener) }.queue;
    darwin::dispatch::object::retain(queue);
    let notification = Notification {
        semaphore: event.semaphore,
        value,
        event: objc::arc::retain(receiver) as usize,
        block: block as usize,
        queue,
    };
    let mut watch = WATCH.lock().expect("event notifications");
    let watch = match &mut *watch {
        Some(watch) => watch,
        slot => {
            let mut kind = vk::SemaphoreTypeCreateInfo::default()
                .semaphore_type(vk::SemaphoreType::TIMELINE)
                .initial_value(0);
            let info = vk::SemaphoreCreateInfo::default().push_next(&mut kind);
            // SAFETY: the event exists, so the device has timeline semaphores.
            let Ok(wake) = (unsafe { event.gpu.device.create_semaphore(&info, None) }) else {
                gpu::trace("vkCreateSemaphore for MTLSharedEvent notifications failed");
                // SAFETY: the references this entry would have owned.
                unsafe { deliver_never(notification) };
                return;
            };
            slot.insert(Watch {
                gpu: event.gpu,
                wake,
                woken: 0,
                pending: Vec::new(),
            })
        }
    };
    if !std::ptr::eq(watch.gpu, event.gpu) {
        // FIXME (ACCURACY): one worker watches one device; a second device's
        // events never notify.
        gpu::trace("MTLSharedEvent notifications are watched on one device only");
        // SAFETY: as above.
        unsafe { deliver_never(notification) };
        return;
    }
    watch.pending.push(notification);
    watch.woken += 1;
    let info = vk::SemaphoreSignalInfo::default()
        .semaphore(watch.wake)
        .value(watch.woken);
    // SAFETY: a timeline semaphore of this device's, raised under the lock
    // so the values arrive in order.
    if let Err(error) = unsafe { watch.gpu.device.signal_semaphore(&info) } {
        gpu::trace(format!("vkSignalSemaphore failed with {error:?}"));
    }
    WORKER.call_once(|| {
        // Detached deliberately, like the completion worker.
        std::thread::Builder::new()
            .name("metal:events".to_string())
            .spawn(watch_events)
            .expect("a thread for MTLSharedEvent notifications");
    });
}

/// Drop a notification which will never be delivered.
///
/// # Safety
///
/// The entry carries one reference to its event, block and queue.
unsafe fn deliver_never(notification: Notification) {
    objc::block::release(notification.block as Id);
    objc::arc::release(notification.event as Id);
    // SAFETY: the caller's reference.
    unsafe { darwin::dispatch::object::release(notification.queue) };
}

fn watch_events() {
    loop {
        let (gpu, semaphores, values) = {
            let watch = WATCH.lock().expect("event notifications");
            let watch = watch.as_ref().expect("a notification was registered");
            let mut semaphores = vec![watch.wake];
            let mut values = vec![watch.woken + 1];
            for notification in &watch.pending {
                semaphores.push(notification.semaphore);
                values.push(notification.value);
            }
            (watch.gpu, semaphores, values)
        };
        let info = vk::SemaphoreWaitInfo::default()
            .flags(vk::SemaphoreWaitFlags::ANY)
            .semaphores(&semaphores)
            .values(&values);
        // SAFETY: timeline semaphores of this device's, each kept alive by a
        // pending entry's event reference or by the watch itself.
        if let Err(error) = unsafe { gpu.device.wait_semaphores(&info, u64::MAX) } {
            gpu::trace(format!(
                "vkWaitSemaphores for MTLSharedEvent notifications failed with {error:?}"
            ));
            return;
        }
        let reached = {
            let mut watch = WATCH.lock().expect("event notifications");
            let watch = watch.as_mut().expect("a notification was registered");
            let (reached, waiting) =
                std::mem::take(&mut watch.pending)
                    .into_iter()
                    .partition(|notification| {
                        // SAFETY: as above.
                        unsafe {
                            gpu.device
                                .get_semaphore_counter_value(notification.semaphore)
                        }
                        .is_ok_and(|current| current >= notification.value)
                    });
            watch.pending = waiting;
            reached
        };
        for notification in reached {
            post(notification);
        }
    }
}

struct Delivery {
    block: usize,
    event: usize,
    value: u64,
}

/// Run a reached notification's block on its listener's queue.
fn post(notification: Notification) {
    let context = Box::into_raw(Box::new(Delivery {
        block: notification.block,
        event: notification.event,
        value: notification.value,
    }));
    // SAFETY: a queue the entry holds a reference to, and `deliver` takes
    // back the box it is handed.
    unsafe {
        darwin::dispatch::queue::async_f(
            notification.queue,
            context as u64,
            deliver as *const () as u64,
        );
        darwin::dispatch::object::release(notification.queue);
    }
}

unsafe extern "C" fn deliver(context: u64) {
    // SAFETY: the box `post` leaked for this call.
    let delivery = unsafe { Box::from_raw(context as *mut Delivery) };
    let block = delivery.block as Id;
    let event = delivery.event as Id;
    // SAFETY: a block this module copied, alive until released below.
    if let Some(invoke) = unsafe { objc::block::invoke(block) } {
        // SAFETY: `MTLSharedEventNotificationBlock` is
        // `void (^)(id<MTLSharedEvent>, uint64_t)`.
        unsafe {
            let call: unsafe extern "C" fn(Id, Id, u64) = std::mem::transmute(invoke);
            call(block, event, delivery.value);
        }
    }
    objc::block::release(block);
    objc::arc::release(event);
}

pub(super) fn install() {
    let event = object::register::<Event>(c"RosemaryMTLEvent");
    let shared = object::register::<Event>(c"RosemaryMTLSharedEvent");
    for class in [event, shared] {
        methods!(class, {
            "device" => event_device, c"@16@0:8";
            "dealloc" => event_dealloc, c"v16@0:8";
        });
    }
    methods!(shared, {
        "signaledValue" => signaled_value, c"Q16@0:8";
        "setSignaledValue:" => set_signaled_value, c"v24@0:8Q16";
        "waitUntilSignaledValue:timeoutMS:" => wait_until_signaled, c"B32@0:8Q16Q24";
        "notifyListener:atValue:block:" => notify_listener, c"v40@0:8@16Q24@?32";
        "newSharedEventHandle" => new_shared_event_handle, c"@16@0:8";
    });
    let listener = object::register::<Listener>(c"MTLSharedEventListener");
    methods!(listener, {
        "init" => listener_init, c"@16@0:8";
        "initWithDispatchQueue:" => listener_init_with_queue, c"@24@0:8@16";
        "dispatchQueue" => listener_queue, c"@16@0:8";
        "dealloc" => listener_dealloc, c"v16@0:8";
    });
}
