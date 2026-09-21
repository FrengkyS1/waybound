//! CurseForge content fingerprint — a clean-room port of PrismLauncher's
//! `libraries/murmur2` (itself Austin Appleby's public-domain MurmurHash2
//! with public-domain incremental modifications): MurmurHash2/32, **seed 1**,
//! over the file bytes minus ASCII whitespace (tab 9, LF 10, CR 13, space
//! 32). Matches what `POST /fingerprints` expects. A stock MurmurHash2
//! (seed 0, no filtering) will silently match nothing — the seed and the
//! filter are the whole trick.

const M: u32 = 0x5bd1e995;
const R: u32 = 24;

/// Fingerprint bytes exactly as CurseForge expects them hashed.
pub fn curseforge_fingerprint(bytes: &[u8]) -> u32 {
    let filtered: Vec<u8> = bytes
        .iter()
        .copied()
        .filter(|b| !matches!(b, 9 | 10 | 13 | 32))
        .collect();
    murmur2_32(&filtered, 1)
}

fn murmur2_32(key: &[u8], seed: u32) -> u32 {
    let len = key.len() as u32;
    let mut h = seed ^ len;
    let mut i = 0;
    while i + 4 <= key.len() {
        let mut k = u32::from_le_bytes([key[i], key[i + 1], key[i + 2], key[i + 3]]);
        k = k.wrapping_mul(M);
        k ^= k >> R;
        k = k.wrapping_mul(M);
        h = h.wrapping_mul(M);
        h ^= k;
        i += 4;
    }
    match &key[i..] {
        [a, b, c] => {
            h ^= (*c as u32) << 16;
            h ^= (*b as u32) << 8;
            h ^= *a as u32;
            h = h.wrapping_mul(M);
        }
        [a, b] => {
            h ^= (*b as u32) << 8;
            h ^= *a as u32;
            h = h.wrapping_mul(M);
        }
        [a] => {
            h ^= *a as u32;
            h = h.wrapping_mul(M);
        }
        _ => {}
    }
    h ^= h >> 13;
    h = h.wrapping_mul(M);
    h ^= h >> 15;
    h
}

#[cfg(test)]
mod fingerprint_tests {
    use super::curseforge_fingerprint;

    #[test]
    fn empty_input_hashes_seed_xor_len_only() {
        // Verified against an independent Python implementation of the same
        // spec (seed 1, whitespace filter, m/r/final mix above).
        assert_eq!(curseforge_fingerprint(b""), 1540447798);
    }

    #[test]
    fn whitespace_bytes_do_not_affect_the_hash() {
        assert_eq!(curseforge_fingerprint(b"ab"), curseforge_fingerprint(b"a b"));
        assert_eq!(
            curseforge_fingerprint(b"ab"),
            curseforge_fingerprint(b"\t\na\r b \n")
        );
    }

    #[test]
    fn known_vectors() {
        assert_eq!(curseforge_fingerprint(b"a"), 626045324);
        assert_eq!(curseforge_fingerprint(b"abc"), 1621425345);
        assert_eq!(curseforge_fingerprint(b"0123456789abcdef"), 3500483126);
    }
}
