//! gcr's `sx-aes-1` secret exchange (gcr/gcr-secret-exchange.c): how the
//! keyring prompter hands a password to the process that asked for it, and
//! how that process hands one back, over a D-Bus that other processes of
//! the session can watch.
//!
//! An exchange is a GKeyFile of one group:
//!
//! ```text
//! [sx-aes-1]
//! public=<base64, big-endian Diffie-Hellman public key, no leading zeros>
//! secret=<base64 AES-128-CBC/PKCS#7 ciphertext>      (when it carries one)
//! iv=<base64, 16 bytes>                              (with the secret)
//! ```
//!
//! The group is `ietf-ike-grp-modp-1536` (RFC 3526 group 5, generator 2),
//! with a private key of up to 1535 bits (egg/egg-dh-libgcrypt.c
//! `egg_dh_gen_pair`). The shared secret is `peer^private mod p` as 192
//! big-endian bytes; HKDF-SHA256 with the default salt and no info turns it
//! into the 16-byte AES key (egg/egg-hkdf-libgcrypt.c). The first exchange
//! string either end receives fixes the key; later `public` fields are
//! ignored, as gcr does.
//!
//! This protects a secret from a process that only reads the bus. The
//! public keys are not authenticated, so a process that can also rewrite
//! messages between the two ends is not stopped. That is gcr's trust model
//! too: the prompter's defense against such a process is that it delivers
//! each prompt's answer only to the bus name that began the prompt, and
//! the bus does not let a process claim another's unique name.
//!
//! Secrets are zeroized when dropped. `num-bigint` does not zeroize its
//! limbs, so the private exponent's integer form (built only while a key is
//! derived or generated) and the shared secret's may linger in freed heap
//! memory; the byte forms held between calls do not. The modular
//! exponentiation is not constant time, which does not matter for a key
//! that lives for one prompt.

use aes::Aes128;
use cbc::cipher::block_padding::Pkcs7;
use cbc::cipher::{BlockModeDecrypt, BlockModeEncrypt, KeyIvInit};
use glib::base64_encode;
use hkdf::Hkdf;
use num_bigint::BigUint;
use sha2::Sha256;
use zeroize::{Zeroize, Zeroizing};

pub const PROTOCOL: &str = "sx-aes-1";
const PRIME_BYTES: usize = 192;
const KEY_BYTES: usize = 16;
const IV_BYTES: usize = 16;
const BLOCK: usize = 16;

/// RFC 3526 §2, `ietf-ike-grp-modp-1536`:
/// 2^1536 − 2^1472 − 1 + 2^64 · (⌊2^1406 π⌋ + 741804).
const PRIME_HEX: &str = "\
FFFFFFFFFFFFFFFFC90FDAA22168C234C4C6628B80DC1CD1\
29024E088A67CC74020BBEA63B139B22514A08798E3404DD\
EF9519B3CD3A431B302B0A6DF25F14374FE1356D6D51C245\
E485B576625E7EC6F44C42E9A637ED6B0BFF5CB6F406B7ED\
EE386BFB5A899FA5AE9F24117C4B1FE649286651ECE45B3D\
C2007CB8A163BF0598DA48361C55D39A69163FA8FD24CF5F\
83655D23DCA3AD961C62F356208552BB9ED529077096966D\
670C354E4ABC9804F1746C08CA237327FFFFFFFFFFFFFFFF";

/// Why an exchange string was refused.
#[derive(Debug, PartialEq, Eq)]
pub enum ExchangeError {
    /// Not a GKeyFile with an `[sx-aes-1]` group.
    Malformed,
    /// No usable `public` key (missing, not base64, or outside 1 < y < p − 1).
    BadPublicKey,
    /// `secret` without a 16-byte `iv`, ciphertext that is not whole
    /// blocks, or padding that does not check out.
    BadSecret,
    /// A secret arrived before the peer's public key fixed the key.
    NoKey,
    /// The system has no random bytes to give.
    NoRandom,
}

impl std::fmt::Display for ExchangeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Malformed => "malformed secret exchange",
            Self::BadPublicKey => "invalid or missing public key in the secret exchange",
            Self::BadSecret => "invalid secret in the secret exchange",
            Self::NoKey => "no key has been exchanged yet",
            Self::NoRandom => "no random bytes available",
        })
    }
}

impl std::error::Error for ExchangeError {}

fn prime() -> BigUint {
    BigUint::parse_bytes(PRIME_HEX.as_bytes(), 16).expect("RFC 3526 prime")
}

fn random(buf: &mut [u8]) -> Result<(), ExchangeError> {
    let mut at = 0;
    while at < buf.len() {
        // SAFETY: the pointer and length describe the unfilled tail of buf.
        let n = unsafe { libc::getrandom(buf[at..].as_mut_ptr().cast(), buf.len() - at, 0) };
        if n < 0 {
            if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err(ExchangeError::NoRandom);
        }
        at += n as usize;
    }
    Ok(())
}

/// The `[sx-aes-1]` group's fields of a GKeyFile, as `g_key_file_get_value`
/// reads them: blank lines and `#` comments skipped, names and values
/// trimmed.
fn parse(text: &str) -> Result<Vec<(&str, &str)>, ExchangeError> {
    let mut in_group = false;
    let mut seen = false;
    let mut fields = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            in_group = name == PROTOCOL;
            seen |= in_group;
        } else if let Some((key, value)) = line.split_once('=') {
            if in_group {
                fields.push((key.trim(), value.trim()));
            }
        } else {
            return Err(ExchangeError::Malformed);
        }
    }
    if seen {
        Ok(fields)
    } else {
        Err(ExchangeError::Malformed)
    }
}

/// Standard base64 with optional `=` padding. GLib's decoder takes any
/// input and returns garbage for it; the exchange refuses what is not
/// base64.
fn base64_decode(text: &str) -> Option<Vec<u8>> {
    let digits = text.trim_end_matches('=');
    if text.len() - digits.len() > 2 || digits.len() % 4 == 1 {
        return None;
    }
    let mut out = Vec::with_capacity(digits.len() / 4 * 3 + 2);
    let (mut acc, mut bits) = (0u32, 0u32);
    for c in digits.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        };
        acc = acc << 6 | u32::from(v);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Some(out)
}

fn field<'a>(fields: &[(&'a str, &'a str)], key: &str) -> Option<&'a str> {
    fields.iter().find(|(k, _)| *k == key).map(|(_, v)| *v)
}

/// HKDF-SHA256 with the default salt and empty info down to the AES key.
fn derive_key(ikm: &[u8]) -> Zeroizing<[u8; KEY_BYTES]> {
    let mut key = Zeroizing::new([0u8; KEY_BYTES]);
    Hkdf::<Sha256>::new(None, ikm)
        .expand(&[], key.as_mut_slice())
        .expect("16 bytes is a valid HKDF-SHA256 length");
    key
}

/// One end of an exchange (`GcrSecretExchange`).
pub struct Exchange {
    private: Zeroizing<Vec<u8>>,
    public: Vec<u8>,
    key: Option<Zeroizing<[u8; KEY_BYTES]>>,
}

impl Exchange {
    /// A fresh key pair from the system's random bytes.
    pub fn new() -> Result<Self, ExchangeError> {
        let mut private = Zeroizing::new(vec![0u8; PRIME_BYTES]);
        loop {
            random(&mut private)?;
            // Below 2^1535, as gcr clears everything above bit 1535.
            private[0] &= 0x7f;
            if private.iter().any(|&b| b != 0) {
                return Ok(Self::from_private(&private));
            }
        }
    }

    /// The key pair of a given private key (big-endian, at most 1535 bits,
    /// not zero). Known-answer tests use it; `new` is for everything else.
    pub(crate) fn from_private(private: &[u8]) -> Self {
        let x = Zeroizing::new(private.to_vec());
        let public = BigUint::from(2u8).modpow(&BigUint::from_bytes_be(&x), &prime());
        Self {
            private: x,
            public: public.to_bytes_be(),
            key: None,
        }
    }

    /// `gcr_secret_exchange_begin`: our public key, to start an exchange.
    pub fn begin(&self) -> String {
        format!("[{PROTOCOL}]\npublic={}\n", base64_encode(&self.public))
    }

    /// `gcr_secret_exchange_receive`: take the peer's exchange. The first
    /// one fixes the key from its public key. Returns the secret it
    /// carries, if any.
    pub fn receive(&mut self, text: &str) -> Result<Option<Zeroizing<Vec<u8>>>, ExchangeError> {
        let fields = parse(text)?;
        if self.key.is_none() {
            let peer = field(&fields, "public")
                .and_then(base64_decode)
                .ok_or(ExchangeError::BadPublicKey)?;
            self.key = Some(self.derive(&peer)?);
        }
        let Some(secret) = field(&fields, "secret") else {
            return Ok(None);
        };
        let key = self.key.as_ref().ok_or(ExchangeError::NoKey)?;
        let iv: [u8; IV_BYTES] = field(&fields, "iv")
            .and_then(base64_decode)
            .and_then(|iv| iv.try_into().ok())
            .ok_or(ExchangeError::BadSecret)?;
        let mut buf = Zeroizing::new(base64_decode(secret).ok_or(ExchangeError::BadSecret)?);
        if buf.is_empty() || buf.len() % BLOCK != 0 {
            return Err(ExchangeError::BadSecret);
        }
        let plain_len = cbc::Decryptor::<Aes128>::new((&**key).into(), (&iv).into())
            .decrypt_padded::<Pkcs7>(&mut buf)
            .map_err(|_| ExchangeError::BadSecret)?
            .len();
        buf.truncate(plain_len);
        Ok(Some(buf))
    }

    /// `gcr_secret_exchange_send`: our public key and, when given, the
    /// secret encrypted under a fresh IV. Only after `receive`.
    pub fn send(&self, secret: Option<&[u8]>) -> Result<String, ExchangeError> {
        let mut iv = [0u8; IV_BYTES];
        if secret.is_some() {
            random(&mut iv)?;
        }
        self.send_with_iv(secret, &iv)
    }

    fn send_with_iv(
        &self,
        secret: Option<&[u8]>,
        iv: &[u8; IV_BYTES],
    ) -> Result<String, ExchangeError> {
        let key = self.key.as_ref().ok_or(ExchangeError::NoKey)?;
        let mut out = format!("[{PROTOCOL}]\npublic={}\n", base64_encode(&self.public));
        if let Some(secret) = secret {
            // PKCS#7 always adds a pad: n + 1..=16 bytes, whole blocks.
            let padded = (secret.len() / BLOCK + 1) * BLOCK;
            let mut buf = Zeroizing::new(vec![0u8; padded]);
            buf[..secret.len()].copy_from_slice(secret);
            let cipher = cbc::Encryptor::<Aes128>::new((&**key).into(), iv.into())
                .encrypt_padded::<Pkcs7>(&mut buf, secret.len())
                .map_err(|_| ExchangeError::BadSecret)?;
            out.push_str(&format!(
                "secret={}\niv={}\n",
                base64_encode(cipher),
                base64_encode(iv)
            ));
        }
        Ok(out)
    }

    /// The AES key for a peer's public key. Public keys outside
    /// 1 < y < p − 1 are refused: gcr takes them, but 1 and p − 1 make the
    /// shared secret one of two known values.
    fn derive(&self, peer: &[u8]) -> Result<Zeroizing<[u8; KEY_BYTES]>, ExchangeError> {
        let p = prime();
        let y = BigUint::from_bytes_be(peer);
        if y <= BigUint::from(1u8) || y >= &p - 1u8 {
            return Err(ExchangeError::BadPublicKey);
        }
        let shared = y.modpow(&BigUint::from_bytes_be(&self.private), &p);
        let bytes = shared.to_bytes_be();
        // gcr pads the secret with zero bytes up to the prime's length.
        let mut ikm = Zeroizing::new(vec![0u8; PRIME_BYTES]);
        ikm[PRIME_BYTES - bytes.len()..].copy_from_slice(&bytes);
        let mut bytes = bytes;
        bytes.zeroize();
        Ok(derive_key(&ikm))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Known answers from `scripts/lib/keyring_sx_aes.py`, an independent
    /// reimplementation checked against libgcr itself
    /// (`scripts/lib/keyring-sx-aes-tests.py`): fixed private keys, the
    /// exchange strings both ends write, the AES key, and a string real
    /// libgcr wrote for our first message.
    const VECTORS: &str = include_str!("keyring_exchange_vectors.json");

    fn vectors() -> serde_json::Value {
        serde_json::from_str(VECTORS).expect("vectors")
    }

    fn hex(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    fn text<'a>(v: &'a serde_json::Value, key: &str) -> &'a str {
        v[key].as_str().unwrap_or_else(|| panic!("vector {key}"))
    }

    fn pair() -> (Exchange, Exchange) {
        let v = vectors();
        (
            Exchange::from_private(&hex(text(&v, "private_a"))),
            Exchange::from_private(&hex(text(&v, "private_b"))),
        )
    }

    #[test]
    fn the_prime_is_rfc_3526_group_5() {
        let p = prime();
        assert_eq!(p.bits(), 1536);
        // 2^1536 − 2^1472 − 1 + 2^64 · (⌊2^1406 π⌋ + 741804): the top and
        // bottom 64 bits are all ones, and p is a safe-prime candidate Fermat
        // accepts for base 2.
        let one = BigUint::from(1u8);
        assert_eq!(BigUint::from(2u8).modpow(&(&p - &one), &p), one);
        assert_eq!(&p & BigUint::from(u64::MAX), BigUint::from(u64::MAX));
        assert_eq!(&p >> 1472usize, (BigUint::from(1u8) << 64usize) - 1u8);
    }

    #[test]
    fn public_keys_and_begin_strings_match_the_reference() {
        let v = vectors();
        let (a, b) = pair();
        assert_eq!(a.begin(), text(&v, "a_begin"));
        assert_eq!(b.begin(), text(&v, "b_begin"));
    }

    #[test]
    fn the_derived_key_matches_the_reference() {
        let v = vectors();
        let (mut a, mut b) = pair();
        a.receive(text(&v, "b_begin")).unwrap();
        b.receive(text(&v, "a_begin")).unwrap();
        assert_eq!(a.key.as_deref().unwrap().as_slice(), hex(text(&v, "key")));
        assert_eq!(b.key.as_deref().unwrap().as_slice(), hex(text(&v, "key")));
    }

    #[test]
    fn sent_exchanges_match_the_reference_byte_for_byte() {
        let v = vectors();
        let (mut a, mut b) = pair();
        b.receive(text(&v, "a_begin")).unwrap();
        a.receive(text(&v, "b_begin")).unwrap();
        let iv: [u8; IV_BYTES] = hex(text(&v, "iv")).try_into().unwrap();
        for case in v["b_send"].as_array().unwrap() {
            let secret = case["secret"].as_str().map(str::as_bytes);
            assert_eq!(
                b.send_with_iv(secret, &iv).unwrap(),
                text(case, "exchange"),
                "{secret:?}"
            );
            let got = a.receive(text(case, "exchange")).unwrap();
            assert_eq!(got.as_deref().map(Vec::as_slice), secret);
        }
    }

    #[test]
    fn a_string_libgcr_wrote_decrypts() {
        let v = vectors();
        let (mut a, _) = pair();
        // libgcr answered a_begin with its own key and this secret.
        let gcr = &v["gcr_send"];
        let secret = a.receive(text(gcr, "exchange")).unwrap().unwrap();
        assert_eq!(secret.as_slice(), text(gcr, "secret").as_bytes());
    }

    #[test]
    fn random_pairs_exchange_in_both_directions() {
        let mut ours = Exchange::new().unwrap();
        let mut theirs = Exchange::new().unwrap();
        assert!(theirs.receive(&ours.begin()).unwrap().is_none());
        let sent = theirs.send(Some("p\u{e4}ssw\u{f6}rd".as_bytes())).unwrap();
        let got = ours.receive(&sent).unwrap().unwrap();
        assert_eq!(got.as_slice(), "p\u{e4}ssw\u{f6}rd".as_bytes());
        // The other way: the prompter side asks nothing back of the client.
        let reply = ours.send(None).unwrap();
        assert!(theirs.receive(&reply).unwrap().is_none());
        assert!(!reply.contains("secret="));
    }

    #[test]
    fn an_empty_secret_is_one_block_of_padding() {
        let (mut a, mut b) = pair();
        b.receive(&a.begin()).unwrap();
        let sent = b.send(Some(b"")).unwrap();
        assert!(sent.contains("secret="));
        assert_eq!(a.receive(&sent).unwrap().unwrap().as_slice(), b"");
    }

    #[test]
    fn hkdf_sha256_matches_rfc_5869_case_3() {
        // Zero-length salt and info, 22 bytes of 0x0b: the key is the first
        // 16 bytes of the RFC's 42-byte output.
        let key = derive_key(&[0x0b; 22]);
        assert_eq!(key.as_slice(), hex("8da4e775a563c18f715f802a063c5a31"));
    }

    #[test]
    fn aes_128_cbc_matches_nist_sp_800_38a_f_2_1() {
        let key = hex("2b7e151628aed2a6abf7158809cf4f3c");
        let iv = hex("000102030405060708090a0b0c0d0e0f");
        let plain = hex(
            "6bc1bee22e409f96e93d7e117393172aae2d8a571e03ac9c9eb76fac45af8e51\
             30c81c46a35ce411e5fbc1191a0a52eff69f2445df4f9b17ad2b417be66c3710",
        );
        let mut buf = vec![0u8; plain.len() + BLOCK];
        buf[..plain.len()].copy_from_slice(&plain);
        let (key, iv): ([u8; 16], [u8; 16]) = (key.try_into().unwrap(), iv.try_into().unwrap());
        let cipher = cbc::Encryptor::<Aes128>::new((&key).into(), (&iv).into())
            .encrypt_padded::<Pkcs7>(&mut buf, plain.len())
            .unwrap();
        assert_eq!(
            &cipher[..plain.len()],
            hex(
                "7649abac8119b246cee98e9b12e9197d5086cb9b507219ee95db113a917678b2\
                 73bed6b8e3c1743b7116e69e222295163ff1caa1681fac09120eca307586e1a7"
            )
        );
    }

    #[test]
    fn peers_that_cannot_be_trusted_are_refused() {
        let mut ours = Exchange::new().unwrap();
        let p = prime();
        let one = [1u8];
        let p_minus_one = (&p - 1u8).to_bytes_be();
        let zero = [0u8];
        let too_big = (&p + 5u8).to_bytes_be();
        for bad in [
            &one[..],
            &zero[..],
            &p_minus_one,
            &p.to_bytes_be(),
            &too_big,
        ] {
            let text = format!("[sx-aes-1]\npublic={}\n", base64_encode(bad));
            assert_eq!(ours.receive(&text), Err(ExchangeError::BadPublicKey));
        }
        for bad in [
            "",
            "garbage",
            "[other]\npublic=AQ==\n",
            "[sx-aes-1]\nsecret=AAAA\n",
            "[sx-aes-1]\npublic=!!!\n",
        ] {
            assert!(ours.receive(bad).is_err(), "{bad:?}");
        }
        assert!(ours.key.is_none());
    }

    #[test]
    fn damaged_secrets_are_refused() {
        let (mut a, mut b) = pair();
        b.receive(&a.begin()).unwrap();
        let good = b.send(Some(b"secret")).unwrap();
        a.receive(&good).unwrap();
        let iv_line = good.lines().find(|l| l.starts_with("iv=")).unwrap();
        let cipher_line = good.lines().find(|l| l.starts_with("secret=")).unwrap();
        for bad in [
            good.replace(iv_line, "iv=AAAA"),
            good.replace(iv_line, ""),
            good.replace(cipher_line, "secret=AAAA"),
            good.replace(cipher_line, "secret="),
            // Another key's ciphertext: the padding does not check out.
            good.replace(cipher_line, "secret=AAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=="),
        ] {
            assert_eq!(a.receive(&bad), Err(ExchangeError::BadSecret), "{bad}");
        }
    }

    #[test]
    fn send_before_receive_has_no_key() {
        let ours = Exchange::new().unwrap();
        assert_eq!(ours.send(Some(b"x")), Err(ExchangeError::NoKey));
    }

    #[test]
    fn the_first_public_key_fixes_the_key() {
        let (mut a, mut b) = pair();
        let mut c = Exchange::new().unwrap();
        a.receive(&b.begin()).unwrap();
        let key = *a.key.as_deref().unwrap();
        // gcr ignores a later `public`; a later secret still decrypts under
        // the first key.
        c.receive(&a.begin()).unwrap();
        a.receive(&c.begin()).unwrap();
        assert_eq!(*a.key.as_deref().unwrap(), key);
        b.receive(&a.begin()).unwrap();
        let sent = b.send(Some(b"again")).unwrap();
        assert_eq!(a.receive(&sent).unwrap().unwrap().as_slice(), b"again");
    }

    #[test]
    fn parse_reads_a_gkeyfile_like_gcr() {
        let fields = parse("# c\n\n[sx-aes-1]\n public = QQ== \nx=y\n[other]\nz=1\n").unwrap();
        assert_eq!(fields, vec![("public", "QQ=="), ("x", "y")]);
    }
}
