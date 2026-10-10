//! ZIP archives: the container an `.xlsx` package lives in.
//!
//! Reading goes through the central directory, as the format intends: find
//! the end-of-central-directory record (EOCD) by scanning back over a
//! possible archive comment, list the entries, then reach each entry's data
//! through its local header. Local headers alone aren't trusted for sizes —
//! writers that stream set bit 3 and leave them zero.
//!
//! Supported: methods 0 (stored) and 8 (deflate), every entry's CRC-32
//! checked. ZIP64, multi-disk archives and encrypted entries are reported
//! rather than misread; spreadsheet packages never need the first two, and
//! Excel's own password protection wraps the package in an OLE compound file
//! instead (see [`OLE_MAGIC`]).
//!
//! Writing produces a plain single-disk archive with UTF-8 names and a fixed
//! 1980-01-01 timestamp, so the same cells always give the same bytes.

use super::XlsxError;
use super::crc32::crc32;
use super::deflate::deflate;
use super::inflate::{MAX_OUTPUT, inflate};

/// An OLE2 compound file — what Excel saves a password-protected workbook
/// as (an `EncryptedPackage` stream inside), and also legacy `.xls`.
pub const OLE_MAGIC: [u8; 8] = [0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1];

const LOCAL_SIG: u32 = 0x0403_4B50;
const CENTRAL_SIG: u32 = 0x0201_4B50;
const EOCD_SIG: u32 = 0x0605_4B50;
const ZIP64_LOCATOR_SIG: u32 = 0x0706_4B50;
const EOCD_LEN: usize = 22;

fn u16_at(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(at..at + 2)?.try_into().ok()?))
}

fn u32_at(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

fn corrupt(what: &str) -> XlsxError {
    XlsxError::Corrupt(format!("zip: {what}"))
}

/// One central-directory entry.
#[derive(Clone, Debug)]
pub struct Entry {
    pub name: String,
    flags: u16,
    method: u16,
    crc: u32,
    compressed: usize,
    size: usize,
    local_offset: usize,
}

/// A parsed archive borrowing the file's bytes; entries decompress on demand.
pub struct Archive<'a> {
    data: &'a [u8],
    pub entries: Vec<Entry>,
}

impl<'a> Archive<'a> {
    pub fn open(data: &'a [u8]) -> Result<Archive<'a>, XlsxError> {
        if data.starts_with(&OLE_MAGIC) {
            return Err(XlsxError::Encrypted);
        }
        let eocd = find_eocd(data).ok_or(XlsxError::NotZip)?;
        if eocd >= 20 && u32_at(data, eocd - 20) == Some(ZIP64_LOCATOR_SIG) {
            return Err(XlsxError::Unsupported("ZIP64 archive".into()));
        }
        let field16 = |o| u16_at(data, eocd + o).unwrap_or(0);
        let field32 = |o| u32_at(data, eocd + o).unwrap_or(0);
        let (disk, cd_disk) = (field16(4), field16(6));
        let (on_disk, count) = (field16(8), field16(10));
        let (cd_size, cd_offset) = (field32(12), field32(16));
        if count == 0xFFFF || cd_size == 0xFFFF_FFFF || cd_offset == 0xFFFF_FFFF {
            return Err(XlsxError::Unsupported("ZIP64 archive".into()));
        }
        if disk != 0 || cd_disk != 0 || on_disk != count {
            return Err(XlsxError::Unsupported("multi-disk archive".into()));
        }
        let cd_start = cd_offset as usize;
        let cd_end = cd_start
            .checked_add(cd_size as usize)
            .filter(|&e| e <= eocd)
            .ok_or_else(|| corrupt("central directory out of range"))?;
        let cd = &data[cd_start..cd_end];
        let mut entries = Vec::with_capacity(count as usize);
        let mut at = 0;
        for _ in 0..count {
            let field16 =
                |o| u16_at(cd, at + o).ok_or_else(|| corrupt("truncated central directory"));
            let field32 =
                |o| u32_at(cd, at + o).ok_or_else(|| corrupt("truncated central directory"));
            if field32(0)? != CENTRAL_SIG {
                return Err(corrupt("bad central directory signature"));
            }
            let flags = field16(8)?;
            let method = field16(10)?;
            let crc = field32(16)?;
            let compressed = field32(20)?;
            let size = field32(24)?;
            let name_len = field16(28)? as usize;
            let extra_len = field16(30)? as usize;
            let comment_len = field16(32)? as usize;
            let local_offset = field32(42)?;
            if compressed == 0xFFFF_FFFF || size == 0xFFFF_FFFF || local_offset == 0xFFFF_FFFF {
                return Err(XlsxError::Unsupported("ZIP64 archive".into()));
            }
            let name = cd
                .get(at + 46..at + 46 + name_len)
                .ok_or_else(|| corrupt("truncated entry name"))?;
            // Names are UTF-8 when flag bit 11 says so and, in practice, in
            // every Office package; anything else is lossily decoded.
            let name = String::from_utf8_lossy(name).replace('\\', "/");
            entries.push(Entry {
                name,
                flags,
                method,
                crc,
                compressed: compressed as usize,
                size: size as usize,
                local_offset: local_offset as usize,
            });
            at += 46 + name_len + extra_len + comment_len;
        }
        Ok(Archive { data, entries })
    }

    /// The entry named `name`. Package part names are case-insensitive
    /// (OPC), so an exact match is preferred but any casing is accepted.
    pub fn find(&self, name: &str) -> Option<&Entry> {
        let name = name.trim_start_matches('/');
        self.entries.iter().find(|e| e.name == name).or_else(|| {
            self.entries
                .iter()
                .find(|e| e.name.eq_ignore_ascii_case(name))
        })
    }

    /// Decompress an entry and verify its size and CRC-32.
    pub fn read(&self, e: &Entry) -> Result<Vec<u8>, XlsxError> {
        if e.flags & 1 != 0 {
            return Err(XlsxError::Encrypted);
        }
        let lh = e.local_offset;
        if u32_at(self.data, lh) != Some(LOCAL_SIG) {
            return Err(corrupt(&format!("bad local header for {}", e.name)));
        }
        let name_len = u16_at(self.data, lh + 26).unwrap_or(0) as usize;
        let extra_len = u16_at(self.data, lh + 28).unwrap_or(0) as usize;
        let start = lh + 30 + name_len + extra_len;
        let raw = start
            .checked_add(e.compressed)
            .and_then(|end| self.data.get(start..end))
            .ok_or_else(|| corrupt(&format!("{} runs past the end of the file", e.name)))?;
        if e.size > MAX_OUTPUT {
            return Err(XlsxError::Unsupported(format!(
                "{} is larger than 1 GiB",
                e.name
            )));
        }
        let out = match e.method {
            0 => raw.to_vec(),
            8 => inflate(raw, e.size).map_err(|err| corrupt(&format!("{}: {err}", e.name)))?,
            m => {
                return Err(XlsxError::Unsupported(format!(
                    "compression method {m} ({})",
                    e.name
                )));
            }
        };
        if out.len() != e.size {
            return Err(corrupt(&format!("{}: size mismatch", e.name)));
        }
        if crc32(&out) != e.crc {
            return Err(corrupt(&format!("{}: CRC-32 mismatch", e.name)));
        }
        Ok(out)
    }
}

/// The offset of the EOCD record. It sits at the very end unless the archive
/// has a comment (up to 64 KiB), so scan back for a signature whose comment
/// length accounts for exactly the bytes that follow — falling back to one
/// that at least fits, for writers that pad the file.
fn find_eocd(data: &[u8]) -> Option<usize> {
    if data.len() < EOCD_LEN {
        return None;
    }
    let lowest = data.len().saturating_sub(EOCD_LEN + 0xFFFF);
    let mut loose = None;
    for at in (lowest..=data.len() - EOCD_LEN).rev() {
        if u32_at(data, at) != Some(EOCD_SIG) {
            continue;
        }
        let comment = u16_at(data, at + 20)? as usize;
        let end = at + EOCD_LEN + comment;
        if end == data.len() {
            return Some(at);
        }
        if end < data.len() && loose.is_none() {
            loose = Some(at);
        }
    }
    loose
}

/// Builds an archive in memory.
pub struct Writer {
    out: Vec<u8>,
    central: Vec<u8>,
    count: u16,
}

impl Default for Writer {
    fn default() -> Writer {
        Writer::new()
    }
}

/// DOS date for 1980-01-01 (the format's epoch); time 00:00:00.
const DOS_DATE: u16 = 1 << 5 | 1;

impl Writer {
    pub fn new() -> Writer {
        Writer {
            out: Vec::new(),
            central: Vec::new(),
            count: 0,
        }
    }

    /// Add a file, deflated when that makes it smaller, stored otherwise.
    ///
    /// Sizes and offsets are 32-bit (no ZIP64); a package of one sheet
    /// within Excel's own row and column limits stays far below that.
    pub fn add(&mut self, name: &str, data: &[u8]) {
        let crc = crc32(data);
        let packed = deflate(data);
        let (method, body): (u16, &[u8]) = if packed.len() < data.len() {
            (8, &packed)
        } else {
            (0, data)
        };
        let offset = self.out.len() as u32;
        let flags: u16 = 1 << 11; // names are UTF-8
        let mut common = Vec::with_capacity(26);
        common.extend_from_slice(&20u16.to_le_bytes()); // version needed: 2.0
        common.extend_from_slice(&flags.to_le_bytes());
        common.extend_from_slice(&method.to_le_bytes());
        common.extend_from_slice(&0u16.to_le_bytes()); // time
        common.extend_from_slice(&DOS_DATE.to_le_bytes());
        common.extend_from_slice(&crc.to_le_bytes());
        common.extend_from_slice(&(body.len() as u32).to_le_bytes());
        common.extend_from_slice(&(data.len() as u32).to_le_bytes());
        common.extend_from_slice(&(name.len() as u16).to_le_bytes());
        common.extend_from_slice(&0u16.to_le_bytes()); // extra length

        self.out.extend_from_slice(&LOCAL_SIG.to_le_bytes());
        self.out.extend_from_slice(&common);
        self.out.extend_from_slice(name.as_bytes());
        self.out.extend_from_slice(body);

        self.central.extend_from_slice(&CENTRAL_SIG.to_le_bytes());
        self.central.extend_from_slice(&20u16.to_le_bytes()); // made by: DOS, 2.0
        self.central.extend_from_slice(&common);
        self.central.extend_from_slice(&0u16.to_le_bytes()); // comment length
        self.central.extend_from_slice(&0u16.to_le_bytes()); // disk
        self.central.extend_from_slice(&0u16.to_le_bytes()); // internal attributes
        self.central.extend_from_slice(&0u32.to_le_bytes()); // external attributes
        self.central.extend_from_slice(&offset.to_le_bytes());
        self.central.extend_from_slice(name.as_bytes());
        self.count += 1;
    }

    pub fn finish(mut self) -> Vec<u8> {
        let cd_offset = self.out.len() as u32;
        let cd_size = self.central.len() as u32;
        self.out.append(&mut self.central);
        self.out.extend_from_slice(&EOCD_SIG.to_le_bytes());
        self.out.extend_from_slice(&[0; 4]); // this disk, central directory disk
        self.out.extend_from_slice(&self.count.to_le_bytes());
        self.out.extend_from_slice(&self.count.to_le_bytes());
        self.out.extend_from_slice(&cd_size.to_le_bytes());
        self.out.extend_from_slice(&cd_offset.to_le_bytes());
        self.out.extend_from_slice(&0u16.to_le_bytes()); // comment length
        self.out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DEFLATED: &[u8] = include_bytes!("testdata/zip/deflated.zip");
    const STORED_COMMENT: &[u8] = include_bytes!("testdata/zip/stored_comment.zip");

    fn read_all(data: &[u8]) -> Vec<(String, Vec<u8>)> {
        let a = Archive::open(data).unwrap();
        a.entries
            .iter()
            .map(|e| (e.name.clone(), a.read(e).unwrap()))
            .collect()
    }

    #[test]
    fn reads_python_deflated_archive() {
        let files = read_all(DEFLATED);
        let names: Vec<&str> = files.iter().map(|f| f.0.as_str()).collect();
        assert_eq!(
            names,
            ["hello.txt", "dir/big.txt", "dir/empty.txt", "unicodé.txt"]
        );
        assert_eq!(files[0].1, b"Hello, zip!\n");
        assert_eq!(files[1].1.len(), 200_000);
        assert!(files[1].1.starts_with(b"line 0\nline 1\n"));
        assert!(files[2].1.is_empty());
        assert_eq!(files[3].1, "naïve ☃\n".as_bytes());
    }

    #[test]
    fn reads_stored_archive_with_trailing_comment() {
        let files = read_all(STORED_COMMENT);
        assert_eq!(files.len(), 2);
        assert_eq!(files[0], ("a.txt".into(), b"alpha".to_vec()));
        assert_eq!(files[1], ("b.txt".into(), b"beta".to_vec()));
    }

    #[test]
    fn find_is_case_insensitive() {
        let a = Archive::open(DEFLATED).unwrap();
        assert!(a.find("DIR/BIG.TXT").is_some());
        assert!(a.find("/hello.txt").is_some());
        assert!(a.find("nope").is_none());
    }

    #[test]
    fn crc_mismatch_detected() {
        let mut data = STORED_COMMENT.to_vec();
        let at = data.windows(5).position(|w| w == b"alpha").unwrap();
        data[at] = b'A';
        let a = Archive::open(&data).unwrap();
        let err = a.read(&a.entries[0]).unwrap_err();
        assert!(
            matches!(&err, XlsxError::Corrupt(m) if m.contains("CRC")),
            "{err}"
        );
    }

    #[test]
    fn encrypted_and_foreign_inputs() {
        let mut ole = OLE_MAGIC.to_vec();
        ole.extend_from_slice(&[0; 600]);
        assert!(matches!(Archive::open(&ole), Err(XlsxError::Encrypted)));
        assert!(matches!(Archive::open(b""), Err(XlsxError::NotZip)));
        assert!(matches!(
            Archive::open(b"a,b,c\n1,2,3\n"),
            Err(XlsxError::NotZip)
        ));

        // Set the "encrypted" flag on the first entry in both headers.
        let mut data = STORED_COMMENT.to_vec();
        data[6] |= 1;
        let cd = data
            .windows(4)
            .position(|w| w == CENTRAL_SIG.to_le_bytes())
            .unwrap();
        data[cd + 8] |= 1;
        let a = Archive::open(&data).unwrap();
        assert!(matches!(a.read(&a.entries[0]), Err(XlsxError::Encrypted)));
    }

    #[test]
    fn zip64_reported() {
        let mut data = STORED_COMMENT.to_vec();
        let eocd = find_eocd(&data).unwrap();
        data[eocd + 8..eocd + 12].copy_from_slice(&[0xFF; 4]);
        assert!(
            matches!(Archive::open(&data), Err(XlsxError::Unsupported(m)) if m.contains("ZIP64"))
        );
    }

    #[test]
    fn writer_round_trip() {
        let mut w = Writer::new();
        let big: Vec<u8> = b"<c r=\"A1\"><v>1</v></c>".repeat(500);
        w.add("a/b.xml", &big);
        w.add("tiny", b"x");
        w.add("empty", b"");
        w.add("ünï.txt", "snow ☃".as_bytes());
        let bytes = w.finish();
        let files = read_all(&bytes);
        assert_eq!(files[0], ("a/b.xml".into(), big.clone()));
        assert_eq!(files[1], ("tiny".into(), b"x".to_vec()));
        assert_eq!(files[2], ("empty".into(), vec![]));
        assert_eq!(files[3], ("ünï.txt".into(), "snow ☃".as_bytes().to_vec()));
        // The XML-like entry was deflated, the tiny one stored.
        let a = Archive::open(&bytes).unwrap();
        assert_eq!(a.entries[0].method, 8);
        assert_eq!(a.entries[1].method, 0);
        assert!(bytes.len() < big.len() / 4);
    }

    #[test]
    fn mutated_archives_never_panic() {
        let mut rng = 0xDEAD_BEEFu64;
        for base in [DEFLATED, STORED_COMMENT] {
            for _ in 0..2000 {
                let mut d = base.to_vec();
                for _ in 0..4 {
                    rng ^= rng << 13;
                    rng ^= rng >> 7;
                    rng ^= rng << 17;
                    let i = (rng % d.len() as u64) as usize;
                    d[i] = (rng >> 40) as u8;
                }
                if let Ok(a) = Archive::open(&d) {
                    for e in &a.entries {
                        let _ = a.read(e);
                    }
                }
            }
        }
    }
}
