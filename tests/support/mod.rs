#![allow(dead_code)]

use fs_core::BlockRead;

pub struct Bytes(pub Vec<u8>);

impl BlockRead for Bytes {
    fn read_at(&self, offset: u64, output: &mut [u8]) -> fs_core::Result<()> {
        assert_eq!(offset % 512, 0);
        assert_eq!(output.len() % 512, 0);
        let start = usize::try_from(offset).map_err(|_| fs_core::Error::Custom("offset".into()))?;
        let data = self
            .0
            .get(start..start + output.len())
            .ok_or(fs_core::Error::ShortRead {
                offset,
                want: output.len(),
                got: 0,
            })?;
        output.copy_from_slice(data);
        Ok(())
    }

    fn size_bytes(&self) -> u64 {
        self.0.len() as u64
    }
}

pub fn put16(data: &mut [u8], offset: usize, value: u16) {
    data[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}

pub fn put32(data: &mut [u8], offset: usize, value: u32) {
    data[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

pub fn put64(data: &mut [u8], offset: usize, value: u64) {
    data[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

pub fn seal_boot(data: &mut [u8], sector: usize) {
    let mut checksum = 0u32;
    for (index, byte) in data[..sector * 11].iter().enumerate() {
        if ![106, 107, 112].contains(&index) {
            checksum = checksum.rotate_right(1).wrapping_add(u32::from(*byte));
        }
    }
    for position in (sector * 11..sector * 12).step_by(4) {
        put32(data, position, checksum);
    }
}

pub fn volume(shift: u8) -> Vec<u8> {
    let sector = 1usize << shift;
    let mut data = vec![0; 2 * 1024 * 1024];
    data[..3].copy_from_slice(&[0xeb, 0x76, 0x90]);
    data[3..11].copy_from_slice(b"EXFAT   ");
    put64(&mut data, 72, (2 * 1024 * 1024 / sector) as u64);
    put32(&mut data, 80, 24);
    put32(&mut data, 84, 8);
    put32(&mut data, 88, 32);
    put32(&mut data, 92, 100);
    put32(&mut data, 96, 2);
    put16(&mut data, 104, 0x100);
    data[108] = shift;
    data[109] = 0;
    data[110] = 1;
    put16(&mut data, 510, 0xaa55);
    for index in 1..=8 {
        put32(&mut data, (index + 1) * sector - 4, 0xaa55_0000);
    }
    seal_boot(&mut data, sector);
    data.copy_within(..sector * 12, sector * 12);
    data
}
