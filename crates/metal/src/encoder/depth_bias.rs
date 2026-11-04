use super::*;

/// The values of `-setDepthBias:slopeScale:clamp:`.
#[derive(Clone, Copy, Debug)]
pub(super) struct DepthBias {
    pub bias: f32,
    pub slope_scale: f32,
    pub clamp: f32,
}

impl DepthBias {
    /// Metal's default, which every pass starts with.
    pub const NONE: Self = Self {
        bias: 0.0,
        slope_scale: 0.0,
        clamp: 0.0,
    };

    // Bitwise, so a -0.0 or NaN the guest sets is still recorded as given.
    fn bits(self) -> [u32; 3] {
        [
            self.bias.to_bits(),
            self.slope_scale.to_bits(),
            self.clamp.to_bits(),
        ]
    }
}

/// The depth bias an encoder's command buffer holds, which is recorded only
/// through [`RecordedDepthBias::record`] so the two cannot disagree. Binding a
/// pipeline does not reset it because every render pipeline keeps depth bias
/// dynamic (`pipeline/render.rs`).
#[derive(Default)]
pub(super) struct RecordedDepthBias(Option<[u32; 3]>);

impl RecordedDepthBias {
    /// Whether `bias` differs from what the command buffer holds, taking it
    /// as held if so.
    fn update(&mut self, bias: DepthBias) -> bool {
        let bits = Some(bias.bits());
        let changed = self.0 != bits;
        self.0 = bits;
        changed
    }

    /// Record `bias` into `command_buffer` unless it already holds it.
    pub fn record(
        &mut self,
        device: &ash::Device,
        command_buffer: vk::CommandBuffer,
        bias: DepthBias,
    ) {
        if !self.update(bias) {
            return;
        }
        // SAFETY: recording into a command buffer inside a render pass.
        #[allow(clippy::disallowed_methods)]
        unsafe {
            device.cmd_set_depth_bias(command_buffer, bias.bias, bias.clamp, bias.slope_scale)
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NONZERO: DepthBias = DepthBias {
        bias: 4.0,
        slope_scale: 1.0,
        clamp: 0.0,
    };

    #[test]
    fn records_the_first_bias() {
        assert!(RecordedDepthBias::default().update(DepthBias::NONE));
    }

    #[test]
    fn skips_a_repeated_bias() {
        let mut recorded = RecordedDepthBias::default();
        assert!(recorded.update(NONZERO));
        assert!(!recorded.update(NONZERO));
        assert!(recorded.update(DepthBias::NONE));
        assert!(!recorded.update(DepthBias::NONE));
    }

    #[test]
    fn distinguishes_negative_zero() {
        let mut recorded = RecordedDepthBias::default();
        recorded.update(DepthBias::NONE);
        assert!(recorded.update(DepthBias {
            bias: -0.0,
            ..DepthBias::NONE
        }));
    }
}
