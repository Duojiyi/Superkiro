//! CRC32 implementation using compile-time generated IEEE 802.3 / ISO-HDLC lookup table.
//! Zero external dependencies.

const fn make_crc_table() -> [u32; 256] {
    let mut table = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut c = i as u32;
        let mut k = 0;
        while k < 8 {
            if (c & 1) != 0 {
                c = 0xEDB8_8320 ^ (c >> 1);
            } else {
                c >>= 1;
            }
            k += 1;
        }
        table[i] = c;
        i += 1;
    }
    table
}

const CRC_TABLE: [u32; 256] = make_crc_table();

/// Calculate CRC32 checksum over data slice using ISO-HDLC / IEEE standard polynomial.
pub fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFF_u32;
    for &b in data {
        crc = (crc >> 8) ^ CRC_TABLE[((crc ^ (b as u32)) & 0xFF) as usize];
    }
    crc ^ 0xFFFF_FFFF_u32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_crc32_empty() {
        assert_eq!(crc32(&[]), 0);
    }

    #[test]
    fn test_crc32_standard_check_value() {
        // "123456789" CRC32 standard check value is 0xCBF43926
        assert_eq!(crc32(b"123456789"), 0xCBF43926);
    }
}
