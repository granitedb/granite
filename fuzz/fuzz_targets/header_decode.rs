//! The message header decoder on arbitrary bytes: it never panics, every
//! rejection is a typed `Error`, and whatever it accepts seals back to the
//! same bytes. The input is tried twice: as it is, and with both checksums
//! fixed, so that the checks after the checksums are reached too.

#![no_main]

use granite_base::checksum::checksum;
use granite_base::constants::FORMAT;
use granite_base::header::Header;
use zerocopy::IntoBytes;
use zerocopy::little_endian::U128;

fn decode(bytes: &[u8]) {
    let Ok(header) = Header::decode(bytes) else {
        return;
    };
    let body = &bytes[FORMAT.header_size as usize..];
    if header.check_body(body).is_ok() {
        let mut sealed = *header;
        sealed.seal(body);
        assert_eq!(sealed.as_bytes(), &bytes[..FORMAT.header_size as usize]);
    }
}

libfuzzer_sys::fuzz_target!(|bytes: &[u8]| {
    decode(bytes);
    let mut fixed = bytes.to_vec();
    if fixed.len() >= FORMAT.header_size as usize {
        let body = checksum(&fixed[FORMAT.header_size as usize..]);
        fixed[16..32].copy_from_slice(U128::new(body).as_bytes());
        let sum = checksum(&fixed[16..FORMAT.header_size as usize]);
        fixed[..16].copy_from_slice(U128::new(sum).as_bytes());
        decode(&fixed);
    }
});
