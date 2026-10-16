//! Modbus PDU decoding, used to describe what a request/reply pair did.

/// What a request/reply pair did to the registers of a device.
#[derive(Debug, PartialEq)]
pub(crate) enum Transaction {
    /// Registers read (functions 3 and 4) or written (6 and 16).
    Access {
        unit: u8,
        write: bool,
        address: u16,
        values: Vec<u16>,
    },
    /// The device rejected the request.
    Exception { unit: u8, function: u8, code: u8 },
}

fn word(frame: &[u8], index: usize) -> Option<u16> {
    Some(u16::from_be_bytes([
        *frame.get(index)?,
        *frame.get(index + 1)?,
    ]))
}

fn words(bytes: &[u8]) -> Vec<u16> {
    bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|w| u16::from_be_bytes(*w))
        .collect()
}

/// Decode a request frame and its reply frame (MBAP header included).
/// `None` for functions that don't access registers and malformed frames.
pub(crate) fn parse(request: &[u8], reply: &[u8]) -> Option<Transaction> {
    let unit = *request.get(6)?;
    let function = *reply.get(7)?;
    if function & 0x80 != 0 {
        return Some(Transaction::Exception {
            unit,
            function: function & 0x7f,
            code: *reply.get(8)?,
        });
    }
    let address = word(request, 8)?;
    let (write, values) = match function {
        3 | 4 => (false, words(reply.get(9..9 + *reply.get(8)? as usize)?)),
        6 => (true, vec![word(request, 10)?]),
        16 => (true, words(request.get(13..)?)),
        _ => return None,
    };
    Some(Transaction::Access {
        unit,
        write,
        address,
        values,
    })
}

pub(crate) fn exception_name(code: u8) -> &'static str {
    match code {
        1 => "illegal function",
        2 => "illegal data address",
        3 => "illegal data value",
        4 => "server device failure",
        5 => "acknowledge",
        6 => "server device busy",
        8 => "memory parity error",
        10 => "gateway path unavailable",
        11 => "gateway target device failed to respond",
        _ => "unknown exception",
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Frame with MBAP header (transaction 1) for `unit` and `pdu`.
    pub(crate) fn adu(unit: u8, pdu: &[u8]) -> Vec<u8> {
        let mut f = vec![0, 1, 0, 0];
        f.extend(((pdu.len() + 1) as u16).to_be_bytes());
        f.push(unit);
        f.extend(pdu);
        f
    }

    #[test]
    fn read_registers() {
        for function in [3, 4] {
            let request = adu(247, &[function, 0x75, 0x30, 0, 2]);
            let reply = adu(247, &[function, 4, 0, 1, 0xff, 0xfe]);
            assert_eq!(
                parse(&request, &reply),
                Some(Transaction::Access {
                    unit: 247,
                    write: false,
                    address: 30000,
                    values: vec![1, 0xfffe],
                })
            );
        }
    }

    #[test]
    fn write_single_register() {
        let request = adu(1, &[6, 0x9c, 0x40, 0, 1]);
        assert_eq!(
            parse(&request, &request),
            Some(Transaction::Access {
                unit: 1,
                write: true,
                address: 40000,
                values: vec![1],
            })
        );
    }

    #[test]
    fn write_multiple_registers() {
        let request = adu(1, &[16, 0x9c, 0x41, 0, 2, 4, 0, 0, 0x13, 0x88]);
        let reply = adu(1, &[16, 0x9c, 0x41, 0, 2]);
        assert_eq!(
            parse(&request, &reply),
            Some(Transaction::Access {
                unit: 1,
                write: true,
                address: 40001,
                values: vec![0, 5000],
            })
        );
    }

    #[test]
    fn exception() {
        let request = adu(1, &[3, 0, 0, 0, 1]);
        let reply = adu(1, &[0x83, 2]);
        assert_eq!(
            parse(&request, &reply),
            Some(Transaction::Exception {
                unit: 1,
                function: 3,
                code: 2,
            })
        );
    }

    #[test]
    fn unsupported_and_malformed() {
        let read = adu(1, &[3, 0, 0, 0, 1]);
        // other function codes
        assert_eq!(parse(&adu(1, &[1, 0, 0, 0, 8]), &adu(1, &[1, 1, 0])), None);
        // truncated frames
        assert_eq!(parse(&read[..6], &read), None);
        assert_eq!(parse(&read, &read[..7]), None);
        assert_eq!(parse(&read, &adu(1, &[0x83])), None);
        assert_eq!(parse(&read[..8], &adu(1, &[3, 2, 0, 1])), None);
        assert_eq!(parse(&read[..9], &adu(1, &[3, 2, 0, 1])), None);
        assert_eq!(parse(&read, &adu(1, &[3])), None);
        assert_eq!(parse(&read, &adu(1, &[3, 4, 0, 1])), None);
        assert_eq!(parse(&adu(1, &[6, 0, 0, 0]), &adu(1, &[6])), None);
        assert_eq!(parse(&adu(1, &[16, 0, 0, 0]), &adu(1, &[16])), None);
    }

    #[test]
    fn exception_names() {
        let names: Vec<_> = (0..=11).map(exception_name).collect();
        assert_eq!(names[2], "illegal data address");
        assert_eq!(names[0], "unknown exception");
        assert_eq!(
            names.iter().filter(|n| **n != "unknown exception").count(),
            9
        );
    }
}
