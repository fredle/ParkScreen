use base64::{engine::general_purpose::URL_SAFE_NO_PAD as B64, Engine};
use ed25519_dalek::{Signer, SigningKey};
use std::{fs, io, path::Path};

/// Host identity: an Ed25519 key pair. `host_id()` is the base64url public key.
///
/// TODO(windows): wrap the stored secret with DPAPI. Until then the file is
/// written with owner-only permissions on Unix.
pub struct Identity(SigningKey);

impl Identity {
    pub fn generate() -> Self {
        Self(SigningKey::generate(&mut rand::rngs::OsRng))
    }

    pub fn load_or_create(path: &Path) -> io::Result<Self> {
        match fs::read_to_string(path) {
            Ok(s) => {
                let bytes = B64.decode(s.trim()).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
                let arr = <[u8; 32]>::try_from(bytes).map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "bad key length"))?;
                Ok(Self(SigningKey::from_bytes(&arr)))
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                let id = Self::generate();
                if let Some(dir) = path.parent() {
                    fs::create_dir_all(dir)?;
                }
                fs::write(path, B64.encode(id.0.to_bytes()))?;
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
                }
                Ok(id)
            }
            Err(e) => Err(e),
        }
    }

    pub fn host_id(&self) -> String {
        B64.encode(self.0.verifying_key().to_bytes())
    }

    /// Sign the server's base64url nonce; returns a base64url signature.
    pub fn sign_challenge(&self, nonce_b64: &str) -> Option<String> {
        let nonce = B64.decode(nonce_b64).ok()?;
        Some(B64.encode(self.0.sign(&nonce).to_bytes()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn persists_across_loads() {
        let dir = std::env::temp_dir().join(format!("ps-id-{}", rand::random::<u32>()));
        let p = dir.join("host.key");
        let a = Identity::load_or_create(&p).unwrap();
        let b = Identity::load_or_create(&p).unwrap();
        assert_eq!(a.host_id(), b.host_id());
        let _ = fs::remove_dir_all(dir);
    }
}
