//! The DS sound hardware (T062, recomp lane L7): 16 channels (PCM8, PCM16, IMA-ADPCM, and on the last
//! eight PSG square waves / noise) mixed to stereo, as the ARM7's sound registers drive it.
//!
//! Registers (ARM7 I/O, offset from `0x04000000`): channel `n` at `0x400 + 0x10 * n`: `CNT` (u32),
//! `SAD` source address (u32), `TMR` timer reload (u16), `PNT` loop start in words (u16), `LEN` length in
//! words (u32); `0x500` SOUNDCNT (master volume bits 0-6, enable bit 15), `0x504` SOUNDBIAS.
//! Channel `CNT`: volume 0-6 (127 = full), divider 8-9 (>>0, >>1, >>2, >>4), hold 15, pan 16-22
//! (0 left, 64 centre, 127 right), PSG duty 24-26, repeat 27-28 (0 manual, 1 loop, 2 one-shot),
//! format 29-30 (0 PCM8, 1 PCM16, 2 ADPCM, 3 PSG/noise), start/busy 31.
//!
//! Output is produced at [`SAMPLE_RATE`] by [`Spu::render`], which reads sample data from memory through
//! [`Mem`]. The channel timer runs at half the system clock (16.757 MHz): a channel advances one sample
//! every `0x10000 - TMR` timer ticks. Capture units and the exact analogue output stage (bias, low-pass)
//! are not modelled. Nothing here comes from the game.
use crate::rt::Mem;

pub const SAMPLE_RATE: u32 = 32_768;
/// Channel timer clock (system clock / 2).
const TIMER_HZ: u64 = 16_756_991;

const INDEX_STEP: [i32; 8] = [-1, -1, -1, -1, 2, 4, 6, 8];
const STEP_TABLE: [i32; 89] = [
    7, 8, 9, 10, 11, 12, 13, 14, 16, 17, 19, 21, 23, 25, 28, 31, 34, 37, 41, 45, 50, 55, 60, 66, 73, 80, 88, 97, 107, 118, 130,
    143, 157, 173, 190, 209, 230, 253, 279, 307, 337, 371, 408, 449, 494, 544, 598, 658, 724, 796, 876, 963, 1060, 1166, 1282,
    1411, 1552, 1707, 1878, 2066, 2272, 2499, 2749, 3024, 3327, 3660, 4026, 4428, 4871, 5358, 5894, 6484, 7132, 7845, 8630, 9493,
    10442, 11487, 12635, 13899, 15289, 16818, 18500, 20350, 22385, 24623, 27086, 29794, 32767,
];

#[derive(Clone, Copy, Default, Debug)]
pub struct Channel {
    pub cnt: u32,
    pub sad: u32,
    pub tmr: u16,
    pub pnt: u16,
    pub len: u32,
    // playback state
    pos: u32,        // current byte address (PCM/ADPCM)
    sample_index: u32, // samples played since the start / loop point (PSG step counter)
    acc: u64,        // timer ticks accumulated toward the next sample
    cur: i16,        // current output sample
    pred: i32,       // ADPCM predictor
    index: i32,      // ADPCM step index
    loop_pred: i32,
    loop_index: i32,
    lfsr: u16,
}

impl Channel {
    pub fn playing(&self) -> bool {
        self.cnt & 0x8000_0000 != 0
    }
    pub fn volume(&self) -> i32 {
        (self.cnt & 0x7F) as i32
    }
    pub fn divider(&self) -> u32 {
        [0, 1, 2, 4][((self.cnt >> 8) & 3) as usize]
    }
    pub fn pan(&self) -> i32 {
        ((self.cnt >> 16) & 0x7F) as i32
    }
    pub fn duty(&self) -> u32 {
        (self.cnt >> 24) & 7
    }
    pub fn repeat(&self) -> u32 {
        (self.cnt >> 27) & 3
    }
    pub fn format(&self) -> u32 {
        (self.cnt >> 29) & 3
    }
}

pub struct Spu {
    pub ch: [Channel; 16],
    pub soundcnt: u16,
    pub bias: u16,
    /// Output sample position accumulator (timer ticks per output sample, as a fraction of `SAMPLE_RATE`).
    frac: u64,
    /// Diagnostics: channel starts seen, per channel.
    pub starts: [u32; 16],
}

impl Default for Spu {
    fn default() -> Self {
        Spu::new()
    }
}

impl Spu {
    pub fn new() -> Spu {
        Spu { ch: [Channel::default(); 16], soundcnt: 0, bias: 0x200, frac: 0, starts: [0; 16] }
    }

    pub fn read(&self, off: u32) -> Option<u32> {
        match off {
            0x400..=0x4FF => {
                let c = &self.ch[((off - 0x400) >> 4) as usize];
                Some(match off & 0xC {
                    0x0 => c.cnt,
                    0x4 => c.sad,
                    0x8 => c.tmr as u32 | (c.pnt as u32) << 16,
                    _ => c.len,
                })
            }
            0x500 => Some(self.soundcnt as u32),
            0x504 => Some(self.bias as u32),
            _ => None,
        }
    }

    /// Write `n` bytes (1, 2 or 4) of value `v` at `off`. Returns whether the offset is a sound register.
    pub fn write(&mut self, off: u32, v: u32, n: usize) -> bool {
        let merge = |old: u32, shift: u32| {
            let mask = if n >= 4 { u32::MAX } else { ((1u32 << (8 * n)) - 1) << shift };
            (old & !mask) | ((v << shift) & mask)
        };
        match off {
            0x400..=0x4FF => {
                let word = off & !3;
                let shift = 8 * (off & 3);
                let chn = ((word - 0x400) >> 4) as usize;
                let c = &mut self.ch[chn];
                match word & 0xC {
                    0x0 => {
                        let was = c.playing();
                        c.cnt = merge(c.cnt, shift);
                        if !was && c.playing() {
                            Self::start(c);
                            self.starts[chn] += 1;
                        }
                    }
                    0x4 => c.sad = merge(c.sad, shift) & 0x07FF_FFFC,
                    0x8 => {
                        let w = merge(c.tmr as u32 | (c.pnt as u32) << 16, shift);
                        c.tmr = w as u16;
                        c.pnt = (w >> 16) as u16;
                    }
                    _ => c.len = merge(c.len, shift) & 0x3F_FFFF,
                }
                true
            }
            0x500 | 0x501 => {
                self.soundcnt = merge(self.soundcnt as u32, 8 * (off & 1)) as u16 & 0xBF7F;
                true
            }
            0x504 | 0x505 => {
                self.bias = merge(self.bias as u32, 8 * (off & 1)) as u16 & 0x3FF;
                true
            }
            _ => false,
        }
    }

    fn start(c: &mut Channel) {
        c.pos = c.sad;
        c.sample_index = 0;
        c.acc = 0;
        c.cur = 0;
        c.lfsr = 0x7FFF;
        c.pred = 0;
        c.index = 0;
        // the first sample is fetched on the first timer tick; ADPCM reads its header then
    }

    /// Renders `frames` stereo samples at [`SAMPLE_RATE`].
    pub fn render(&mut self, mem: &mut dyn Mem, frames: usize) -> Vec<[i16; 2]> {
        let mut out = Vec::with_capacity(frames);
        let enabled = self.soundcnt & 0x8000 != 0;
        let master = (self.soundcnt & 0x7F) as i64;
        for _ in 0..frames {
            // timer ticks that elapse during one output sample
            self.frac += TIMER_HZ;
            let ticks = self.frac / SAMPLE_RATE as u64;
            self.frac %= SAMPLE_RATE as u64;
            let (mut l, mut r) = (0i64, 0i64);
            for n in 0..16 {
                let c = &mut self.ch[n];
                if !c.playing() {
                    continue;
                }
                Self::advance(c, n, mem, ticks);
                if !c.playing() {
                    continue;
                }
                let mut s = c.cur as i64 * c.volume() as i64;
                s >>= c.divider();
                // /128 for the volume scale; pan splits it (0 = all left, 127 = all right)
                s >>= 7;
                let pan = c.pan() as i64;
                l += s * (127 - pan) / 127;
                r += s * pan / 127;
            }
            if !enabled {
                out.push([0, 0]);
                continue;
            }
            let scale = |v: i64| -> i16 { (v * master / 127).clamp(-32768, 32767) as i16 };
            out.push([scale(l), scale(r)]);
        }
        out
    }

    fn advance(c: &mut Channel, n: usize, mem: &mut dyn Mem, ticks: u64) {
        let period = 0x10000u64 - c.tmr as u64;
        c.acc += ticks;
        let steps = c.acc / period;
        c.acc %= period;
        for _ in 0..steps {
            if !Self::step(c, n, mem) {
                c.cnt &= !0x8000_0000; // finished
                c.cur = 0;
                return;
            }
        }
    }

    /// Produce the next sample of the channel; false when a one-shot ended.
    fn step(c: &mut Channel, n: usize, mem: &mut dyn Mem) -> bool {
        match c.format() {
            0 | 1 => {
                let wide = c.format() == 1;
                let unit = if wide { 2 } else { 1 };
                let end = c.sad + (c.pnt as u32 + c.len) * 4;
                if c.pos >= end {
                    if c.repeat() == 1 {
                        c.pos = c.sad + c.pnt as u32 * 4;
                    } else {
                        return false;
                    }
                }
                c.cur = if wide { mem.r16(c.pos) as i16 } else { (mem.r8(c.pos) as i8 as i16) << 8 };
                c.pos += unit;
            }
            2 => {
                if c.sample_index == 0 && c.pos == c.sad {
                    // header: initial predictor (16 bit) and step index (7 bit)
                    let h = mem.r32(c.pos);
                    c.pred = h as u16 as i16 as i32;
                    c.index = ((h >> 16) & 0x7F).min(88) as i32;
                    c.pos += 4;
                    c.sample_index = 1;
                }
                let loop_start = c.sad + c.pnt as u32 * 4;
                let end = c.sad + (c.pnt as u32 + c.len) * 4;
                // the loop point is a word boundary: remember the decoder state when we reach it
                if c.pos == loop_start && (c.sample_index & 1) == 1 {
                    c.loop_pred = c.pred;
                    c.loop_index = c.index;
                }
                if c.pos >= end {
                    if c.repeat() == 1 {
                        c.pos = loop_start;
                        c.pred = c.loop_pred;
                        c.index = c.loop_index;
                        c.sample_index = 1;
                    } else {
                        return false;
                    }
                }
                let byte = mem.r8(c.pos);
                // two samples per byte, low nibble first; an odd sample_index means the low nibble is next
                let half_low = c.sample_index & 1 == 1;
                let code = if half_low { byte & 0xF } else { byte >> 4 } as i32;
                let step = STEP_TABLE[c.index as usize];
                let mut diff = step >> 3;
                if code & 1 != 0 {
                    diff += step >> 2;
                }
                if code & 2 != 0 {
                    diff += step >> 1;
                }
                if code & 4 != 0 {
                    diff += step;
                }
                c.pred = if code & 8 != 0 { c.pred - diff } else { c.pred + diff }.clamp(-32768, 32767);
                c.index = (c.index + INDEX_STEP[(code & 7) as usize]).clamp(0, 88);
                c.cur = c.pred as i16;
                if !half_low {
                    c.pos += 1; // both nibbles of the byte are done
                }
                c.sample_index += 1;
            }
            _ => {
                if n >= 14 {
                    // noise: 15-bit LFSR, output +-0x7FFF
                    let carry = c.lfsr & 1 != 0;
                    c.lfsr >>= 1;
                    if carry {
                        c.lfsr ^= 0x6000;
                        c.cur = -0x7FFF;
                    } else {
                        c.cur = 0x7FFF;
                    }
                } else if n >= 8 {
                    // square wave: 8 steps per period, the first (duty + 1) high ... duty 7 is silent
                    let step = (c.sample_index & 7) as u32;
                    c.cur = if c.duty() == 7 {
                        -0x7FFF
                    } else if step < c.duty() + 1 {
                        0x7FFF
                    } else {
                        -0x7FFF
                    };
                    c.sample_index += 1;
                } else {
                    c.cur = 0; // channels 0-7 cannot play PSG
                }
            }
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[derive(Default)]
    struct M(HashMap<u32, u8>);
    impl Mem for M {
        fn r8(&mut self, a: u32) -> u8 {
            *self.0.get(&a).unwrap_or(&0)
        }
        fn r16(&mut self, a: u32) -> u16 {
            self.r8(a) as u16 | (self.r8(a + 1) as u16) << 8
        }
        fn r32(&mut self, a: u32) -> u32 {
            self.r16(a) as u32 | (self.r16(a + 2) as u32) << 16
        }
        fn w8(&mut self, a: u32, v: u8) {
            self.0.insert(a, v);
        }
        fn w16(&mut self, a: u32, v: u16) {
            self.w8(a, v as u8);
            self.w8(a + 1, (v >> 8) as u8);
        }
        fn w32(&mut self, a: u32, v: u32) {
            self.w16(a, v as u16);
            self.w16(a + 2, (v >> 16) as u16);
        }
    }

    /// Timer reload so a channel steps once per output sample (about 511 timer ticks per output sample).
    const TMR_1X: u16 = (0x10000 - 511) as u16;

    fn setup(spu: &mut Spu, chn: u32, cnt: u32, sad: u32, pnt: u16, len: u32) {
        let base = 0x400 + 0x10 * chn;
        spu.write(0x500, 0x8000 | 127, 2);
        spu.write(base + 4, sad, 4);
        spu.write(base + 8, TMR_1X as u32 | (pnt as u32) << 16, 4);
        spu.write(base + 0xC, len, 4);
        spu.write(base, cnt | 0x8000_0000, 4);
    }

    #[test]
    fn pcm16_one_shot_plays_then_stops() {
        let mut m = M::default();
        for i in 0..4u32 {
            m.w16(0x100 + 2 * i, (1000 * (i + 1)) as u16);
        }
        let mut s = Spu::new();
        // volume 127, centre pan, PCM16, one-shot; 2 words = 4 samples
        setup(&mut s, 0, 127 | 64 << 16 | 2 << 27 | 1 << 29, 0x100, 0, 2);
        let out = s.render(&mut m, 8);
        let left: Vec<i16> = out.iter().map(|f| f[0]).collect();
        assert!(left[0] > 0 && left[1] > left[0] && left[3] > left[2]);
        assert_eq!(left[5], 0, "silent after the data ends");
        assert!(!s.ch[0].playing());
        // centre pan puts about half in each ear
        assert!((out[2][0] as i32 - out[2][1] as i32).abs() <= out[2][0] as i32 / 40 + 1);
    }

    #[test]
    fn pcm8_loops_back_to_the_loop_point() {
        let mut m = M::default();
        for (i, b) in [10u8, 20, 30, 40, 50, 60, 70, 80].iter().enumerate() {
            m.w8(0x200 + i as u32, *b);
        }
        let mut s = Spu::new();
        // PCM8, loop from word 1 (byte 4), length 1 word: plays 10 20 30 40 | 50 60 70 80 | 50 60 ...
        setup(&mut s, 3, 127 | 1 << 27, 0x200, 1, 1);
        s.ch[3].cnt |= 127 << 16; // pan hard right
        let out = s.render(&mut m, 12);
        let right: Vec<i16> = out.iter().map(|f| f[1]).collect();
        assert!(right[8] > 0 && out[8][0] == 0);
        // after 8 samples the channel loops: sample 8 == sample 4
        assert_eq!(right[8], right[4]);
        assert_eq!(right[9], right[5]);
        assert!(s.ch[3].playing());
    }

    #[test]
    fn adpcm_decodes_known_codes() {
        let mut m = M::default();
        // header: predictor 0, index 0; data byte 0x70 = codes 7 (high nibble first), 0
        m.w32(0x300, 0);
        m.w8(0x304, 0x07);
        let mut s = Spu::new();
        // length: header word + 1 data word = 2 words
        setup(&mut s, 1, 127 | 64 << 16 | 2 << 27 | 2 << 29, 0x300, 0, 2);
        let out = s.render(&mut m, 3);
        // step 7: code 0 -> diff 7>>3 = 0 ; our decoder takes the low nibble second per hardware order (low first)
        // 0x07: low nibble 7 first: diff = 0 + 1 + 3 + 7 = 11 ; then index += 8 -> 8, step 16: code 0 -> diff 2
        let l0 = (11i64 * 127 >> 7) * 63 / 127;
        assert!((out[0][0] as i64 - l0 * 127 / 127).abs() <= 8, "first sample about {l0}, got {}", out[0][0]);
        assert!(out[1][0] > out[0][0], "second sample steps further up by the larger step");
    }

    #[test]
    fn psg_square_duty_and_noise() {
        let mut m = M::default();
        let mut s = Spu::new();
        // channel 8, duty 3 (4 of 8 steps high), TMR for one step per output sample
        setup(&mut s, 8, 127 | 64 << 16 | 3 << 24 | 3 << 29, 0, 0, 0);
        let out: Vec<i16> = s.render(&mut m, 16).iter().map(|f| f[0]).collect();
        let high = out.iter().filter(|&&v| v > 0).count();
        assert_eq!(high, 8, "50% duty over two periods");
        // channel 14 is noise: both polarities appear
        let mut s = Spu::new();
        setup(&mut s, 14, 127 | 64 << 16 | 3 << 29, 0, 0, 0);
        let out: Vec<i16> = s.render(&mut m, 64).iter().map(|f| f[0]).collect();
        assert!(out.iter().any(|&v| v > 0) && out.iter().any(|&v| v < 0));
        // channels below 8 cannot play PSG
        let mut s = Spu::new();
        setup(&mut s, 2, 127 | 64 << 16 | 3 << 29, 0, 0, 0);
        assert!(s.render(&mut m, 8).iter().all(|f| f[0] == 0));
    }

    #[test]
    fn volume_divider_master_and_enable() {
        let mut m = M::default();
        for i in 0..8u32 {
            m.w16(0x400 + 2 * i, 20000);
        }
        let run = |cnt: u32, master: u32| {
            let mut s = Spu::new();
            setup(&mut s, 0, cnt | 64 << 16 | 1 << 29 | 1 << 27, 0x400, 0, 4);
            s.write(0x500, 0x8000 | master, 2);
            s.render(&mut M(m.0.clone()), 4)[3][0] as i64
        };
        let full = run(127, 127);
        assert!(full > 0);
        assert!((run(127 | 1 << 8, 127) - full / 2).abs() <= 2, "divider 1 halves");
        assert!((run(127 | 3 << 8, 127) - full / 16).abs() <= 2, "divider 3 is >>4");
        assert!((run(64, 127) - full / 2).abs() <= 400, "half volume");
        assert!((run(127, 64) - full / 2).abs() <= 400, "half master volume");
        // master disabled
        let mut s = Spu::new();
        setup(&mut s, 0, 127 | 1 << 29 | 1 << 27, 0x400, 0, 4);
        s.write(0x500, 127, 2);
        assert!(s.render(&mut M(m.0.clone()), 4).iter().all(|f| *f == [0, 0]));
    }

    #[test]
    fn timer_sets_the_playback_speed() {
        let mut m = M::default();
        for i in 0..64u32 {
            m.w16(0x500 + 2 * i, (i * 100) as u16);
        }
        let mut s = Spu::new();
        // half-speed timer: one sample per two output samples
        setup(&mut s, 0, 127 | 64 << 16 | 1 << 29 | 1 << 27, 0x500, 0, 32);
        s.write(0x400 + 8, ((0x10000 - 1022) as u32) & 0xFFFF, 2);
        let out: Vec<i16> = s.render(&mut m, 8).iter().map(|f| f[0]).collect();
        // the timer needs two output periods per step, so the first sample appears late and is then held twice
        assert_eq!(out[0], out[1]);
        assert_eq!(out[2], out[1]);
        assert!(out[3] > out[2], "next sample after two more output periods");
        assert_eq!(out[4], out[3], "held for two outputs");
    }

    #[test]
    fn register_readback() {
        let mut s = Spu::new();
        assert!(s.write(0x410, 0x1234, 4));
        assert_eq!(s.read(0x410), Some(0x1234));
        s.write(0x404 + 0x10, 0x0200_0123, 4);
        assert_eq!(s.read(0x414), Some(0x0200_0120), "source address is word aligned");
        assert!(!s.write(0x130, 0, 2));
        assert_eq!(s.read(0x130), None);
    }
}
