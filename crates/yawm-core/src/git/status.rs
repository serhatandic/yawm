//! Parser for `git status --porcelain=v1 -z`.
//!
//! Each entry is `XY<space><path>` terminated by NUL, where `X` is the staged
//! state and `Y` the unstaged state. Paths retain their raw bytes so distinct
//! counts and deletion evidence do not collapse names through display decoding.

use crate::model::DirtyCounts;
use std::collections::BTreeSet;
use std::fs::{File, Metadata};
use std::io::{self, Cursor, Read};
use std::path::{Path, PathBuf};

/// One changed path reported by git status.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusEntry {
    pub staged: bool,
    pub unstaged: bool,
    pub untracked: bool,
    /// The raw `XY` pair git printed, kept verbatim.
    ///
    /// The three booleans above collapse it: `D ` and `M ` are both "staged",
    /// and a deletion swapped for a modification of the same path leaves every
    /// count and every name untouched. The bytes are what tell them apart, so
    /// they are carried rather than derived back.
    pub code: [u8; 2],
    /// The path bytes git emitted. These are the identity; `path` is display.
    pub raw_path: Vec<u8>,
    pub path: String,
}

/// Parse `git status --porcelain=v1 -z --no-renames` output.
pub fn parse_status(bytes: &[u8]) -> Vec<StatusEntry> {
    let nul_separated = bytes.contains(&0);
    let separator = if nul_separated { 0 } else { b'\n' };

    let mut entries = Vec::new();
    for record in bytes.split(|b| *b == separator) {
        let record = record.strip_suffix(b"\r").unwrap_or(record);
        // `XY path` needs at least a status pair, a space, and one character.
        if record.len() < 4 {
            continue;
        }

        let x = record[0];
        let y = record[1];
        // Byte 2 is the separating space in porcelain v1.
        let raw_path = record[3..].to_vec();
        let path = String::from_utf8_lossy(&raw_path).into_owned();

        let untracked = x == b'?' && y == b'?';
        let ignored = x == b'!' && y == b'!';
        if ignored {
            continue;
        }

        entries.push(StatusEntry {
            staged: !untracked && x != b' ',
            unstaged: !untracked && y != b' ',
            untracked,
            code: [x, y],
            raw_path,
            path,
        });
    }
    entries
}

/// Summarise status entries into the counts the verdict engine consumes.
pub fn count_status(entries: &[StatusEntry]) -> DirtyCounts {
    let mut counts = DirtyCounts::default();
    let mut paths = BTreeSet::new();
    for entry in entries {
        paths.insert(entry.raw_path.as_slice());
        if entry.untracked {
            counts.untracked += 1;
        } else {
            if entry.staged {
                counts.staged += 1;
            }
            if entry.unstaged {
                counts.unstaged += 1;
            }
        }
    }
    counts.paths = paths.len();
    counts
}

/// One entry of `git ls-files -v --stage -z`.
///
/// The whole index is parsed rather than only the flagged part of it, because
/// the staged blob of an ordinary path is evidence too: `git add` of different
/// bytes leaves the porcelain code, the file name, and every count exactly
/// where they were.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexEntry {
    /// `ls-files -v`'s tag: `H` for a plain entry, `S`/lowercase for the ones
    /// git's own optimisations tell it not to look at.
    pub tag: u8,
    pub mode: String,
    pub oid: String,
    /// Which stage of the path this entry is.
    ///
    /// `0` for an ordinary entry. A path in conflict has no stage 0 at all and
    /// instead has up to three entries — `1` the common ancestor, `2` "ours",
    /// `3` "theirs" — each naming a different blob. Dropping the number left
    /// three different resolutions of the same conflict indistinguishable from
    /// one another, because only whichever entry happened to be read last was
    /// kept.
    pub stage: u8,
    /// The path bytes git emitted. These are the identity; `path` is display.
    pub raw_path: Vec<u8>,
    pub path: String,
}

impl IndexEntry {
    /// Whether git normally declines to inspect this entry's file.
    pub fn is_flagged(&self) -> bool {
        self.tag == b'S' || self.tag.is_ascii_lowercase()
    }

    /// This entry as one comparable token, stage first so a path's entries sort
    /// in git's own stage order.
    pub fn identity(&self) -> String {
        format!("{} {} {}", self.stage, self.mode, self.oid)
    }
}

/// Parse `git ls-files -v --stage -z` in full.
pub fn parse_index(bytes: &[u8]) -> Vec<IndexEntry> {
    bytes
        .split(|byte| *byte == 0)
        .filter_map(|record| {
            let tag = *record.first()?;
            let tab = record.iter().position(|byte| *byte == b'\t')?;
            let metadata = std::str::from_utf8(record.get(2..tab)?).ok()?;
            let mut fields = metadata.split_ascii_whitespace();
            let mode = fields.next()?;
            let oid = fields.next()?;
            // Absent only from malformed output; a missing stage is read as an
            // unmerged entry of unknown stage rather than silently as stage 0,
            // which is the one value that means "not in conflict".
            let stage = fields.next().and_then(|stage| stage.parse().ok())?;
            let raw_path = record.get(tab + 1..)?.to_vec();
            let path = String::from_utf8_lossy(&raw_path).into_owned();
            Some(IndexEntry {
                tag,
                mode: mode.to_string(),
                oid: oid.to_string(),
                stage,
                raw_path,
                path,
            })
        })
        .collect()
}

/// An index entry whose promise to git can conceal worktree changes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlaggedIndexEntry {
    pub oid: String,
    pub raw_path: Vec<u8>,
    pub path: String,
}

/// Parse `git ls-files -v --stage -z`, retaining only entries that git normally
/// declines to inspect.
pub fn parse_flagged_index(bytes: &[u8]) -> Vec<FlaggedIndexEntry> {
    flagged(&parse_index(bytes))
}

/// The flagged entries of an already-parsed index.
pub fn flagged(index: &[IndexEntry]) -> Vec<FlaggedIndexEntry> {
    index
        .iter()
        .filter(|entry| entry.is_flagged())
        .map(|entry| FlaggedIndexEntry {
            oid: entry.oid.clone(),
            raw_path: entry.raw_path.clone(),
            path: entry.path.clone(),
        })
        .collect()
}

/// Compare a worktree file with the blob recorded in the index without asking
/// git to trust the very flags under investigation.
pub fn matches_index_blob(root: &Path, entry: &FlaggedIndexEntry) -> bool {
    blob_oid(&root.join(path_from_git(&entry.raw_path)), entry.oid.len())
        .is_ok_and(|oid| oid == entry.oid)
}

#[cfg(unix)]
pub(crate) fn path_from_git(raw: &[u8]) -> PathBuf {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;

    PathBuf::from(OsStr::from_bytes(raw))
}

#[cfg(not(unix))]
pub(crate) fn path_from_git(raw: &[u8]) -> PathBuf {
    PathBuf::from(String::from_utf8_lossy(raw).into_owned())
}

/// The object name git would give this path's current contents.
///
/// Streamed in fixed blocks, so a large file costs no more memory than a small
/// one, and the digest is the repository's own — the same string the index and
/// `git status` speak in.
pub fn blob_oid(path: &Path, oid_len: usize) -> io::Result<String> {
    git_blob_oid(path, oid_len)
}

/// A sha256 over bytes already in memory, hex-encoded.
///
/// The same compressor the object hashing uses; nothing here invents a second
/// hash implementation to keep in step with the first.
pub fn digest_hex(bytes: &[u8]) -> String {
    let total = bytes.len() as u64;
    let words = sha256(Cursor::new(bytes), total).expect("hashing an in-memory buffer cannot fail");
    words.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn git_blob_oid(path: &Path, oid_len: usize) -> io::Result<String> {
    let metadata = std::fs::symlink_metadata(path)?;
    #[cfg(unix)]
    if metadata.file_type().is_symlink() {
        use std::os::unix::ffi::OsStrExt;
        let target = std::fs::read_link(path)?;
        let bytes = target.as_os_str().as_bytes().to_vec();
        return hash_blob(Cursor::new(bytes.clone()), bytes.len() as u64, oid_len);
    }
    hash_open_file(path, &metadata, oid_len, || {})
}

fn hash_blob<R: Read>(reader: R, size: u64, oid_len: usize) -> io::Result<String> {
    let header = format!("blob {size}\0").into_bytes();
    let total = header.len() as u64 + size;
    let reader = Cursor::new(header).chain(reader);
    let words = match oid_len {
        40 => sha1(reader, total)?.to_vec(),
        64 => sha256(reader, total)?.to_vec(),
        _ => return Err(io::Error::other("unsupported git object format")),
    };
    Ok(words.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn hash_open_file(
    path: &Path,
    path_metadata: &Metadata,
    oid_len: usize,
    after_open: impl FnOnce(),
) -> io::Result<String> {
    let mut file = File::open(path)?;
    let before = file.metadata()?;
    if !before.is_file()
        || !same_file(path_metadata, &before)
        || !metadata_unchanged(path_metadata, &before)
    {
        return Err(io::Error::other("index entry changed while it was opened"));
    }
    let size = before.len();
    after_open();

    let oid = hash_blob((&mut file).take(size), size, oid_len)?;
    let mut extra = [0u8; 1];
    if file.read(&mut extra)? != 0 {
        return Err(io::Error::other("index entry grew while it was hashed"));
    }

    let after = file.metadata()?;
    let path_after = std::fs::symlink_metadata(path)?;
    if !same_file(&before, &after)
        || !same_file(&before, &path_after)
        || !metadata_unchanged(&before, &after)
    {
        return Err(io::Error::other("index entry changed while it was hashed"));
    }
    Ok(oid)
}

#[cfg(unix)]
fn same_file(left: &Metadata, right: &Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;

    left.dev() == right.dev() && left.ino() == right.ino()
}

#[cfg(not(unix))]
fn same_file(left: &Metadata, right: &Metadata) -> bool {
    left.file_type() == right.file_type()
        && left.len() == right.len()
        && left.created().ok() == right.created().ok()
}

#[cfg(unix)]
fn metadata_unchanged(before: &Metadata, after: &Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;

    before.len() == after.len()
        && before.mode() == after.mode()
        && before.mtime() == after.mtime()
        && before.mtime_nsec() == after.mtime_nsec()
        && before.ctime() == after.ctime()
        && before.ctime_nsec() == after.ctime_nsec()
}

#[cfg(not(unix))]
fn metadata_unchanged(before: &Metadata, after: &Metadata) -> bool {
    before.len() == after.len()
        && before.permissions() == after.permissions()
        && before.modified().ok() == after.modified().ok()
}

fn read_hash_blocks<R: Read, const N: usize>(
    mut reader: R,
    total: u64,
    mut state: [u32; N],
    compress: fn(&mut [u32; N], &[u8; 64]),
) -> io::Result<[u32; N]> {
    let mut remaining = total;
    let mut block = [0u8; 64];
    while remaining >= 64 {
        reader.read_exact(&mut block)?;
        compress(&mut state, &block);
        remaining -= 64;
    }

    let tail = remaining as usize;
    reader.read_exact(&mut block[..tail])?;
    block[tail] = 0x80;
    if tail >= 56 {
        compress(&mut state, &block);
        block = [0; 64];
    }
    block[56..].copy_from_slice(&total.wrapping_mul(8).to_be_bytes());
    compress(&mut state, &block);
    Ok(state)
}

fn sha1<R: Read>(reader: R, total: u64) -> io::Result<[u8; 20]> {
    let state = read_hash_blocks(
        reader,
        total,
        [
            0x6745_2301,
            0xefcd_ab89,
            0x98ba_dcfe,
            0x1032_5476,
            0xc3d2_e1f0,
        ],
        sha1_compress,
    )?;
    let mut out = [0; 20];
    for (chunk, word) in out.chunks_exact_mut(4).zip(state) {
        chunk.copy_from_slice(&word.to_be_bytes());
    }
    Ok(out)
}

fn sha1_compress(state: &mut [u32; 5], block: &[u8; 64]) {
    let mut words = [0u32; 80];
    for (word, chunk) in words.iter_mut().zip(block.chunks_exact(4)) {
        *word = u32::from_be_bytes(chunk.try_into().expect("four-byte chunk"));
    }
    for index in 16..80 {
        words[index] =
            (words[index - 3] ^ words[index - 8] ^ words[index - 14] ^ words[index - 16])
                .rotate_left(1);
    }

    let [mut a, mut b, mut c, mut d, mut e] = *state;
    for (index, word) in words.into_iter().enumerate() {
        let (function, constant) = match index {
            0..=19 => ((b & c) | ((!b) & d), 0x5a82_7999),
            20..=39 => (b ^ c ^ d, 0x6ed9_eba1),
            40..=59 => ((b & c) | (b & d) | (c & d), 0x8f1b_bcdc),
            _ => (b ^ c ^ d, 0xca62_c1d6),
        };
        let next = a
            .rotate_left(5)
            .wrapping_add(function)
            .wrapping_add(e)
            .wrapping_add(constant)
            .wrapping_add(word);
        e = d;
        d = c;
        c = b.rotate_left(30);
        b = a;
        a = next;
    }
    for (slot, value) in state.iter_mut().zip([a, b, c, d, e]) {
        *slot = slot.wrapping_add(value);
    }
}

fn sha256<R: Read>(reader: R, total: u64) -> io::Result<[u8; 32]> {
    let state = read_hash_blocks(
        reader,
        total,
        [
            0x6a09_e667,
            0xbb67_ae85,
            0x3c6e_f372,
            0xa54f_f53a,
            0x510e_527f,
            0x9b05_688c,
            0x1f83_d9ab,
            0x5be0_cd19,
        ],
        sha256_compress,
    )?;
    let mut out = [0; 32];
    for (chunk, word) in out.chunks_exact_mut(4).zip(state) {
        chunk.copy_from_slice(&word.to_be_bytes());
    }
    Ok(out)
}

fn sha256_compress(state: &mut [u32; 8], block: &[u8; 64]) {
    const K: [u32; 64] = [
        0x428a_2f98,
        0x7137_4491,
        0xb5c0_fbcf,
        0xe9b5_dba5,
        0x3956_c25b,
        0x59f1_11f1,
        0x923f_82a4,
        0xab1c_5ed5,
        0xd807_aa98,
        0x1283_5b01,
        0x2431_85be,
        0x550c_7dc3,
        0x72be_5d74,
        0x80de_b1fe,
        0x9bdc_06a7,
        0xc19b_f174,
        0xe49b_69c1,
        0xefbe_4786,
        0x0fc1_9dc6,
        0x240c_a1cc,
        0x2de9_2c6f,
        0x4a74_84aa,
        0x5cb0_a9dc,
        0x76f9_88da,
        0x983e_5152,
        0xa831_c66d,
        0xb003_27c8,
        0xbf59_7fc7,
        0xc6e0_0bf3,
        0xd5a7_9147,
        0x06ca_6351,
        0x1429_2967,
        0x27b7_0a85,
        0x2e1b_2138,
        0x4d2c_6dfc,
        0x5338_0d13,
        0x650a_7354,
        0x766a_0abb,
        0x81c2_c92e,
        0x9272_2c85,
        0xa2bf_e8a1,
        0xa81a_664b,
        0xc24b_8b70,
        0xc76c_51a3,
        0xd192_e819,
        0xd699_0624,
        0xf40e_3585,
        0x106a_a070,
        0x19a4_c116,
        0x1e37_6c08,
        0x2748_774c,
        0x34b0_bcb5,
        0x391c_0cb3,
        0x4ed8_aa4a,
        0x5b9c_ca4f,
        0x682e_6ff3,
        0x748f_82ee,
        0x78a5_636f,
        0x84c8_7814,
        0x8cc7_0208,
        0x90be_fffa,
        0xa450_6ceb,
        0xbef9_a3f7,
        0xc671_78f2,
    ];
    let mut words = [0u32; 64];
    for (word, chunk) in words.iter_mut().zip(block.chunks_exact(4)) {
        *word = u32::from_be_bytes(chunk.try_into().expect("four-byte chunk"));
    }
    for index in 16..64 {
        let s0 = words[index - 15].rotate_right(7)
            ^ words[index - 15].rotate_right(18)
            ^ (words[index - 15] >> 3);
        let s1 = words[index - 2].rotate_right(17)
            ^ words[index - 2].rotate_right(19)
            ^ (words[index - 2] >> 10);
        words[index] = words[index - 16]
            .wrapping_add(s0)
            .wrapping_add(words[index - 7])
            .wrapping_add(s1);
    }

    let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = *state;
    for index in 0..64 {
        let sigma1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
        let choose = (e & f) ^ ((!e) & g);
        let first = h
            .wrapping_add(sigma1)
            .wrapping_add(choose)
            .wrapping_add(K[index])
            .wrapping_add(words[index]);
        let sigma0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
        let majority = (a & b) ^ (a & c) ^ (b & c);
        let second = sigma0.wrapping_add(majority);
        h = g;
        g = f;
        f = e;
        e = d.wrapping_add(first);
        d = c;
        c = b;
        b = a;
        a = first.wrapping_add(second);
    }
    for (slot, value) in state.iter_mut().zip([a, b, c, d, e, f, g, h]) {
        *slot = slot.wrapping_add(value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nul(entries: &[&str]) -> Vec<u8> {
        let mut out = Vec::new();
        for e in entries {
            out.extend_from_slice(e.as_bytes());
            out.push(0);
        }
        out
    }

    #[test]
    fn clean_worktree_has_no_entries() {
        assert!(parse_status(b"").is_empty());
        assert!(!count_status(&[]).is_dirty());
    }

    #[test]
    fn counts_staged_unstaged_and_untracked() {
        let input = nul(&["M  staged.rs", " M unstaged.rs", "?? new.rs"]);
        let counts = count_status(&parse_status(&input));

        assert_eq!(counts.staged, 1);
        assert_eq!(counts.unstaged, 1);
        assert_eq!(counts.untracked, 1);
        assert!(counts.is_dirty());
        assert_eq!(counts.total(), 3);
    }

    #[test]
    fn distinct_paths_do_not_double_count_a_staged_and_modified_file() {
        let input = nul(&["MM both.rs", "?? new.rs", "?? nested/other.rs"]);
        let counts = count_status(&parse_status(&input));

        assert_eq!(counts.staged, 1);
        assert_eq!(counts.unstaged, 1);
        assert_eq!(counts.untracked, 2);
        assert_eq!(counts.paths, 3);
        assert_eq!(counts.total(), 4);
    }

    #[test]
    fn added_files_count_as_staged_not_untracked() {
        let input = nul(&["A  added.rs"]);
        let counts = count_status(&parse_status(&input));

        assert_eq!(counts.staged, 1);
        assert_eq!(counts.untracked, 0);
    }

    #[test]
    fn deleted_files_are_counted() {
        let input = nul(&["D  gone.rs", " D also-gone.rs"]);
        let counts = count_status(&parse_status(&input));

        assert_eq!(counts.staged, 1);
        assert_eq!(counts.unstaged, 1);
    }

    #[test]
    fn ignored_entries_are_skipped() {
        // `!!` only appears with --ignored, but must never be counted as work.
        let input = nul(&["!! target/", "?? real.rs"]);
        let counts = count_status(&parse_status(&input));

        assert_eq!(counts.untracked, 1);
        assert_eq!(counts.total(), 1);
    }

    #[test]
    fn paths_with_spaces_are_preserved() {
        let input = nul(&["?? my file.rs"]);
        let entries = parse_status(&input);

        assert_eq!(entries[0].path, "my file.rs");
        assert!(entries[0].untracked);
    }

    #[test]
    fn lossy_display_paths_keep_distinct_raw_identities() {
        let entries = parse_status(b"?? odd-\xfe\0?? odd-\xff\0");

        assert_eq!(entries[0].path, entries[1].path);
        assert_ne!(entries[0].raw_path, entries[1].raw_path);
    }

    #[test]
    fn parses_newline_separated_fallback_output() {
        let input = b"M  a.rs\n?? b.rs\n";
        let counts = count_status(&parse_status(input));

        assert_eq!(counts.staged, 1);
        assert_eq!(counts.untracked, 1);
    }

    #[test]
    fn untracked_is_never_also_staged() {
        let entries = parse_status(&nul(&["?? new.rs"]));

        assert!(entries[0].untracked);
        assert!(!entries[0].staged);
        assert!(!entries[0].unstaged);
    }

    /// A conflicted path has no stage 0 and up to three stages that each name
    /// a different blob. They are separate records of separate content, and
    /// keeping one of them — whichever happened to be parsed last — described
    /// three different resolutions of a merge identically.
    #[test]
    fn every_stage_of_a_conflicted_path_is_a_separate_entry() {
        let input = nul(&[
            "M 100644 1111111111111111111111111111111111111111 1\tconflict.txt",
            "M 100644 2222222222222222222222222222222222222222 2\tconflict.txt",
            "M 100644 3333333333333333333333333333333333333333 3\tconflict.txt",
        ]);

        let entries = parse_index(&input);

        assert_eq!(entries.len(), 3);
        let identities: Vec<String> = entries.iter().map(IndexEntry::identity).collect();
        assert_eq!(
            identities,
            vec![
                "1 100644 1111111111111111111111111111111111111111",
                "2 100644 2222222222222222222222222222222222222222",
                "3 100644 3333333333333333333333333333333333333333",
            ],
            "the stage leads each token, so a path's entries sort in git's order"
        );
        assert!(entries.iter().all(|entry| entry.stage != 0));
    }

    /// Reading a stageless entry as stage 0 would claim a conflicted path is
    /// an ordinary one. The entry is dropped instead, and the caller that
    /// needed it says so rather than believing a fabricated value.
    #[test]
    fn an_entry_with_no_stage_is_not_read_as_stage_zero() {
        let input = nul(&["H 100644 aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\tno-stage.txt"]);

        assert!(parse_index(&input).is_empty());
    }

    #[test]
    fn parses_only_index_entries_that_can_hide_changes() {
        let input = nul(&[
            "H 100644 aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa 0\tordinary.txt",
            "h 100644 bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb 0\tassumed.txt",
            "S 100644 cccccccccccccccccccccccccccccccccccccccc 0\tskipped.txt",
        ]);

        assert_eq!(
            parse_flagged_index(&input)
                .into_iter()
                .map(|entry| entry.path)
                .collect::<Vec<_>>(),
            ["assumed.txt", "skipped.txt"]
        );
    }

    #[test]
    fn hashes_worktree_files_as_git_blobs_for_both_object_formats() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("file");
        std::fs::write(&path, "before\n").unwrap();

        assert_eq!(
            git_blob_oid(&path, 40).unwrap(),
            "90be1f3056c4f471f977a28497b8d4b392c55a02"
        );
        assert_eq!(
            git_blob_oid(&path, 64).unwrap(),
            "d5e4cf563fb67895f4ab12ebf6963c84390ac573d85babf8348acd9d06ffe10a"
        );
    }

    #[test]
    fn an_append_after_open_cannot_be_hashed_as_the_old_prefix() {
        use std::fs::OpenOptions;
        use std::io::Write;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("file");
        std::fs::write(&path, "before\n").unwrap();
        let metadata = std::fs::symlink_metadata(&path).unwrap();

        let result = hash_open_file(&path, &metadata, 40, || {
            let mut append = OpenOptions::new().append(true).open(&path).unwrap();
            append.write_all(b"after\n").unwrap();
        });

        assert!(
            result.is_err(),
            "the opened file grew, so hashing only its original prefix must fail closed"
        );
    }
}
