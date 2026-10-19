use sha2::{Digest, Sha256};

/// Receipt hashes are always 64 lowercase hex characters, including leading zeroes.
pub fn finalize_hex(hash: Sha256) -> String {
    hash.finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_receipt_vectors_preserve_exact_encoding() {
        for (input, expected) in [
            (
                b"".as_slice(),
                "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
            ),
            (
                b"abc".as_slice(),
                "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
            ),
            (
                b"286".as_slice(),
                "00328ce57bbc14b33bd6695bc8eb32cdf2fb5f3a7d89ec14a42825e15d39df60",
            ),
            (
                b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq".as_slice(),
                "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1",
            ),
        ] {
            let mut hash = Sha256::new();
            hash.update(input);
            assert_eq!(finalize_hex(hash), expected);

            let mut streamed = Sha256::new();
            for chunk in input.chunks(2) {
                streamed.update(chunk);
            }
            assert_eq!(finalize_hex(streamed), expected);
        }
    }
}
