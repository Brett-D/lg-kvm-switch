//! LG's DDC/CI input-select protocol.

/// 7-bit I2C address of a monitor's DDC/CI endpoint.
pub const DDC_ADDR: u8 = 0x37;
/// LG monitors only honor input changes sent from this host address. The
/// standard DDC/CI host address is 0x51, which is all Windows' API can send.
pub const LG_SOURCE_ADDR: u8 = 0x50;
/// LG's proprietary input-select VCP code.
pub const LG_INPUT_VCP: u8 = 0xF4;

/// Named inputs: (config name, LG input code, label).
pub const INPUTS: &[(&str, u16, &str)] = &[
    ("dp", 0xD0, "DisplayPort"),
    ("usbc", 0xD1, "USB-C"),
    ("hdmi1", 0x90, "HDMI 1"),
    ("hdmi2", 0x91, "HDMI 2"),
];

/// Parses an input name (`dp`, `usbc`, `hdmi1`, `hdmi2`) or a raw LG input
/// code such as `0xD0` or `208`.
pub fn parse_input(text: &str) -> Result<u16, String> {
    let key = text
        .trim()
        .to_ascii_lowercase()
        .replace(['-', '_', ' '], "");
    let named = match key.as_str() {
        "dp" | "displayport" | "dp1" => Some(0xD0),
        "usbc" | "typec" | "thunderbolt" | "tb" => Some(0xD1),
        "hdmi" | "hdmi1" => Some(0x90),
        "hdmi2" => Some(0x91),
        _ => None,
    };
    if let Some(value) = named {
        return Ok(value);
    }
    let parsed = match key.strip_prefix("0x") {
        Some(hex) => u16::from_str_radix(hex, 16),
        None => key.parse::<u16>(),
    };
    parsed.map_err(|_| {
        format!("unknown input '{text}' (use dp, usbc, hdmi1, hdmi2, or a code such as 0xD0)")
    })
}

/// Human-readable name of an LG input code.
pub fn input_name(value: u16) -> String {
    INPUTS
        .iter()
        .find(|(_, code, _)| *code == value)
        .map(|(_, _, label)| (*label).to_string())
        .unwrap_or_else(|| format!("input 0x{value:02X}"))
}

/// Builds a DDC/CI "Set VCP Feature" packet, starting with the source address.
/// The checksum covers the destination address (0x6E) and every byte before it.
pub fn set_vcp_packet(source: u8, vcp: u8, value: u16) -> [u8; 7] {
    let [hi, lo] = value.to_be_bytes();
    let mut packet = [source, 0x84, 0x03, vcp, hi, lo, 0];
    packet[6] = packet[..6].iter().fold(DDC_ADDR << 1, |sum, b| sum ^ b);
    packet
}

/// The packet that makes an LG monitor switch to `input`.
pub fn input_packet(input: u16) -> [u8; 7] {
    set_vcp_packet(LG_SOURCE_ADDR, LG_INPUT_VCP, input)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_packets_match_hardware_verified_bytes() {
        assert_eq!(
            input_packet(0xD1),
            [0x50, 0x84, 0x03, 0xF4, 0x00, 0xD1, 0x9C]
        );
        assert_eq!(
            input_packet(0xD0),
            [0x50, 0x84, 0x03, 0xF4, 0x00, 0xD0, 0x9D]
        );
    }

    #[test]
    fn parses_names_aliases_and_codes() {
        assert_eq!(parse_input("dp"), Ok(0xD0));
        assert_eq!(parse_input("USB-C"), Ok(0xD1));
        assert_eq!(parse_input("hdmi2"), Ok(0x91));
        assert_eq!(parse_input("0x1D1"), Ok(0x1D1));
        assert_eq!(parse_input("208"), Ok(0xD0));
        assert!(parse_input("vga").is_err());
    }

    #[test]
    fn names_unknown_codes() {
        assert_eq!(input_name(0xD1), "USB-C");
        assert_eq!(input_name(0xE0), "input 0xE0");
    }
}
