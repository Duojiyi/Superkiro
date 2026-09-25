"""Sign desktop client updates with the offline release key.

Installed clients install an update only when its release entry carries a signature by a
key compiled into them (`UPDATE_KEYS` in crates/desktop-host/src/update.rs). The private
key stays on the release operator's machine, never on the download server, so whoever
controls that server still cannot make clients run anything we did not sign.

  python deploy/update_signing.py keygen   create the key (never overwrites one)
  python deploy/update_signing.py public   print the public key to compile into the client

The key file is git-ignored. Back it up offline: without it no installed client accepts
another update, and they would all have to be replaced by hand.
"""
import argparse
from pathlib import Path
import re

from cryptography.exceptions import InvalidSignature
from cryptography.hazmat.primitives import serialization
from cryptography.hazmat.primitives.asymmetric.ed25519 import (Ed25519PrivateKey,
                                                               Ed25519PublicKey)

ROOT = Path(__file__).resolve().parents[1]
KEY = ROOT / '.acceptance' / 'update-signing-key.pem'
CLIENT_KEYS = ROOT / 'crates' / 'desktop-host' / 'src' / 'update.rs'
TARGETS = {('windows', 'x64'), ('macos', 'arm64'), ('macos', 'x64')}
# Numeric, dotted, one to four parts: what clients can compare. New releases are stricter,
# see RELEASE_VERSION.
VERSION = re.compile(r'[0-9]{1,9}(\.[0-9]{1,9}){0,3}')
# What a release is published as: MAJOR.MINOR.PATCH without leading zeros ("0.1.1").
RELEASE_VERSION = re.compile(r'(0|[1-9][0-9]{0,8})\.(0|[1-9][0-9]{0,8})\.(0|[1-9][0-9]{0,8})')


def message(item):
    """The bytes a client verifies: exactly these fields, in this order."""
    if ((item.get('platform'), item.get('arch')) not in TARGETS
            or not isinstance(item.get('version'), str) or not VERSION.fullmatch(item['version'])
            or not isinstance(item.get('sha256'), str)
            or not re.fullmatch(r'[0-9a-f]{64}', item['sha256'])
            or type(item.get('size')) is not int or not 0 < item['size'] <= 512 * 1024 * 1024):
        raise ValueError('Cannot sign an invalid release entry')
    return '\n'.join([
        'superkiro-update/1',
        'platform=' + item['platform'],
        'arch=' + item['arch'],
        'version=' + item['version'],
        'sha256=' + item['sha256'],
        'size=' + str(item['size']),
    ]).encode()


def load(path=KEY):
    key = serialization.load_pem_private_key(Path(path).read_bytes(), password=None)
    if not isinstance(key, Ed25519PrivateKey):
        raise ValueError('The update signing key must be an Ed25519 key')
    return key


def public_hex(key):
    return key.public_key().public_bytes(serialization.Encoding.Raw,
                                         serialization.PublicFormat.Raw).hex()


def sign(item, key):
    return key.sign(message(item)).hex()


def client_keys(source=CLIENT_KEYS):
    """The public keys compiled into the client, as hex."""
    text = Path(source).read_text(encoding='utf-8')
    block = re.search(r'const UPDATE_KEYS: &\[&str\] = &\[(.*?)\];', text, re.S)
    return re.findall(r'"([0-9a-f]{64})"', block.group(1)) if block else []


def verify(item, signature, keys):
    """Whether one of `keys` (hex) signed `item`, as a client checks it."""
    try:
        raw = bytes.fromhex(signature)
    except (TypeError, ValueError):
        return False
    for key in keys:
        try:
            Ed25519PublicKey.from_public_bytes(bytes.fromhex(key)).verify(raw, message(item))
            return True
        except InvalidSignature:
            continue
    return False


def signed_entry(item, key, mandatory):
    """`item` with its update signature, checked against the keys clients trust."""
    signature = sign(item, key)
    if not verify(item, signature, client_keys()):
        raise ValueError('The signing key is not one the client trusts (UPDATE_KEYS)')
    return dict(item, updateSignature=signature, mandatory=bool(mandatory))


def release_marker(version):
    """The bytes a client built for `version` carries (build.rs, update.rs)."""
    return f'superkiro-release:{version};'.encode()


# Compiled only into debug builds (update.rs): the test key they trust and the variable that
# points them at another update server. Neither may ever reach a customer.
DEBUG_ONLY = (b'197f6b23e16c8532c6abc838facd5ea789be0c76b2920334039bfa8b3d368d61',
              b'SUPERKIRO_UPDATE_URL')


def debug_build(data):
    """Whether `data` (an executable) is a debug build, which must never be published."""
    return any(marker in data for marker in DEBUG_ONLY)


def keygen(path=KEY):
    path = Path(path)
    path.parent.mkdir(parents=True, exist_ok=True)
    key = Ed25519PrivateKey.generate()
    pem = key.private_bytes(serialization.Encoding.PEM, serialization.PrivateFormat.PKCS8,
                            serialization.NoEncryption())
    # Exclusive creation: a replaced key strands every installed client.
    with open(path, 'xb') as out:
        out.write(pem)
    return key


def main():
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument('command', choices=['keygen', 'public'])
    parser.add_argument('--key', type=Path, default=KEY)
    args = parser.parse_args()
    key = keygen(args.key) if args.command == 'keygen' else load(args.key)
    print(public_hex(key))


if __name__ == '__main__':
    main()
