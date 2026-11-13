//! Times, attributes and the volume label.

use super::dir::{
    attributes, seal, set_attributes, set_time, Stamp, ATTRIBUTE_DIRECTORY, SETTABLE_ATTRIBUTES,
};
use super::WriteVolume;
use crate::timestamp::Timestamp;
use crate::{Error, Result};
use fs_core::BlockDevice;

const ENTRY: usize = 32;
const LABEL: u8 = 0x83;
/// Section 7.3.2: a label holds at most 11 UTF-16 units.
const LABEL_UNITS: usize = 11;

/// Which of a file's times to change; `None` leaves one as it is.
/// Create and LastModified retain hundredths of a second. LastAccessed has
/// two-second resolution: odd seconds round down and milliseconds are discarded.
/// All three retain their UTC offsets.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SetTimes {
    pub created: Option<Timestamp>,
    pub modified: Option<Timestamp>,
    pub accessed: Option<Timestamp>,
}

impl<Device: BlockDevice> WriteVolume<Device> {
    /// Sets a file's or directory's Create, LastModified and LastAccessed
    /// times, retaining each UTC offset. Create and LastModified retain
    /// hundredths of a second; LastAccessed has no 10ms increment field, so
    /// odd seconds round down and milliseconds are discarded (two-second
    /// resolution). The root directory has no entry set and so no times.
    pub fn set_times(&mut self, path: &str, times: SetTimes) -> Result<()> {
        self.operation(|volume| {
            let target = volume.resolve(path)?;
            let (mut set, _) = target.found.ok_or(Error::NotFound)?;
            for (stamp, time) in [
                (Stamp::Created, times.created),
                (Stamp::Modified, times.modified),
                (Stamp::Accessed, times.accessed),
            ] {
                if let Some(time) = time {
                    set_time(&mut set.raw, stamp, &time)?;
                }
            }
            seal(&mut set.raw);
            volume.write_set(&set)
        })
    }

    /// Sets ReadOnly (0x01), Hidden (0x02), System (0x04) and Archive (0x20);
    /// the Directory bit stays as it is and any other bit is refused.
    pub fn set_attributes(&mut self, path: &str, value: u16) -> Result<()> {
        if value & !SETTABLE_ATTRIBUTES != 0 {
            return Err(Error::Invalid(
                "attributes other than ReadOnly, Hidden, System and Archive",
            ));
        }
        self.operation(|volume| {
            let target = volume.resolve(path)?;
            let (mut set, _) = target.found.ok_or(Error::NotFound)?;
            let kept = attributes(&set.raw) & ATTRIBUTE_DIRECTORY;
            set_attributes(&mut set.raw, kept | value);
            seal(&mut set.raw);
            volume.write_set(&set)
        })
    }

    /// Sets the volume label (Section 7.3): up to 11 UTF-16 units, with the
    /// characters a file name may not hold refused. An empty label is a
    /// Volume Label entry with no characters, as the macOS formatter writes.
    pub fn set_label(&mut self, label: &str) -> Result<()> {
        let units: Vec<u16> = label.encode_utf16().collect();
        if units.len() > LABEL_UNITS {
            return Err(Error::Invalid("volume label longer than 11 characters"));
        }
        if units
            .iter()
            .any(|unit| *unit < 0x20 || [34, 42, 47, 58, 60, 62, 63, 92, 124].contains(unit))
        {
            return Err(Error::Invalid("volume label character"));
        }
        let mut entry = vec![0u8; ENTRY];
        entry[0] = LABEL;
        entry[1] = units.len() as u8;
        for (index, unit) in units.iter().enumerate() {
            entry[2 + index * 2..4 + index * 2].copy_from_slice(&unit.to_le_bytes());
        }
        self.operation(|volume| {
            volume.root_changed = true;
            let (mut root, existing) = volume.root_entry(LABEL)?;
            match existing {
                Some(offset) => {
                    let set = super::dir::Set {
                        dir: root.first,
                        offset,
                        raw: entry,
                    };
                    volume.write_set(&set)
                }
                None => volume.insert(&mut root, entry).map(|_| ()),
            }
        })
    }
}
