//! Lightweight Linux hardware probe (system RAM and DRM/GPU presence).

use std::fs;
use std::path::Path;

#[derive(Debug, Clone)]
pub struct HardwareInfo {
    pub total_ram_bytes: u64,
    pub available_ram_bytes: u64,
    pub has_dri_gpu: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FitStatus {
    Comfortable,
    Tight,
    TooLarge,
}

impl FitStatus {
    pub fn label(self) -> String {
        match self {
            Self::Comfortable => crate::i18n::gettext("Fits comfortably"),
            Self::Tight => crate::i18n::gettext("Tight fit"),
            Self::TooLarge => crate::i18n::gettext("Exceeds RAM"),
        }
    }
}

/// Probe host RAM via `/proc/meminfo` and check `/dev/dri` for GPU acceleration support.
pub fn probe_hardware() -> HardwareInfo {
    let (total_ram_bytes, available_ram_bytes) = read_meminfo().unwrap_or((0, 0));
    let has_dri_gpu = Path::new("/dev/dri").exists();

    HardwareInfo {
        total_ram_bytes,
        available_ram_bytes,
        has_dri_gpu,
    }
}

/// Estimate whether a GGUF of `model_bytes` will fit in `total_ram_bytes` (allowing ~1.2x KV-cache overhead).
pub fn assess_model_fit(model_bytes: u64, total_ram_bytes: u64) -> FitStatus {
    if total_ram_bytes == 0 || model_bytes == 0 {
        return FitStatus::Tight;
    }
    let estimated_needed = (model_bytes as f64) * 1.25;
    let total = total_ram_bytes as f64;
    if estimated_needed <= total * 0.70 {
        FitStatus::Comfortable
    } else if estimated_needed <= total * 0.95 {
        FitStatus::Tight
    } else {
        FitStatus::TooLarge
    }
}

fn read_meminfo() -> Option<(u64, u64)> {
    let content = fs::read_to_string("/proc/meminfo").ok()?;
    let mut total_kb = None;
    let mut avail_kb = None;

    for line in content.lines() {
        if let Some(rest) = line.strip_prefix("MemTotal:") {
            total_kb = parse_kb(rest);
        } else if let Some(rest) = line.strip_prefix("MemAvailable:") {
            avail_kb = parse_kb(rest);
        }
        if total_kb.is_some() && avail_kb.is_some() {
            break;
        }
    }

    Some((
        total_kb.unwrap_or(0) * 1024,
        avail_kb.unwrap_or(0) * 1024,
    ))
}

fn parse_kb(value: &str) -> Option<u64> {
    value
        .split_whitespace()
        .next()
        .and_then(|num| num.parse::<u64>().ok())
}
