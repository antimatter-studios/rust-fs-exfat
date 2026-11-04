mod support;

use fs_exfat::timestamp::{Timestamp, Timestamps};
use support::{file_set, put32};

fn raw(year: u16, month: u8, day: u8) -> u32 {
    (u32::from(year - 1980) << 25) | (u32::from(month) << 21) | (u32::from(day) << 16)
}

#[test]
fn increments_add_odd_seconds_and_ten_millisecond_resolution() {
    let stamp = Timestamp::decode(raw(2000, 1, 1) | 29, 199, 0x80).unwrap();
    assert_eq!((stamp.second, stamp.millisecond), (59, 990));
    assert_eq!(stamp.unix_milliseconds(), Some(946_684_859_990));
}

#[test]
fn utc_offsets_are_signed_quarter_hours_and_unknown_is_explicit() {
    for (encoded, minutes) in [
        (0x80, 0),
        (0x84, 60),
        (0xff, -15),
        (0xc0, -960),
        (0xbf, 945),
    ] {
        let stamp = Timestamp::decode(raw(2000, 1, 1), 0, encoded).unwrap();
        assert_eq!(stamp.utc_offset_minutes, Some(minutes));
        assert_eq!(
            stamp.unix_milliseconds(),
            Some(946_684_800_000 - i64::from(minutes) * 60_000)
        );
    }
    let stamp = Timestamp::decode(raw(2000, 1, 1), 0, 0).unwrap();
    assert_eq!(stamp.utc_offset_minutes, None);
    assert_eq!(stamp.unix_milliseconds(), None);
    assert!(Timestamp::decode(raw(2000, 1, 1), 0, 1).is_err());
}

#[test]
fn leap_years_and_calendar_bounds_are_checked() {
    for year in [1980, 2000, 2024] {
        assert!(Timestamp::decode(raw(year, 2, 29), 0, 0).is_ok());
    }
    for (year, month, day) in [
        (2100, 2, 29),
        (2023, 2, 29),
        (2024, 4, 31),
        (2024, 0, 1),
        (2024, 13, 1),
        (2024, 1, 0),
    ] {
        assert!(Timestamp::decode(raw(year, month, day), 0, 0).is_err());
    }
    assert!(Timestamp::decode(raw(2107, 12, 31), 0, 0).is_ok());
    for time in [30, 60 << 5, 24 << 11] {
        assert!(Timestamp::decode(raw(2024, 1, 1) | time, 0, 0).is_err());
    }
    assert!(Timestamp::decode(raw(2024, 1, 1), 200, 0).is_err());
}

#[test]
fn all_three_entry_timestamps_decode_and_access_has_no_increment() {
    let mut set = file_set("time", 0x20, 0, 0, false);
    put32(&mut set, 8, raw(2024, 2, 29));
    put32(&mut set, 12, raw(2026, 11, 4));
    set[20] = 123;
    set[21] = 99;
    let timestamps = Timestamps::parse(&set[..32]).unwrap();
    assert_eq!(timestamps.created.day, 29);
    assert_eq!(
        (timestamps.created.second, timestamps.created.millisecond),
        (1, 230)
    );
    assert_eq!(timestamps.modified.millisecond, 990);
    assert_eq!(timestamps.accessed.millisecond, 0);
    assert!(Timestamps::parse(&set[..31]).is_err());
}
