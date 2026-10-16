//! Register maps: describe the registers a request touched, in words.
//!
//! A map is a JSON file listing the registers of a device family, see
//! `registers/sigenergy.json` and `tools/gen_register_map.py`.

use std::collections::BTreeMap;

use serde::Deserialize;

use crate::modbus::{exception_name, parse, Transaction};
use crate::Result;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Kind {
    U16,
    S16,
    U32,
    S32,
    U64,
    String,
}

#[derive(Debug, Deserialize)]
struct Register {
    address: u16,
    count: u16,
    #[serde(rename = "type")]
    kind: Kind,
    gain: u32,
    #[serde(default)]
    unit: String,
    name: String,
    #[serde(default)]
    description: String,
}

impl Register {
    /// Number of registers the type occupies, `None` if any (strings).
    fn width(&self) -> Option<u16> {
        match self.kind {
            Kind::U16 | Kind::S16 => Some(1),
            Kind::U32 | Kind::S32 => Some(2),
            Kind::U64 => Some(4),
            Kind::String => None,
        }
    }

    /// Value of the register in `words` (big endian, high word first),
    /// scaled by the gain and with its unit.
    fn value(&self, words: &[u16]) -> String {
        let unsigned = words.iter().fold(0u64, |acc, w| acc << 16 | u64::from(*w));
        let raw = match self.kind {
            Kind::String => return format!("{:?}", text(words)),
            Kind::U16 | Kind::U32 | Kind::U64 => i128::from(unsigned),
            Kind::S16 => i128::from(unsigned as u16 as i16),
            Kind::S32 => i128::from(unsigned as u32 as i32),
        };
        let value = if self.gain == 1 {
            raw.to_string()
        } else {
            let decimals = f64::from(self.gain).log10().ceil() as usize;
            format!("{:.*}", decimals, raw as f64 / f64::from(self.gain))
        };
        if self.unit.is_empty() {
            value
        } else {
            format!("{} {}", value, self.unit)
        }
    }
}

fn text(words: &[u16]) -> String {
    let bytes: Vec<u8> = words.iter().flat_map(|w| w.to_be_bytes()).collect();
    String::from_utf8_lossy(&bytes)
        .trim_end_matches(['\0', ' '])
        .to_string()
}

#[derive(Debug, Deserialize)]
struct MapFile {
    registers: Vec<Register>,
}

#[derive(Debug)]
pub(crate) struct RegisterMap {
    registers: BTreeMap<u16, Register>,
}

impl RegisterMap {
    pub(crate) fn load(path: &str) -> Result<Self> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {}", path, e))?;
        let file: MapFile = serde_json::from_str(&text).map_err(|e| format!("{}: {}", path, e))?;
        let mut registers = BTreeMap::new();
        for register in file.registers {
            let address = register.address;
            if register.width().is_some_and(|w| w != register.count) || register.gain == 0 {
                return Err(
                    format!("{}: register {} has invalid count or gain", path, address).into(),
                );
            }
            if registers.insert(address, register).is_some() {
                return Err(format!("{}: register {} defined twice", path, address).into());
            }
        }
        Ok(Self { registers })
    }

    /// One line per known register touched by `request`, decoded from
    /// the values read or written.
    pub(crate) fn describe(&self, request: &[u8], reply: &[u8]) -> Vec<String> {
        let (unit, write, address, values) = match parse(request, reply) {
            None => return vec![],
            Some(Transaction::Exception {
                unit,
                function,
                code,
            }) => {
                return vec![format!(
                    "unit {} function {} failed: exception {} ({})",
                    unit,
                    function,
                    code,
                    exception_name(code)
                )]
            }
            Some(Transaction::Access {
                unit,
                write,
                address,
                values,
            }) => (unit, write, address, values),
        };
        let op = if write { "write" } else { "read" };
        let end = u32::from(address) + values.len() as u32;
        let lines: Vec<String> = self
            .registers
            .range(address..)
            .take_while(|(a, _)| u32::from(**a) < end)
            .map(|(a, register)| {
                let offset = usize::from(a - address);
                let value = values
                    .get(offset..offset + usize::from(register.count))
                    .map_or("<partial>".to_string(), |words| register.value(words));
                let mut line = format!("unit {} {} {} {} = {}", unit, op, a, register.name, value);
                if !register.description.is_empty() {
                    line = format!("{} ({})", line, register.description);
                }
                line
            })
            .collect();
        if lines.is_empty() {
            return vec![format!(
                "unit {} {} {}+{}: no known registers",
                unit,
                op,
                address,
                values.len()
            )];
        }
        lines
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modbus::tests::adu;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn map(json: &str) -> Result<RegisterMap> {
        static N: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "modbus-proxy-test-map-{}-{}.json",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::write(&path, json).unwrap();
        let result = RegisterMap::load(path.to_str().unwrap());
        std::fs::remove_file(path).unwrap();
        result
    }

    const MAP: &str = r#"{"registers": [
        {"address": 100, "count": 1, "type": "u16", "gain": 1, "name": "mode", "description": "Work mode"},
        {"address": 101, "count": 1, "type": "s16", "gain": 10, "unit": "°C", "name": "temp"},
        {"address": 102, "count": 2, "type": "u32", "gain": 100, "unit": "kWh", "name": "energy"},
        {"address": 104, "count": 2, "type": "s32", "gain": 1000, "unit": "kW", "name": "power"},
        {"address": 106, "count": 4, "type": "u64", "gain": 1, "name": "total"},
        {"address": 110, "count": 3, "type": "string", "gain": 1, "name": "serial", "extra": "ignored"}
    ]}"#;

    fn read(map: &RegisterMap, address: u16, data: &[u16]) -> Vec<String> {
        let request = adu(
            1,
            &[
                [3u8].as_slice(),
                &address.to_be_bytes(),
                &(data.len() as u16).to_be_bytes(),
            ]
            .concat(),
        );
        let bytes: Vec<u8> = data.iter().flat_map(|w| w.to_be_bytes()).collect();
        let reply = adu(1, &[[3u8, bytes.len() as u8].as_slice(), &bytes].concat());
        map.describe(&request, &reply)
    }

    #[test]
    fn decodes_all_types() {
        let map = map(MAP).unwrap();
        let lines = read(
            &map,
            100,
            &[
                7, 0xff38, 0, 12345, 0xffff, 0xfb2e, 0, 0, 1, 2, 0x5347, 0x3132, 0,
            ],
        );
        assert_eq!(
            lines,
            [
                "unit 1 read 100 mode = 7 (Work mode)",
                "unit 1 read 101 temp = -20.0 °C",
                "unit 1 read 102 energy = 123.45 kWh",
                "unit 1 read 104 power = -1.234 kW",
                "unit 1 read 106 total = 65538",
                "unit 1 read 110 serial = \"SG12\"",
            ]
        );
    }

    #[test]
    fn partial_and_unknown_ranges() {
        let map = map(MAP).unwrap();
        // energy only half read
        assert_eq!(
            read(&map, 102, &[0]),
            ["unit 1 read 102 energy = <partial>"]
        );
        // reserved gap
        assert_eq!(
            read(&map, 108, &[0, 0]),
            ["unit 1 read 108+2: no known registers"]
        );
    }

    #[test]
    fn describes_writes_exceptions_and_others() {
        let map = map(MAP).unwrap();
        let write = adu(1, &[6, 0, 100, 0, 2]);
        assert_eq!(
            map.describe(&write, &write),
            ["unit 1 write 100 mode = 2 (Work mode)"]
        );
        let request = adu(1, &[3, 0, 100, 0, 1]);
        assert_eq!(
            map.describe(&request, &adu(1, &[0x83, 2])),
            ["unit 1 function 3 failed: exception 2 (illegal data address)"]
        );
        let coils = adu(1, &[1, 0, 0, 0, 8]);
        assert!(map.describe(&coils, &adu(1, &[1, 1, 0])).is_empty());
    }

    #[test]
    fn load_errors() {
        let missing = RegisterMap::load("/nonexistent/map.json").unwrap_err();
        assert!(missing.to_string().starts_with("/nonexistent/map.json: "));
        assert!(map("not json").is_err());
        let err = map(
            r#"{"registers": [{"address": 1, "count": 1, "type": "u32", "gain": 1, "name": "x"}]}"#,
        );
        assert!(err
            .unwrap_err()
            .to_string()
            .contains("register 1 has invalid count or gain"));
        let err = map(
            r#"{"registers": [{"address": 1, "count": 1, "type": "u16", "gain": 0, "name": "x"}]}"#,
        );
        assert!(err
            .unwrap_err()
            .to_string()
            .contains("invalid count or gain"));
        let err = map(r#"{"registers": [
                {"address": 1, "count": 1, "type": "u16", "gain": 1, "name": "x"},
                {"address": 1, "count": 1, "type": "u16", "gain": 1, "name": "y"}]}"#);
        assert!(err
            .unwrap_err()
            .to_string()
            .contains("register 1 defined twice"));
    }

    #[test]
    fn sigenergy_map_loads() {
        let map = RegisterMap::load(crate::test_util::SIGENERGY_MAP).unwrap();
        assert_eq!(map.registers.len(), 358);
        // grid power -1.234 kW read from the plant
        let request = adu(247, &[3, 0x75, 0x35, 0, 2]);
        let reply = adu(247, &[3, 4, 0xff, 0xff, 0xfb, 0x2e]);
        assert_eq!(
            map.describe(&request, &reply),
            [
                "unit 247 read 30005 plant_grid_sensor_active_power = -1.234 kW \
              (Grid Active Power (>0 buy from grid; <0 sell to grid))"
            ]
        );
        // power factor is signed (corrected against the protocol PDF)
        let request = adu(1, &[3, 0x79, 0x2f, 0, 1]);
        let reply = adu(1, &[3, 2, 0xfc, 0x49]);
        assert_eq!(
            map.describe(&request, &reply),
            ["unit 1 read 31023 inverter_power_factor = -0.951 (Power Factor)"]
        );
    }
}
