//! Where the host's private key lives. On Windows it is a generic credential in Credential
//! Manager, under the user's logon, so it is not a file anyone can copy out of the app data
//! folder. Elsewhere the core's `FileKeyProvider` keeps it in `<link dir>/host_key`, mode 0600.
//! The key never crosses IPC and is never formatted: `SecretKey` has no printable form.

use std::path::Path;

use orchestra_core::link::keys::{KEY_BYTES, KeyError, KeyProvider, SecretKey};

/// The bundle identifier plus what is stored. Deleting this credential gives the host a new
/// key, and every paired phone then refuses it as "host key changed" until it pairs again.
pub const TARGET: &str = "com.telosrg.orchestra/link/host_key";

/// The provider for this OS. Windows never reads or writes `link/host_key`.
pub fn provider_for(link_dir: &Path) -> Box<dyn KeyProvider> {
    #[cfg(windows)]
    {
        let _ = link_dir;
        Box::new(win::CredentialKeyProvider::new(TARGET))
    }
    #[cfg(not(windows))]
    {
        Box::new(orchestra_core::link::keys::FileKeyProvider::new(link_dir))
    }
}

/// A stored blob is a key only if it is exactly 32 bytes; anything else is refused and never
/// overwritten, as with the key file.
#[cfg_attr(not(any(windows, test)), allow(dead_code))]
fn key_from_blob(blob: &[u8]) -> Result<SecretKey, KeyError> {
    let Ok(mut bytes) = <[u8; KEY_BYTES]>::try_from(blob) else {
        return Err(KeyError::BadFile);
    };
    let key = SecretKey::from_bytes(bytes);
    wipe(&mut bytes);
    Ok(key)
}

#[cfg_attr(not(any(windows, test)), allow(dead_code))]
fn wipe(bytes: &mut [u8]) {
    for b in bytes.iter_mut() {
        // SAFETY: `b` is a valid, aligned, exclusive reference; volatile keeps the write.
        unsafe { std::ptr::write_volatile(b, 0) };
    }
}

#[cfg(windows)]
mod win {
    use std::io::ErrorKind;
    use std::ptr;

    use orchestra_core::link::keys::{KeyError, KeyProvider, SecretKey};
    use windows_sys::Win32::Foundation::{ERROR_NOT_FOUND, GetLastError};
    use windows_sys::Win32::Security::Credentials::{
        CRED_PERSIST_LOCAL_MACHINE, CRED_TYPE_GENERIC, CREDENTIALW, CredFree, CredReadW, CredWriteW,
    };
    #[cfg(test)]
    use windows_sys::Win32::Security::Credentials::CredDeleteW;

    use super::{key_from_blob, wipe};

    pub struct CredentialKeyProvider {
        /// The target name, UTF-16 and NUL-terminated.
        target: Vec<u16>,
    }

    impl CredentialKeyProvider {
        pub fn new(target: &str) -> Self {
            Self { target: target.encode_utf16().chain(std::iter::once(0)).collect() }
        }

        /// `None` when no credential has this name.
        fn read(&self) -> Result<Option<SecretKey>, KeyError> {
            let mut cred: *mut CREDENTIALW = ptr::null_mut();
            // SAFETY: `target` is NUL-terminated and outlives the call. On success `cred`
            // points at a buffer the API allocated; it is freed with CredFree below.
            let ok = unsafe { CredReadW(self.target.as_ptr(), CRED_TYPE_GENERIC, 0, &mut cred) };
            if ok == 0 {
                // SAFETY: reads the calling thread's last error, nothing else.
                let code = unsafe { GetLastError() };
                return if code == ERROR_NOT_FOUND { Ok(None) } else { Err(KeyError::Io(ErrorKind::Other)) };
            }
            if cred.is_null() {
                return Err(KeyError::Io(ErrorKind::Other));
            }
            // SAFETY: `cred` is non-null and valid until CredFree. The blob is copied out
            // (`to_vec`) and then wiped inside the API's buffer before that buffer is freed.
            let result = unsafe {
                let c = &*cred;
                let len = c.CredentialBlobSize as usize;
                let result = if c.CredentialBlob.is_null() {
                    Err(KeyError::BadFile)
                } else {
                    let blob = std::slice::from_raw_parts_mut(c.CredentialBlob, len);
                    let mut copy = blob.to_vec();
                    let key = key_from_blob(&copy);
                    wipe(&mut copy);
                    wipe(blob);
                    key
                };
                CredFree(cred as *const _);
                result
            };
            result.map(Some)
        }

        fn write(&self, blob: &[u8]) -> Result<(), KeyError> {
            let mut copy = blob.to_vec();
            let mut cred = CREDENTIALW::default();
            cred.Type = CRED_TYPE_GENERIC;
            cred.TargetName = self.target.as_ptr() as *mut u16;
            cred.CredentialBlobSize = copy.len() as u32;
            cred.CredentialBlob = copy.as_mut_ptr();
            cred.Persist = CRED_PERSIST_LOCAL_MACHINE;
            // SAFETY: `cred` and what it points at (`target`, `copy`) live past the call, which
            // only reads them. The target is NUL-terminated.
            let ok = unsafe { CredWriteW(&cred, 0) };
            wipe(&mut copy);
            if ok == 0 { Err(KeyError::Io(ErrorKind::Other)) } else { Ok(()) }
        }

        /// Remove the credential. Tests only: the product never deletes the host key.
        #[cfg(test)]
        pub(super) fn delete(&self) {
            // SAFETY: `target` is NUL-terminated and outlives the call.
            unsafe { CredDeleteW(self.target.as_ptr(), CRED_TYPE_GENERIC, 0) };
        }

        #[cfg(test)]
        pub(super) fn write_raw(&self, blob: &[u8]) -> Result<(), KeyError> {
            self.write(blob)
        }
    }

    impl KeyProvider for CredentialKeyProvider {
        fn load_or_create(&self) -> Result<SecretKey, KeyError> {
            if let Some(key) = self.read()? {
                return Ok(key);
            }
            let key = SecretKey::generate()?;
            self.write(key.expose())?;
            // Return what is stored, which narrows two first starts at once to this read.
            self.read()?.ok_or(KeyError::Io(ErrorKind::Other))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keyring_blob_must_be_exactly_32_bytes() {
        let key = key_from_blob(&[7u8; 32]).unwrap();
        assert_eq!(key.expose(), &[7u8; 32]);
        for len in [0usize, 1, 31, 33, 64] {
            assert_eq!(key_from_blob(&vec![1u8; len]).unwrap_err(), KeyError::BadFile, "len {len}");
        }
    }

    #[test]
    fn keyring_target_names_the_bundle() {
        assert!(TARGET.starts_with("com.telosrg.orchestra/"));
    }

    #[cfg(windows)]
    mod credential_manager {
        use super::super::win::CredentialKeyProvider;
        use super::*;

        /// A per-test target that is deleted afterwards, so the real credential is never used.
        struct Scratch(CredentialKeyProvider);

        impl Scratch {
            fn new(name: &str) -> Self {
                let target = format!("com.telosrg.orchestra.test/{}/{name}", std::process::id());
                let s = Scratch(CredentialKeyProvider::new(&target));
                s.0.delete();
                s
            }
        }

        impl Drop for Scratch {
            fn drop(&mut self) {
                self.0.delete();
            }
        }

        #[test]
        fn keyring_credential_round_trip_returns_the_same_key() {
            let s = Scratch::new("roundtrip");
            let first = s.0.load_or_create().expect("Credential Manager write and read");
            let again = s.0.load_or_create().unwrap();
            assert_eq!(first.expose(), again.expose());
            assert_ne!(first.expose(), &[0u8; 32]);
            assert_eq!(first.public(), again.public());
        }

        #[test]
        fn keyring_credential_of_the_wrong_length_is_refused_and_kept() {
            let s = Scratch::new("badblob");
            s.0.write_raw(&[5u8; 31]).expect("Credential Manager write");
            assert_eq!(s.0.load_or_create().unwrap_err(), KeyError::BadFile);
            // Still the same 31 bytes: a second call is refused the same way, not replaced.
            assert_eq!(s.0.load_or_create().unwrap_err(), KeyError::BadFile);
        }
    }
}
