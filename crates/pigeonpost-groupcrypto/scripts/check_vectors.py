#!/usr/bin/env python3
"""Independent check of tests/vectors/groupcrypto-v1.txt with nothing but the Python standard
library: mailbox addresses, device fingerprints and digits, safety numbers, the attachment hash
and the envelope signing input are recomputed from the formulas in docs/group-e2ee.md.
(Ed25519 signatures and XChaCha20-Poly1305 need third-party libraries and are checked by the Rust
test tests/vectors.rs.)

    python3 -I crates/pigeonpost-groupcrypto/scripts/check_vectors.py
"""
import hashlib
import pathlib
import sys

FILE = pathlib.Path(__file__).resolve().parent.parent / "tests" / "vectors" / "groupcrypto-v1.txt"
B32 = "0123456789abcdefghjkmnpqrstvwxyz"


def address(public_key: bytes) -> str:
    n = int.from_bytes(hashlib.sha256(public_key).digest()[:16], "big") << 2
    return "/k/" + "".join(B32[(n >> (5 * (25 - i))) & 31] for i in range(26))


def digits(data: bytes, groups: int) -> str:
    return " ".join("%05d" % (int.from_bytes(data[i * 5:i * 5 + 5], "big") % 100000) for i in range(groups))


def fingerprint(mailbox: str, device: str, key: bytes) -> bytes:
    h = hashlib.sha256(b"pigeonpost/groupcrypto/device-fingerprint/v1\0")
    for part in (mailbox.encode(), device.encode(), key):
        h.update(len(part).to_bytes(2, "big"))
        h.update(part)
    return h.digest()


def safety_number(fps) -> str:
    s = sorted(set(fps))
    h = hashlib.sha512(b"pigeonpost/groupcrypto/safety-number/v1\0" + len(s).to_bytes(4, "big") + b"".join(s))
    return digits(h.digest()[:60], 12)


def main() -> int:
    lines = FILE.read_text().splitlines()
    v = dict(l.split(" = ", 1) for l in lines if l and not l.startswith("#"))
    names = ("alice", "bob", "carol")
    for p in names:
        assert address(bytes.fromhex(v[f"address.{p}.public_key"])) == v[f"address.{p}"], p
        fp = bytes.fromhex(v[f"fingerprint.{p}"])
        assert digits(fp, 6) == v[f"fingerprint.{p}.digits"], p
    assert fingerprint(v["address.alice"], v["input.alice_device_id"],
                       bytes.fromhex(v["attestation.alice.device_public_key"])) == bytes.fromhex(v["fingerprint.alice"])
    fps = [bytes.fromhex(v[f"fingerprint.{p}"]) for p in names]
    assert safety_number(fps[:2]) == v["safety_number.alice_bob"]
    assert safety_number(fps) == v["safety_number.alice_bob_carol"]
    assert safety_number([]) == v["safety_number.empty"]
    assert hashlib.sha256(bytes.fromhex(v["attachment.sealed"])).hexdigest() == v["attachment.sealed_sha256"]
    assert bytes.fromhex(v["envelope.signing_input"]) == (
        b"pigeonpost/groupcrypto/envelope/v1\0" + hashlib.sha256(v["envelope.message_ascii"].encode()).digest())
    print("groupcrypto vectors: independent check OK")
    return 0


if __name__ == "__main__":
    sys.exit(main())
