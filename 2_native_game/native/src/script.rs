//! Ordered host input snapshots for reproducible local runtime diagnostics.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Snapshot {
    pub frame: u64,
    pub buttons: u16,
    pub touch: Option<(u8, u8)>,
}

pub struct Script {
    events: Vec<Snapshot>,
    next: usize,
    current: Snapshot,
}

impl Script {
    pub fn parse(text: &str) -> Result<Self, String> {
        let mut events: Vec<Snapshot> = Vec::new();
        for (i, line) in text.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') || line == "frame,buttons,x,y" {
                continue;
            }
            let error = || {
                format!(
                    "Invalid input-script row {}: expected frame,buttons,x,y (blank x/y = pen up)",
                    i + 1
                )
            };
            let cols: Vec<_> = line.split(',').map(str::trim).collect();
            if cols.len() != 4 {
                return Err(error());
            }
            let frame = cols[0].parse::<u64>().map_err(|_| error())?;
            let buttons = if let Some(hex) = cols[1].strip_prefix("0x") {
                u16::from_str_radix(hex, 16)
            } else {
                cols[1].parse()
            }
            .map_err(|_| error())?;
            if buttons & !0xfff != 0 || events.last().is_some_and(|s| s.frame >= frame) {
                return Err(error());
            }
            let touch = if cols[2].is_empty() && cols[3].is_empty() {
                None
            } else {
                let x = cols[2].parse::<u8>().map_err(|_| error())?;
                let y = cols[3].parse::<u8>().map_err(|_| error())?;
                if y >= 192 {
                    return Err(error());
                }
                Some((x, y))
            };
            events.push(Snapshot {
                frame,
                buttons,
                touch,
            });
        }
        Ok(Self {
            events,
            next: 0,
            current: Snapshot::default(),
        })
    }

    pub fn at(&mut self, frame: u64) -> Snapshot {
        while let Some(&event) = self.events.get(self.next) {
            if event.frame > frame {
                break;
            }
            self.current = event;
            self.next += 1;
        }
        self.current
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn held_keys_release_and_skipped_frames_preserve_latest_snapshot() {
        let mut script = Script::parse("frame,buttons,x,y\n2,0x9,128,96\n7,0,,\n10,1,,").unwrap();
        assert_eq!(script.at(0), Snapshot::default());
        assert_eq!(script.at(2).touch, Some((128, 96)));
        assert_eq!(script.at(6).buttons, 9);
        assert_eq!(script.at(8).touch, None);
        assert_eq!(script.at(20).buttons, 1);
    }
    #[test]
    fn reject_ambiguous_order_and_invalid_host_input() {
        for text in [
            "2,1,,\n2,0,,",
            "2,1,,\n1,0,,",
            "0,0,12,192",
            "0,0,,12",
            "0,4096,,",
        ] {
            assert!(Script::parse(text).is_err(), "{text}");
        }
    }
}
