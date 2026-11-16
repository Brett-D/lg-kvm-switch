//! LG's DDC/CI protocol: input select, Picture-by-Picture, and the standard
//! picture and audio controls.

/// 7-bit I2C address of a monitor's DDC/CI endpoint.
pub const DDC_ADDR: u8 = 0x37;
/// LG monitors only honor input changes sent from this host address. The
/// standard DDC/CI host address is 0x51, which is all Windows' API can send.
pub const LG_SOURCE_ADDR: u8 = 0x50;
/// The standard DDC/CI host address, used for everything except input select.
pub const STD_SOURCE_ADDR: u8 = 0x51;
/// The host address a monitor's replies are checksummed against.
const REPLY_DEST_ADDR: u8 = 0x50;
/// LG's proprietary input-select VCP code.
pub const LG_INPUT_VCP: u8 = 0xF4;

pub const VCP_BRIGHTNESS: u8 = 0x10;
pub const VCP_CONTRAST: u8 = 0x12;
pub const VCP_VOLUME: u8 = 0x62;
/// Audio mute: [`MUTE_ON`] or [`MUTE_OFF`].
pub const VCP_MUTE: u8 = 0x8D;
pub const MUTE_ON: u16 = 0x01;
pub const MUTE_OFF: u16 = 0x02;

/// LG's Picture-by-Picture layout code. With PBP on, selecting an input puts
/// it on the left half and moves the previous input to the right.
pub const LG_PBP_VCP: u8 = 0xD7;
pub const PBP_OFF: u16 = 0x01;
/// Left/right 50/50 split.
pub const PBP_SIDE_BY_SIDE: u16 = 0x05;

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

/// Name of an LG input code as written in the config (`dp`, `usbc`, …).
pub fn input_key(value: u16) -> String {
    INPUTS
        .iter()
        .find(|(_, code, _)| *code == value)
        .map(|(key, _, _)| (*key).to_string())
        .unwrap_or_else(|| format!("0x{value:02X}"))
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

/// Builds a DDC/CI "Get VCP Feature" request.
pub fn get_vcp_packet(vcp: u8) -> [u8; 5] {
    let mut packet = [STD_SOURCE_ADDR, 0x82, 0x01, vcp, 0];
    packet[4] = packet[..4].iter().fold(DDC_ADDR << 1, |sum, b| sum ^ b);
    packet
}

/// Extracts the current value from a "Get VCP Feature" reply:
/// `6E 88 02 <result> <vcp> <type> <max hi> <max lo> <cur hi> <cur lo> <checksum>`.
/// Returns `None` for null, error, mismatched, or corrupt replies.
pub fn parse_vcp_reply(reply: &[u8], vcp: u8) -> Option<u16> {
    let reply = reply.get(..11)?;
    let checksum = reply[..10].iter().fold(REPLY_DEST_ADDR, |sum, b| sum ^ b);
    let valid = reply[0] == DDC_ADDR << 1
        && reply[1] == 0x88
        && reply[2] == 0x02
        && reply[3] == 0x00
        && reply[4] == vcp
        && reply[10] == checksum;
    valid.then(|| u16::from_be_bytes([reply[8], reply[9]]))
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
        assert_eq!(input_key(0xD1), "usbc");
        assert_eq!(parse_input(&input_key(0xE0)), Ok(0xE0));
    }

    #[test]
    fn standard_packets_match_hardware_verified_bytes() {
        assert_eq!(
            set_vcp_packet(STD_SOURCE_ADDR, LG_PBP_VCP, PBP_SIDE_BY_SIDE),
            [0x51, 0x84, 0x03, 0xD7, 0x00, 0x05, 0x6A]
        );
        assert_eq!(get_vcp_packet(LG_PBP_VCP), [0x51, 0x82, 0x01, 0xD7, 0x6B]);
    }

    #[test]
    fn parses_replies_captured_from_the_monitor() {
        let pbp_off = [
            0x6E, 0x88, 0x02, 0x00, 0xD7, 0x01, 0x00, 0xFF, 0x00, 0x01, 0x9C,
        ];
        let brightness = [
            0x6E, 0x88, 0x02, 0x00, 0x10, 0x00, 0x00, 0x64, 0x00, 0x64, 0xA4,
        ];
        assert_eq!(parse_vcp_reply(&pbp_off, LG_PBP_VCP), Some(PBP_OFF));
        assert_eq!(parse_vcp_reply(&brightness, VCP_BRIGHTNESS), Some(100));
        // A reply for a different code, a corrupt checksum, and a null message.
        assert_eq!(parse_vcp_reply(&brightness, VCP_CONTRAST), None);
        let mut corrupt = brightness;
        corrupt[9] = 0x63;
        assert_eq!(parse_vcp_reply(&corrupt, VCP_BRIGHTNESS), None);
        let null = [
            0x6E, 0x80, 0xBE, 0x00, 0xE6, 0xD2, 0xD0, 0xA4, 0x0A, 0xF9, 0xAB,
        ];
        assert_eq!(parse_vcp_reply(&null, LG_PBP_VCP), None);
    }
}
