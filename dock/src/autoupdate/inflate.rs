//! A small DEFLATE (RFC 1951) decoder for the update zip, after Mark
//! Adler's puff.c: stored, fixed and dynamic Huffman blocks. Every read is
//! bounds-checked (bad input is an error, never a panic) and the output
//! may not grow past `max_out`. The zip has already passed its SHA-256
//! check when this runs; the CRC-32 of each file is checked after it.

const MAXBITS: usize = 15;
const MAXLCODES: usize = 286;
const MAXDCODES: usize = 30;
const FIXLCODES: usize = 288;

struct Input<'a> {
    data: &'a [u8],
    pos: usize,
    bitbuf: u32,
    bitcnt: u32,
}

impl Input<'_> {
    fn bits(&mut self, need: u32) -> Result<u32, String> {
        let mut val = self.bitbuf;
        while self.bitcnt < need {
            let b = *self.data.get(self.pos).ok_or("deflate: input ends early")?;
            self.pos += 1;
            val |= (b as u32) << self.bitcnt;
            self.bitcnt += 8;
        }
        self.bitbuf = if need >= 32 { 0 } else { val >> need };
        self.bitcnt -= need;
        Ok(if need >= 32 { val } else { val & ((1u32 << need) - 1) })
    }
}

struct Huffman {
    count: [u16; MAXBITS + 1],
    symbol: Vec<u16>,
}

impl Huffman {
    /// Builds the decoding tables from code lengths. Returns the table and
    /// how many codes are left over: 0 = complete, > 0 = incomplete, < 0 =
    /// over-subscribed.
    fn new(lengths: &[u16]) -> (Huffman, i32) {
        let mut h = Huffman { count: [0; MAXBITS + 1], symbol: vec![0; lengths.len()] };
        for &l in lengths {
            h.count[l as usize] += 1;
        }
        if h.count[0] as usize == lengths.len() {
            return (h, 0);
        }
        let mut left: i32 = 1;
        for len in 1..=MAXBITS {
            left <<= 1;
            left -= h.count[len] as i32;
            if left < 0 {
                return (h, left);
            }
        }
        let mut offs = [0u16; MAXBITS + 1];
        for len in 1..MAXBITS {
            offs[len + 1] = offs[len] + h.count[len];
        }
        for (sym, &l) in lengths.iter().enumerate() {
            if l != 0 {
                h.symbol[offs[l as usize] as usize] = sym as u16;
                offs[l as usize] += 1;
            }
        }
        (h, left)
    }

    fn decode(&self, inp: &mut Input) -> Result<u16, String> {
        let (mut code, mut first, mut index) = (0i32, 0i32, 0i32);
        for len in 1..=MAXBITS {
            code |= inp.bits(1)? as i32;
            let count = self.count[len] as i32;
            if code - count < first {
                return self.symbol.get((index + (code - first)) as usize).copied().ok_or_else(|| "deflate: bad code".to_string());
            }
            index += count;
            first += count;
            first <<= 1;
            code <<= 1;
        }
        Err("deflate: ran out of codes".into())
    }
}

const LBASE: [u16; 29] = [3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131, 163, 195, 227, 258];
const LEXT: [u8; 29] = [0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0];
const DBASE: [u16; 30] = [1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537, 2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577];
const DEXT: [u8; 30] = [0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13, 13];

fn push(out: &mut Vec<u8>, b: u8, max_out: usize) -> Result<(), String> {
    if out.len() >= max_out {
        return Err(format!("deflate: output over {max_out} bytes"));
    }
    out.push(b);
    Ok(())
}

fn codes(inp: &mut Input, out: &mut Vec<u8>, max_out: usize, lencode: &Huffman, distcode: &Huffman) -> Result<(), String> {
    loop {
        let sym = lencode.decode(inp)? as usize;
        if sym < 256 {
            push(out, sym as u8, max_out)?;
        } else if sym == 256 {
            return Ok(());
        } else {
            let sym = sym - 257;
            if sym >= 29 {
                return Err("deflate: bad length symbol".into());
            }
            let len = LBASE[sym] as usize + inp.bits(LEXT[sym] as u32)? as usize;
            let dsym = distcode.decode(inp)? as usize;
            if dsym >= 30 {
                return Err("deflate: bad distance symbol".into());
            }
            let dist = DBASE[dsym] as usize + inp.bits(DEXT[dsym] as u32)? as usize;
            if dist > out.len() {
                return Err("deflate: distance too far back".into());
            }
            if out.len() + len > max_out {
                return Err(format!("deflate: output over {max_out} bytes"));
            }
            let start = out.len() - dist;
            // Byte by byte: the copy may overlap what it writes.
            for i in 0..len {
                let b = out[start + i];
                out.push(b);
            }
        }
    }
}

fn stored(inp: &mut Input, out: &mut Vec<u8>, max_out: usize) -> Result<(), String> {
    // Discard the bits left in the current byte.
    inp.bitbuf = 0;
    inp.bitcnt = 0;
    let hdr = inp.data.get(inp.pos..inp.pos + 4).ok_or("deflate: stored header cut short")?;
    let len = u16::from_le_bytes([hdr[0], hdr[1]]);
    let nlen = u16::from_le_bytes([hdr[2], hdr[3]]);
    if len != !nlen {
        return Err("deflate: stored length check failed".into());
    }
    inp.pos += 4;
    let data = inp.data.get(inp.pos..inp.pos + len as usize).ok_or("deflate: stored block cut short")?;
    if out.len() + data.len() > max_out {
        return Err(format!("deflate: output over {max_out} bytes"));
    }
    out.extend_from_slice(data);
    inp.pos += len as usize;
    Ok(())
}

fn fixed(inp: &mut Input, out: &mut Vec<u8>, max_out: usize) -> Result<(), String> {
    let mut lengths = [0u16; FIXLCODES];
    for (sym, l) in lengths.iter_mut().enumerate() {
        *l = match sym {
            0..=143 => 8,
            144..=255 => 9,
            256..=279 => 7,
            _ => 8,
        };
    }
    let (lencode, _) = Huffman::new(&lengths);
    let (distcode, _) = Huffman::new(&[5u16; MAXDCODES]);
    codes(inp, out, max_out, &lencode, &distcode)
}

fn dynamic(inp: &mut Input, out: &mut Vec<u8>, max_out: usize) -> Result<(), String> {
    const ORDER: [usize; 19] = [16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15];
    let nlen = inp.bits(5)? as usize + 257;
    let ndist = inp.bits(5)? as usize + 1;
    let ncode = inp.bits(4)? as usize + 4;
    if nlen > MAXLCODES || ndist > MAXDCODES {
        return Err("deflate: too many codes".into());
    }
    let mut lengths = [0u16; MAXLCODES + MAXDCODES];
    for &o in ORDER.iter().take(ncode) {
        lengths[o] = inp.bits(3)? as u16;
    }
    let (lencode, left) = Huffman::new(&lengths[..19]);
    if left != 0 {
        return Err("deflate: incomplete code-length code".into());
    }
    let mut index = 0;
    while index < nlen + ndist {
        let sym = lencode.decode(inp)?;
        if sym < 16 {
            lengths[index] = sym;
            index += 1;
            continue;
        }
        let (len, repeat) = match sym {
            16 => {
                if index == 0 {
                    return Err("deflate: repeat with no previous length".into());
                }
                (lengths[index - 1], 3 + inp.bits(2)? as usize)
            }
            17 => (0, 3 + inp.bits(3)? as usize),
            _ => (0, 11 + inp.bits(7)? as usize),
        };
        if index + repeat > nlen + ndist {
            return Err("deflate: too many lengths".into());
        }
        for l in &mut lengths[index..index + repeat] {
            *l = len;
        }
        index += repeat;
    }
    if lengths[256] == 0 {
        return Err("deflate: no end-of-block code".into());
    }
    let (lencode, left) = Huffman::new(&lengths[..nlen]);
    if left < 0 || (left > 0 && nlen - lencode.count[0] as usize != 1) {
        return Err("deflate: bad literal/length code".into());
    }
    let (distcode, left) = Huffman::new(&lengths[nlen..nlen + ndist]);
    if left < 0 || (left > 0 && ndist - distcode.count[0] as usize != 1) {
        return Err("deflate: bad distance code".into());
    }
    codes(inp, out, max_out, &lencode, &distcode)
}

/// Inflates a raw DEFLATE stream; at most `max_out` bytes come out.
pub fn inflate(data: &[u8], max_out: usize) -> Result<Vec<u8>, String> {
    let mut inp = Input { data, pos: 0, bitbuf: 0, bitcnt: 0 };
    let mut out = Vec::new();
    loop {
        let last = inp.bits(1)?;
        match inp.bits(2)? {
            0 => stored(&mut inp, &mut out, max_out)?,
            1 => fixed(&mut inp, &mut out, max_out)?,
            2 => dynamic(&mut inp, &mut out, max_out)?,
            _ => return Err("deflate: bad block type".into()),
        }
        if last == 1 {
            return Ok(out);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stored_blocks() {
        // Two stored blocks: "ab" (not last), "c" (last).
        let data = [0x00, 0x02, 0x00, 0xfd, 0xff, b'a', b'b', 0x01, 0x01, 0x00, 0xfe, 0xff, b'c'];
        assert_eq!(inflate(&data, 100).unwrap(), b"abc");
        assert!(inflate(&data, 2).is_err(), "output cap");
        // NLEN that does not match.
        assert!(inflate(&[0x01, 0x02, 0x00, 0x00, 0x00, b'a', b'b'], 100).is_err());
        // Cut short.
        assert!(inflate(&[0x01, 0x05, 0x00, 0xfa, 0xff, b'a'], 100).is_err());
    }

    #[test]
    fn fixed_huffman() {
        // .NET DeflateStream of "hello hello hello hello" (fixed codes,
        // with a back reference).
        let data = [0xcb, 0x48, 0xcd, 0xc9, 0xc9, 0x57, 0xc8, 0x40, 0x27, 0x01];
        assert_eq!(inflate(&data, 100).unwrap(), b"hello hello hello hello");
        assert!(inflate(&data, 10).is_err(), "output cap");
    }

    #[test]
    fn garbage_is_an_error_not_a_panic() {
        let mut seed = 0x1234_5678u32;
        for n in 0..2000 {
            let len = (n % 64) + 1;
            let data: Vec<u8> = (0..len)
                .map(|_| {
                    seed ^= seed << 13;
                    seed ^= seed >> 17;
                    seed ^= seed << 5;
                    seed as u8
                })
                .collect();
            let _ = inflate(&data, 4096);
        }
        assert!(inflate(&[], 10).is_err());
        assert!(inflate(&[0x07], 10).is_err(), "block type 3");
    }
}
