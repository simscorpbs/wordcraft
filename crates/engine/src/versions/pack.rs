//! How versions of a zip package (`.docx`, `.odt`…) are stored without keeping every copy whole.
//!
//! A `.docx` is a zip archive whose parts are compressed one by one, so changing one letter
//! changes most of the file's bytes, and a byte diff of two saves is nearly as big as the file.
//! The history therefore opens the archive and keeps each part as its own object, named by the
//! SHA-256 of its content, as git names its blobs: a picture, a font or a style part that did not
//! change between two saves is stored once and costs nothing more. A part that did change (the
//! body text in `word/document.xml`, say) is stored as a delta against the same part in the
//! previous version, compressed; every [`KEYFRAME_INTERVAL`] links, or when the delta is not much
//! smaller than the part, the part is stored whole again, so restoring never walks a long chain
//! and one damaged object only affects the versions that lead through it.
//!
//! A version is then a manifest: the parts in archive order, with their names, compression
//! method, dates and content hashes. Opening the version builds the archive again. The bytes of
//! the rebuilt zip may differ from the file that was saved (the parts are compressed again), but
//! every part's content is checked against its hash, so the document is the same.
//!
//! Nothing here touches the file system; `versions.rs` stores the objects.

use serde::{Deserialize, Serialize};
use std::io::{Cursor, Read, Write};

/// Most parts in one archive that are kept as separate objects. A bigger archive (or a zip
/// bomb's millions of entries) is kept whole, as before.
pub(crate) const MAX_PARTS: usize = 10_000;
/// Largest uncompressed part kept as an object.
pub(crate) const MAX_PART_BYTES: u64 = 256 << 20;
/// Largest total uncompressed size of an archive kept as objects: the declared sizes are
/// checked before anything is inflated, and every read is capped as well.
pub(crate) const MAX_UNPACKED_BYTES: u64 = 1 << 30;
/// A changed part is stored whole again after this many deltas in a row (git's default depth
/// is 50; a shorter chain keeps restores fast and limits what one damaged object can affect).
pub(crate) const KEYFRAME_INTERVAL: u16 = 10;
/// Longest chain an object file may claim, even a hostile one.
pub(crate) const MAX_CHAIN: usize = 64;
/// A delta is kept only when it is at most this fraction (numerator, denominator) of the part
/// stored whole and compressed.
const DELTA_FRACTION: (usize, usize) = (1, 2);
/// Block size the delta encoder matches on.
const BLOCK: usize = 32;
/// Compression level for objects (miniz's default balance of speed and size).
const LEVEL: u8 = 6;

const MAGIC: &[u8; 4] = b"WCVO";
const OBJECT_VERSION: u8 = 1;
const KIND_FULL: u8 = 0;
const KIND_DELTA: u8 = 1;
const HEADER_FULL: usize = 24;
const HEADER_DELTA: usize = HEADER_FULL + 32;
/// How many bytes of an object file [`ObjectHeader::parse`] needs.
pub(crate) const HEADER_MAX: usize = HEADER_DELTA;

/// How a part was compressed inside the archive.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Method {
    Stored,
    Deflated,
}

/// One part of the archive as the manifest records it.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ManifestPart {
    pub name: String,
    pub method: Method,
    /// The MS-DOS date and time fields of the entry, kept as they were.
    pub date: u16,
    pub time: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<u32>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub dir: bool,
    pub size: u64,
    /// SHA-256 of the uncompressed content: the object's name.
    pub sha256: String,
}

/// A version stored as parts. It also carries the version's metadata, so a lost index can be
/// rebuilt from the manifests.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Manifest {
    pub format: u32,
    pub info: super::VersionInfo,
    /// The archive comment, as hex.
    #[serde(default)]
    pub comment: String,
    pub parts: Vec<ManifestPart>,
}

/// A part read from an archive, with its content.
pub(crate) struct Part {
    pub meta: ManifestPart,
    pub data: Vec<u8>,
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) {
        return None;
    }
    (0..text.len()).step_by(2).map(|i| text.get(i..i + 2).and_then(|pair| u8::from_str_radix(pair, 16).ok())).collect()
}

/// Is `text` a SHA-256 in lower-case hex (an object name)?
pub(crate) fn hash_ok(text: &str) -> bool {
    text.len() == 64 && text.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// Split a zip archive into its parts. `None` when `bytes` is not a zip this module keeps as
/// parts: not a zip, encrypted entries, a compression other than stored or deflated, duplicate
/// names, or more than [`MAX_PARTS`] entries, [`MAX_PART_BYTES`] in one part or
/// [`MAX_UNPACKED_BYTES`] in all. Such a file is kept whole.
pub(crate) fn split_zip(bytes: &[u8]) -> Option<(Vec<Part>, String)> {
    split_zip_within(bytes, Limits { parts: MAX_PARTS, part_bytes: MAX_PART_BYTES, total_bytes: MAX_UNPACKED_BYTES })
}

/// The limits [`split_zip`] applies (smaller in tests).
#[derive(Clone, Copy)]
pub(crate) struct Limits {
    pub parts: usize,
    pub part_bytes: u64,
    pub total_bytes: u64,
}

pub(crate) fn split_zip_within(bytes: &[u8], limits: Limits) -> Option<(Vec<Part>, String)> {
    if bytes.get(..4) != Some(b"PK\x03\x04".as_slice()) {
        return None;
    }
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes)).ok()?;
    if archive.len() > limits.parts {
        return None;
    }
    let comment = hex(archive.comment());
    let mut parts = Vec::with_capacity(archive.len());
    let mut names = std::collections::BTreeSet::new();
    let mut total = 0u64;
    for index in 0..archive.len() {
        let mut file = archive.by_index(index).ok()?;
        if file.encrypted() {
            return None;
        }
        let method = match file.compression() {
            zip::CompressionMethod::Stored => Method::Stored,
            zip::CompressionMethod::Deflated => Method::Deflated,
            _ => return None,
        };
        let size = file.size();
        total = total.checked_add(size)?;
        if size > limits.part_bytes || total > limits.total_bytes {
            return None;
        }
        let name = file.name().to_owned();
        if !names.insert(name.clone()) {
            return None;
        }
        let (date, time) = file.last_modified().map_or((0, 0), |dt| (dt.datepart(), dt.timepart()));
        let dir = file.is_dir();
        let mode = file.unix_mode();
        // The declared size is the archive's word only: the read is capped by it, so a part
        // that inflates to more than it claims is caught without inflating it all.
        let mut data = Vec::with_capacity(usize::try_from(size).ok()?);
        file.by_ref().take(size.saturating_add(1)).read_to_end(&mut data).ok()?;
        if data.len() as u64 != size {
            return None;
        }
        let sha256 = super::digest_hex(&data);
        parts.push(Part { meta: ManifestPart { name, method, date, time, mode, dir, size, sha256 }, data });
    }
    Some((parts, comment))
}

/// Build the archive again from a manifest and the parts' contents (in manifest order).
pub(crate) fn assemble_zip(manifest: &Manifest, contents: &[Vec<u8>]) -> Result<Vec<u8>, String> {
    if contents.len() != manifest.parts.len() {
        return Err("version manifest and parts do not match".into());
    }
    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    for (part, data) in manifest.parts.iter().zip(contents) {
        let method = match part.method {
            Method::Stored => zip::CompressionMethod::Stored,
            Method::Deflated => zip::CompressionMethod::Deflated,
        };
        let mut options = zip::write::SimpleFileOptions::default().compression_method(method);
        if let Ok(when) = zip::DateTime::try_from_msdos(part.date, part.time) {
            options = options.last_modified_time(when);
        }
        if let Some(mode) = part.mode {
            options = options.unix_permissions(mode);
        }
        if part.dir {
            writer.add_directory(part.name.as_str(), options).map_err(|e| e.to_string())?;
        } else {
            writer.start_file(part.name.as_str(), options).map_err(|e| e.to_string())?;
            writer.write_all(data).map_err(|e| e.to_string())?;
        }
    }
    if !manifest.comment.is_empty() {
        let comment = unhex(&manifest.comment).ok_or("version manifest has a bad archive comment")?;
        writer.set_raw_comment(comment.into_boxed_slice());
    }
    writer.finish().map(Cursor::into_inner).map_err(|e| e.to_string())
}

/// Check a manifest read from disk before anything is built from it.
pub(crate) fn manifest_ok(manifest: &Manifest) -> bool {
    let mut total = 0u64;
    manifest.format == 1
        && manifest.parts.len() <= MAX_PARTS
        && manifest.comment.len() <= 2 * 65_535
        && manifest.parts.iter().all(|part| {
            total = total.saturating_add(part.size);
            hash_ok(&part.sha256) && part.size <= MAX_PART_BYTES && !part.name.is_empty() && part.name.len() <= 4_096
        })
        && total <= MAX_UNPACKED_BYTES
}

/// The fixed start of an object file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ObjectHeader {
    /// Links to the nearest whole copy: 0 for a whole part.
    pub depth: u16,
    pub content_len: u64,
    payload_len: u64,
    /// The object this one is a delta of.
    pub base: Option<String>,
}

impl ObjectHeader {
    pub(crate) fn parse(bytes: &[u8]) -> Result<ObjectHeader, String> {
        let bad = || "version object is damaged".to_string();
        if bytes.get(..4) != Some(MAGIC.as_slice()) || bytes.get(4) != Some(&OBJECT_VERSION) {
            return Err(bad());
        }
        let kind = *bytes.get(5).ok_or_else(bad)?;
        let depth = u16::from_le_bytes(bytes.get(6..8).and_then(|b| b.try_into().ok()).ok_or_else(bad)?);
        let content_len = u64::from_le_bytes(bytes.get(8..16).and_then(|b| b.try_into().ok()).ok_or_else(bad)?);
        let payload_len = u64::from_le_bytes(bytes.get(16..24).and_then(|b| b.try_into().ok()).ok_or_else(bad)?);
        if content_len > MAX_PART_BYTES || payload_len > MAX_PART_BYTES.saturating_mul(2).saturating_add(1 << 20) {
            return Err(bad());
        }
        let base = match kind {
            KIND_FULL if depth == 0 => None,
            KIND_DELTA if depth > 0 => Some(hex(bytes.get(HEADER_FULL..HEADER_DELTA).ok_or_else(bad)?)),
            _ => return Err(bad()),
        };
        Ok(ObjectHeader { depth, content_len, payload_len, base })
    }

    fn data_start(&self) -> usize {
        if self.base.is_some() { HEADER_DELTA } else { HEADER_FULL }
    }
}

fn header(kind: u8, depth: u16, content_len: usize, payload_len: usize, base: Option<&[u8]>) -> Vec<u8> {
    let mut out = Vec::with_capacity(HEADER_DELTA);
    out.extend_from_slice(MAGIC);
    out.push(OBJECT_VERSION);
    out.push(kind);
    out.extend_from_slice(&depth.to_le_bytes());
    out.extend_from_slice(&(content_len as u64).to_le_bytes());
    out.extend_from_slice(&(payload_len as u64).to_le_bytes());
    if let Some(base) = base {
        out.extend_from_slice(base);
    }
    out
}

/// A part stored whole.
pub(crate) fn encode_full(content: &[u8]) -> Vec<u8> {
    let mut out = header(KIND_FULL, 0, content.len(), content.len(), None);
    out.extend_from_slice(&miniz_oxide::deflate::compress_to_vec(content, LEVEL));
    out
}

/// The object for `content`: a delta against `base` (whose object is `base_hash`, `base_depth`
/// links from a whole copy) when that is allowed and worth it, else the part whole.
pub(crate) fn encode(content: &[u8], base: Option<(&str, u16, &[u8])>) -> Vec<u8> {
    let full = encode_full(content);
    let Some((base_hash, base_depth, base_content)) = base else { return full };
    let depth = base_depth.saturating_add(1);
    let Some(base_raw) = unhex(base_hash).filter(|raw| raw.len() == 32) else { return full };
    if depth > KEYFRAME_INTERVAL {
        return full;
    }
    let ops = delta(base_content, content);
    let mut out = header(KIND_DELTA, depth, content.len(), ops.len(), Some(&base_raw));
    out.extend_from_slice(&miniz_oxide::deflate::compress_to_vec(&ops, LEVEL));
    if out.len().saturating_mul(DELTA_FRACTION.1) <= full.len().saturating_mul(DELTA_FRACTION.0) { out } else { full }
}

/// The content of an object file; `base` is the content of [`ObjectHeader::base`] for a delta.
/// The caller checks the result against the object's name.
pub(crate) fn decode(bytes: &[u8], base: Option<&[u8]>) -> Result<Vec<u8>, String> {
    let header = ObjectHeader::parse(bytes)?;
    let data = bytes.get(header.data_start()..).ok_or("version object is damaged")?;
    let limit = usize::try_from(header.payload_len).map_err(|e| e.to_string())?;
    let payload = miniz_oxide::inflate::decompress_to_vec_with_limit(data, limit).map_err(|_| "version object is damaged".to_string())?;
    if payload.len() != limit {
        return Err("version object is damaged".into());
    }
    let content_len = usize::try_from(header.content_len).map_err(|e| e.to_string())?;
    let content = match (&header.base, base) {
        (None, _) => payload,
        (Some(_), Some(base)) => apply(base, &payload, content_len)?,
        (Some(_), None) => return Err("version object needs its base".into()),
    };
    if content.len() != content_len {
        return Err("version object is damaged".into());
    }
    Ok(content)
}

const OP_INSERT: u8 = 0;
const OP_COPY: u8 = 1;

fn put_varint(out: &mut Vec<u8>, mut value: u64) {
    while value >= 0x80 {
        out.push((value as u8) | 0x80);
        value >>= 7;
    }
    out.push(value as u8);
}

fn get_varint(bytes: &[u8], at: &mut usize) -> Option<u64> {
    let mut value = 0u64;
    for shift in (0..64).step_by(7) {
        let byte = *bytes.get(*at)?;
        *at += 1;
        value |= u64::from(byte & 0x7f).checked_shl(shift)?;
        if byte & 0x80 == 0 {
            return Some(value);
        }
    }
    None
}

fn flush_insert(out: &mut Vec<u8>, target: &[u8], start: usize, end: usize) {
    if end > start
        && let Some(bytes) = target.get(start..end)
    {
        out.push(OP_INSERT);
        put_varint(out, bytes.len() as u64);
        out.extend_from_slice(bytes);
    }
}

fn push_copy(out: &mut Vec<u8>, offset: usize, len: usize) {
    if len > 0 {
        out.push(OP_COPY);
        put_varint(out, offset as u64);
        put_varint(out, len as u64);
    }
}

/// Polynomial hash of one block; [`roll`] moves it one byte along.
const PRIME: u64 = 0x100_0000_01b3;

fn block_hash(block: &[u8]) -> u64 {
    block.iter().fold(0u64, |h, b| h.wrapping_mul(PRIME).wrapping_add(u64::from(*b)))
}

fn roll(hash: u64, out: u8, input: u8, top: u64) -> u64 {
    hash.wrapping_sub(u64::from(out).wrapping_mul(top)).wrapping_mul(PRIME).wrapping_add(u64::from(input))
}

/// Copy/insert instructions that turn `base` into `target`. The common start and end are
/// copied straight away (most edits are local), and the rest is matched in [`BLOCK`]-byte
/// blocks of `base` with a rolling hash, as rsync and git's delta encoder do.
pub(crate) fn delta(base: &[u8], target: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let prefix = base.iter().zip(target).take_while(|(a, b)| a == b).count();
    let max_suffix = base.len().min(target.len()).saturating_sub(prefix);
    let suffix = base.iter().rev().zip(target.iter().rev()).take(max_suffix).take_while(|(a, b)| a == b).count();
    push_copy(&mut out, 0, prefix);
    let end = target.len().saturating_sub(suffix);
    let mut blocks: std::collections::HashMap<u64, usize> = std::collections::HashMap::new();
    let mut offset = 0;
    while let Some(block) = base.get(offset..offset + BLOCK) {
        blocks.entry(block_hash(block)).or_insert(offset);
        offset += BLOCK;
    }
    let top = (1..BLOCK).fold(1u64, |p, _| p.wrapping_mul(PRIME));
    let mut pending = prefix;
    let mut at = prefix;
    let mut hash = target.get(at..at + BLOCK).map(block_hash);
    while at + BLOCK <= end {
        let Some(current) = hash else { break };
        let found = blocks.get(&current).copied().filter(|&from| base.get(from..from + BLOCK) == target.get(at..at + BLOCK));
        if let Some(from) = found {
            let mut len = BLOCK;
            while at + len < end && base.get(from + len).is_some_and(|b| target.get(at + len) == Some(b)) {
                len += 1;
            }
            flush_insert(&mut out, target, pending, at);
            push_copy(&mut out, from, len);
            at += len;
            pending = at;
            hash = target.get(at..at + BLOCK).map(block_hash);
            continue;
        }
        hash = match (target.get(at), target.get(at + BLOCK)) {
            (Some(&gone), Some(&next)) => Some(roll(current, gone, next, top)),
            _ => None,
        };
        at += 1;
    }
    flush_insert(&mut out, target, pending, end);
    push_copy(&mut out, base.len().saturating_sub(suffix), suffix);
    out
}

/// Apply [`delta`] instructions to `base`, refusing anything that reads outside `base` or
/// writes more than `expected` bytes.
pub(crate) fn apply(base: &[u8], ops: &[u8], expected: usize) -> Result<Vec<u8>, String> {
    let bad = || "version delta is damaged".to_string();
    let mut out = Vec::with_capacity(expected);
    let mut at = 0;
    while at < ops.len() {
        let op = *ops.get(at).ok_or_else(bad)?;
        at += 1;
        match op {
            OP_INSERT => {
                let len = usize::try_from(get_varint(ops, &mut at).ok_or_else(bad)?).map_err(|_| bad())?;
                let bytes = ops.get(at..at.checked_add(len).ok_or_else(bad)?).ok_or_else(bad)?;
                at += len;
                if out.len().saturating_add(len) > expected {
                    return Err(bad());
                }
                out.extend_from_slice(bytes);
            }
            OP_COPY => {
                let from = usize::try_from(get_varint(ops, &mut at).ok_or_else(bad)?).map_err(|_| bad())?;
                let len = usize::try_from(get_varint(ops, &mut at).ok_or_else(bad)?).map_err(|_| bad())?;
                let bytes = base.get(from..from.checked_add(len).ok_or_else(bad)?).ok_or_else(bad)?;
                if out.len().saturating_add(len) > expected {
                    return Err(bad());
                }
                out.extend_from_slice(bytes);
            }
            _ => return Err(bad()),
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn noise(len: usize, seed: u64) -> Vec<u8> {
        let mut state = seed | 1;
        (0..len)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                state as u8
            })
            .collect()
    }

    #[test]
    fn deltas_round_trip_and_stay_small() {
        let base: Vec<u8> = (0..20_000u32).flat_map(|i| format!("<w:p><w:r><w:t>Line {i}</w:t></w:r></w:p>").into_bytes()).collect();
        let mut target = base.clone();
        target.splice(5_000..5_010, b"an edit in the middle".iter().copied());
        target.extend_from_slice(b"<w:p>new last paragraph</w:p>");
        target.drain(100_000..100_400);
        let ops = delta(&base, &target);
        assert_eq!(apply(&base, &ops, target.len()).unwrap(), target);
        assert!(ops.len() < 2_000, "{} bytes of instructions for three small edits", ops.len());
        // Moved blocks are found too, not only a shared start and end.
        let mut moved = base.get(300_000..).unwrap().to_vec();
        moved.extend_from_slice(base.get(..300_000).unwrap());
        let ops = delta(&base, &moved);
        assert_eq!(apply(&base, &ops, moved.len()).unwrap(), moved);
        assert!(ops.len() < 1_000);
        // Edge cases.
        for (a, b) in [(Vec::new(), Vec::new()), (Vec::new(), b"x".to_vec()), (b"x".to_vec(), Vec::new()), (noise(5_000, 1), noise(5_000, 2))] {
            assert_eq!(apply(&a, &delta(&a, &b), b.len()).unwrap(), b);
        }
    }

    #[test]
    fn hostile_deltas_and_objects_are_errors() {
        let base = b"0123456789".to_vec();
        for ops in
            [vec![OP_COPY, 5, 50], vec![OP_INSERT, 200], vec![9], vec![OP_COPY, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x01]]
        {
            assert!(apply(&base, &ops, 10).is_err(), "{ops:?}");
        }
        // Writing past the expected length is refused even when the reads are in bounds.
        assert!(apply(&base, &[OP_COPY, 0, 10, OP_COPY, 0, 10], 15).is_err());
        let object = encode_full(b"hello");
        assert_eq!(decode(&object, None).unwrap(), b"hello");
        let mut truncated = object.clone();
        truncated.truncate(object.len() - 2);
        assert!(decode(&truncated, None).is_err());
        let mut lying = object.clone();
        lying[8] = 200; // claims 200 bytes of content
        assert!(decode(&lying, None).is_err());
        let mut huge = object;
        huge[16..24].copy_from_slice(&u64::MAX.to_le_bytes());
        assert!(ObjectHeader::parse(&huge).is_err());
        assert!(decode(b"WCVO", None).is_err());
        // A delta whose base is missing is an error, not a panic.
        let base_content = noise(20_000, 5);
        let mut edited = base_content.clone();
        edited.extend_from_slice(b", edited");
        let base_hash = super::super::digest_hex(&base_content);
        let delta_object = encode(&edited, Some((&base_hash, 0, &base_content)));
        assert!(ObjectHeader::parse(&delta_object).unwrap().base.is_some());
        assert_eq!(decode(&delta_object, Some(&base_content)).unwrap(), edited);
        assert!(decode(&delta_object, None).is_err());
    }

    #[test]
    fn keyframes_after_the_interval_or_when_a_delta_does_not_pay() {
        let base = noise(10_000, 3);
        let hash = super::super::digest_hex(&base);
        let mut edited = base.clone();
        edited[10] ^= 1;
        let near = encode(&edited, Some((&hash, 0, &base)));
        assert_eq!(ObjectHeader::parse(&near).unwrap().depth, 1);
        let at_limit = encode(&edited, Some((&hash, KEYFRAME_INTERVAL, &base)));
        assert_eq!(ObjectHeader::parse(&at_limit).unwrap().depth, 0, "the chain ends at the interval");
        let unrelated = encode(&noise(10_000, 4), Some((&hash, 0, &base)));
        assert_eq!(ObjectHeader::parse(&unrelated).unwrap().depth, 0, "a delta as big as the part is not kept");
    }

    #[test]
    fn zips_split_and_assemble_with_the_same_parts() {
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let stored = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
        let deflated = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        writer.start_file("mimetype", stored).unwrap();
        writer.write_all(b"application/vnd.oasis.opendocument.text").unwrap();
        writer.add_directory("word/", deflated).unwrap();
        writer.start_file("word/document.xml", deflated).unwrap();
        writer.write_all(b"<w:document>text</w:document>").unwrap();
        writer.set_raw_comment(b"comment".to_vec().into_boxed_slice());
        let bytes = writer.finish().unwrap().into_inner();
        let (parts, comment) = split_zip(&bytes).unwrap();
        let manifest = Manifest { format: 1, info: Default::default(), comment, parts: parts.iter().map(|p| p.meta.clone()).collect() };
        assert!(manifest_ok(&manifest));
        let rebuilt = assemble_zip(&manifest, &parts.iter().map(|p| p.data.clone()).collect::<Vec<_>>()).unwrap();
        let (again, again_comment) = split_zip(&rebuilt).unwrap();
        assert_eq!(again_comment, manifest.comment);
        assert_eq!(again.iter().map(|p| p.meta.clone()).collect::<Vec<_>>(), manifest.parts, "same names, order, methods, dates and content");
        // Not a zip, or a truncated one: kept whole.
        assert!(split_zip(b"plain text").is_none());
        assert!(split_zip(bytes.get(..bytes.len() / 2).unwrap()).is_none());
    }

    fn zipped(parts: &[(&str, &[u8])]) -> Vec<u8> {
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let deflated = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        for (name, data) in parts {
            writer.start_file(*name, deflated).unwrap();
            writer.write_all(data).unwrap();
        }
        writer.finish().unwrap().into_inner()
    }

    #[test]
    fn zip_bombs_are_kept_whole_not_inflated() {
        let limits = Limits { parts: 4, part_bytes: 1 << 20, total_bytes: 3 << 20 };
        // A part that inflates past the per-part limit: 4 MiB of zeros in a few kilobytes.
        let zeros = vec![0u8; 4 << 20];
        let bomb = zipped(&[("bomb.xml", &zeros)]);
        assert!(bomb.len() < 64 << 10);
        assert!(split_zip_within(&bomb, limits).is_none());
        // Parts each under the limit but too much in all.
        let mb = vec![0u8; 1 << 20];
        assert!(split_zip_within(&zipped(&[("a", &mb), ("b", &mb), ("c", &mb), ("d", b"x")]), limits).is_none());
        // Too many parts.
        assert!(split_zip_within(&zipped(&[("a", b""), ("b", b""), ("c", b""), ("d", b""), ("e", b"")]), limits).is_none());
        // Within the limits it splits.
        assert_eq!(split_zip_within(&zipped(&[("a", b"1"), ("b", b"2")]), limits).map(|(p, _)| p.len()), Some(2));
        // A part whose size fields understate what it inflates to: the read stops at the
        // stated size and the archive is refused, never inflated in full.
        let mut lying = zipped(&[("a", &zeros)]);
        let stated = (4u32 << 20).to_le_bytes();
        let small = 16u32.to_le_bytes();
        let mut patched = 0;
        let mut at = 0;
        while let Some(found) = lying.get(at..).and_then(|rest| rest.windows(4).position(|w| w == stated)) {
            lying.get_mut(at + found..at + found + 4).unwrap().copy_from_slice(&small);
            at += found + 4;
            patched += 1;
        }
        assert!(patched >= 1);
        assert!(split_zip(&lying).is_none());
        // A hostile manifest is refused before anything is built from it.
        let part = ManifestPart {
            name: "a".into(),
            method: Method::Stored,
            date: 0,
            time: 0,
            mode: None,
            dir: false,
            size: MAX_PART_BYTES + 1,
            sha256: "0".repeat(64),
        };
        assert!(!manifest_ok(&Manifest { format: 1, info: Default::default(), comment: String::new(), parts: vec![part.clone()] }));
        assert!(!manifest_ok(&Manifest {
            format: 1,
            info: Default::default(),
            comment: String::new(),
            parts: vec![ManifestPart { size: 1, sha256: "../x".into(), ..part }]
        }));
    }
}
