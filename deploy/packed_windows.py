"""Validate an operator-approved packed derivative without altering CI provenance.

CI identity applies to the original bytes, not the packed binary. The receipt is
an explicit operator attestation of that transformation, not proof of equivalence.
"""
import ctypes
import hashlib
import json
import os
from pathlib import Path
import re
import struct
import tempfile


def sha256(data):
    return hashlib.sha256(data).hexdigest()


def check_pe(data):
    if len(data) < 64 or data[:2] != b'MZ':
        raise ValueError('Packed executable has no valid DOS header')
    offset = struct.unpack_from('<I', data, 0x3c)[0]
    if (offset + 26 > len(data) or data[offset:offset + 4] != b'PE\0\0'
            or struct.unpack_from('<H', data, offset + 4)[0] != 0x8664
            or struct.unpack_from('<H', data, offset + 24)[0] != 0x20b):
        raise ValueError('Packed executable must be a Windows x64 PE32+ binary')


def check_versions(data, version):
    # Read resources from an exact-byte temporary snapshot; never execute it or
    # re-open a mutable input after approving its digest.
    if os.name != 'nt':
        raise ValueError('Packed executable version validation requires Windows')
    from ctypes import wintypes as w
    api = ctypes.WinDLL('version', use_last_error=True)
    api.GetFileVersionInfoSizeW.argtypes = [w.LPCWSTR, ctypes.POINTER(w.DWORD)]
    api.GetFileVersionInfoSizeW.restype = w.DWORD
    api.GetFileVersionInfoW.argtypes = [w.LPCWSTR, w.DWORD, w.DWORD, w.LPVOID]
    api.GetFileVersionInfoW.restype = w.BOOL
    api.VerQueryValueW.argtypes = [w.LPCVOID, w.LPCWSTR, ctypes.POINTER(w.LPVOID), ctypes.POINTER(w.UINT)]
    api.VerQueryValueW.restype = w.BOOL
    with tempfile.TemporaryDirectory(prefix='superkiro-packed-verify-') as folder:
        path = Path(folder) / 'release.exe'
        path.write_bytes(data)
        handle = w.DWORD()
        size = api.GetFileVersionInfoSizeW(str(path), ctypes.byref(handle))
        if not size or size > 1024 * 1024:
            raise ValueError('Packed executable has no bounded version resource')
        buffer = ctypes.create_string_buffer(size)
        if not api.GetFileVersionInfoW(str(path), 0, size, buffer):
            raise ValueError('Cannot read packed executable version resource')
        pointer, length = w.LPVOID(), w.UINT()
        if not api.VerQueryValueW(buffer, '\\', ctypes.byref(pointer), ctypes.byref(length)) or length.value < 52:
            raise ValueError('Packed executable has no fixed version information')
        info = ctypes.cast(pointer, ctypes.POINTER(w.DWORD * 13)).contents
        expected = tuple(map(int, version.split('.'))) + (0,)
        versions = [tuple(part for value in info[start:start + 2]
                          for part in (value >> 16, value & 0xffff)) for start in (2, 4)]
        if info[0] != 0xFEEF04BD or any(v != expected for v in versions) or info[6] & info[7] & 1:
            raise ValueError('Packed file/product version mismatch or debug resource')


def verify_packed(data, version, receipt, original_path, provenance_path):
    if original_path is None or provenance_path is None or not isinstance(receipt, dict):
        raise ValueError('Packing requires original EXE, CI provenance and an explicit receipt')
    original = Path(original_path).read_bytes()
    provenance_bytes = Path(provenance_path).read_bytes()
    provenance = json.loads(provenance_bytes)
    packing = receipt.get('packing')
    expected = dict(originalSha256=sha256(original), originalSize=len(original),
                    provenanceSha256=sha256(provenance_bytes), packedSha256=sha256(data), packedSize=len(data))
    if (not isinstance(packing, dict) or packing.get('approvedForPackedPublication') is not True
            or any(packing.get(k) != v for k, v in expected.items())):
        raise ValueError('Packing approval must bind original, provenance and final exact bytes')
    if (not re.fullmatch(r'[0-9a-f]{40}', str(receipt.get('sourceCommit', '')))
            or not re.fullmatch(r'[0-9]+', str(receipt.get('buildRun', '')))):
        raise ValueError('Packing approval requires a source commit and CI run')
    required = dict(source_commit=receipt['sourceCommit'], sha256=expected['originalSha256'],
                    size=len(original), platform='windows', arch='x64', artifact=Path(original_path).name)
    if (not isinstance(provenance, dict) or provenance.get('source_dirty') is not False
            or any(provenance.get(k) != v for k, v in required.items())
            or not isinstance(provenance.get('build'), dict)
            or provenance['build'].get('GITHUB_RUN_ID') != str(receipt['buildRun'])
            or provenance['build'].get('GITHUB_REPOSITORY') != 'Duojiyi/Superkiro'):
        raise ValueError('Original artifact does not match the approved CI provenance')
    check_pe(data)
    check_versions(data, version)
    return original, packing
