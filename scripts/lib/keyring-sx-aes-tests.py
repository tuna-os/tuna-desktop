#!/usr/bin/python3
"""Tests for keyring_sx_aes.py, and the generator of the Rust module's
known-answer vectors (crates/shell-gtk/src/keyring_exchange_vectors.json).

    keyring-sx-aes-tests.py                  run the tests
    keyring-sx-aes-tests.py --write-vectors  regenerate the vectors file

The vectors are deterministic (fixed private keys and IV) except
"gcr_send", a string real libgcr wrote for the first message; it is kept
as recorded, and the tests check it decrypts. When libgcr is installed
(libgcr-4 or libgcr-base-3), the tests also run the reference against the
real GcrSecretExchange in both directions; without it they skip.
"""
import ctypes
import hashlib
import json
import os
import sys
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import keyring_sx_aes as sx  # noqa: E402

VECTORS = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "..", "crates",
                       "shell-gtk", "src", "keyring_exchange_vectors.json")
PRIVATE_A = bytes(range(0x01, 0x21))
PRIVATE_B = bytes(range(0xA0, 0xC0))
IV = bytes(range(0x10, 0x20))
GCR_SECRET = "gcr says hi"
SECRETS = ["correct horse battery staple", "p\u00e4ssw\u00f6rd \U0001f511", "", None]


def load_gcr():
    for name in ("libgcr-4.so.4", "libgcr-base-3.so.1"):
        try:
            lib = ctypes.CDLL(name)
        except OSError:
            continue
        lib.gcr_secret_exchange_new.restype = ctypes.c_void_p
        lib.gcr_secret_exchange_new.argtypes = [ctypes.c_char_p]
        lib.gcr_secret_exchange_begin.restype = ctypes.c_void_p
        lib.gcr_secret_exchange_begin.argtypes = [ctypes.c_void_p]
        lib.gcr_secret_exchange_receive.restype = ctypes.c_int
        lib.gcr_secret_exchange_receive.argtypes = [ctypes.c_void_p, ctypes.c_char_p]
        lib.gcr_secret_exchange_send.restype = ctypes.c_void_p
        lib.gcr_secret_exchange_send.argtypes = [ctypes.c_void_p, ctypes.c_char_p, ctypes.c_ssize_t]
        lib.gcr_secret_exchange_get_secret.restype = ctypes.c_char_p
        lib.gcr_secret_exchange_get_secret.argtypes = [ctypes.c_void_p,
                                                       ctypes.POINTER(ctypes.c_size_t)]
        return lib
    return None


GCR = load_gcr()


def gcr_text(pointer):
    return ctypes.string_at(pointer).decode()


def build_vectors(gcr_send):
    a = sx.Exchange(int.from_bytes(PRIVATE_A, "big"))
    b = sx.Exchange(int.from_bytes(PRIVATE_B, "big"))
    a_begin, b_begin = a.begin(), b.begin()
    b.receive(a_begin)
    a.receive(b_begin)
    assert a.key == b.key
    shared = pow(b.public, int.from_bytes(PRIVATE_A, "big"), sx.PRIME).to_bytes(sx.PRIME_BYTES, "big")
    return {
        "private_a": PRIVATE_A.hex(),
        "private_b": PRIVATE_B.hex(),
        "a_begin": a_begin,
        "b_begin": b_begin,
        "shared_sha256": hashlib.sha256(shared).hexdigest(),
        "key": a.key.hex(),
        "iv": IV.hex(),
        "b_send": [
            {"secret": s, "exchange": b.send(None if s is None else s.encode(), IV)}
            for s in SECRETS
        ],
        "gcr_send": {"secret": GCR_SECRET, "exchange": gcr_send},
    }


def record_gcr_send():
    """What real libgcr sends in answer to a's first message."""
    a = sx.Exchange(int.from_bytes(PRIVATE_A, "big"))
    gcr = GCR.gcr_secret_exchange_new(None)
    assert GCR.gcr_secret_exchange_receive(gcr, a.begin().encode())
    return gcr_text(GCR.gcr_secret_exchange_send(gcr, GCR_SECRET.encode(), -1))


class Reference(unittest.TestCase):
    def test_aes_fips_197_c1(self):
        key = bytes(range(16))
        plain = bytes.fromhex("00112233445566778899aabbccddeeff")
        cipher = sx.aes128_encrypt_block(key, plain)
        self.assertEqual(cipher.hex(), "69c4e0d86a7b0430d8cdb78070b4c55a")
        self.assertEqual(sx.aes128_decrypt_block(key, cipher), plain)

    def test_cbc_sp_800_38a_f_2_1(self):
        key = bytes.fromhex("2b7e151628aed2a6abf7158809cf4f3c")
        iv = bytes.fromhex("000102030405060708090a0b0c0d0e0f")
        plain = bytes.fromhex("6bc1bee22e409f96e93d7e117393172aae2d8a571e03ac9c9eb76fac45af8e51"
                              "30c81c46a35ce411e5fbc1191a0a52eff69f2445df4f9b17ad2b417be66c3710")
        want = ("7649abac8119b246cee98e9b12e9197d5086cb9b507219ee95db113a917678b2"
                "73bed6b8e3c1743b7116e69e222295163ff1caa1681fac09120eca307586e1a7")
        self.assertEqual(sx.cbc_encrypt(key, iv, plain)[:64].hex(), want)
        self.assertEqual(sx.cbc_decrypt(key, iv, sx.cbc_encrypt(key, iv, plain)), plain)

    def test_hkdf_rfc_5869_case_3(self):
        okm = sx.hkdf_sha256(bytes([0x0B]) * 22, 42, salt=b"")
        self.assertEqual(okm.hex(), "8da4e775a563c18f715f802a063c5a31b8a11f5c5ee1879ec3454e5f3c738d2d"
                                    "9d201395faa4b61a96c8")
        # An empty salt is the default salt of hash-length zeros (RFC 5869 §2.2).
        self.assertEqual(sx.hkdf_sha256(bytes([0x0B]) * 22, 42), okm)

    def test_the_prime(self):
        self.assertEqual(sx.PRIME.bit_length(), 1536)
        self.assertEqual(pow(2, sx.PRIME - 1, sx.PRIME), 1)
        p = (1 << 1536) - (1 << 1472) - 1 + (1 << 64) * (_pi_term() + 741804)
        self.assertEqual(sx.PRIME, p)

    def test_padding(self):
        for n in range(0, 40):
            self.assertEqual(sx.pkcs7_unpad(sx.pkcs7_pad(bytes(n))), bytes(n))
        for bad in (b"", b"\x00" * 16, b"\x11" * 16, b"a" * 15 + b"\x02"):
            with self.assertRaises(ValueError):
                sx.pkcs7_unpad(bad)

    def test_round_trip(self):
        a, b = sx.Exchange(), sx.Exchange()
        self.assertIsNone(b.receive(a.begin()))
        self.assertEqual(a.receive(b.send(b"hunter2")), b"hunter2")

    def test_vectors_file_is_what_the_generator_writes(self):
        with open(VECTORS, encoding="utf-8") as fh:
            recorded = json.load(fh)
        self.assertEqual(recorded, build_vectors(recorded["gcr_send"]["exchange"]))

    def test_recorded_gcr_string_decrypts(self):
        with open(VECTORS, encoding="utf-8") as fh:
            recorded = json.load(fh)
        a = sx.Exchange(int.from_bytes(PRIVATE_A, "big"))
        got = a.receive(recorded["gcr_send"]["exchange"])
        self.assertEqual(got, GCR_SECRET.encode())


@unittest.skipIf(GCR is None, "libgcr is not installed")
class AgainstLibgcr(unittest.TestCase):
    def test_gcr_begins_we_answer_with_a_secret(self):
        gcr = GCR.gcr_secret_exchange_new(None)
        ours = sx.Exchange()
        ours.receive(gcr_text(GCR.gcr_secret_exchange_begin(gcr)))
        self.assertTrue(GCR.gcr_secret_exchange_receive(gcr, ours.send("p\u00e4ss".encode()).encode()))
        self.assertEqual(GCR.gcr_secret_exchange_get_secret(gcr, None), "p\u00e4ss".encode())

    def test_we_begin_gcr_answers_with_a_secret(self):
        gcr = GCR.gcr_secret_exchange_new(None)
        ours = sx.Exchange()
        self.assertTrue(GCR.gcr_secret_exchange_receive(gcr, ours.begin().encode()))
        self.assertEqual(ours.receive(gcr_text(GCR.gcr_secret_exchange_send(gcr, b"from gcr", -1))),
                         b"from gcr")

    def test_gcr_as_the_prompter_side(self):
        # The prompter begins; the client answers with its key and no secret;
        # the prompter then sends the password. All three steps as gcr writes them.
        prompter = GCR.gcr_secret_exchange_new(None)
        client = sx.Exchange()
        first = gcr_text(GCR.gcr_secret_exchange_begin(prompter))
        self.assertIsNone(client.receive(first))
        self.assertTrue(GCR.gcr_secret_exchange_receive(prompter, client.send(None).encode()))
        answer = gcr_text(GCR.gcr_secret_exchange_send(prompter, b"s3cret", -1))
        self.assertEqual(client.receive(answer), b"s3cret")


def _pi_term():
    """floor(2**1406 * pi), from Machin's formula in integers."""
    bits = 1406 + 64
    one = 1 << bits

    def arctan_inv(x):
        total = term = one // x
        n, sign = 3, -1
        while term:
            term //= x * x
            total += sign * (term // n)
            n += 2
            sign = -sign
        return total

    pi = 4 * (4 * arctan_inv(5) - arctan_inv(239))
    return pi >> 64


if __name__ == "__main__":
    if "--write-vectors" in sys.argv:
        if GCR is None:
            sys.exit("libgcr is needed to record a string it wrote")
        with open(VECTORS, "w", encoding="utf-8") as fh:
            json.dump(build_vectors(record_gcr_send()), fh, indent=1, ensure_ascii=False)
            fh.write("\n")
        print("wrote", os.path.normpath(VECTORS))
    else:
        unittest.main()
