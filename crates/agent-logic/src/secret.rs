//! Fixed-size secrets kept AES-256-GCM encrypted in memory. Plaintext is
//! reachable only through a short-lived [`Exposed`] guard that zeroes on drop.

use std::fmt;
use std::ops::Deref;
use std::sync::Arc;

use aes_gcm::aead::Aead;
use aes_gcm::{Aes256Gcm, KeyInit, Nonce};
use rand::RngCore;
use zeroize::Zeroize;

pub use zeroize::Zeroizing;

const NONCE_LEN: usize = 12;

/// Sealed key material. Clones are cheap and share the same sealed buffer.
#[derive(Clone)]
pub struct Secret<const N: usize> {
    inner: Arc<Sealed>,
}

/// Per-secret cipher key plus `nonce || ciphertext || tag`.
struct Sealed {
    key: [u8; 32],
    payload: Vec<u8>,
}

impl Drop for Sealed {
    fn drop(&mut self) {
        self.key.zeroize();
        self.payload.zeroize();
    }
}

impl<const N: usize> Secret<N> {
    /// Seal `bytes`; the input buffer is zeroed before returning.
    pub fn seal(bytes: &mut [u8; N]) -> Self {
        // The cipher key is generated in place on the heap, never on the stack.
        let mut inner = Arc::new(Sealed {
            key: [0u8; 32],
            payload: Vec::new(),
        });
        let sealed = Arc::get_mut(&mut inner).expect("freshly created Arc is unique");
        let mut nonce = [0u8; NONCE_LEN];
        rand::rngs::OsRng.fill_bytes(&mut sealed.key);
        rand::rngs::OsRng.fill_bytes(&mut nonce);
        let ciphertext = Aes256Gcm::new_from_slice(&sealed.key)
            .expect("cipher key is 32 bytes")
            .encrypt(&Nonce::from(nonce), bytes.as_slice())
            .expect("in-memory AES-GCM encryption is infallible");
        bytes.zeroize();
        sealed.payload.reserve_exact(NONCE_LEN + ciphertext.len());
        sealed.payload.extend_from_slice(&nonce);
        sealed.payload.extend_from_slice(&ciphertext);
        Self { inner }
    }

    /// Decrypt into a guard; the plaintext copy is zeroed when the guard
    /// drops. Panics only if the sealed buffer was altered (program bug).
    pub fn expose(&self) -> Exposed<N> {
        let cipher = Aes256Gcm::new_from_slice(&self.inner.key).expect("cipher key is 32 bytes");
        let (nonce, ct) = self.inner.payload.split_at(NONCE_LEN);
        let nonce: [u8; NONCE_LEN] = nonce.try_into().expect("sealed secret failed to open");
        let plaintext = Zeroizing::new(
            cipher
                .decrypt(&Nonce::from(nonce), ct)
                .expect("sealed secret failed to open"),
        );
        let mut bytes = Box::new([0u8; N]);
        bytes.copy_from_slice(&plaintext);
        Exposed { bytes }
    }
}

impl<const N: usize> fmt::Debug for Secret<N> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Secret<{N}>(sealed)")
    }
}

/// Decrypted view of a [`Secret`]; zeroes its buffer on drop.
pub struct Exposed<const N: usize> {
    bytes: Box<[u8; N]>,
}

impl<const N: usize> Deref for Exposed<N> {
    type Target = [u8; N];
    fn deref(&self) -> &[u8; N] {
        &self.bytes
    }
}

impl<const N: usize> Drop for Exposed<N> {
    fn drop(&mut self) {
        (*self.bytes).zeroize();
    }
}

impl<const N: usize> fmt::Debug for Exposed<N> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Exposed<{N}>(..)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_seal_zeroes_input_and_roundtrips() {
        let mut input = [7u8; 32];
        let secret: Secret<32> = Secret::seal(&mut input);
        assert_eq!(input, [0u8; 32]);
        assert_eq!(&*secret.expose(), &[7u8; 32]);
    }

    #[test]
    fn test_clone_shares_sealed_bytes() {
        let secret = Secret::seal(&mut [9u8; 32]);
        let clone = secret.clone();
        drop(secret);
        assert_eq!(&*clone.expose(), &[9u8; 32]);
    }

    #[test]
    fn test_seal_roundtrips_other_sizes() {
        let small = Secret::seal(&mut [3u8; 16]);
        assert_eq!(&*small.expose(), &[3u8; 16]);
        let large = Secret::seal(&mut [5u8; 64]);
        assert_eq!(&*large.expose(), &[5u8; 64]);
    }

    #[test]
    fn test_debug_is_redacted() {
        let secret = Secret::seal(&mut [0xAB; 32]);
        assert_eq!(format!("{secret:?}"), "Secret<32>(sealed)");
        assert_eq!(format!("{:?}", secret.expose()), "Exposed<32>(..)");
    }
}
