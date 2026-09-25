"""Publish accepted macOS app archives using the shared immutable publication path.

Signed for in-place updates like the Windows release (publish_native_windows.py)."""
import argparse
import hashlib
import io
import json
from pathlib import Path, PurePosixPath
import plistlib
import struct
import sys
import tarfile

from publish_native_windows import ROOT, check_version, publish
import update_signing


def validate_archive(data, arch):
    """Inspect without extraction. Only our single-architecture Tauri layout is accepted."""
    cpu = {'arm64': 0x0100000C, 'x64': 0x01000007}.get(arch)
    if cpu is None:
        raise ValueError('Unsupported macOS architecture')
    with tarfile.open(fileobj=io.BytesIO(data), mode='r:gz') as archive:
        members = {}
        total = 0
        for member in archive:
            path = PurePosixPath(member.name)
            if (path.is_absolute() or '..' in path.parts or '\\' in member.name
                    or not path.parts or path.parts[0] != 'Superkiro.app'
                    or not (member.isfile() or member.isdir())):
                raise ValueError('Unexpected archive path or entry type')
            # Current Tauri bundles have no links. Reject links/special files rather
            # than creating extraction semantics different from the user's system.
            name = str(path)
            if name in members or member.mode & 0o7000:
                raise ValueError('Duplicate archive member or unsafe permissions')
            members[name] = member
            total += member.size
            if len(members) > 10000 or total > 1024 * 1024 * 1024:
                raise ValueError('Archive exceeds publication limits')
        exe = members.get('Superkiro.app/Contents/MacOS/Superkiro')
        info = members.get('Superkiro.app/Contents/Info.plist')
        if not exe or not exe.isfile() or not exe.mode & 0o111 or not info or not info.isfile() or info.size > 1024 * 1024:
            raise ValueError('Missing executable app bundle metadata')
        header = archive.extractfile(exe).read(12)
        if len(header) != 12 or struct.unpack('<III', header)[:2] != (0xFEEDFACF, cpu):
            raise ValueError('Mach-O architecture does not match release target')
        metadata = plistlib.loads(archive.extractfile(info).read())
        if metadata.get('CFBundleIdentifier') != 'app.superkiro.desktop' or metadata.get('CFBundleExecutable') != 'Superkiro':
            raise ValueError('Unexpected app identity')


def carries_release(data, version):
    """Whether the app's executable is a release build of `version` (see
    publish_native_windows): its marker once, and nothing only a debug build carries."""
    with tarfile.open(fileobj=io.BytesIO(data), mode='r:gz') as archive:
        executable = archive.extractfile('Superkiro.app/Contents/MacOS/Superkiro').read()
    return (executable.count(update_signing.release_marker(version)) == 1
            and not update_signing.debug_build(executable))


# Until a real Mac has taken over Kiro and held a conversation, a Mac release says so.
MAC_BETA = '测试版 · 真机接管验收未完成'


def prepare(source, version, acceptance, arch, key, mandatory=True):
    check_version(version)
    data = Path(source).read_bytes()
    validate_archive(data, arch)
    if not carries_release(data, version):
        raise ValueError('The app was not built as this release '
                         '(set SUPERKIRO_RELEASE_VERSION to it when building)')
    digest = hashlib.sha256(data).hexdigest()
    expected = dict(version=version, sha256=digest, size=len(data), platform='macos', arch=arch)
    receipt = json.loads(Path(acceptance).read_text(encoding='utf-8-sig'))
    if (not isinstance(receipt, dict) or receipt.get('approvedForPublication') is not True
            or any(receipt.get(k) != v for k, v in expected.items())):
        raise ValueError('Acceptance receipt does not approve these exact artifact bytes')
    item = dict(expected, url=f"/downloads/Superkiro-{version}-Mac-{'ARM64' if arch == 'arm64' else 'Intel'}.app.tar.gz",
                signature='unsigned',
                systemRequirements='macOS · ' + ('Apple Silicon' if arch == 'arm64' else 'Intel x64')
                + ' · 无 Developer ID 签名或 Apple 公证 · 解压后运行 .app'
                # Said on every Mac release until a Mac has run a full takeover of Kiro.
                + ' · ' + MAC_BETA)
    return data, update_signing.signed_entry(item, key, mandatory)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--version', required=True)
    parser.add_argument('--source', type=Path, required=True)
    parser.add_argument('--acceptance', type=Path, required=True)
    parser.add_argument('--arch', choices=['arm64', 'x64'], required=True)
    parser.add_argument('--update-key', type=Path, default=update_signing.KEY)
    parser.add_argument('--optional', action='store_true',
                        help='let installed clients postpone this update')
    args = parser.parse_args()
    data, item = prepare(args.source, args.version, args.acceptance, args.arch,
                         update_signing.load(args.update_key), mandatory=not args.optional)
    sys.path.insert(0, str(ROOT))
    from deploy.release_candidate import pinned_connection
    ssh = pinned_connection(json.load(sys.stdin))
    try:
        publish(ssh, data, item)
    finally:
        ssh.close()


if __name__ == '__main__':
    main()
