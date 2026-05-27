//! Region presets for MeshCore radio configuration.
//!
//! Shared between the setup wizard (`setup.rs`) and the CLI `node set-radio`
//! command (`main.rs`).

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
