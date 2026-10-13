"""Distinct complete RAW.gzip contract; tiny transport is not full-image admission."""
import argparse
import hashlib
import json
import math
import os
from pathlib import Path
import re
import stat
import time
import zlib

GIB = 1024**3
RAW_BYTES = 18*GIB
MAX_TRANSFER = 5*GIB
METHOD = 'raw-gzip-upload'
POLICY = 'full-paired-raw-gzip-20gi-v1'


def require(ok, message):
    if not ok:
        raise ValueError(message)


def integer(value):
    return type(value) is int


def regular(path):
    fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    info = os.fstat(fd)
    if not stat.S_ISREG(info.st_mode):
        os.close(fd)
        raise ValueError('regular immutable input required; FIFO/symlink refused')
    return os.fdopen(fd, 'rb'), info


def unchanged(stream, before):
    after = os.fstat(stream.fileno())
    require((before.st_dev, before.st_ino, before.st_size, before.st_mtime_ns, before.st_ctime_ns) ==
            (after.st_dev, after.st_ino, after.st_size, after.st_mtime_ns, after.st_ctime_ns),
            'input changed during streaming observation')


def check_gzip(path, raw_bytes, raw_sha256, *, timeout=900):
    """Hash exactly one gzip member; reject trailer-only sizing and concatenation.

    Invoke in the existing owned command supervisor for a process deadline:
    Python checks bound ordinary reads/CPU but cannot cancel blocked kernel I/O.
    """
    require(integer(raw_bytes) and 0 < raw_bytes <= RAW_BYTES, 'raw byte bound')
    require(re.fullmatch('[0-9a-f]{64}', raw_sha256 or ''), 'raw SHA required')
    deadline = time.monotonic()+timeout
    decoder = zlib.decompressobj(16+zlib.MAX_WBITS)
    raw_hash = hashlib.sha256(); transfer_hash = hashlib.sha256()
    total = 0
    with regular(path)[0] as source:
        before = os.fstat(source.fileno())
        require(0 < before.st_size <= MAX_TRANSFER, 'gzip transfer bound')
        while True:
            require(time.monotonic() < deadline, 'gzip verification deadline')
            chunk = source.read(1024*1024)
            if not chunk:
                break
            transfer_hash.update(chunk)
            pending = chunk
            while pending:
                require(time.monotonic() < deadline, 'gzip decoder deadline')
                data = decoder.decompress(pending, min(1024*1024, raw_bytes-total+1))
                total += len(data)
                require(total <= raw_bytes, 'gzip expands beyond original raw length')
                raw_hash.update(data)
                require(not decoder.unused_data, 'extra gzip member or trailing payload refused')
                pending = decoder.unconsumed_tail
        require(decoder.eof and total == raw_bytes and raw_hash.hexdigest() == raw_sha256,
                'gzip original full RAW length/hash mismatch or truncation')
        unchanged(source, before)
        require(time.monotonic() < deadline, 'gzip final verification deadline')
    return {'raw_bytes': total, 'raw_sha256': raw_hash.hexdigest(),
            'transfer_bytes': before.st_size, 'gzip_sha256': transfer_hash.hexdigest(),
            'single_member_verified': True}


def compress(raw, target, expected_sha, *, timeout=900):
    """CI-only operation on an existing owned complete RAW; never a reformat."""
    deadline = time.monotonic()+timeout
    output = Path(target)
    require(not output.exists(), 'gzip overwrite refused')
    digest = hashlib.sha256(); count = 0
    try:
        with regular(raw)[0] as source, output.open('xb') as dest:
            before = os.fstat(source.fileno())
            require(before.st_size == RAW_BYTES, 'complete original 18 GiB RAW required')
            require(os.statvfs(output.parent).f_bavail*os.statvfs(output.parent).f_frsize >= MAX_TRANSFER+GIB,
                    'CI gzip space reserve refused')
            encoder = zlib.compressobj(6, zlib.DEFLATED, 16+zlib.MAX_WBITS)
            while True:
                require(time.monotonic() < deadline, 'gzip compression deadline')
                chunk = source.read(1024*1024)
                if not chunk:
                    break
                count += len(chunk); digest.update(chunk); dest.write(encoder.compress(chunk))
                require(dest.tell() <= MAX_TRANSFER, 'gzip compressed transfer exceeds 5 GiB')
            dest.write(encoder.flush())
            require(dest.tell() <= MAX_TRANSFER, 'gzip compressed transfer exceeds 5 GiB')
            require(count == RAW_BYTES and digest.hexdigest() == expected_sha,
                    'original RAW changed before compression')
            unchanged(source, before)
        remaining = deadline-time.monotonic()
        require(remaining > 0, 'gzip post-compression deadline')
        result = check_gzip(output, RAW_BYTES, expected_sha, timeout=remaining)
        result.update(method=METHOD, policy=POLICY, requested_pvc='20Gi',
                      virtual_bytes=RAW_BYTES, performance_qualified=False)
        return result
    except BaseException:
        output.unlink(missing_ok=True)
        raise


def storage(receipt):
    require(receipt.get('single_member_verified') is True and receipt.get('method') == METHOD
            and receipt.get('policy') == POLICY and receipt.get('raw_bytes') == RAW_BYTES,
            'distinct verified complete RAW.gzip required')
    require(all(re.fullmatch('[0-9a-f]{64}', receipt.get(k, '')) for k in ('raw_sha256', 'gzip_sha256')),
            'complete raw/gzip SHA identities required')
    require(integer(receipt.get('transfer_bytes')) and 0 < receipt['transfer_bytes'] <= MAX_TRANSFER,
            'gzip transfer bound')
    return {'policy': POLICY, 'virtual_bytes': RAW_BYTES, 'transfer_bytes': receipt['transfer_bytes'],
            'requested_pvc': '20Gi', 'requested_pvc_bytes': 20*GIB,
            'guest_write_allowance_bytes': GIB//2, 'node_peak_qualified': False}


def usable_target(available, overhead):
    """CDI v1.66.1 ceil(available/(1+overhead)), rounded down to 1 MiB."""
    require(integer(available) and available > 0, 'actual target available bytes required')
    require(type(overhead) in (float, int) and math.isfinite(overhead) and 0 <= overhead < 1,
            'actual CDI filesystem overhead required')
    usable = (math.ceil(available/(1+overhead))//(1024**2))*(1024**2)
    require(usable >= RAW_BYTES+GIB//2, 'actual target cannot fit full RAW plus guest margin')
    return usable


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('operation', choices=['compress', 'verify'])
    parser.add_argument('--input', type=Path, required=True)
    parser.add_argument('--output', type=Path)
    parser.add_argument('--sha256', required=True)
    args = parser.parse_args()
    if args.operation == 'compress':
        require(args.output is not None, 'output required')
        result = compress(args.input, args.output, args.sha256)
    else:
        result = check_gzip(args.input, RAW_BYTES, args.sha256)
    print(json.dumps(result, sort_keys=True))
