//! NVIDIA GPU metrics, behind the optional `gpu` feature.
//!
//! Spec: `docs/specs/SPEC-006-search-monitor.md` §5.6.
//!
//! Two rules (04 §7):
//!
//! - **Degrade to `None`, never to an error.** No NVIDIA card, no driver, or a
//!   build without the feature all mean the same thing to the UI: hide the GPU
//!   section. A red "NVML failed" banner on a machine that simply has no NVIDIA
//!   GPU is noise.
//! - **`Nvml::init()` exactly once, and cache the failure too.** Retrying init on
//!   every 3s sample costs a driver handshake per tick on machines where it will
//!   never succeed ([SPEC-006 §9 #14]).
//!
//! With the feature off the whole thing compiles down to `sample() -> None`, which
//! is why the default build needs no CUDA toolchain at all.

use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GpuMetrics {
    pub name: String,
    /// Percent of the sample period with at least one kernel running.
    pub usage: u32,
    /// Framebuffer memory, bytes.
    pub memory_total: u64,
    pub memory_used: u64,
    /// Die temperature in °C, absent if the sensor is unreadable.
    pub temperature_c: Option<u32>,
}

#[cfg(feature = "gpu")]
mod imp {
    use std::sync::OnceLock;

    use nvml_wrapper::{Nvml, enum_wrappers::device::TemperatureSensor, error::NvmlError};

    use super::GpuMetrics;

    /// The `Result` is cached, not just the `Ok`: a failed init is a permanent
    /// fact about this machine, and re-attempting it every sample is the bug this
    /// `OnceLock` exists to prevent.
    static NVML: OnceLock<Result<Nvml, NvmlError>> = OnceLock::new();

    pub fn sample() -> Option<GpuMetrics> {
        let nvml = NVML.get_or_init(Nvml::init).as_ref().ok()?;
        // Device 0 only: a multi-GPU box is out of scope for this phase (§10), and
        // a panel that shows one card is better than one that shows none.
        let device = nvml.device_by_index(0).ok()?;
        let memory = device.memory_info().ok()?;
        Some(GpuMetrics {
            name: device.name().ok()?,
            usage: device.utilization_rates().map(|u| u.gpu).unwrap_or(0),
            memory_total: memory.total,
            memory_used: memory.used,
            temperature_c: device.temperature(TemperatureSensor::Gpu).ok(),
        })
    }
}

#[cfg(not(feature = "gpu"))]
mod imp {
    use super::GpuMetrics;

    pub fn sample() -> Option<GpuMetrics> {
        None
    }
}

/// Current GPU metrics, or `None` when there is nothing to report.
pub fn sample() -> Option<GpuMetrics> {
    imp::sample()
}

#[cfg(test)]
mod tests {
    /// Without the feature the answer is unconditionally `None` — the property
    /// the default build depends on.
    #[test]
    #[cfg(not(feature = "gpu"))]
    fn is_absent_without_the_feature() {
        assert!(super::sample().is_none());
    }

    /// With the feature on, the only portable claim is that it does not panic:
    /// CI has no NVIDIA card, so `None` is the expected answer there and a real
    /// reading is the expected answer on a workstation.
    #[test]
    #[cfg(feature = "gpu")]
    fn never_panics_when_no_driver_is_present() {
        let first = super::sample();
        let second = super::sample();
        assert_eq!(first.is_some(), second.is_some(), "init must be cached");
    }
}
