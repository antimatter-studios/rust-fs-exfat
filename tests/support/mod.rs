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

pub fn upcase_bytes() -> Vec<u8> {
    let mut words = vec![0xffff, 97];
    words.extend(65..=90);
    words.extend([0xffff, 65413]);
    words.into_iter().flat_map(u16::to_le_bytes).collect()
}

pub fn metadata_volume() -> Vec<u8> {
    let mut data = volume(9);
    for (cluster, value) in [
        (0, 0xffff_fff8),
        (1, 0xffff_ffff),
        (2, 0xffff_ffff),
        (3, 0xffff_ffff),
        (4, 0xffff_ffff),
    ] {
        put32(&mut data, 24 * 512 + cluster * 4, value);
    }
    let root = 32 * 512;
    data[root] = 0x81;
    put32(&mut data, root + 20, 3);
    put64(&mut data, root + 24, 13);
    let upcase = upcase_bytes();
    data[root + 32] = 0x82;
    put32(
        &mut data,
        root + 36,
        fs_exfat::metadata::checksum32(&upcase),
    );
    put32(&mut data, root + 52, 4);
    put64(&mut data, root + 56, upcase.len() as u64);
    data[root + 64] = 0x83;
    data[root + 65] = 6;
    for (index, unit) in "M1TEST".encode_utf16().enumerate() {
        put16(&mut data, root + 66 + index * 2, unit);
    }
    data[33 * 512] = 7;
    data[34 * 512..34 * 512 + upcase.len()].copy_from_slice(&upcase);
    data
}

pub fn file_set(name: &str, attributes: u16, first: u32, length: u64, contiguous: bool) -> Vec<u8> {
    let name: Vec<_> = name.encode_utf16().collect();
    let count = name.len().div_ceil(15);
    let mut set = vec![0; (count + 2) * 32];
    set[0] = 0x85;
    set[1] = (count + 1) as u8;
    put16(&mut set, 4, attributes);
    for offset in [8, 12, 16] {
        put32(&mut set, offset, (46 << 25) | (11 << 21) | (4 << 16));
    }
    set[22..25].fill(0x80);
    set[32] = 0xc0;
    set[33] = if contiguous { 3 } else { 1 };
    set[35] = name.len() as u8;
    let upcase = upcase_bytes();
    let table =
        fs_exfat::metadata::UpcaseTable::parse(&upcase, fs_exfat::metadata::checksum32(&upcase))
            .unwrap();
    put16(&mut set, 36, table.name_hash(&name));
    put64(&mut set, 40, length);
    put32(&mut set, 52, first);
    put64(&mut set, 56, length);
    for (index, chunk) in name.chunks(15).enumerate() {
        set[(index + 2) * 32] = 0xc1;
        for (position, unit) in chunk.iter().enumerate() {
            put16(&mut set, (index + 2) * 32 + 2 + position * 2, *unit);
        }
    }
    seal_set(&mut set);
    set
}

pub fn seal_set(set: &mut [u8]) {
    let checksum = fs_exfat::metadata::set_checksum(set);
    put16(set, 2, checksum);
}
