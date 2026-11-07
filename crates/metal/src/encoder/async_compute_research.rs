//! Measurement only: whether a compute dispatch's bound resources were ever
//! touched by render or blit work, which is the necessary condition for it
//! to be safe on a Vulkan queue of its own, concurrent with the graphics
//! queue.
//!
//! Nothing here changes what runs or when. Every dispatch still goes through
//! the single queue and the full barrier `hazards` already gives it. This
//! only counts, via [`crate::stats::Counter::AsyncComputeEligible`] and
//! [`crate::stats::Counter::AsyncComputeIneligible`], how often a dispatch
//! would have qualified for real cross-queue work - which this runtime does
//! not have the resource hazard tracking to do safely yet.
//!
//! A resource counts as touched the moment any render or blit encoder binds
//! it, and never again counts as untouched: the question this answers is
//! "has this resource's whole lifetime been compute-only so far", not "is it
//! free of conflict with what is pending right now". That is deliberately
//! the more conservative question - a resource this marks ineligible may
//! still really be safe, but nothing this marks eligible can be wrong.
//!
//! A texture view defers to the texture it sees into, as [`tracked_image`]
//! does; a texture placed in an explicit heap, or buffer, may alias memory
//! this cannot see all of, so it is always ineligible, regardless of its
//! flag.

use super::*;

/// Record that render or blit work bound `buffer`.
pub(super) fn mark_buffer_touched(buffer: Id) {
    if buffer.is_null() {
        return;
    }
    // SAFETY: only a buffer of this crate's is ever bound.
    let body = unsafe { object::body::<Buffer>(buffer) };
    body.touched_outside_compute.store(true, Ordering::Relaxed);
}

/// Record that render or blit work bound `texture`, or the texture a view
/// of it sees into.
pub(super) fn mark_texture_touched(texture: Id) {
    let mut current = texture;
    while !current.is_null() {
        // SAFETY: only a texture of this crate's is ever bound.
        let body = unsafe { object::body::<Texture>(current) };
        if body.parent.is_null() {
            body.touched_outside_compute.store(true, Ordering::Relaxed);
            return;
        }
        current = body.parent;
    }
}

/// Whether `buffer`'s whole lifetime so far has been compute-only. `true`
/// for a null buffer: a dispatch which bound nothing there has nothing to
/// disqualify it.
fn buffer_is_compute_only(buffer: Id) -> bool {
    if buffer.is_null() {
        return true;
    }
    // SAFETY: only a buffer of this crate's is ever bound.
    let body = unsafe { object::body::<Buffer>(buffer) };
    // A heap-placed buffer may share memory with a resource this cannot see.
    body.inner.placement.is_none() && !body.touched_outside_compute.load(Ordering::Relaxed)
}

/// Whether `texture`, or the texture a view of it sees into, has a whole
/// lifetime so far that has been compute-only. `true` for a null texture.
fn texture_is_compute_only(texture: Id) -> bool {
    let mut current = texture;
    while !current.is_null() {
        // SAFETY: only a texture of this crate's is ever bound.
        let body = unsafe { object::body::<Texture>(current) };
        if body.parent.is_null() {
            // A heap-placed texture or a texture buffer may alias memory
            // this cannot see all of.
            return body.image.placement.is_none()
                && !body.is_texture_buffer()
                && !body.touched_outside_compute.load(Ordering::Relaxed);
        }
        current = body.parent;
    }
    true
}

/// How many dispatches [`Encoder::count_async_compute_eligibility`] found
/// each way, for a test to read without the `ROSEMARY_METAL_STATS` counters
/// `tick()` resets on its own interval.
#[cfg(test)]
pub(crate) static TEST_ASYNC_COMPUTE_ELIGIBLE: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);
#[cfg(test)]
pub(crate) static TEST_ASYNC_COMPUTE_INELIGIBLE: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

impl Encoder {
    /// Count whether every resource this compute encoder currently has bound
    /// was, for its whole lifetime, never touched outside compute work -
    /// research for [`async_compute_research`](self), not acted on.
    pub(super) fn count_async_compute_eligibility(&self) {
        #[cfg(not(test))]
        if !stats::enabled() {
            return;
        }
        let eligible = !self.writes_elsewhere
            && self.buffers[COMPUTE]
                .iter()
                .all(|binding| buffer_is_compute_only(binding.object))
            && self.textures[COMPUTE]
                .iter()
                .all(|texture| texture_is_compute_only(*texture));
        stats::count(if eligible {
            Counter::AsyncComputeEligible
        } else {
            Counter::AsyncComputeIneligible
        });
        #[cfg(test)]
        (if eligible {
            &TEST_ASYNC_COMPUTE_ELIGIBLE
        } else {
            &TEST_ASYNC_COMPUTE_INELIGIBLE
        })
        .fetch_add(1, Ordering::Relaxed);
    }
}
