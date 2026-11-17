use super::*;

/// Metal's largest viewport array across supported GPU families.
pub(super) const MAX_VIEWPORTS: u32 = 16;

enum Scissors {
    Broadcast(vk::Rect2D),
    Array(Vec<vk::Rect2D>),
}

pub(super) struct ViewportState {
    viewports: Vec<vk::Viewport>,
    scissors: Scissors,
    dirty: bool,
}

impl Default for ViewportState {
    fn default() -> Self {
        Self {
            viewports: Vec::new(),
            scissors: Scissors::Broadcast(vk::Rect2D::default()),
            dirty: true,
        }
    }
}

impl ViewportState {
    pub(super) fn new(area: vk::Rect2D) -> Self {
        Self {
            viewports: vec![vk::Viewport {
                x: 0.0,
                y: area.extent.height as f32,
                width: area.extent.width as f32,
                height: -(area.extent.height as f32),
                min_depth: 0.0,
                max_depth: 1.0,
            }],
            scissors: Scissors::Broadcast(area),
            dirty: true,
        }
    }

    pub(super) fn count(&self) -> u32 {
        self.viewports.len() as u32
    }

    pub(super) fn record(&mut self, device: &ash::Device, command: vk::CommandBuffer) {
        if !self.dirty {
            return;
        }
        let broadcast;
        let scissors = match &self.scissors {
            Scissors::Broadcast(rect) => {
                broadcast = vec![*rect; self.viewports.len()];
                &broadcast
            }
            Scissors::Array(rectangles) => rectangles,
        };
        // Validate at draw time: callers may set either array first.
        if scissors.len() != self.viewports.len() {
            objc::fail!(
                "Metal: scissor count {} does not match viewport count {}",
                scissors.len(),
                self.viewports.len()
            );
        }
        // SAFETY: recording owned arrays whose counts fit the enabled device limit.
        unsafe {
            device.cmd_set_viewport(command, 0, &self.viewports);
            device.cmd_set_scissor(command, 0, scissors);
        }
        self.dirty = false;
    }
}

fn check_count(count: u64, maximum: u32) {
    if count == 0 || count > u64::from(maximum.min(MAX_VIEWPORTS)) {
        objc::fail!(
            "Metal: viewport/scissor count {count} exceeds the supported range 1..={}",
            maximum.min(MAX_VIEWPORTS)
        );
    }
}

pub(super) unsafe extern "C" fn set_viewport(receiver: Id, sel: Sel, viewport: Viewport) {
    // SAFETY: the single value remains readable for this call.
    unsafe { set_viewports(receiver, sel, &viewport, 1) };
}

pub(super) unsafe extern "C" fn set_viewports(
    receiver: Id,
    _sel: Sel,
    viewports: *const Viewport,
    count: u64,
) {
    // SAFETY: an encoder of this crate's.
    let encoder = unsafe { object::body::<Encoder>(receiver) };
    check_count(count, encoder.command().gpu.max_viewports);
    if viewports.is_null() {
        objc::fail!("Metal: setViewports:count: received a null array");
    }
    // SAFETY: Metal requires count readable structures; the count is bounded above.
    let viewports = unsafe { std::slice::from_raw_parts(viewports, count as usize) };
    encoder.viewports.viewports.clear();
    encoder
        .viewports
        .viewports
        .extend(viewports.iter().map(|viewport| {
            // Metal's framebuffer origin is top left and clip-space Y points up.
            vk::Viewport {
                x: viewport.origin_x as f32,
                y: (viewport.origin_y + viewport.height) as f32,
                width: viewport.width as f32,
                height: -(viewport.height as f32),
                min_depth: viewport.znear as f32,
                max_depth: viewport.zfar as f32,
            }
        }));
    encoder.viewports.dirty = true;
}

fn scissor(rect: ScissorRect) -> vk::Rect2D {
    let Ok(x) = i32::try_from(rect.x) else {
        objc::fail!("Metal: scissor x is out of range");
    };
    let Ok(y) = i32::try_from(rect.y) else {
        objc::fail!("Metal: scissor y is out of range");
    };
    let Ok(width) = u32::try_from(rect.width) else {
        objc::fail!("Metal: scissor width is out of range");
    };
    let Ok(height) = u32::try_from(rect.height) else {
        objc::fail!("Metal: scissor height is out of range");
    };
    if rect.x + rect.width > i32::MAX as u64 || rect.y + rect.height > i32::MAX as u64 {
        objc::fail!("Metal: scissor extent is out of range");
    }
    vk::Rect2D {
        offset: vk::Offset2D { x, y },
        extent: vk::Extent2D { width, height },
    }
}

pub(super) unsafe extern "C" fn set_scissor(receiver: Id, _sel: Sel, rect: ScissorRect) {
    // SAFETY: an encoder of this crate's.
    let encoder = unsafe { object::body::<Encoder>(receiver) };
    encoder.viewports.scissors = Scissors::Broadcast(scissor(rect));
    encoder.viewports.dirty = true;
}

pub(super) unsafe extern "C" fn set_scissors(
    receiver: Id,
    _sel: Sel,
    rectangles: *const ScissorRect,
    count: u64,
) {
    // SAFETY: an encoder of this crate's.
    let encoder = unsafe { object::body::<Encoder>(receiver) };
    check_count(count, encoder.command().gpu.max_viewports);
    if rectangles.is_null() {
        objc::fail!("Metal: setScissorRects:count: received a null array");
    }
    // SAFETY: Metal requires count readable structures; the count is bounded above.
    let rectangles = unsafe { std::slice::from_raw_parts(rectangles, count as usize) };
    encoder.viewports.scissors = match rectangles {
        // Metal treats a one-element array like setScissorRect:.
        [rectangle] => Scissors::Broadcast(scissor(*rectangle)),
        _ => Scissors::Array(rectangles.iter().copied().map(scissor).collect()),
    };
    encoder.viewports.dirty = true;
}
