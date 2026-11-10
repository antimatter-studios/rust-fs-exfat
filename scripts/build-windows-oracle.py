from pathlib import Path
import struct
import sys


def checksum(data, bits=32, excluded=()):
    total = 0
    for index, byte in enumerate(data):
        if index not in excluded:
            total = (((total >> 1) | (total << (bits - 1))) + byte) & ((1 << bits) - 1)
    return total


def build(destination, reference, sector):
    donor = reference.read_bytes()
    donor_cluster = sector << donor[109]
    donor_heap = struct.unpack_from('<I', donor, 88)[0] * sector
    donor_root = struct.unpack_from('<I', donor, 96)[0]
    root_start = donor_heap + (donor_root - 2) * donor_cluster
    entries = donor[root_start:root_start + donor_cluster]
    upcase_entry = next(entries[index:index + 32] for index in range(0, len(entries), 32)
                        if entries[index] == 0x82)
    upcase_start, upcase_length = struct.unpack_from('<IQ', upcase_entry, 20)
    donor_fat = struct.unpack_from('<I', donor, 80)[0] * sector
    upcase = bytearray()
    seen = set()
    while upcase_start < 0xfffffff8:
        assert upcase_start >= 2 and upcase_start not in seen
        seen.add(upcase_start)
        position = donor_heap + (upcase_start - 2) * donor_cluster
        upcase.extend(donor[position:position + donor_cluster])
        upcase_start = struct.unpack_from('<I', donor, donor_fat + upcase_start * 4)[0]
    upcase = upcase[:upcase_length]
    assert len(upcase) == 5836 and checksum(upcase) == 0xe619d30d
    mappings = []
    words = iter(struct.unpack('<' + 'H' * (len(upcase) // 2), upcase))
    for value in words:
        if value == 0xffff and len(mappings) < 65535:
            mappings.extend(range(len(mappings), len(mappings) + next(words)))
        else:
            mappings.append(value)
    assert len(mappings) == 65536

    size, cluster, fat, heap = 64 * 1024 * 1024, 32768, 128 * 1024, 1024 * 1024
    count = (size - heap) // cluster
    bitmap = bytearray((count + 7) // 8)
    fat_bytes = bytearray(128 * 1024)
    struct.pack_into('<II', fat_bytes, 0, 0xfffffff8, 0xffffffff)
    writes = []

    def stream(clusters, contents, contiguous=False):
        assert len(contents) <= len(clusters) * cluster
        for index, number in enumerate(clusters):
            bitmap[(number - 2) // 8] |= 1 << ((number - 2) % 8)
            if not contiguous:
                following = clusters[index + 1] if index + 1 < len(clusters) else 0xffffffff
                struct.pack_into('<I', fat_bytes, number * 4, following)
            writes.append((heap + (number - 2) * cluster, contents[index * cluster:(index + 1) * cluster]))

    def file_entry(name, contents, clusters, contiguous):
        units = list(struct.unpack('<' + 'H' * (len(name.encode('utf-16le')) // 2), name.encode('utf-16le')))
        slots = (len(units) + 14) // 15
        entry = bytearray((2 + slots) * 32)
        entry[0:2] = bytes([0x85, 1 + slots])
        struct.pack_into('<H', entry, 4, 0x20)
        for offset in (8, 12, 16):
            struct.pack_into('<I', entry, offset, (44 << 25) | (1 << 21) | (2 << 16))
        entry[22:25] = bytes([0x80] * 3)
        entry[32:34] = bytes([0xc0, 3 if contiguous else 1])
        entry[35] = len(units)
        name_bytes = b''.join(struct.pack('<H', mappings[unit]) for unit in units)
        struct.pack_into('<H', entry, 36, checksum(name_bytes, 16))
        struct.pack_into('<Q', entry, 40, len(contents))
        struct.pack_into('<IQ', entry, 52, clusters[0] if clusters else 0, len(contents))
        for index in range(slots):
            entry[(2 + index) * 32] = 0xc1
            encoded = b''.join(struct.pack('<H', unit) for unit in units[index * 15:(index + 1) * 15])
            entry[(2 + index) * 32 + 2:(2 + index) * 32 + 2 + len(encoded)] = encoded
        struct.pack_into('<H', entry, 2, checksum(entry, 16, (2, 3)))
        stream(clusters, contents, contiguous)
        return entry

    root = bytearray(96)
    root[0:2] = bytes([0x83, 9])
    root[2:20] = 'WINORACLE'.encode('utf-16le')
    root[32] = 0x81
    struct.pack_into('<IQ', root, 52, 2, len(bitmap))
    root[64] = 0x82
    struct.pack_into('<I', root, 68, checksum(upcase))
    struct.pack_into('<IQ', root, 84, 5, len(upcase))
    for name, contents, clusters, contiguous in [
        ('MixedCase.txt', b'Windows-layout spec-built oracle\n', [12], True),
        ('long Unicode grüße-😀.txt', bytes(range(256)) * 300, [14, 18, 16], False),
        ('contiguous.bin', bytes(range(256)) * 400, [20, 21, 22, 23], True),
        ('empty.txt', b'', [], False),
    ]:
        root.extend(file_entry(name, contents, clusters, contiguous))
    stream([5], upcase)
    stream([9], root)
    stream([2], bitmap)

    boot = bytearray(12 * sector)
    boot[:11] = b'\xeb\x76\x90EXFAT   '
    struct.pack_into('<QQIIIIII', boot, 64, 0, size // sector, fat // sector,
                     len(fat_bytes) // sector, heap // sector, count, 9, 0x57494e31)
    struct.pack_into('<HH', boot, 104, 0x100, 0)
    boot[108:113] = bytes([sector.bit_length() - 1, (cluster // sector).bit_length() - 1,
                           1, 0x80, sum(bin(byte).count('1') for byte in bitmap) * 100 // count])
    boot[510:512] = b'\x55\xaa'
    for index in range(1, 9):
        struct.pack_into('<I', boot, (index + 1) * sector - 4, 0xaa550000)
    value = checksum(boot[:11 * sector], excluded=(106, 107, 112))
    boot[11 * sector:] = struct.pack('<I', value) * (sector // 4)
    with destination.open('xb') as output:
        output.truncate(size)
        for offset, contents in [(0, boot), (12 * sector, boot), (fat, fat_bytes)] + writes:
            output.seek(offset)
            output.write(contents)
    print(f'windows-layout: spec-built {sector}-byte sectors, {cluster}-byte clusters, checksum {value:08x}')


if __name__ == '__main__':
    build(Path(sys.argv[1]), Path(sys.argv[2]), int(sys.argv[3]))
