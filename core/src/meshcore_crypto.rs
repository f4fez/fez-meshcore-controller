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

use aes::cipher::{generic_array::GenericArray, BlockDecrypt};
use aes::Aes128;
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};

type HmacSha256 = Hmac<Sha256>;

/// Size in bytes of the truncated HMAC-SHA256 MAC prefixing an
/// encrypt-then-MAC payload (`CIPHER_MAC_SIZE`, `src/MeshCore.h`).
pub const CIPHER_MAC_SIZE: usize = 2;

/// AES-128 block/key size used for direct-message and group-channel
/// payload encryption (`CIPHER_KEY_SIZE`, `src/MeshCore.h`).
pub const CIPHER_KEY_SIZE: usize = 16;

/// Derives a region's 16-byte transport key from its name: `SHA256(name)`,
/// truncated to 16 bytes. Deterministic — two independent implementations
/// that know only the region's name compute the identical key.
pub fn derive_region_key(name: &str) -> [u8; 16] {
    let digest = Sha256::digest(name.as_bytes());
    let mut key = [0u8; 16];
    key.copy_from_slice(&digest[..16]);
    key
}

/// Derives a "Hashtag Channel"'s 16-byte PSK from its name, including the
/// leading `#` (added if the caller omitted it) — `docs/companion_protocol.md`:
/// "Uses a secret key derived from the channel name... the first 16
/// bytes of `sha256("#test")`". Independently verified via Python's
/// hashlib: `SHA256("#test")[:16]` == the doc's own example key
/// `9cd8fcf22a47333b591d96a2b848b73f`.
///
/// Same formula as [`derive_region_key`] (`SHA256(name)`, truncated), but
/// kept as its own named function: region transport keys and channel PSKs
/// are different firmware concepts that happen to share this derivation,
/// not the same value for the same name (a channel key hashes the name
/// *with* its `#`; nothing requires a region's name to have one).
pub fn hashtag_channel_key(name: &str) -> [u8; 16] {
    if name.starts_with('#') {
        derive_region_key(name)
    } else {
        derive_region_key(&format!("#{name}"))
    }
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

/// Derives a `GroupChannel`'s 32-byte shared secret and 1-byte channel
/// hash from its PSK, mirroring the firmware's `BaseChatMesh::addChannel`
/// (`src/helpers/BaseChatMesh.cpp`):
///
/// ```cpp
/// memset(dest->channel.secret, 0, sizeof(dest->channel.secret));   // 32 bytes
/// int len = decode_base64(psk_base64, strlen(psk_base64), dest->channel.secret);
/// mesh::Utils::sha256(dest->channel.hash, sizeof(dest->channel.hash), dest->channel.secret, len);
/// ```
///
/// The secret buffer (`GroupChannel::secret`, `PUB_KEY_SIZE` = 32 bytes)
/// is the PSK zero-padded to 32 bytes; the hash is `SHA256(psk)`, of
/// which only the first byte is ever compared to identify a channel from
/// a received packet (`Mesh::onRecvPacket`'s `searchChannelsByHash`
/// checks `hash[0]` only).
pub fn group_channel_secret_and_hash(psk: &[u8]) -> ([u8; 32], u8) {
    let len = psk.len().min(32);
    let mut secret = [0u8; 32];
    secret[..len].copy_from_slice(&psk[..len]);
    let hash = Sha256::digest(&psk[..len]);
    (secret, hash[0])
}

/// Verifies the MAC and decrypts a `mac || ciphertext` payload — mirrors
/// `Utils::MACThenDecrypt` (`src/Utils.cpp`): the MAC is the first
/// [`CIPHER_MAC_SIZE`] bytes of `HMAC-SHA256(secret, ciphertext)`; the
/// ciphertext is AES-128-ECB, `secret`'s first [`CIPHER_KEY_SIZE`] bytes
/// as the key (`Utils::encrypt` zero-pads its final partial block, so
/// `ciphertext.len()` is always a multiple of the block size).
///
/// Returns `None` if the input is too short, the ciphertext isn't a
/// multiple of the AES block size, or the MAC doesn't match (wrong
/// secret, or the bytes simply aren't a valid encrypt-then-MAC payload).
pub fn mac_then_decrypt(secret: &[u8; 32], mac_and_ciphertext: &[u8]) -> Option<Vec<u8>> {
    if mac_and_ciphertext.len() <= CIPHER_MAC_SIZE {
        return None;
    }
    let (mac, ciphertext) = mac_and_ciphertext.split_at(CIPHER_MAC_SIZE);
    if ciphertext.is_empty() || ciphertext.len() % CIPHER_KEY_SIZE != 0 {
        return None;
    }

    let mut hmac = HmacSha256::new_from_slice(secret).expect("HMAC accepts a key of any length");
    hmac.update(ciphertext);
    let expected_mac = hmac.finalize().into_bytes();
    if expected_mac[..CIPHER_MAC_SIZE] != *mac {
        return None;
    }

    let key = GenericArray::from_slice(&secret[..CIPHER_KEY_SIZE]);
    let cipher = <Aes128 as aes::cipher::KeyInit>::new(key);
    let mut plaintext = ciphertext.to_vec();
    for block in plaintext.chunks_exact_mut(CIPHER_KEY_SIZE) {
        cipher.decrypt_block(GenericArray::from_mut_slice(block));
    }
    Some(plaintext)
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
    fn hashtag_channel_key_matches_the_verified_companion_protocol_doc_vector() {
        // `docs/companion_protocol.md`'s own worked example, independently
        // re-verified via Python's hashlib (not taken on faith from the
        // doc): SHA256("#test")[:16] == 9cd8fcf22a47333b591d96a2b848b73f.
        const EXPECTED: [u8; 16] = [
            0x9c, 0xd8, 0xfc, 0xf2, 0x2a, 0x47, 0x33, 0x3b, 0x59, 0x1d, 0x96, 0xa2, 0xb8, 0x48,
            0xb7, 0x3f,
        ];
        assert_eq!(hashtag_channel_key("#test"), EXPECTED);
    }

    #[test]
    fn hashtag_channel_key_adds_a_missing_leading_hash() {
        assert_eq!(hashtag_channel_key("test"), hashtag_channel_key("#test"));
    }

    #[test]
    fn avoid_reserved_codes_nudges_only_the_two_reserved_values() {
        assert_eq!(avoid_reserved_codes(0x0000), 0x0001);
        assert_eq!(avoid_reserved_codes(0xFFFF), 0xFFFE);
        assert_eq!(avoid_reserved_codes(0x0001), 0x0001);
        assert_eq!(avoid_reserved_codes(0x1234), 0x1234);
    }

    // --- GroupChannel decryption (well-known "Public" channel) -----------

    // Base64 `izOH6cXN6mrJ5e26oRXNcg==` (`PUBLIC_GROUP_PSK`,
    // `examples/companion_radio/MyMesh.cpp`), decoded.
    const PUBLIC_PSK: [u8; 16] = [
        0x8b, 0x33, 0x87, 0xe9, 0xc5, 0xcd, 0xea, 0x6a, 0xc9, 0xe5, 0xed, 0xba, 0xa1, 0x15, 0xcd,
        0x72,
    ];

    #[test]
    fn group_channel_secret_and_hash_matches_the_verified_public_channel_vector() {
        // Independently verified via Python's hashlib (not derived from
        // this implementation): SHA256(PUBLIC_PSK)[0] == 0x11.
        let (secret, hash) = group_channel_secret_and_hash(&PUBLIC_PSK);
        assert_eq!(hash, 0x11);
        assert_eq!(&secret[..16], &PUBLIC_PSK);
        assert_eq!(&secret[16..], &[0u8; 16]);
    }

    #[test]
    fn mac_then_decrypt_matches_a_vector_cross_checked_via_openssl_and_python_hmac() {
        // AES-128-ECB ciphertext generated independently via the `openssl`
        // CLI, MAC via Python's hmac/hashlib — not derived from this
        // implementation. Plaintext: timestamp=0x64ABCD12 (LE), txt_type=0,
        // text="Hello, mesh!\0", zero-padded to 32 bytes (2 AES blocks).
        const MAC_AND_CIPHERTEXT: [u8; 34] = [
            0xec, 0xe6, 0xf1, 0xd5, 0x9f, 0x9d, 0x82, 0xb8, 0x91, 0x39, 0xe8, 0xf5, 0x14, 0xbe,
            0x20, 0xfe, 0x91, 0x4a, 0xb3, 0xd7, 0x62, 0xd2, 0x9a, 0x9a, 0x22, 0x43, 0x8c, 0xb6,
            0x9d, 0x87, 0x89, 0xf9, 0x9c, 0xe3,
        ];
        let (secret, _hash) = group_channel_secret_and_hash(&PUBLIC_PSK);

        let plaintext = mac_then_decrypt(&secret, &MAC_AND_CIPHERTEXT).expect("MAC should verify");

        let mut expected = vec![0x12, 0xcd, 0xab, 0x64, 0x00];
        expected.extend_from_slice(b"Hello, mesh!\0");
        expected.resize(32, 0); // trailing AES block zero-padding
        assert_eq!(plaintext, expected);
    }

    #[test]
    fn mac_then_decrypt_rejects_a_tampered_mac() {
        let (secret, _hash) = group_channel_secret_and_hash(&PUBLIC_PSK);
        let mut mac_and_ciphertext: [u8; 34] = [
            0xec, 0xe6, 0xf1, 0xd5, 0x9f, 0x9d, 0x82, 0xb8, 0x91, 0x39, 0xe8, 0xf5, 0x14, 0xbe,
            0x20, 0xfe, 0x91, 0x4a, 0xb3, 0xd7, 0x62, 0xd2, 0x9a, 0x9a, 0x22, 0x43, 0x8c, 0xb6,
            0x9d, 0x87, 0x89, 0xf9, 0x9c, 0xe3,
        ];
        mac_and_ciphertext[0] ^= 0xFF;

        assert!(mac_then_decrypt(&secret, &mac_and_ciphertext).is_none());
    }

    #[test]
    fn mac_then_decrypt_rejects_a_ciphertext_not_a_multiple_of_the_block_size() {
        let (secret, _hash) = group_channel_secret_and_hash(&PUBLIC_PSK);
        let malformed = [0u8; CIPHER_MAC_SIZE + 5]; // 5 "ciphertext" bytes, not a block multiple

        assert!(mac_then_decrypt(&secret, &malformed).is_none());
    }

    #[test]
    fn mac_then_decrypt_rejects_input_not_longer_than_the_mac() {
        let (secret, _hash) = group_channel_secret_and_hash(&PUBLIC_PSK);

        assert!(mac_then_decrypt(&secret, &[0u8; CIPHER_MAC_SIZE]).is_none());
    }
}
