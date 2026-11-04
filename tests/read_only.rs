use fs_core::BlockRead;
use fs_exfat::device::Reader;
use std::sync::{Arc, Mutex};

type Calls = Arc<Mutex<Vec<(u64, usize)>>>;

struct ReadOnly {
    calls: Calls,
}

impl BlockRead for ReadOnly {
    fn read_at(&self, offset: u64, output: &mut [u8]) -> fs_core::Result<()> {
        assert_eq!(offset % 512, 0);
        assert_eq!(output.len() % 512, 0);
        self.calls.lock().unwrap().push((offset, output.len()));
        for (index, byte) in output.iter_mut().enumerate() {
            *byte = (offset as usize + index) as u8;
        }
        Ok(())
    }

    fn size_bytes(&self) -> u64 {
        2048
    }
}

fn reader() -> (Reader<ReadOnly>, Calls) {
    let calls = Arc::new(Mutex::new(Vec::new()));
    (
        Reader::new(ReadOnly {
            calls: calls.clone(),
        }),
        calls,
    )
}

#[test]
fn accepts_a_device_implementing_only_block_read() {
    let (reader, _) = reader();
    assert_eq!(reader.size_bytes(), 2048);
}

#[test]
fn unaligned_read_uses_only_aligned_device_reads() {
    let (reader, calls) = reader();
    let mut output = [0; 517];
    reader.read_at(509, &mut output).unwrap();
    assert_eq!(*calls.lock().unwrap(), [(0, 512), (512, 512), (1024, 512)]);
    assert_eq!(output[0], 253);
    assert_eq!(output[516], 1);
}

#[test]
fn out_of_bounds_and_overflow_do_not_reach_device() {
    let (reader, calls) = reader();
    assert!(reader.read_at(2048, &mut [0]).is_err());
    assert!(reader.read_at(u64::MAX, &mut [0]).is_err());
    assert!(calls.lock().unwrap().is_empty());
}

#[test]
fn empty_read_at_end_does_not_reach_device() {
    let (reader, calls) = reader();
    reader.read_at(2048, &mut []).unwrap();
    assert!(calls.lock().unwrap().is_empty());
}
