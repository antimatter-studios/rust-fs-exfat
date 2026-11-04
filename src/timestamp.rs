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
