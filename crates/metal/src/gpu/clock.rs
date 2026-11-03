//! The device clock, read together with the host's.

use ash::{Entry, Instance, vk};

use super::Gpu;

/// `VK_KHR_calibrated_timestamps`, or its EXT predecessor, where the device
/// can sample its own clock alongside `CLOCK_MONOTONIC`.
pub(super) struct Calibration {
    get_calibrated_timestamps: vk::PFN_vkGetCalibratedTimestampsKHR,
}

impl Calibration {
    /// The extension to enable, if the device pairs the clocks Metal needs.
    pub(super) fn extension(
        entry: &Entry,
        instance: &Instance,
        physical: vk::PhysicalDevice,
        has: impl Fn(&std::ffi::CStr) -> bool,
    ) -> Option<&'static std::ffi::CStr> {
        let domains = if has(ash::khr::calibrated_timestamps::NAME) {
            let api = ash::khr::calibrated_timestamps::Instance::new(entry, instance);
            // SAFETY: the extension is advertised by this physical device.
            let domains = unsafe { api.get_physical_device_calibrateable_time_domains(physical) };
            (ash::khr::calibrated_timestamps::NAME, domains)
        } else if has(ash::ext::calibrated_timestamps::NAME) {
            let api = ash::ext::calibrated_timestamps::Instance::new(entry, instance);
            // SAFETY: the extension is advertised by this physical device.
            let domains = unsafe { api.get_physical_device_calibrateable_time_domains(physical) };
            (ash::ext::calibrated_timestamps::NAME, domains)
        } else {
            return None;
        };
        let (name, Ok(domains)) = domains else {
            return None;
        };
        [
            vk::TimeDomainKHR::DEVICE,
            vk::TimeDomainKHR::CLOCK_MONOTONIC,
        ]
        .iter()
        .all(|domain| domains.contains(domain))
        .then_some(name)
    }

    pub(super) fn new(
        instance: &Instance,
        device: &ash::Device,
        extension: &std::ffi::CStr,
    ) -> Self {
        let get_calibrated_timestamps = if extension == ash::khr::calibrated_timestamps::NAME {
            ash::khr::calibrated_timestamps::Device::new(instance, device)
                .fp()
                .get_calibrated_timestamps_khr
        } else {
            ash::ext::calibrated_timestamps::Device::new(instance, device)
                .fp()
                .get_calibrated_timestamps_ext
        };
        Self {
            get_calibrated_timestamps,
        }
    }
}

impl Gpu {
    /// `CLOCK_MONOTONIC` in nanoseconds - the `mach_absolute_time` domain -
    /// and the device's timestamp counter, sampled at the same moment.
    pub fn sample_timestamps(&self) -> Option<(u64, u64)> {
        let calibration = self.calibration.as_ref()?;
        let infos = [
            vk::TimeDomainKHR::CLOCK_MONOTONIC,
            vk::TimeDomainKHR::DEVICE,
        ]
        .map(|domain| vk::CalibratedTimestampInfoKHR::default().time_domain(domain));
        let mut timestamps = [0u64; 2];
        let mut max_deviation = 0;
        // SAFETY: both domains were checked as calibrateable when the
        // extension was enabled, and the arrays hold one entry per info.
        let result = unsafe {
            (calibration.get_calibrated_timestamps)(
                self.device.handle(),
                infos.len() as u32,
                infos.as_ptr(),
                timestamps.as_mut_ptr(),
                &mut max_deviation,
            )
        };
        (result == vk::Result::SUCCESS).then_some((timestamps[0], timestamps[1]))
    }
}
