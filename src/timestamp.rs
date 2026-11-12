use crate::{Error, Result};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Timestamp {
    pub year: u16,
    pub month: u8,
    pub day: u8,
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
    pub millisecond: u16,
    pub utc_offset_minutes: Option<i16>,
}

fn leap(year: u16) -> bool {
    year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400))
}

fn month_days(year: u16, month: u8) -> Option<u8> {
    match month {
        2 => Some(if leap(year) { 29 } else { 28 }),
        4 | 6 | 9 | 11 => Some(30),
        1 | 3 | 5 | 7 | 8 | 10 | 12 => Some(31),
        _ => None,
    }
}

impl Timestamp {
    pub fn decode(raw: u32, increment: u8, offset: u8) -> Result<Self> {
        if increment > 199 || raw & 31 > 29 || (offset & 0x80 == 0 && offset != 0) {
            return Err(Error::Invalid("timestamp seconds, increment or UTC offset"));
        }
        let timestamp = Self {
            year: 1980 + (raw >> 25) as u16,
            month: ((raw >> 21) & 15) as u8,
            day: ((raw >> 16) & 31) as u8,
            hour: ((raw >> 11) & 31) as u8,
            minute: ((raw >> 5) & 63) as u8,
            second: ((raw & 31) * 2) as u8 + increment / 100,
            millisecond: u16::from(increment % 100) * 10,
            utc_offset_minutes: if offset & 0x80 == 0 {
                None
            } else {
                Some(i16::from(((offset << 1) as i8) >> 1) * 15)
            },
        };
        timestamp.validate()?;
        Ok(timestamp)
    }

    fn validate(&self) -> Result<()> {
        let days = month_days(self.year, self.month).ok_or(Error::Invalid("timestamp month"))?;
        if !(1980..=2107).contains(&self.year)
            || self.day == 0
            || self.day > days
            || self.hour > 23
            || self.minute > 59
            || self.second > 59
            || self.millisecond > 999
            || self
                .utc_offset_minutes
                .is_some_and(|offset| !(-960..=945).contains(&offset) || offset % 15 != 0)
        {
            return Err(Error::Invalid("timestamp calendar date or time"));
        }
        Ok(())
    }

    /// The timestamp at `milliseconds` since the Unix epoch, as local time at
    /// `utc_offset_minutes` east of UTC, which must be a multiple of 15
    /// minutes from -16:00 to +15:45 (Section 7.4.10). exFAT stores
    /// hundredths of a second, so milliseconds are truncated to tens.
    pub fn from_unix_milliseconds(milliseconds: i64, utc_offset_minutes: i16) -> Result<Self> {
        if !(-960..=945).contains(&utc_offset_minutes) || utc_offset_minutes % 15 != 0 {
            return Err(Error::Invalid("timestamp UTC offset"));
        }
        let local = milliseconds
            .checked_add(i64::from(utc_offset_minutes) * 60_000)
            .ok_or(Error::Invalid("timestamp outside the exFAT range"))?;
        let days = local.div_euclid(86_400_000);
        let within = local.rem_euclid(86_400_000);
        let (year, month, day) = civil_from_days(days);
        let year = u16::try_from(year)
            .ok()
            .filter(|year| (1980..=2107).contains(year))
            .ok_or(Error::Invalid("timestamp outside the exFAT range"))?;
        let timestamp = Self {
            year,
            month,
            day,
            hour: (within / 3_600_000) as u8,
            minute: (within / 60_000 % 60) as u8,
            second: (within / 1000 % 60) as u8,
            millisecond: (within % 1000 / 10 * 10) as u16,
            utc_offset_minutes: Some(utc_offset_minutes),
        };
        timestamp.validate()?;
        Ok(timestamp)
    }

    /// The current time, in UTC.
    pub fn now() -> Result<Self> {
        let elapsed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|_| Error::Invalid("system clock before the Unix epoch"))?;
        let milliseconds =
            i64::try_from(elapsed.as_millis()).map_err(|_| Error::Invalid("system clock"))?;
        Self::from_unix_milliseconds(milliseconds, 0)
    }

    /// The Timestamp field, the 10ms increment and the UTC offset byte of
    /// Sections 7.4.8 to 7.4.10. The increment holds the odd second and the
    /// hundredths, since the Timestamp field counts seconds in twos.
    pub fn encode(&self) -> Result<(u32, u8, u8)> {
        self.validate()?;
        let raw = (u32::from(self.year - 1980) << 25)
            | (u32::from(self.month) << 21)
            | (u32::from(self.day) << 16)
            | (u32::from(self.hour) << 11)
            | (u32::from(self.minute) << 5)
            | u32::from(self.second / 2);
        let increment = (self.second % 2) * 100 + (self.millisecond / 10) as u8;
        let offset = match self.utc_offset_minutes {
            Some(minutes) => 0x80 | ((minutes / 15) as i8 as u8 & 0x7f),
            None => 0,
        };
        Ok((raw, increment, offset))
    }

    pub fn unix_milliseconds(&self) -> Option<i64> {
        self.validate().ok()?;
        let offset = self.utc_offset_minutes?;
        let mut days: i64 = (1970..self.year)
            .map(|year| if leap(year) { 366 } else { 365 })
            .sum();
        days += (1..self.month)
            .map(|month| i64::from(month_days(self.year, month).unwrap()))
            .sum::<i64>();
        days += i64::from(self.day) - 1;
        Some(
            days * 86_400_000
                + i64::from(self.hour) * 3_600_000
                + i64::from(self.minute) * 60_000
                + i64::from(self.second) * 1000
                + i64::from(self.millisecond)
                - i64::from(offset) * 60_000,
        )
    }
}

/// The proleptic Gregorian date `days` after 1970-01-01.
fn civil_from_days(days: i64) -> (i64, u8, u8) {
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let of_era = shifted.rem_euclid(146_097);
    let year_of_era = (of_era - of_era / 1460 + of_era / 36_524 - of_era / 146_096) / 365;
    let day_of_year = of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * shifted_month + 2) / 5 + 1) as u8;
    let month = if shifted_month < 10 {
        shifted_month + 3
    } else {
        shifted_month - 9
    } as u8;
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Timestamps {
    pub created: Timestamp,
    pub modified: Timestamp,
    pub accessed: Timestamp,
}

impl Timestamps {
    pub fn parse(primary: &[u8]) -> Result<Self> {
        if primary.len() != 32 {
            return Err(Error::Invalid("timestamp primary entry length"));
        }
        let raw = |offset| u32::from_le_bytes(primary[offset..offset + 4].try_into().unwrap());
        Ok(Self {
            created: Timestamp::decode(raw(8), primary[20], primary[22])?,
            modified: Timestamp::decode(raw(12), primary[21], primary[23])?,
            accessed: Timestamp::decode(raw(16), 0, primary[24])?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encoding_round_trips_through_the_decoder_across_the_range() {
        for milliseconds in [
            315_532_800_000i64,
            951_782_399_990,
            1_700_000_001_120,
            1_709_251_199_990,
            4_354_819_199_990,
        ] {
            for offset in [-960, -330, 0, 15, 945] {
                let utc = milliseconds - i64::from(offset) * 60_000;
                let Ok(timestamp) = Timestamp::from_unix_milliseconds(utc, offset) else {
                    continue;
                };
                let (raw, increment, byte) = timestamp.encode().unwrap();
                let decoded = Timestamp::decode(raw, increment, byte).unwrap();
                assert_eq!(decoded, timestamp);
                assert_eq!(decoded.unix_milliseconds(), Some(utc));
            }
        }
    }

    #[test]
    fn the_range_and_offsets_are_enforced() {
        assert!(Timestamp::from_unix_milliseconds(315_532_799_999, 0).is_err());
        assert!(Timestamp::from_unix_milliseconds(4_354_819_200_000, 0).is_err());
        assert!(Timestamp::from_unix_milliseconds(0, 7).is_err());
        assert!(Timestamp::from_unix_milliseconds(i64::MAX, 945).is_err());
        let leap = Timestamp::from_unix_milliseconds(951_825_600_123, 0).unwrap();
        assert_eq!(
            (leap.year, leap.month, leap.day, leap.millisecond),
            (2000, 2, 29, 120)
        );
        let (_, increment, offset) = Timestamp::from_unix_milliseconds(1_700_000_001_120, -60)
            .unwrap()
            .encode()
            .unwrap();
        assert_eq!((increment, offset), (112, 0x80 | 0x7c));
    }
}
