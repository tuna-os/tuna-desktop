"""gcr's "sx-aes-1" secret exchange (gcr-secret-exchange.c), in pure Python.

An independent reimplementation for the proofs, written from gcr's source
(gcr/gcr-secret-exchange.c, egg/egg-dh.c, egg/egg-hkdf-libgcrypt.c,
egg/egg-padding.c): the tests and the keyring prompter fixture use it as
the other end of the shell's Rust implementation, and generate its
known-answer vectors. It is slow and not constant time; it is test code.

The exchange is a GKeyFile with one group:

    [sx-aes-1]
    public=<base64 big-endian DH public key, no leading zeros>
    secret=<base64 AES-128-CBC/PKCS7 ciphertext>     (optional)
    iv=<base64 16 bytes>                             (with secret)

Group ietf-ike-grp-modp-1536 (RFC 3526 group 5, generator 2), a private key
below 2**1535, the shared secret the 192-byte big-endian g**xy mod p,
HKDF-SHA256 with the default salt and empty info down to a 16-byte key.
"""
import base64
import hashlib
import hmac
import os

PROTOCOL = "sx-aes-1"

# RFC 3526, section 2: 2**1536 - 2**1472 - 1 + 2**64 * (floor(2**1406 * pi) + 741804)
PRIME = int(
    "FFFFFFFFFFFFFFFFC90FDAA22168C234C4C6628B80DC1CD1"
    "29024E088A67CC74020BBEA63B139B22514A08798E3404DD"
    "EF9519B3CD3A431B302B0A6DF25F14374FE1356D6D51C245"
    "E485B576625E7EC6F44C42E9A637ED6B0BFF5CB6F406B7ED"
    "EE386BFB5A899FA5AE9F24117C4B1FE649286651ECE45B3D"
    "C2007CB8A163BF0598DA48361C55D39A69163FA8FD24CF5F"
    "83655D23DCA3AD961C62F356208552BB9ED529077096966D"
    "670C354E4ABC9804F1746C08CA237327FFFFFFFFFFFFFFFF", 16)
BASE = 2
PRIME_BYTES = 192
KEY_BYTES = 16


# ---- AES-128 (FIPS 197) ----------------------------------------------------

def _build_sbox():
    sbox = [0] * 256
    p = q = 1
    while True:
        # multiply p by 3, divide q by 3 in GF(2**8)
        p = p ^ ((p << 1) & 0xFF) ^ (0x1B if p & 0x80 else 0)
        q ^= q << 1
        q ^= q << 2
        q ^= q << 4
        q &= 0xFF
        if q & 0x80:
            q ^= 0x09
        x = q ^ (q << 1 | q >> 7) & 0xFF ^ (q << 2 | q >> 6) & 0xFF \
            ^ (q << 3 | q >> 5) & 0xFF ^ (q << 4 | q >> 4) & 0xFF
        sbox[p] = (x ^ 0x63) & 0xFF
        if p == 1:
            break
    sbox[0] = 0x63
    return sbox


SBOX = _build_sbox()
INV_SBOX = [0] * 256
for _i, _v in enumerate(SBOX):
    INV_SBOX[_v] = _i


def _xtime(a):
    return ((a << 1) ^ 0x1B) & 0xFF if a & 0x80 else a << 1


def _mul(a, b):
    out = 0
    while b:
        if b & 1:
            out ^= a
        a = _xtime(a)
        b >>= 1
    return out


def _expand_key(key):
    assert len(key) == 16
    words = [list(key[i:i + 4]) for i in range(0, 16, 4)]
    rcon = 1
    for i in range(4, 44):
        t = list(words[i - 1])
        if i % 4 == 0:
            t = t[1:] + t[:1]
            t = [SBOX[b] for b in t]
            t[0] ^= rcon
            rcon = _xtime(rcon)
        words.append([a ^ b for a, b in zip(words[i - 4], t)])
    return [sum(words[4 * r:4 * r + 4], []) for r in range(11)]


def _add(state, rk):
    return [a ^ b for a, b in zip(state, rk)]


def _shift_rows(s):
    return [s[(c + r) % 4 * 4 + r] for c in range(4) for r in range(4)]


def _inv_shift_rows(s):
    return [s[(c - r) % 4 * 4 + r] for c in range(4) for r in range(4)]


def _mix(s, m):
    out = []
    for c in range(4):
        col = s[4 * c:4 * c + 4]
        for r in range(4):
            out.append(
                _mul(col[0], m[(0 - r) % 4]) ^ _mul(col[1], m[(1 - r) % 4])
                ^ _mul(col[2], m[(2 - r) % 4]) ^ _mul(col[3], m[(3 - r) % 4]))
    return out


def aes128_encrypt_block(key, block):
    rks = _expand_key(key)
    s = _add(list(block), rks[0])
    for r in range(1, 10):
        s = _mix(_shift_rows([SBOX[b] for b in s]), (2, 3, 1, 1))
        s = _add(s, rks[r])
    return bytes(_add(_shift_rows([SBOX[b] for b in s]), rks[10]))


def aes128_decrypt_block(key, block):
    rks = _expand_key(key)
    s = _add(list(block), rks[10])
    for r in range(9, 0, -1):
        s = [INV_SBOX[b] for b in _inv_shift_rows(s)]
        s = _mix(_add(s, rks[r]), (14, 11, 13, 9))
    return bytes(_add([INV_SBOX[b] for b in _inv_shift_rows(s)], rks[0]))


def pkcs7_pad(data, block=16):
    n = block - len(data) % block
    return data + bytes([n]) * n


def pkcs7_unpad(data, block=16):
    n = data[-1] if data else 0
    if n == 0 or n > block or n > len(data) or data[-n:] != bytes([n]) * n:
        raise ValueError("bad padding")
    return data[:-n]


def cbc_encrypt(key, iv, plain):
    out, prev = b"", iv
    padded = pkcs7_pad(plain)
    for i in range(0, len(padded), 16):
        prev = aes128_encrypt_block(key, bytes(a ^ b for a, b in zip(padded[i:i + 16], prev)))
        out += prev
    return out


def cbc_decrypt(key, iv, cipher):
    if len(cipher) % 16:
        raise ValueError("cipher text is not a whole number of blocks")
    out, prev = b"", iv
    for i in range(0, len(cipher), 16):
        block = cipher[i:i + 16]
        out += bytes(a ^ b for a, b in zip(aes128_decrypt_block(key, block), prev))
        prev = block
    return pkcs7_unpad(out)


# ---- HKDF-SHA256 (RFC 5869) --------------------------------------------------

def hkdf_sha256(ikm, length, salt=None, info=b""):
    salt = salt if salt is not None else b"\0" * 32
    prk = hmac.new(salt, ikm, hashlib.sha256).digest()
    out, t, i = b"", b"", 1
    while len(out) < length:
        t = hmac.new(prk, t + info + bytes([i]), hashlib.sha256).digest()
        out += t
        i += 1
    return out[:length]


# ---- the exchange -------------------------------------------------------------

def _int_bytes(n):
    return n.to_bytes(max(1, (n.bit_length() + 7) // 8), "big")


def _write(fields):
    return "[%s]\n" % PROTOCOL + "".join("%s=%s\n" % (k, v) for k, v in fields)


def parse(text):
    """GKeyFile's view of an exchange: {key: value} of the sx-aes-1 group."""
    group, fields = None, {}
    for line in text.splitlines():
        line = line.strip()
        if not line or line.startswith("#"):
            continue
        if line.startswith("[") and line.endswith("]"):
            group = line[1:-1]
        elif group == PROTOCOL and "=" in line:
            key, _, value = line.partition("=")
            fields[key.strip()] = value.strip()
    return fields


class Exchange:
    """One end of an exchange, as GcrSecretExchange."""

    def __init__(self, private=None):
        if private is None:
            while True:
                private = int.from_bytes(os.urandom(PRIME_BYTES), "big") & ((1 << 1535) - 1)
                if private:
                    break
        self.private = private
        self.public = pow(BASE, private, PRIME)
        self.key = None

    def begin(self):
        return _write([("public", base64.b64encode(_int_bytes(self.public)).decode())])

    def receive(self, text):
        """Derive the key from the peer's public key; return the secret when sent."""
        fields = parse(text)
        if self.key is None:
            peer = int.from_bytes(base64.b64decode(fields["public"]), "big")
            shared = pow(peer, self.private, PRIME).to_bytes(PRIME_BYTES, "big")
            self.key = hkdf_sha256(shared, KEY_BYTES)
        if "secret" not in fields:
            return None
        return cbc_decrypt(self.key, base64.b64decode(fields["iv"]),
                           base64.b64decode(fields["secret"]))

    def send(self, secret=None, iv=None):
        assert self.key is not None, "receive() first"
        fields = [("public", base64.b64encode(_int_bytes(self.public)).decode())]
        if secret is not None:
            iv = iv if iv is not None else os.urandom(16)
            fields.append(("secret", base64.b64encode(cbc_encrypt(self.key, iv, secret)).decode()))
            fields.append(("iv", base64.b64encode(iv).decode()))
        return _write(fields)
