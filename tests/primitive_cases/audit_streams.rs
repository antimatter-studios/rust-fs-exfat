use fs_exfat::primitives::{
    bitmap_contains_all, bitmap_count, claim_bitmap_cluster, decode_fat_entry, directory_record,
    walk_allocation, FatChainStep, FatValue, FatWindow, StreamCursor, StreamReadError, VolumeState,
};
use fs_exfat::Error;
use std::cell::RefCell;

#[test]
fn volume_state_retains_active_fat_and_both_health_flags() {
    for flags in 0..=u16::MAX {
        let state = VolumeState::from_flags(flags);
        assert_eq!(state.active_fat, usize::from(flags & 1));
        assert_eq!(state.dirty, flags & 2 != 0);
        assert_eq!(state.media_failure, flags & 4 != 0);
    }
}

#[test]
fn directory_record_extents_preserve_deleted_and_benign_framing() {
    for kind in 0..=u8::MAX {
        for count in [0, 1, 18, 255] {
            let mut bytes = [0; 32];
            bytes[0] = kind;
            bytes[1] = count;
            let record = directory_record(&bytes).unwrap();
            let secondaries = if kind & 0x80 != 0 && (kind == 0x85 || kind & 0xe0 == 0xa0) {
                usize::from(count)
            } else {
                0
            };
            assert_eq!(record.entry_type.value, kind);
            assert_eq!(record.length, (secondaries + 1) * 32);
        }
    }
    for length in 0..32 {
        assert!(directory_record(&[0; 32][..length]).is_none());
    }
}

#[test]
fn fat_windows_cache_chunks_without_hiding_raw_damage_or_io_failures() {
    let mut bytes = vec![0; 65536 + 16];
    bytes[8..12].copy_from_slice(&u32::MAX.to_le_bytes());
    bytes[65536..65540].copy_from_slice(&0xffff_fff7u32.to_le_bytes());
    bytes[65540..65544].copy_from_slice(&1u32.to_le_bytes());
    let mut window = FatWindow::new(4096, bytes.len() as u64, 20000);
    let calls = RefCell::new(Vec::new());
    let mut read = |offset: u64, output: &mut [u8]| {
        calls.borrow_mut().push((offset, output.len()));
        let start = (offset - 4096) as usize;
        output.copy_from_slice(&bytes[start..start + output.len()]);
        Ok(())
    };
    assert_eq!(window.entry(2, &mut read).unwrap().kind, FatValue::End);
    assert_eq!(window.entry(3, &mut read).unwrap().kind, FatValue::Free);
    assert_eq!(window.entry(16384, &mut read).unwrap().kind, FatValue::Bad);
    let damaged = window.entry(16385, &mut read).unwrap();
    assert_eq!(damaged.kind, FatValue::Reserved);
    assert_eq!(damaged.value, 1);
    assert!(!damaged.valid);
    assert_eq!(*calls.borrow(), [(4096, 65536), (69632, 16)]);
    assert!(matches!(
        window.entry(2, |_, _| Err(Error::Unsupported("injected read"))),
        Err(Error::Unsupported("injected read"))
    ));
    assert_eq!(window.entry(2, &mut read).unwrap().kind, FatValue::End);
    assert_eq!(calls.borrow().len(), 3);
    assert!(matches!(window.entry(20000, &mut read), Err(Error::Bounds)));
    let mut overflowing = FatWindow::new(u64::MAX, 65552, 20000);
    assert!(matches!(
        overflowing.entry(16384, |_, _| panic!("overflowing physical FAT read")),
        Err(Error::Bounds)
    ));
}

#[test]
fn allocation_walks_are_lazy_and_leave_cycle_ownership_to_the_caller() {
    let reads = RefCell::new(Vec::new());
    let mut walk = walk_allocation(2, 10, None, |cluster| {
        reads.borrow_mut().push(cluster);
        Ok(decode_fat_entry(if cluster == 2 { 3 } else { 2 }, 10))
    });
    assert!(std::mem::size_of_val(&walk) < 256);
    assert!(
        matches!(walk.next().unwrap().unwrap(), FatChainStep::Cluster(position) if position.cluster == 2)
    );
    assert!(reads.borrow().is_empty());
    assert!(matches!(
        walk.next().unwrap().unwrap(),
        FatChainStep::Entry { cluster: 2, .. }
    ));
    assert!(
        matches!(walk.next().unwrap().unwrap(), FatChainStep::Cluster(position) if position.cluster == 3)
    );
    assert!(matches!(
        walk.next().unwrap().unwrap(),
        FatChainStep::Entry { cluster: 3, .. }
    ));
    assert!(
        matches!(walk.next().unwrap().unwrap(), FatChainStep::Cluster(position) if position.cluster == 2)
    );
    assert_eq!(*reads.borrow(), [2, 3]);
    let contiguous: Vec<_> = walk_allocation(10, 10, Some(3), |_| panic!("contiguous FAT read"))
        .map(|step| match step.unwrap() {
            FatChainStep::Cluster(position) => (position.cluster, position.valid),
            FatChainStep::Entry { .. } => panic!("contiguous FAT entry"),
        })
        .collect();
    assert_eq!(contiguous, [(10, true), (11, true), (12, false)]);
    assert!(
        walk_allocation(0, 10, Some(0), |_| panic!("empty FAT read"))
            .next()
            .is_none()
    );
    let mut overflow = walk_allocation(u32::MAX, u32::MAX, Some(2), |_| unreachable!());
    assert!(overflow.next().unwrap().is_ok());
    assert!(matches!(overflow.next().unwrap(), Err(Error::Bounds)));
    for value in [0, 1, 0xffff_fff7, u32::MAX] {
        let steps: Vec<_> =
            walk_allocation(2, 10, None, |_| Ok(decode_fat_entry(value, 10))).collect();
        assert_eq!(steps.len(), 2);
        assert!(matches!(&steps[1], Ok(FatChainStep::Entry { entry, .. }) if entry.value == value));
    }
    let mut failed = walk_allocation(2, 10, None, |_| Err(Error::Unsupported("injected FAT")));
    assert!(failed.next().unwrap().is_ok());
    assert!(matches!(
        failed.next().unwrap(),
        Err(Error::Unsupported("injected FAT"))
    ));
    assert!(failed.next().is_none());
}

#[test]
fn bitmap_claims_and_counts_ignore_only_unused_tail_bits() {
    let mut ownership = [0; 2];
    assert_eq!(claim_bitmap_cluster(&mut ownership, 2), Some(false));
    assert_eq!(claim_bitmap_cluster(&mut ownership, 2), Some(true));
    assert_eq!(claim_bitmap_cluster(&mut ownership, 10), Some(false));
    assert_eq!(ownership, [1, 1]);
    assert_eq!(claim_bitmap_cluster(&mut ownership, 1), None);
    assert_eq!(claim_bitmap_cluster(&mut ownership, 18), None);
    assert!(bitmap_contains_all(&[3, 1], &ownership));
    assert!(!bitmap_contains_all(&[0, 1], &ownership));
    assert!(!bitmap_contains_all(&[1], &ownership));
    for count in 0..=16 {
        assert_eq!(bitmap_count(&[255, 255], count), u64::from(count));
    }
    assert_eq!(bitmap_count(&[0b0101_0101, 255], 10), 6);
    assert_eq!(bitmap_count(&[], 10), 0);
}

#[test]
fn stream_cursors_preserve_chunk_reads_and_fragmented_cluster_order() {
    for contiguous in [false, true] {
        let length = 131072 + 32;
        let mut cursor = StreamCursor::new(2, length, contiguous);
        let reads = RefCell::new(Vec::new());
        let links = RefCell::new(Vec::new());
        let mut next = |cluster| {
            links.borrow_mut().push(cluster);
            Ok(decode_fat_entry(7, 10))
        };
        let mut read = |cluster, within, output: &mut [u8]| {
            reads.borrow_mut().push((cluster, within, output.len()));
            output.fill(cluster as u8);
            Ok(())
        };
        let mut output = vec![0; length as usize];
        cursor
            .read(131072, &mut next, &mut read, &mut output[..33])
            .unwrap();
        assert!(!cursor.finished());
        cursor
            .read(131072, &mut next, &mut read, &mut output[33..])
            .unwrap();
        assert!(cursor.finished());
        let last = if contiguous { 3 } else { 7 };
        assert_eq!(
            *reads.borrow(),
            [(2, 0, 65536), (2, 65536, 65536), (last, 0, 32)]
        );
        assert_eq!(links.borrow().len(), usize::from(!contiguous));
        assert!(output[..131072].iter().all(|byte| *byte == 2));
        assert_eq!(&output[131072..], &[last as u8; 32]);
        cursor.read(131072, &mut next, &mut read, &mut []).unwrap();
    }
}

#[test]
fn stream_cursors_report_short_streams_raw_links_and_read_errors() {
    let mut invalid = StreamCursor::new(2, 512, false);
    assert!(matches!(
        invalid.read(0, |_| unreachable!(), |_, _, _| unreachable!(), &mut [0]),
        Err(StreamReadError::Failure(Error::Bounds))
    ));
    let mut empty = StreamCursor::new(0, 0, false);
    assert!(empty.finished());
    assert!(matches!(
        empty.read(512, |_| unreachable!(), |_, _, _| unreachable!(), &mut [0]),
        Err(StreamReadError::End)
    ));
    for value in [0, 1, 0xffff_fff7, u32::MAX] {
        let mut cursor = StreamCursor::new(2, 1024, false);
        let error = cursor
            .read(
                512,
                |_| Ok(decode_fat_entry(value, 10)),
                |_, _, output| {
                    output.fill(42);
                    Ok(())
                },
                &mut [0; 513],
            )
            .unwrap_err();
        assert!(matches!(error, StreamReadError::Link(entry) if entry.value == value));
    }
    let mut cursor = StreamCursor::new(2, 512, true);
    assert!(matches!(
        cursor.read(
            512,
            |_| unreachable!(),
            |_, _, _| Err(Error::Unsupported("injected stream")),
            &mut [0]
        ),
        Err(StreamReadError::Failure(Error::Unsupported(
            "injected stream"
        )))
    ));
    let mut cursor = StreamCursor::new(2, 1024, false);
    assert!(matches!(
        cursor.read(512, |_| Err(Error::Bounds), |_, _, _| Ok(()), &mut [0; 513]),
        Err(StreamReadError::Failure(Error::Bounds))
    ));
    let mut cursor = StreamCursor::new(u32::MAX, 1024, true);
    assert!(matches!(
        cursor.read(512, |_| unreachable!(), |_, _, _| Ok(()), &mut [0; 513]),
        Err(StreamReadError::Failure(Error::Bounds))
    ));
}
