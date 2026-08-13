// Copyright 2026 Florian MAZEN (F4FEZ)
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! MeshCore firmware-compatible crypto primitives for the region/transport-code
//! scheme, verified against the `meshcore-dev/MeshCore` firmware source
//! (`src/helpers/TransportKeyStore.cpp`):
//!
//! ```cpp
//! // TransportKeyStore::getAutoKeyFor(uint16_t id, const char* name, TransportKey& dest)
//! // "calc key for publicly-known hashtag region name"
//! SHA256 sha;
//! sha.update(name, strlen(name));
//! sha.finalize(&dest.key, sizeof(dest.key));   // dest.key: uint8_t[16]
//!
//! // TransportKey::calcTransportCode(const mesh::Packet* packet)
//! SHA256 sha;
//! sha.resetHMAC(key, sizeof(key));
//! uint8_t type = packet->getPayloadType();
//! sha.update(&type, 1);
//! sha.update(packet->payload, packet->payload_len);
//! sha.finalizeHMAC(key, sizeof(key), &code, 2);
//! if (code == 0) code++;            // reserve 0x0000
//! else if (code == 0xFFFF) code--;  // reserve 0xFFFF
//! ```
//!
//! A region's key is derived deterministically from its name alone — no
//! secret provisioning needed, per the firmware's own "publicly-known
//! hashtag region name" comment. The final 2-byte code, however, is *not*
//! a fixed per-region value: it's an HMAC over the packet's own payload
//! type and bytes, so it varies per packet.

use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};

type HmacSha256 = Hmac<Sha256>;

/// Derives a region's 16-byte transport key from its name: `SHA256(name)`,
/// truncated to 16 bytes. Deterministic — two independent implementations
/// that know only the region's name compute the identical key.
pub fn derive_region_key(name: &str) -> [u8; 16] {
    let digest = Sha256::digest(name.as_bytes());
    let mut key = [0u8; 16];
    key.copy_from_slice(&digest[..16]);
    key
}

/// Computes the 2-byte transport code a region would produce for a given
/// packet: HMAC-SHA256(`key`, `payload_type_raw || payload`), truncated to
/// the first 2 bytes, with the firmware's 0x0000/0xFFFF reservation nudge
/// applied. Returns raw bytes (not a parsed integer) so callers can compare
/// directly against wire bytes without needing to resolve endianness.
pub fn calc_transport_code(key: &[u8; 16], payload_type_raw: u8, payload: &[u8]) -> [u8; 2] {
    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC accepts a key of any length");
    mac.update(&[payload_type_raw]);
    mac.update(payload);
    let digest = mac.finalize().into_bytes();

    let code = u16::from_le_bytes([digest[0], digest[1]]);
    avoid_reserved_codes(code).to_le_bytes()
}

/// The firmware reserves `0x0000`/`0xFFFF` as transport code values,
/// nudging a code that happens to land on either to the nearest non-reserved
/// value: `if (code == 0) code++; else if (code == 0xFFFF) code--;`.
fn avoid_reserved_codes(code: u16) -> u16 {
    if code == 0 {
        1
    } else if code == 0xFFFF {
        0xFFFE
    } else {
        code
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Independently verified via Python's hashlib/hmac (not derived from
    // this implementation).
    const TEST_REGION_NAME: &str = "TestRegion";
    const TEST_KEY: [u8; 16] = [
        0xfb, 0x70, 0x5f, 0x20, 0xc7, 0x1a, 0xfb, 0x2c, 0xf4, 0x17, 0xeb, 0x2e, 0xda, 0xba, 0x6e,
        0x26,
    ];
    const TEST_PAYLOAD_TYPE: u8 = 2; // TextMsg
    const TEST_PAYLOAD: [u8; 4] = [0xde, 0xad, 0xbe, 0xef];
    const TEST_CODE: [u8; 2] = [0xa5, 0x18];

    #[test]
    fn derive_region_key_matches_the_verified_vector() {
        assert_eq!(derive_region_key(TEST_REGION_NAME), TEST_KEY);
    }

    #[test]
    fn calc_transport_code_matches_the_verified_vector() {
        let code = calc_transport_code(&TEST_KEY, TEST_PAYLOAD_TYPE, &TEST_PAYLOAD);
        assert_eq!(code, TEST_CODE);
    }

    #[test]
    fn different_payloads_produce_different_codes() {
        // The whole point of the scheme: the code is per-packet, not a
        // fixed per-region value.
        let code_a = calc_transport_code(&TEST_KEY, TEST_PAYLOAD_TYPE, &TEST_PAYLOAD);
        let code_b = calc_transport_code(&TEST_KEY, TEST_PAYLOAD_TYPE, &[0x00]);
        assert_ne!(code_a, code_b);
    }

    #[test]
    fn different_region_names_derive_different_keys() {
        assert_ne!(derive_region_key("World"), derive_region_key("Europe"));
    }

    #[test]
    fn avoid_reserved_codes_nudges_only_the_two_reserved_values() {
        assert_eq!(avoid_reserved_codes(0x0000), 0x0001);
        assert_eq!(avoid_reserved_codes(0xFFFF), 0xFFFE);
        assert_eq!(avoid_reserved_codes(0x0001), 0x0001);
        assert_eq!(avoid_reserved_codes(0x1234), 0x1234);
    }
}
