//! Overlay residency by content. Overlays share guest addresses, so a pc only identifies lifted
//! code together with the overlay that is actually in RAM. The generator records, per segment, the
//! hash of every lifted function's byte range (entry..entry+size, literal pools included, since the
//! lifter bakes their values in). A segment is resident only if guest memory matches all of them
//! exactly; a stale or partially overwritten overlay therefore never verifies.

/// FNV-1a 64-bit hash (stable, dependency-free; used by the generator and the runtime alike).
pub fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// One contiguous run of lifted code: guest address, length in bytes, hash of the ROM bytes.
pub struct CodeRange {
    pub start: u32,
    pub len: u32,
    pub hash: u64,
}

/// The code signature of one segment.
pub struct SegmentSig {
    pub segment: &'static str,
    pub ranges: &'static [CodeRange],
}

/// Overlay segments are the ones that can share addresses; everything else is always resident.
pub fn is_overlay(segment: &str) -> bool {
    segment.starts_with("ov9_")
}

/// True if guest memory holds exactly this segment's code. `read(addr, buf)` fills `buf` from
/// guest memory and returns false if the range is unreadable.
pub fn verify(sig: &SegmentSig, read: &dyn Fn(u32, &mut [u8]) -> bool) -> bool {
    let mut buf = Vec::new();
    !sig.ranges.is_empty()
        && sig.ranges.iter().all(|r| {
            buf.resize(r.len as usize, 0);
            read(r.start, &mut buf) && fnv1a64(&buf) == r.hash
        })
}

/// Names of the overlay segments currently resident in guest memory.
pub fn resident_overlays(sigs: &'static [SegmentSig], read: &dyn Fn(u32, &mut [u8]) -> bool) -> Vec<&'static str> {
    sigs.iter().filter(|s| is_overlay(s.segment) && verify(s, read)).map(|s| s.segment).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: &[u8] = &[1, 2, 3, 4, 5, 6, 7, 8];
    const B: &[u8] = &[9, 9, 9, 9, 5, 6, 7, 8];
    static RA: [CodeRange; 1] = [CodeRange { start: 0x100, len: 8, hash: 0 }];

    fn sig_of(seg: &'static str, bytes: &[u8]) -> SegmentSig {
        let ranges: &'static [CodeRange] =
            Box::leak(Box::new([CodeRange { start: 0x100, len: bytes.len() as u32, hash: fnv1a64(bytes) }]));
        SegmentSig { segment: seg, ranges }
    }

    fn ram(bytes: &'static [u8]) -> impl Fn(u32, &mut [u8]) -> bool {
        move |addr, buf| {
            let Some(o) = addr.checked_sub(0x100) else { return false };
            match bytes.get(o as usize..o as usize + buf.len()) {
                Some(s) => {
                    buf.copy_from_slice(s);
                    true
                }
                None => false,
            }
        }
    }

    #[test]
    fn only_the_overlay_present_in_ram_verifies() {
        let sigs: &'static [SegmentSig] = Box::leak(Box::new([sig_of("ov9_000", A), sig_of("ov9_001", B), sig_of("arm9_main", A)]));
        assert_eq!(resident_overlays(sigs, &ram(A)), ["ov9_000"]);
        assert_eq!(resident_overlays(sigs, &ram(B)), ["ov9_001"]);
        // partially overwritten (matches neither), and unreadable memory
        static MIX: [u8; 8] = [1, 2, 3, 4, 9, 9, 9, 9];
        assert!(resident_overlays(sigs, &ram(&MIX)).is_empty());
        assert!(resident_overlays(sigs, &|_, _| false).is_empty());
    }

    #[test]
    fn empty_signature_never_verifies() {
        let s = SegmentSig { segment: "ov9_002", ranges: &[] };
        assert!(!verify(&s, &ram(A)));
        let _ = &RA;
    }
}
