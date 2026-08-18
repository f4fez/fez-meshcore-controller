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

//! MeshCore device-signed MQTT auth token construction, matching
//! `michaelhart/meshcore-decoder`'s `createAuthToken`/`verifyAuthToken`
//! (`src/utils/auth-token.ts`, read directly, not assumed) field-for-field —
//! this is the scheme LetsMesh and MeshMapper's public MQTT brokers require:
//!
//! ```ts
//! const header = { alg: 'Ed25519', typ: 'JWT' };
//! payload.publicKey = publicKeyHex.toUpperCase();
//! const headerEncoded = base64urlEncode(utf8(JSON.stringify(header)));
//! const payloadEncoded = base64urlEncode(utf8(JSON.stringify(payload)));
//! const signingInput = `${headerEncoded}.${payloadEncoded}`;
//! const signatureHex = await sign(hex(utf8(signingInput)), privateKeyHex, payload.publicKey);
//! return `${headerEncoded}.${payloadEncoded}.${signatureHex}`;
//! ```
//!
//! The `sign()` call above round-trips its message through a hex string only
//! because that's the WASM boundary's calling convention (`hexToBytes` right
//! before signing) — the bytes actually signed are exactly the ASCII bytes
//! of `signingInput`, with no extra hashing/framing. This module signs
//! nothing itself: [`signing_input`] produces those exact bytes for the
//! caller to pass to the node's own `sign()` command (never extracting the
//! node's private key), and [`assemble_token`] appends the resulting
//! signature, hex-encoded uppercase like the reference implementation's own
//! `bytesToHex` (deliberately hex, not base64url — that source's comment:
//! "for consistency with MeshCore").

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use serde::Serialize;

use crate::mesh::hex_encode;

/// Claims for a MeshCore device-signed MQTT auth token.
pub struct AuthTokenClaims {
    /// The signing node's public key, hex (any case — uppercased on encode,
    /// matching `createAuthToken`'s own `payload.publicKey = ...toUpperCase()`).
    pub public_key_hex: String,
    pub iat: i64,
    pub exp: i64,
    pub aud: String,
}

#[derive(Serialize)]
struct Header {
    alg: &'static str,
    typ: &'static str,
}

#[derive(Serialize)]
struct Payload<'a> {
    #[serde(rename = "publicKey")]
    public_key: &'a str,
    iat: i64,
    exp: i64,
    aud: &'a str,
}

/// Builds `"{header_b64}.{payload_b64}"` — the exact bytes that must be
/// signed (via the node's own `sign()` command) to authenticate against a
/// device-signing MQTT broker.
pub fn signing_input(claims: &AuthTokenClaims) -> String {
    let public_key_upper = claims.public_key_hex.to_uppercase();
    let header_json = serde_json::to_vec(&Header {
        alg: "Ed25519",
        typ: "JWT",
    })
    .expect("Header is always serializable");
    let payload_json = serde_json::to_vec(&Payload {
        public_key: &public_key_upper,
        iat: claims.iat,
        exp: claims.exp,
        aud: &claims.aud,
    })
    .expect("Payload is always serializable");

    format!(
        "{}.{}",
        URL_SAFE_NO_PAD.encode(header_json),
        URL_SAFE_NO_PAD.encode(payload_json)
    )
}

/// Appends a signature (produced by signing [`signing_input`]'s output
/// bytes) to complete the token: `"{header_b64}.{payload_b64}.{signature_hex}"`.
pub fn assemble_token(signing_input: &str, signature: &[u8]) -> String {
    format!("{signing_input}.{}", hex_encode(signature).to_uppercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_claims() -> AuthTokenClaims {
        AuthTokenClaims {
            public_key_hex: "ab".repeat(32),
            iat: 1_700_000_000,
            exp: 1_700_003_600,
            aud: "mqtt-us-v1.letsmesh.net".to_string(),
        }
    }

    #[test]
    fn signing_input_matches_the_vector_cross_checked_via_python() {
        // Independently computed via Python's json.dumps(separators=(',',
        // ':'))/base64.urlsafe_b64encode — not derived from this
        // implementation.
        const EXPECTED: &str = "eyJhbGciOiJFZDI1NTE5IiwidHlwIjoiSldUIn0.\
            eyJwdWJsaWNLZXkiOiJBQkFCQUJBQkFCQUJBQkFCQUJBQkFCQUJBQkFCQUJBQkFCQUJBQkFCQUJBQkFCQUJBQkFCQUJBQkFCQUJBQkFCIiwiaWF0IjoxNzAwMDAwMDAwLCJleHAiOjE3MDAwMDM2MDAsImF1ZCI6Im1xdHQtdXMtdjEubGV0c21lc2gubmV0In0";

        assert_eq!(signing_input(&sample_claims()), EXPECTED);
    }

    #[test]
    fn assemble_token_matches_the_vector_cross_checked_via_python() {
        // Same Python cross-check, appending a fixed 64-byte 0x00..0x3F
        // signature hex-encoded uppercase (`bytes(range(64)).hex().upper()`).
        let signature: Vec<u8> = (0u8..64).collect();
        let input = signing_input(&sample_claims());

        let token = assemble_token(&input, &signature);

        assert!(token.starts_with(&input));
        assert!(token.ends_with(
            "000102030405060708090A0B0C0D0E0F101112131415161718191A1B1C1D1E1F\
             202122232425262728292A2B2C2D2E2F303132333435363738393A3B3C3D3E3F"
        ));
        assert_eq!(token.matches('.').count(), 2);
    }

    #[test]
    fn signing_input_is_url_safe_and_unpadded() {
        let input = signing_input(&sample_claims());
        assert!(!input.contains('+'));
        assert!(!input.contains('/'));
        assert!(!input.contains('='));
    }

    #[test]
    fn signing_input_round_trips_to_the_expected_json_shape() {
        let claims = sample_claims();
        let input = signing_input(&claims);
        let (header_b64, payload_b64) = input.split_once('.').expect("one dot separator");

        let header_bytes = URL_SAFE_NO_PAD.decode(header_b64).unwrap();
        let header: serde_json::Value = serde_json::from_slice(&header_bytes).unwrap();
        assert_eq!(header["alg"], "Ed25519");
        assert_eq!(header["typ"], "JWT");

        let payload_bytes = URL_SAFE_NO_PAD.decode(payload_b64).unwrap();
        let payload: serde_json::Value = serde_json::from_slice(&payload_bytes).unwrap();
        assert_eq!(payload["publicKey"], "AB".repeat(32));
        assert_eq!(payload["iat"], claims.iat);
        assert_eq!(payload["exp"], claims.exp);
        assert_eq!(payload["aud"], claims.aud);
    }
}
