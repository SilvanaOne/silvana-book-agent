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

/// Why a secret could not be sealed or opened.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SecretError {
    /// The OS random source failed while sealing.
    Rng,
    /// Encryption failed while sealing.
    Seal,
    /// The sealed buffer does not open to `N` bytes.
    Corrupt,
}

impl fmt::Display for SecretError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Rng => "random source failed while sealing a secret",
            Self::Seal => "could not seal a secret",
            Self::Corrupt => "sealed secret is corrupt",
        })
    }
}

impl std::error::Error for SecretError {}

/// Sealed key material. Clones are cheap and share the same sealed buffer.
#[derive(Clone)]
pub struct Secret<const N: usize> {
    inner: Arc<Sealed>,
}

/// Per-secret cipher key plus `nonce || ciphertext || tag`.
struct Sealed {
    key: Box<[u8; 32]>,
    payload: Vec<u8>,
}

impl Drop for Sealed {
    fn drop(&mut self) {
        self.key.zeroize();
        self.payload.zeroize();
    }
}

impl<const N: usize> Secret<N> {
    /// Seal `bytes`; the input buffer is zeroed before returning, also on error.
    pub fn seal(bytes: &mut [u8; N]) -> Result<Self, SecretError> {
        let sealed = seal_with(bytes, &mut rand::rngs::OsRng);
        bytes.zeroize();
        sealed.map(|s| Self { inner: Arc::new(s) })
    }

    /// Decrypt into a guard; the plaintext copy is zeroed when the guard drops.
    pub fn expose(&self) -> Result<Exposed<N>, SecretError> {
        let cipher = Aes256Gcm::new((&*self.inner.key).into());
        let (nonce, ct) = self
            .inner
            .payload
            .split_first_chunk::<NONCE_LEN>()
            .ok_or(SecretError::Corrupt)?;
        let plaintext = Zeroizing::new(
            cipher
                .decrypt(&Nonce::from(*nonce), ct)
                .map_err(|_| SecretError::Corrupt)?,
        );
        let src: &[u8; N] = plaintext.as_slice().try_into().map_err(|_| SecretError::Corrupt)?;
        let mut bytes = Box::new([0u8; N]);
        for (dst, b) in bytes.iter_mut().zip(src) {
            *dst = *b;
        }
        Ok(Exposed { bytes })
    }

    /// A secret that never opens, for tests of the failure paths.
    #[doc(hidden)]
    pub fn corrupt_for_tests() -> Self {
        Self {
            inner: Arc::new(Sealed { key: Box::new([0u8; 32]), payload: Vec::new() }),
        }
    }
}

/// The key lives on the heap from the start; a failed seal zeroes it on drop.
fn seal_with<const N: usize, R: RngCore>(bytes: &[u8; N], rng: &mut R) -> Result<Sealed, SecretError> {
    let mut sealed = Sealed { key: Box::new([0u8; 32]), payload: Vec::new() };
    let mut nonce = [0u8; NONCE_LEN];
    rng.try_fill_bytes(&mut *sealed.key).map_err(|_| SecretError::Rng)?;
    rng.try_fill_bytes(&mut nonce).map_err(|_| SecretError::Rng)?;
    let ciphertext = Aes256Gcm::new((&*sealed.key).into())
        .encrypt(&Nonce::from(nonce), bytes.as_slice())
        .map_err(|_| SecretError::Seal)?;
    sealed.payload.reserve_exact(NONCE_LEN.saturating_add(ciphertext.len()));
    sealed.payload.extend_from_slice(&nonce);
    sealed.payload.extend_from_slice(&ciphertext);
    Ok(sealed)
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
        let secret: Secret<32> = Secret::seal(&mut input).unwrap();
        assert_eq!(input, [0u8; 32]);
        assert_eq!(&*secret.expose().unwrap(), &[7u8; 32]);
    }

    #[test]
    fn test_clone_shares_sealed_bytes() {
        let secret = Secret::seal(&mut [9u8; 32]).unwrap();
        let clone = secret.clone();
        drop(secret);
        assert_eq!(&*clone.expose().unwrap(), &[9u8; 32]);
    }

    #[test]
    fn test_seal_roundtrips_other_sizes() {
        let small = Secret::seal(&mut [3u8; 16]).unwrap();
        assert_eq!(&*small.expose().unwrap(), &[3u8; 16]);
        let large = Secret::seal(&mut [5u8; 64]).unwrap();
        assert_eq!(&*large.expose().unwrap(), &[5u8; 64]);
    }

    #[test]
    fn test_debug_is_redacted() {
        let secret = Secret::seal(&mut [0xAB; 32]).unwrap();
        assert_eq!(format!("{secret:?}"), "Secret<32>(sealed)");
        assert_eq!(format!("{:?}", secret.expose().unwrap()), "Exposed<32>(..)");
    }

    fn with_payload<const N: usize>(from: &Secret<N>, edit: impl FnOnce(&mut Vec<u8>)) -> Secret<N> {
        let mut payload = from.inner.payload.clone();
        edit(&mut payload);
        Secret { inner: Arc::new(Sealed { key: from.inner.key.clone(), payload }) }
    }

    // Truncated, altered or empty sealed buffers are errors, not panics
    #[test]
    fn a_corrupted_secret_does_not_open() {
        let secret: Secret<32> = Secret::seal(&mut [1u8; 32]).unwrap();
        let short = with_payload(&secret, |p| p.truncate(NONCE_LEN - 1));
        assert_eq!(short.expose().unwrap_err(), SecretError::Corrupt);
        let flipped = with_payload(&secret, |p| {
            if let Some(b) = p.last_mut() {
                *b ^= 1;
            }
        });
        assert_eq!(flipped.expose().unwrap_err(), SecretError::Corrupt);
        assert_eq!(Secret::<32>::corrupt_for_tests().expose().unwrap_err(), SecretError::Corrupt);
        assert_eq!(&*secret.expose().unwrap(), &[1u8; 32]);
    }

    // A buffer that opens to the wrong length is rejected
    #[test]
    fn a_secret_of_another_size_does_not_open() {
        let small: Secret<16> = Secret::seal(&mut [2u8; 16]).unwrap();
        let as_large: Secret<32> = Secret { inner: small.inner.clone() };
        assert_eq!(as_large.expose().unwrap_err(), SecretError::Corrupt);
    }

    struct FailingRng;

    impl RngCore for FailingRng {
        fn next_u32(&mut self) -> u32 {
            0
        }
        fn next_u64(&mut self) -> u64 {
            0
        }
        fn fill_bytes(&mut self, dest: &mut [u8]) {
            dest.fill(0);
        }
        fn try_fill_bytes(&mut self, _dest: &mut [u8]) -> Result<(), rand::Error> {
            Err(rand::Error::new("no entropy"))
        }
    }

    // A failing random source is reported instead of panicking
    #[test]
    fn a_failing_random_source_is_an_error() {
        assert_eq!(seal_with(&[4u8; 32], &mut FailingRng).err(), Some(SecretError::Rng));
    }

    #[test]
    fn errors_name_the_failure_and_convert_to_anyhow() {
        let e: anyhow::Error = SecretError::Corrupt.into();
        assert_eq!(e.to_string(), "sealed secret is corrupt");
        assert_eq!(SecretError::Rng.to_string(), "random source failed while sealing a secret");
    }
}
