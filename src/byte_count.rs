use std::fmt::{self, Write};

pub struct ByteCount { bytes: u64 }

impl From<u64> for ByteCount {
    fn from(value: u64) -> Self {
        return ByteCount { bytes: value };
    }
}

impl fmt::Display for ByteCount {
    fn fmt(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        const UNITS: [char; 7] = ['K', 'M', 'G', 'T', 'P', 'E', 'Z'];

        // we start at kibibytes
        let mut bytes = self.bytes as f64 / 1024.0;
        let mut n: usize = 0;
        while bytes >= 1024.0 || n == UNITS.len() {
            bytes /= 1024.0;
            n += 1;
        }

        bytes.fmt(formatter)?;
        formatter.write_char(UNITS[n])?;
        return Ok(());
    }
}
