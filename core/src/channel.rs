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

//! `GroupText` channel decoding business logic: precomputing candidate
//! channel keys (the well-known "Public" channel plus any configured
//! [`crate::config::Config::hashtag_channels`]) and matching/decrypting a
//! packet's `GroupText` payload against them. The actual crypto
//! primitives live in [`crate::meshcore_crypto`].
//!
//! Mirrors [`crate::region`]'s split for the (unrelated) transport-code
//! matching feature: "channel" here is the firmware's own chat/group
//! concept (`GroupText`/`GroupData` payloads, `BaseChatMesh`), distinct
//! from a "region" (transport-code scoping, `RegionMap`) even though both
//! ultimately hash a name via [`crate::meshcore_crypto::derive_region_key`]-style
//! truncated SHA256.

use crate::mesh::PacketLogEntry;
use crate::meshcore_crypto::{
    group_channel_secret_and_hash, hashtag_channel_key, mac_then_decrypt,
};

/// Display name of the well-known "Public" channel — see [`PUBLIC_CHANNEL_PSK`].
pub const PUBLIC_CHANNEL_NAME: &str = "Public";

/// The well-known "Public" channel PSK the firmware pre-configures out of
/// the box (`PUBLIC_GROUP_PSK`, `examples/companion_radio/MyMesh.cpp`:
/// `addChannel("Public", PUBLIC_GROUP_PSK)`) — base64
/// `izOH6cXN6mrJ5e26oRXNcg==`, decoded. Not a secret: it's the same
/// hardcoded value in every stock firmware build, which is what makes
/// messages on this channel (unlike a private channel) decodable without
/// prior key exchange.
const PUBLIC_CHANNEL_PSK: [u8; 16] = [
    0x8b, 0x33, 0x87, 0xe9, 0xc5, 0xcd, 0xea, 0x6a, 0xc9, 0xe5, 0xed, 0xba, 0xa1, 0x15, 0xcd, 0x72,
];

/// A `GroupText` message successfully decrypted because its channel hash
/// matched a channel we have the key for: the well-known "Public" channel
/// or one of the configured `hashtag_channels`. Hashtag channels are also
/// effectively public — their key is derived purely from their name
/// (`docs/companion_protocol.md`: "anyone who knows or guesses the
/// channel name can derive the key... should not be treated as private").
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedChannelText {
    /// [`PUBLIC_CHANNEL_NAME`], or the configured hashtag channel name
    /// (e.g. `"#test"`).
    pub channel_name: String,
    pub text: String,
}

/// Precomputes every candidate channel's shared secret and 1-byte hash:
/// the well-known "Public" channel (always first), plus each configured
/// hashtag channel name. Call once when a fresh config/snapshot arrives,
/// not per packet — see [`decode_group_text`].
pub fn precompute_channel_keys(hashtag_channels: &[String]) -> Vec<(String, [u8; 32], u8)> {
    let mut keys = Vec::with_capacity(1 + hashtag_channels.len());

    let (public_secret, public_hash) = group_channel_secret_and_hash(&PUBLIC_CHANNEL_PSK);
    keys.push((PUBLIC_CHANNEL_NAME.to_string(), public_secret, public_hash));

    for name in hashtag_channels {
        let key = hashtag_channel_key(name);
        let (secret, hash) = group_channel_secret_and_hash(&key);
        keys.push((name.clone(), secret, hash));
    }
    keys
}

/// Attempts to decrypt a packet's `GroupText` payload against every
/// candidate channel (see [`precompute_channel_keys`]): matches the
/// payload's channel hash byte, verifies the MAC and decrypts via
/// [`crate::meshcore_crypto::mac_then_decrypt`], then parses the
/// plaintext layout `BaseChatMesh::sendGroupMessage` produces
/// (`src/helpers/BaseChatMesh.cpp`): `[timestamp: 4 bytes LE][txt_type: 1
/// byte][text, NUL-terminated]`, rejecting `txt_type` values the firmware
/// itself doesn't support (`(txt_type >> 2) != 0`).
///
/// Returns `None` if the packet has no decoded header, isn't a
/// `GroupText` payload, is too short, or no candidate channel's MAC
/// verifies (a different/unknown channel, or plain noise).
pub fn decode_group_text(
    entry: &PacketLogEntry,
    channel_keys: &[(String, [u8; 32], u8)],
) -> Option<DecodedChannelText> {
    let header = entry.header.as_ref()?;
    if header.payload_type != "GroupText" || header.payload_version != 0 {
        return None;
    }
    let payload = meshcore_rs::parsing::hex_decode(&entry.payload_hex).ok()?;
    let (channel_hash, mac_and_ciphertext) = payload.split_first()?;

    channel_keys.iter().find_map(|(name, secret, hash)| {
        if channel_hash != hash {
            return None;
        }
        let plaintext = mac_then_decrypt(secret, mac_and_ciphertext)?;
        parse_group_text_plaintext(&plaintext).map(|text| DecodedChannelText {
            channel_name: name.clone(),
            text,
        })
    })
}

/// Parses a decrypted `GroupText` plaintext: `[timestamp: 4 bytes LE]
/// [txt_type: 1 byte][text, NUL-terminated]` — see [`decode_group_text`].
fn parse_group_text_plaintext(plaintext: &[u8]) -> Option<String> {
    if plaintext.len() < 5 {
        return None;
    }
    let txt_type = plaintext[4];
    if (txt_type >> 2) != 0 {
        return None;
    }
    let text_end = plaintext[5..]
        .iter()
        .position(|&b| b == 0)
        .map(|i| 5 + i)
        .unwrap_or(plaintext.len());
    Some(String::from_utf8_lossy(&plaintext[5..text_end]).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mesh::PacketHeaderInfo;

    fn entry(header: Option<PacketHeaderInfo>, payload_hex: &str) -> PacketLogEntry {
        PacketLogEntry {
            id: 1,
            at_unix: 0,
            snr: 1.0,
            rssi: -90,
            header,
            payload_hex: payload_hex.to_string(),
            payload_len: payload_hex.len() / 2,
        }
    }

    fn group_text_header(payload_version: u8) -> PacketHeaderInfo {
        PacketHeaderInfo {
            route_type: "Flood".to_string(),
            payload_type: "GroupText".to_string(),
            payload_type_raw: 5,
            payload_version,
            hops: 1,
            path_hash_size: 1,
            path_hex: String::new(),
            transport_code_hex: None,
            dest_hash_hex: None,
            src_hash_hex: None,
            channel_hash_hex: None,
            advertisement: None,
        }
    }

    // `channel_hash(0x11) || mac_and_ciphertext`, cross-checked
    // independently via the `openssl` CLI (AES-128-ECB) and Python's
    // hmac/hashlib (see `meshcore_crypto::tests`) — decrypts to
    // `[timestamp=0x64ABCD12 LE][txt_type=0]"Hello, mesh!\0"` (then
    // zero-padded to a 32-byte multiple of the AES block size).
    const PUBLIC_GROUP_TEXT_PAYLOAD_HEX: &str =
        "11ece6f1d59f9d82b89139e8f514be20fe914ab3d762d29a9a22438cb69d8789f99ce3";

    #[test]
    fn decode_group_text_decrypts_a_public_channel_message() {
        let channel_keys = precompute_channel_keys(&[]);
        let e = entry(Some(group_text_header(0)), PUBLIC_GROUP_TEXT_PAYLOAD_HEX);

        let decoded = decode_group_text(&e, &channel_keys).expect("should decrypt");
        assert_eq!(decoded.channel_name, "Public");
        assert_eq!(decoded.text, "Hello, mesh!");
    }

    #[test]
    fn decode_group_text_none_when_no_configured_channel_matches_the_hash() {
        // No hashtag channels configured, and the payload's hash (0x11)
        // isn't one we have another key for — only "Public" is tried,
        // and it does match here, so flip the hash byte to prove the
        // negative case.
        let channel_keys = precompute_channel_keys(&["#unrelated".to_string()]);
        let mut payload = hex_bytes(PUBLIC_GROUP_TEXT_PAYLOAD_HEX);
        payload[0] = 0x00; // not Public's hash, and not #unrelated's either
        let e = entry(Some(group_text_header(0)), &hex_string(&payload));

        assert!(decode_group_text(&e, &channel_keys).is_none());
    }

    // `channel_hash(0x61) || mac || ciphertext` for channel `"#mytest"`,
    // independently generated via `openssl` (AES-128-ECB) and Python's
    // hmac/hashlib — decrypts to `[timestamp=0x11223344 LE][txt_type=0]
    // "Topic chat\0"` (zero-padded to 16 bytes, 1 AES block).
    const HASHTAG_GROUP_TEXT_PAYLOAD_HEX: &str = "6197dfa79aead70a65177d5a0b785700bae78c";

    #[test]
    fn decode_group_text_decrypts_a_configured_hashtag_channel_message() {
        let channel_keys = precompute_channel_keys(&["#mytest".to_string()]);
        let e = entry(Some(group_text_header(0)), HASHTAG_GROUP_TEXT_PAYLOAD_HEX);

        let decoded = decode_group_text(&e, &channel_keys).expect("should decrypt");
        assert_eq!(decoded.channel_name, "#mytest");
        assert_eq!(decoded.text, "Topic chat");
    }

    #[test]
    fn decode_group_text_none_for_an_unconfigured_hashtag_channel() {
        // Same payload, but the channel isn't in the configured list —
        // only "Public" is tried, whose hash doesn't match 0x61.
        let channel_keys = precompute_channel_keys(&[]);
        let e = entry(Some(group_text_header(0)), HASHTAG_GROUP_TEXT_PAYLOAD_HEX);

        assert!(decode_group_text(&e, &channel_keys).is_none());
    }

    #[test]
    fn decode_group_text_none_for_non_group_text_payload_type() {
        let channel_keys = precompute_channel_keys(&[]);
        let mut header = group_text_header(0);
        header.payload_type = "GroupData".to_string();
        let e = entry(Some(header), PUBLIC_GROUP_TEXT_PAYLOAD_HEX);

        assert!(decode_group_text(&e, &channel_keys).is_none());
    }

    #[test]
    fn decode_group_text_none_for_a_non_payload_ver_1_packet() {
        let channel_keys = precompute_channel_keys(&[]);
        let e = entry(Some(group_text_header(1)), PUBLIC_GROUP_TEXT_PAYLOAD_HEX);

        assert!(decode_group_text(&e, &channel_keys).is_none());
    }

    #[test]
    fn decode_group_text_none_without_a_decoded_header() {
        let channel_keys = precompute_channel_keys(&[]);
        let e = entry(None, PUBLIC_GROUP_TEXT_PAYLOAD_HEX);

        assert!(decode_group_text(&e, &channel_keys).is_none());
    }

    fn hex_bytes(hex: &str) -> Vec<u8> {
        (0..hex.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
            .collect()
    }

    fn hex_string(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }
}
