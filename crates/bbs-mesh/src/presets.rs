//! Region presets and radio-parameter resolution for MeshCore.
//!
//! Used by [`crate::transport`] to resolve the operator's `[plugins.mesh.radio]`
//! config into concrete values on every connect, so the transport can diff them
//! against what the device reports and push a correction when they've drifted.
//!
//! The top-level `supply-drop-bbs` binary crate (CLI `node set-radio`, the setup
//! wizard) has its own copy of this preset table in `src/mesh_presets.rs` — it
//! cannot depend on `bbs-mesh` directly there without a `transport-mesh`-feature
//! cycle, since `mesh_presets.rs` must compile even when that feature is off.
//! **Keep the two tables in sync when a preset changes.**

use crate::config::RadioConfig;

/// A named radio region preset with default LoRa parameters.
pub struct RegionPreset {
    pub name: &'static str,
    /// Carrier frequency in Hz.
    pub frequency_hz: u64,
    /// Channel bandwidth in Hz.
    pub bandwidth_hz: u32,
    pub spreading_factor: u8,
    /// Coding rate denominator (5 = 4/5, 8 = 4/8).
    pub coding_rate: u8,
    /// Transmit power in dBm.
    pub tx_power_dbm: i32,
}

pub const REGION_PRESETS: &[RegionPreset] = &[
    RegionPreset {
        name: "Australia",
        frequency_hz: 915_800_000,
        bandwidth_hz: 250_000,
        spreading_factor: 10,
        coding_rate: 5,
        tx_power_dbm: 20,
    },
    RegionPreset {
        name: "Australia (Narrow)",
        frequency_hz: 916_575_000,
        bandwidth_hz: 62_500,
        spreading_factor: 7,
        coding_rate: 5,
        tx_power_dbm: 20,
    },
    RegionPreset {
        name: "Australia SA, WA, QLD",
        frequency_hz: 923_125_000,
        bandwidth_hz: 62_500,
        spreading_factor: 8,
        coding_rate: 5,
        tx_power_dbm: 20,
    },
    RegionPreset {
        name: "Czech Republic",
        frequency_hz: 869_432_000,
        bandwidth_hz: 62_500,
        spreading_factor: 7,
        coding_rate: 5,
        tx_power_dbm: 14,
    },
    RegionPreset {
        name: "EU 433MHz",
        frequency_hz: 433_650_000,
        bandwidth_hz: 250_000,
        spreading_factor: 11,
        coding_rate: 5,
        tx_power_dbm: 20,
    },
    RegionPreset {
        name: "EU/UK (Long Range)",
        frequency_hz: 869_525_000,
        bandwidth_hz: 250_000,
        spreading_factor: 11,
        coding_rate: 5,
        tx_power_dbm: 14,
    },
    RegionPreset {
        name: "EU/UK (Medium Range)",
        frequency_hz: 869_525_000,
        bandwidth_hz: 250_000,
        spreading_factor: 10,
        coding_rate: 5,
        tx_power_dbm: 14,
    },
    RegionPreset {
        name: "EU/UK (Narrow)",
        frequency_hz: 869_618_000,
        bandwidth_hz: 62_500,
        spreading_factor: 8,
        coding_rate: 5,
        tx_power_dbm: 14,
    },
    RegionPreset {
        name: "New Zealand",
        frequency_hz: 917_375_000,
        bandwidth_hz: 250_000,
        spreading_factor: 11,
        coding_rate: 5,
        tx_power_dbm: 20,
    },
    RegionPreset {
        name: "New Zealand (Narrow)",
        frequency_hz: 917_375_000,
        bandwidth_hz: 62_500,
        spreading_factor: 7,
        coding_rate: 5,
        tx_power_dbm: 20,
    },
    RegionPreset {
        name: "Portugal 433",
        frequency_hz: 433_375_000,
        bandwidth_hz: 62_500,
        spreading_factor: 9,
        coding_rate: 5,
        tx_power_dbm: 20,
    },
    RegionPreset {
        name: "Portugal 869",
        frequency_hz: 869_618_000,
        bandwidth_hz: 62_500,
        spreading_factor: 7,
        coding_rate: 5,
        tx_power_dbm: 14,
    },
    RegionPreset {
        name: "Switzerland",
        frequency_hz: 869_618_000,
        bandwidth_hz: 62_500,
        spreading_factor: 8,
        coding_rate: 5,
        tx_power_dbm: 14,
    },
    RegionPreset {
        name: "USA Arizona",
        frequency_hz: 908_205_000,
        bandwidth_hz: 62_500,
        spreading_factor: 10,
        coding_rate: 5,
        tx_power_dbm: 20,
    },
    RegionPreset {
        name: "USA/Canada",
        frequency_hz: 910_525_000,
        bandwidth_hz: 62_500,
        spreading_factor: 7,
        coding_rate: 5,
        tx_power_dbm: 20,
    },
    RegionPreset {
        name: "Vietnam",
        frequency_hz: 920_250_000,
        bandwidth_hz: 250_000,
        spreading_factor: 11,
        coding_rate: 5,
        tx_power_dbm: 20,
    },
    RegionPreset {
        name: "Off-Grid 433",
        frequency_hz: 433_000_000,
        bandwidth_hz: 250_000,
        spreading_factor: 11,
        coding_rate: 8,
        tx_power_dbm: 20,
    },
    RegionPreset {
        name: "Off-Grid 869",
        frequency_hz: 869_000_000,
        bandwidth_hz: 250_000,
        spreading_factor: 11,
        coding_rate: 8,
        tx_power_dbm: 14,
    },
    RegionPreset {
        name: "Off-Grid 918",
        frequency_hz: 918_000_000,
        bandwidth_hz: 250_000,
        spreading_factor: 11,
        coding_rate: 8,
        tx_power_dbm: 20,
    },
];

/// Concrete radio parameters resolved from a preset and/or explicit overrides.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolvedRadio {
    pub frequency_hz: u32,
    pub bandwidth_hz: u32,
    pub spreading_factor: u8,
    pub coding_rate: u8,
    pub tx_power_dbm: i32,
}

/// Resolve final radio parameters by layering: preset → config fields → explicit
/// overrides. Returns `Err` if any of the five parameters is left unset after
/// layering (e.g. no preset and an incomplete `[plugins.mesh.radio]`).
pub fn resolve_radio(
    cfg_radio: Option<&RadioConfig>,
    preset_override: Option<&str>,
    freq_override: Option<u64>,
    bw_override: Option<u32>,
    sf_override: Option<u8>,
    cr_override: Option<u8>,
    pwr_override: Option<i32>,
) -> Result<ResolvedRadio, String> {
    let mut freq: Option<u32> = None;
    let mut bw: Option<u32> = None;
    let mut sf: Option<u8> = None;
    let mut cr: Option<u8> = None;
    let mut pwr: Option<i32> = None;

    // 1. Named preset.
    let preset_name = preset_override.or_else(|| cfg_radio.and_then(|r| r.preset.as_deref()));
    if let Some(name) = preset_name {
        let p = REGION_PRESETS
            .iter()
            .find(|p| p.name.eq_ignore_ascii_case(name))
            .ok_or_else(|| format!("unknown preset '{name}'"))?;
        freq = Some(p.frequency_hz as u32);
        bw = Some(p.bandwidth_hz);
        sf = Some(p.spreading_factor);
        cr = Some(p.coding_rate);
        pwr = Some(p.tx_power_dbm);
    }

    // 2. Individual config.toml fields overlay preset.
    if let Some(r) = cfg_radio {
        if let Some(v) = r.frequency_hz {
            freq = Some(v as u32);
        }
        if let Some(v) = r.bandwidth_hz {
            bw = Some(v);
        }
        if let Some(v) = r.spreading_factor {
            sf = Some(v);
        }
        if let Some(v) = r.coding_rate {
            cr = Some(v);
        }
        if let Some(v) = r.tx_power_dbm {
            pwr = Some(v);
        }
    }

    // 3. Explicit overrides take highest precedence.
    if let Some(v) = freq_override {
        freq = Some(v as u32);
    }
    if let Some(v) = bw_override {
        bw = Some(v);
    }
    if let Some(v) = sf_override {
        sf = Some(v);
    }
    if let Some(v) = cr_override {
        cr = Some(v);
    }
    if let Some(v) = pwr_override {
        pwr = Some(v);
    }

    Ok(ResolvedRadio {
        frequency_hz: freq
            .ok_or("frequency_hz not set — specify a preset or frequency_hz in config")?,
        bandwidth_hz: bw.ok_or("bandwidth_hz not set")?,
        spreading_factor: sf.ok_or("spreading_factor not set")?,
        coding_rate: cr.ok_or("coding_rate not set")?,
        tx_power_dbm: pwr.ok_or("tx_power_dbm not set")?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_from_preset_only() {
        let r = resolve_radio(None, Some("USA/Canada"), None, None, None, None, None).unwrap();
        assert_eq!(r.frequency_hz, 910_525_000);
        assert_eq!(r.bandwidth_hz, 62_500);
        assert_eq!(r.spreading_factor, 7);
        assert_eq!(r.coding_rate, 5);
        assert_eq!(r.tx_power_dbm, 20);
    }

    #[test]
    fn preset_lookup_is_case_insensitive() {
        assert!(resolve_radio(None, Some("usa/canada"), None, None, None, None, None).is_ok());
    }

    #[test]
    fn unknown_preset_errors() {
        assert!(resolve_radio(None, Some("Narnia"), None, None, None, None, None).is_err());
    }

    #[test]
    fn config_fields_overlay_preset() {
        let cfg = RadioConfig {
            preset: Some("USA/Canada".to_owned()),
            tx_power_dbm: Some(17),
            ..RadioConfig::default()
        };
        let r = resolve_radio(Some(&cfg), None, None, None, None, None, None).unwrap();
        assert_eq!(r.frequency_hz, 910_525_000); // from preset
        assert_eq!(r.tx_power_dbm, 17); // overlaid
    }

    #[test]
    fn incomplete_config_without_preset_errors() {
        let cfg = RadioConfig {
            frequency_hz: Some(915_000_000),
            ..RadioConfig::default()
        };
        assert!(resolve_radio(Some(&cfg), None, None, None, None, None, None).is_err());
    }

    #[test]
    fn no_config_and_no_preset_errors() {
        assert!(resolve_radio(None, None, None, None, None, None, None).is_err());
    }
}
