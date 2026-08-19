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

//! Decodes CayenneLPP-encoded telemetry responses from a MeshCore node
//! (`REQ_TYPE_GET_TELEMETRY_DATA` / `BinaryReqType::Telemetry`).
//!
//! Hand-rolled against the exact source MeshCore firmware links --
//! `electroniccats/CayenneLPP@1.6.1` (pinned in `library.json`/
//! `platformio.ini`), fetched and read directly (`CayenneLPP.h`'s type
//! codes/sizes/multipliers, `CayenneLPP.cpp`'s `addField`/`addGPS`/
//! `getTypeSigned` for signedness and the two's-complement encoding) --
//! not assumed or copied from a third-party crate. A prior attempt using
//! the `cayenne_lpp` 0.4.0 crate hit a confirmed, unfixed bounds-check bug
//! that crashed the daemon against real hardware.
//!
//! Covers only the channel types MeshCore firmware actually emits,
//! confirmed via `gh search code --repo meshcore-dev/MeshCore` across
//! every `.add*` call site (`src/helpers/sensors/EnvironmentSensorManager
//! .cpp` and every `variants/*/target.cpp` sensor manager, plus the base
//! repeater/companion/room/sensor examples): `AnalogInput`,
//! `GenericSensor`, `Luminosity`, `Temperature`, `RelativeHumidity`,
//! `BarometricPressure`, `Voltage`, `Current`, `Frequency`, `Percentage`,
//! `Altitude`, `Power`, `Distance`, `GPS`. Never `DigitalInput`/
//! `DigitalOutput`/`AnalogOutput`/`Presence`/`Concentration`/`Energy`/
//! `Direction`/`UnixTime`/`Gyrometer`/`Accelerometer`/`Colour`/`Switch` --
//! decoding stops cleanly at any type code outside this list rather than
//! guessing, so a future firmware sensor this doesn't cover yet degrades
//! to "not shown" rather than misdecoding.
//!
//! Pure decoding, no domain types.

/// A single decoded telemetry channel reading. GPS is the only
/// multi-component type MeshCore emits; it's flattened into one reading
/// per component (latitude/longitude/altitude), sharing the same
/// `channel`.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct TelemetryReading {
    pub channel: u8,
    pub label: String,
    pub value: f64,
    pub unit: String,
}

/// CayenneLPP type code for GPS -- handled separately from [`TYPES`] since
/// it's the one multi-component type MeshCore emits: three signed 3-byte
/// fields (lat, lon, alt) instead of a single N-byte field
/// (`CayenneLPP.cpp`'s `addGPS`, `CayenneLPP.h`'s `LPP_GPS_SIZE = 9`).
const LPP_GPS: u8 = 136;
const GPS_COMPONENT_SIZE: usize = 3;
const GPS_LAT_LON_MULT: f64 = 10_000.0;
const GPS_ALT_MULT: f64 = 100.0;

/// `(type code, byte size, signed, multiplier, label, unit)` for every
/// CayenneLPP channel type MeshCore firmware is confirmed to emit (see
/// module doc comment) -- verified against `CayenneLPP.h`/`.cpp` at the
/// exact tag (`1.6.1`) the firmware links, not assumed. A decoded value is
/// `raw_integer / multiplier`.
const TYPES: &[(u8, usize, bool, f64, &str, &str)] = &[
    (2, 2, true, 100.0, "Analog input", ""),
    (100, 4, false, 1.0, "Generic sensor", ""),
    (101, 2, false, 1.0, "Luminosity", "lux"),
    (103, 2, true, 10.0, "Temperature", "\u{b0}C"),
    (104, 1, false, 2.0, "Relative humidity", "%"),
    (115, 2, false, 10.0, "Barometric pressure", "hPa"),
    (116, 2, true, 100.0, "Voltage", "V"),
    (117, 2, true, 1000.0, "Current", "A"),
    (118, 4, false, 1.0, "Frequency", "Hz"),
    (120, 1, false, 1.0, "Percentage", "%"),
    (121, 2, true, 1.0, "Altitude", "m"),
    (128, 2, false, 1.0, "Power", "W"),
    (130, 4, false, 1000.0, "Distance", "m"),
];

/// Decodes a raw CayenneLPP telemetry payload (as returned by
/// `MeshClient::request_telemetry`) into a list of readings.
///
/// Best-effort, not all-or-nothing: stops and returns whatever decoded
/// cleanly the moment it hits a channel/type it doesn't recognize (see
/// [`TYPES`]) or doesn't have enough remaining bytes for, rather than
/// failing the whole response. Confirmed necessary against real hardware:
/// a repeater with no extra sensors reports only `Voltage`+`Temperature`,
/// but the payload the mesh forwards keeps going past that with trailing
/// zero bytes (buffer padding of unknown origin, not real sensor data) --
/// `channel=0, type=0` isn't a type this decoder recognizes (MeshCore
/// never emits `DigitalInput`, type code `0`), so parsing simply stops
/// there on its own, no special-casing needed.
///
/// Every read is bounds-checked before it happens, so unlike the earlier
/// `cayenne_lpp`-crate-based implementation this cannot panic on
/// malformed/truncated input, whatever a future/different repeater's
/// payload shape turns out to be.
pub fn decode(raw: &[u8]) -> Vec<TelemetryReading> {
    let mut readings = Vec::new();
    let mut i = 0;

    while i + 2 <= raw.len() {
        let channel = raw[i];
        let type_code = raw[i + 1];
        let data_start = i + 2;

        if type_code == LPP_GPS {
            let Some(fields) = decode_gps(channel, &raw[data_start..]) else {
                break;
            };
            readings.extend(fields);
            i = data_start + GPS_COMPONENT_SIZE * 3;
            continue;
        }

        let Some(&(_, size, signed, mult, label, unit)) =
            TYPES.iter().find(|&&(code, ..)| code == type_code)
        else {
            break;
        };
        let Some(bytes) = raw.get(data_start..data_start + size) else {
            break;
        };

        let raw_value = if signed {
            read_signed(bytes)
        } else {
            read_unsigned(bytes)
        };
        readings.push(TelemetryReading {
            channel,
            label: label.to_string(),
            value: raw_value as f64 / mult,
            unit: unit.to_string(),
        });
        i = data_start + size;
    }

    readings
}

fn decode_gps(channel: u8, data: &[u8]) -> Option<[TelemetryReading; 3]> {
    let lat = data.get(0..GPS_COMPONENT_SIZE)?;
    let lon = data.get(GPS_COMPONENT_SIZE..GPS_COMPONENT_SIZE * 2)?;
    let alt = data.get(GPS_COMPONENT_SIZE * 2..GPS_COMPONENT_SIZE * 3)?;
    Some([
        TelemetryReading {
            channel,
            label: "GPS latitude".to_string(),
            value: read_signed(lat) as f64 / GPS_LAT_LON_MULT,
            unit: "\u{b0}".to_string(),
        },
        TelemetryReading {
            channel,
            label: "GPS longitude".to_string(),
            value: read_signed(lon) as f64 / GPS_LAT_LON_MULT,
            unit: "\u{b0}".to_string(),
        },
        TelemetryReading {
            channel,
            label: "GPS altitude".to_string(),
            value: read_signed(alt) as f64 / GPS_ALT_MULT,
            unit: "m".to_string(),
        },
    ])
}

/// Reads a big-endian unsigned integer of `bytes.len()` bytes (1-4).
fn read_unsigned(bytes: &[u8]) -> i64 {
    bytes.iter().fold(0i64, |acc, &b| (acc << 8) | b as i64)
}

/// Reads a big-endian, two's-complement signed integer of `bytes.len()`
/// bytes (1-4) -- matches `CayenneLPP.cpp`'s `addField`, which encodes a
/// negative value as `mask - magnitude + 1` (`mask = 2^(8*size)-1`), the
/// arithmetic form of two's-complement negation within that field width.
fn read_signed(bytes: &[u8]) -> i64 {
    let raw = read_unsigned(bytes);
    let bits = bytes.len() * 8;
    let sign_bit = 1i64 << (bits - 1);
    if raw & sign_bit != 0 {
        raw - (1i64 << bits)
    } else {
        raw
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_empty_payload_is_empty() {
        assert_eq!(decode(&[]), vec![]);
    }

    /// A repeater's `TELEM_CHANNEL_SELF` always reports battery voltage
    /// (and MCU temperature when supported) -- this is the shape a real
    /// `request_telemetry` response is expected to have.
    #[test]
    fn decode_voltage_and_temperature() {
        let raw = [
            0x01, 0x74, 0x01, 0x73, // channel 1, Voltage, 0x0173 = 371 -> 3.71V
            0x01, 0x67, 0x00, 0xF3, // channel 1, Temperature, 0x00F3 = 243 -> 24.3C
        ];
        let readings = decode(&raw);
        assert_eq!(readings.len(), 2);
        assert_eq!(readings[0].channel, 1);
        assert_eq!(readings[0].label, "Voltage");
        assert_eq!(readings[0].unit, "V");
        assert!((readings[0].value - 3.71).abs() < 1e-9);
        assert_eq!(readings[1].label, "Temperature");
        assert!((readings[1].value - 24.3).abs() < 1e-9);
    }

    /// Cross-checked independently (hand-computed two's complement, not
    /// just self-consistent with this module's own encoder -- there isn't
    /// one): -5.5C at 0.1C resolution is magnitude 55, negated within a
    /// 16-bit field is `0x10000 - 55 = 0xFFC9`.
    #[test]
    fn decode_negative_temperature() {
        let raw = [0x01, 0x67, 0xFF, 0xC9];
        let readings = decode(&raw);
        assert_eq!(readings.len(), 1);
        assert!((readings[0].value - (-5.5)).abs() < 1e-9);
    }

    /// Cross-checked independently: -12m at 1m resolution, negated within
    /// a 16-bit field is `0x10000 - 12 = 0xFFF4`.
    #[test]
    fn decode_negative_altitude() {
        let raw = [0x01, 0x79, 0xFF, 0xF4];
        let readings = decode(&raw);
        assert_eq!(readings.len(), 1);
        assert_eq!(readings[0].label, "Altitude");
        assert!((readings[0].value - (-12.0)).abs() < 1e-9);
    }

    /// Cross-checked independently: `GenericSensor` is a plain unsigned
    /// 4-byte field, multiplier 1 -- 12345 encodes as `0x00003039`.
    #[test]
    fn decode_generic_sensor_four_byte_field() {
        let raw = [0x01, 0x64, 0x00, 0x00, 0x30, 0x39];
        let readings = decode(&raw);
        assert_eq!(readings.len(), 1);
        assert_eq!(readings[0].label, "Generic sensor");
        assert_eq!(readings[0].value, 12345.0);
    }

    /// Cross-checked independently: lat/lon/alt each hand-converted via
    /// `CayenneLPP.cpp`'s `addGPS` formula (`value * multiplier`, 3-byte
    /// big-endian) -- 48.8566 / 2.3522 / 35.0 -> `0x077476` / `0x005BE2` /
    /// `0x000DAC`.
    #[test]
    fn decode_gps_matches_the_firmware_encoding_formula() {
        let raw = [
            0x01, 0x88, // channel 1, GPS
            0x07, 0x74, 0x76, // latitude
            0x00, 0x5B, 0xE2, // longitude
            0x00, 0x0D, 0xAC, // altitude
        ];
        let readings = decode(&raw);
        assert_eq!(readings.len(), 3);
        assert_eq!(readings[0].label, "GPS latitude");
        assert!((readings[0].value - 48.8566).abs() < 1e-4);
        assert_eq!(readings[1].label, "GPS longitude");
        assert!((readings[1].value - 2.3522).abs() < 1e-4);
        assert_eq!(readings[2].label, "GPS altitude");
        assert!((readings[2].value - 35.0).abs() < 1e-4);
        assert!(readings.iter().all(|r| r.channel == 1));
    }

    #[test]
    fn decode_truncated_payload_yields_no_readings() {
        let raw = [0x01, 0x74, 0x01]; // Voltage needs 2 data bytes, only 1 given
        assert_eq!(decode(&raw), vec![]);
    }

    #[test]
    fn decode_stops_at_a_type_meshcore_never_emits_keeping_prior_readings() {
        // 0x00 (DigitalInput) is a real CayenneLPP type, but MeshCore
        // firmware never emits it -- not in `TYPES`, so parsing stops
        // here rather than guessing at its shape.
        let mut raw = vec![0x01, 0x74, 0x01, 0x73]; // Voltage, decodes fine
        raw.extend_from_slice(&[0x00, 0x00, 0x00]); // channel 0, DigitalInput, value 0
        let readings = decode(&raw);
        assert_eq!(readings.len(), 1);
        assert_eq!(readings[0].label, "Voltage");
    }

    /// Regression test: the exact payload reported from a real repeater
    /// (`repeater telemetry` against actual hardware) that crashed the
    /// daemon under the previous `cayenne_lpp`-crate-based decoder --
    /// Voltage + Temperature (`TELEM_CHANNEL_SELF`) followed by four
    /// trailing zero bytes that are not real sensor data (buffer padding
    /// of unknown origin). `channel=0, type=0` isn't a recognized type,
    /// so decoding stops cleanly there with exactly the two genuine
    /// readings -- no panic, no spurious extra reading.
    #[test]
    fn decode_real_repeater_payload_ignores_trailing_zero_padding() {
        let raw = [
            0x01, 0x74, 0x01, 0x9b, // channel 1, Voltage, 4.11V
            0x01, 0x67, 0x01, 0x10, // channel 1, Temperature, 27.2C
            0x00, 0x00, 0x00, 0x00, // trailing padding (not real data)
        ];
        let readings = decode(&raw);
        assert_eq!(readings.len(), 2);
        assert_eq!(readings[0].label, "Voltage");
        assert!((readings[0].value - 4.11).abs() < 1e-9);
        assert_eq!(readings[1].label, "Temperature");
        assert!((readings[1].value - 27.2).abs() < 1e-9);
    }
}
