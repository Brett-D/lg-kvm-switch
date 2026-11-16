//! Just enough EDID parsing to recognize a monitor.

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EdidInfo {
    /// Three-letter PNP manufacturer ID (`GSM` for LG).
    pub vendor: String,
    pub product: u16,
    pub name: Option<String>,
}

const HEADER: [u8; 8] = [0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x00];

impl EdidInfo {
    pub fn parse(edid: &[u8]) -> Option<Self> {
        if edid.len() < 128 || edid[..8] != HEADER {
            return None;
        }
        let packed = u16::from_be_bytes([edid[8], edid[9]]);
        let vendor = [10, 5, 0]
            .iter()
            .map(|shift| (((packed >> shift) & 0x1F) as u8 + b'@') as char)
            .collect();
        let product = u16::from_le_bytes([edid[10], edid[11]]);
        let name = (0..4)
            .map(|i| &edid[54 + i * 18..72 + i * 18])
            .find(|d| d[0] == 0 && d[1] == 0 && d[3] == 0xFC)
            .map(|d| {
                let text = String::from_utf8_lossy(&d[5..]);
                text.split('\n')
                    .next()
                    .unwrap_or_default()
                    .trim()
                    .to_string()
            })
            .filter(|name| !name.is_empty());
        Some(Self {
            vendor,
            product,
            name,
        })
    }

    pub fn matches(&self, vendor: Option<&str>, product: Option<u16>) -> bool {
        vendor.map_or(true, |v| self.vendor.eq_ignore_ascii_case(v))
            && product.map_or(true, |p| self.product == p)
    }

    pub fn describe(&self) -> String {
        format!(
            "{} ({}{:04X})",
            self.name.as_deref().unwrap_or("unnamed monitor"),
            self.vendor,
            self.product
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lg_edid() -> Vec<u8> {
        let mut edid = vec![0u8; 128];
        edid[..8].copy_from_slice(&HEADER);
        // "GSM" packed as 5-bit letters, product 0x9E9D little-endian.
        edid[8..12].copy_from_slice(&[0x1E, 0x6D, 0x9D, 0x9E]);
        edid[54..72].copy_from_slice(b"\0\0\0\xFC\0LG ULTRAGEAR+");
        edid
    }

    #[test]
    fn parses_vendor_product_and_name() {
        let info = EdidInfo::parse(&lg_edid()).unwrap();
        assert_eq!(info.vendor, "GSM");
        assert_eq!(info.product, 0x9E9D);
        assert_eq!(info.name.as_deref(), Some("LG ULTRAGEAR+"));
        assert!(info.matches(Some("gsm"), None));
        assert!(!info.matches(Some("GSM"), Some(0x1234)));
    }

    #[test]
    fn rejects_non_edid_data() {
        assert_eq!(EdidInfo::parse(&[0u8; 128]), None);
        assert_eq!(EdidInfo::parse(&HEADER), None);
    }
}
