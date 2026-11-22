//! Update metadata and signatures.
//!
//! Every release publishes `latest.json` (a [`Manifest`]): per platform, the
//! download URL, the SHA-256 of the file and an Ed25519 signature over
//! [`signed_message`] — `platform`, `version` and the hash together, so a
//! validly signed file can't be relabeled as another version or platform.
//! The app only installs files whose signature verifies against one of
//! [`TRUSTED_KEYS`]; the private key lives only in CI secrets.

use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;

use base64::Engine as _;
use ed25519_dalek::{Signature, Signer as _, SigningKey, Verifier as _, VerifyingKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

/// Public keys (base64) whose signatures the app accepts. Add the new key
/// here before rotating, keep the old one until every install has updated.
pub const TRUSTED_KEYS: &[&str] = &["REPLACE_WITH_PUBLIC_KEY"];

/// Where the app asks for updates (overridable with `DBCLAW_UPDATE_URL`).
pub const DEFAULT_UPDATE_URL: &str = "https://dbclaw.vilaz.dev";

/// This build's platform name as used in manifests, e.g. `macos-arm64`.
pub fn current_platform() -> &'static str {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => "macos-arm64",
        ("macos", "x86_64") => "macos-x86_64",
        ("windows", "x86_64") => "windows-x86_64",
        ("windows", "aarch64") => "windows-arm64",
        ("linux", "x86_64") => "linux-x86_64",
        ("linux", "aarch64") => "linux-arm64",
        _ => "unsupported",
    }
}

// ---------------------------------------------------------------------------
// Versions

/// `major.minor.patch[-pre]`, compared like SemVer (a pre-release sorts
/// before its release; pre-release strings compare piecewise).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Version {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
    pub pre: Option<String>,
}

impl FromStr for Version {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        let s = s.trim().trim_start_matches('v');
        let s = s.split('+').next().unwrap_or(s); // build metadata is ignored
        let (core, pre) = match s.split_once('-') {
            Some((c, p)) if !p.is_empty() => (c, Some(p.to_string())),
            _ => (s, None),
        };
        let mut parts = core.split('.');
        let mut next = || -> Result<u64, String> {
            parts
                .next()
                .ok_or_else(|| format!("invalid version `{s}`"))?
                .parse()
                .map_err(|_| format!("invalid version `{s}`"))
        };
        let v = Version { major: next()?, minor: next()?, patch: next()?, pre };
        if parts.next().is_some() {
            return Err(format!("invalid version `{s}`"));
        }
        Ok(v)
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)?;
        if let Some(p) = &self.pre {
            write!(f, "-{p}")?;
        }
        Ok(())
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        (self.major, self.minor, self.patch)
            .cmp(&(other.major, other.minor, other.patch))
            .then_with(|| match (&self.pre, &other.pre) {
                (None, None) => Ordering::Equal,
                (None, Some(_)) => Ordering::Greater,
                (Some(_), None) => Ordering::Less,
                (Some(a), Some(b)) => cmp_pre(a, b),
            })
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

fn cmp_pre(a: &str, b: &str) -> Ordering {
    let mut ia = a.split('.');
    let mut ib = b.split('.');
    loop {
        match (ia.next(), ib.next()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(x), Some(y)) => {
                let o = match (x.parse::<u64>(), y.parse::<u64>()) {
                    (Ok(nx), Ok(ny)) => nx.cmp(&ny),
                    (Ok(_), Err(_)) => Ordering::Less,
                    (Err(_), Ok(_)) => Ordering::Greater,
                    _ => x.cmp(y),
                };
                if o != Ordering::Equal {
                    return o;
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Manifest

/// `latest.json`, published with every release.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Manifest {
    pub version: String,
    #[serde(default)]
    pub pub_date: String,
    #[serde(default)]
    pub notes: String,
    #[serde(default)]
    pub notes_url: String,
    pub platforms: BTreeMap<String, Artifact>,
}

/// One downloadable update for a platform.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Artifact {
    pub url: String,
    /// Lower-case hex SHA-256 of the file.
    pub sha256: String,
    /// Base64 Ed25519 signature over [`signed_message`].
    pub signature: String,
    /// How the app applies it: `installer` (Windows setup.exe),
    /// `app-tarball` (macOS .app.tar.gz), `appimage` (Linux).
    pub kind: String,
}

/// What the update server answers when an update is available.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Offer {
    pub version: String,
    pub platform: String,
    #[serde(default)]
    pub notes: String,
    #[serde(default)]
    pub notes_url: String,
    pub url: String,
    pub sha256: String,
    pub signature: String,
    pub kind: String,
}

/// The bytes that get signed for an artifact.
pub fn signed_message(platform: &str, version: &str, sha256_hex: &str) -> String {
    format!("dbclaw-update:v1:{platform}:{version}:{}", sha256_hex.to_ascii_lowercase())
}

pub fn sha256_hex(data: &[u8]) -> String {
    let digest = Sha256::digest(data);
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

fn b64() -> base64::engine::GeneralPurpose {
    base64::engine::general_purpose::STANDARD
}

/// Sign an artifact (release tooling). `secret_key_b64` is the 32-byte seed.
pub fn sign(secret_key_b64: &str, platform: &str, version: &str, sha256_hex: &str) -> Result<String, String> {
    let seed: [u8; 32] = b64()
        .decode(secret_key_b64.trim())
        .map_err(|e| format!("bad signing key: {e}"))?
        .try_into()
        .map_err(|_| "signing key must be 32 bytes".to_string())?;
    let key = SigningKey::from_bytes(&seed);
    Ok(b64().encode(key.sign(signed_message(platform, version, sha256_hex).as_bytes()).to_bytes()))
}

/// Public key (base64) for a secret key seed.
pub fn public_key_for(secret_key_b64: &str) -> Result<String, String> {
    let seed: [u8; 32] = b64()
        .decode(secret_key_b64.trim())
        .map_err(|e| format!("bad signing key: {e}"))?
        .try_into()
        .map_err(|_| "signing key must be 32 bytes".to_string())?;
    Ok(b64().encode(SigningKey::from_bytes(&seed).verifying_key().to_bytes()))
}

/// Verify a downloaded file: its hash must match `sha256_hex`, and the
/// signature must be valid for one of `keys`.
pub fn verify(
    keys: &[&str],
    platform: &str,
    version: &str,
    data: &[u8],
    sha256_expected: &str,
    signature_b64: &str,
) -> Result<(), String> {
    let actual = sha256_hex(data);
    if !actual.eq_ignore_ascii_case(sha256_expected.trim()) {
        return Err(format!("checksum mismatch (expected {sha256_expected}, got {actual})"));
    }
    let sig_bytes: [u8; 64] = b64()
        .decode(signature_b64.trim())
        .map_err(|e| format!("bad signature encoding: {e}"))?
        .try_into()
        .map_err(|_| "signature must be 64 bytes".to_string())?;
    let signature = Signature::from_bytes(&sig_bytes);
    let message = signed_message(platform, version, &actual);
    for key in keys {
        let Ok(bytes) = b64().decode(key.trim()) else { continue };
        let Ok(bytes): Result<[u8; 32], _> = bytes.try_into() else { continue };
        let Ok(vk) = VerifyingKey::from_bytes(&bytes) else { continue };
        if vk.verify(message.as_bytes(), &signature).is_ok() {
            return Ok(());
        }
    }
    Err("signature is not valid for any trusted key".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> Version {
        s.parse().unwrap()
    }

    #[test]
    fn versions() {
        assert!(v("0.2.0") > v("0.1.9"));
        assert!(v("1.0.0") > v("0.99.99"));
        assert!(v("v0.2.0") == v("0.2.0"));
        assert!(v("0.2.0") > v("0.2.0-beta.2"));
        assert!(v("0.2.0-beta.10") > v("0.2.0-beta.2"));
        assert!(v("0.2.0-rc.1") > v("0.2.0-beta.9"));
        assert!(v("0.2.0+build.5") == v("0.2.0"));
        assert_eq!(v("1.2.3-alpha").to_string(), "1.2.3-alpha");
        assert!("1.2".parse::<Version>().is_err());
        assert!("1.2.3.4".parse::<Version>().is_err());
        assert!("x.y.z".parse::<Version>().is_err());
    }

    // Test-only key (seed of 32 x 7).
    const SEED: &str = "BwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwc=";

    #[test]
    fn sign_and_verify() {
        let data = b"installer bytes";
        let sha = sha256_hex(data);
        let sig = sign(SEED, "windows-x86_64", "0.2.0", &sha).unwrap();
        let public = public_key_for(SEED).unwrap();
        let keys = [public.as_str()];
        verify(&keys, "windows-x86_64", "0.2.0", data, &sha, &sig).unwrap();
        // Tampered file.
        assert!(verify(&keys, "windows-x86_64", "0.2.0", b"evil", &sha, &sig).unwrap_err().contains("checksum"));
        // Relabeled as another version or platform.
        assert!(verify(&keys, "windows-x86_64", "9.9.9", data, &sha, &sig).is_err());
        assert!(verify(&keys, "macos-arm64", "0.2.0", data, &sha, &sig).is_err());
        // Untrusted key.
        let other = public_key_for("CAgICAgICAgICAgICAgICAgICAgICAgICAgICAgICAg=").unwrap();
        assert!(verify(&[other.as_str()], "windows-x86_64", "0.2.0", data, &sha, &sig).is_err());
        // Key rotation: any trusted key works.
        verify(&[other.as_str(), public.as_str()], "windows-x86_64", "0.2.0", data, &sha, &sig).unwrap();
    }

    #[test]
    fn manifest_json() {
        let json = r#"{
            "version": "0.2.0",
            "notes": "hi",
            "platforms": {
                "macos-arm64": {"url": "https://x/a.tar.gz", "sha256": "ab", "signature": "c2ln", "kind": "app-tarball"}
            }
        }"#;
        let m: Manifest = serde_json::from_str(json).unwrap();
        assert_eq!(m.platforms["macos-arm64"].kind, "app-tarball");
        assert_eq!(m.pub_date, "");
    }

    #[test]
    fn platform_name_is_known() {
        assert_ne!(current_platform(), "unsupported");
    }
}
