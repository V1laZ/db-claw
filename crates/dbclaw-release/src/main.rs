//! Release tooling for DB Claw updates.
//!
//! ```text
//! dbclaw-release keygen
//!     New Ed25519 key: prints the secret (keep it in CI secrets / a password
//!     manager) and the public key (goes into dbclaw_core::update::TRUSTED_KEYS).
//! dbclaw-release manifest --version 0.2.0 --notes-file notes.md \
//!     --base-url https://github.com/OWNER/REPO/releases/download/v0.2.0 \
//!     --artifact windows-x86_64=installer=dist/DB-Claw-0.2.0-setup.exe ... > latest.json
//!     Hashes and signs each artifact (secret from $UPDATE_SIGNING_KEY).
//! dbclaw-release verify --version 0.2.0 --platform macos-arm64 --sha256 H --signature S FILE
//! ```

use std::collections::BTreeMap;

use anyhow::{Context as _, Result, anyhow, bail};
use base64::Engine as _;
use dbclaw_core::update::{self, Artifact, Manifest, TRUSTED_KEYS};

fn main() {
    if let Err(e) = run() {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}

fn flag(args: &[String], name: &str) -> Option<String> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1).cloned())
}

fn run() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("keygen") => {
            let mut seed = [0u8; 32];
            getrandom::fill(&mut seed).map_err(|e| anyhow!("no randomness: {e}"))?;
            let secret = base64::engine::general_purpose::STANDARD.encode(seed);
            let public = update::public_key_for(&secret).map_err(|e| anyhow!(e))?;
            println!("secret: {secret}");
            println!("public: {public}");
            Ok(())
        }
        Some("manifest") => {
            let version = flag(&args, "--version").context("--version is required")?;
            let base_url = flag(&args, "--base-url").context("--base-url is required")?;
            let notes = match flag(&args, "--notes-file") {
                Some(p) => std::fs::read_to_string(&p).with_context(|| format!("reading {p}"))?,
                None => String::new(),
            };
            let secret = std::env::var("UPDATE_SIGNING_KEY").context("UPDATE_SIGNING_KEY is not set")?;
            let public = update::public_key_for(&secret).map_err(|e| anyhow!(e))?;
            if !TRUSTED_KEYS.contains(&public.as_str()) {
                bail!("the signing key's public key {public} is not in TRUSTED_KEYS: apps would reject this release");
            }
            let mut platforms = BTreeMap::new();
            for (i, a) in args.iter().enumerate() {
                if a != "--artifact" {
                    continue;
                }
                let spec = args.get(i + 1).context("--artifact needs PLATFORM=KIND=PATH")?;
                let mut parts = spec.splitn(3, '=');
                let (platform, kind, path) = match (parts.next(), parts.next(), parts.next()) {
                    (Some(p), Some(k), Some(f)) => (p, k, f),
                    _ => bail!("--artifact needs PLATFORM=KIND=PATH, got {spec}"),
                };
                let data = std::fs::read(path).with_context(|| format!("reading {path}"))?;
                let sha256 = update::sha256_hex(&data);
                let signature = update::sign(&secret, platform, &version, &sha256).map_err(|e| anyhow!(e))?;
                // Self-check with the same code the app runs.
                update::verify(&[public.as_str()], platform, &version, &data, &sha256, &signature)
                    .map_err(|e| anyhow!(e))?;
                let file = std::path::Path::new(path).file_name().unwrap().to_string_lossy();
                platforms.insert(
                    platform.to_string(),
                    Artifact {
                        url: format!("{}/{}", base_url.trim_end_matches('/'), file),
                        sha256,
                        signature,
                        kind: kind.to_string(),
                    },
                );
            }
            if platforms.is_empty() {
                bail!("no --artifact given");
            }
            let manifest = Manifest {
                version: version.clone(),
                pub_date: flag(&args, "--date").unwrap_or_default(),
                notes,
                notes_url: flag(&args, "--notes-url").unwrap_or_default(),
                platforms,
            };
            println!("{}", serde_json::to_string_pretty(&manifest)?);
            Ok(())
        }
        Some("verify") => {
            let file = args.last().context("file missing")?;
            let data = std::fs::read(file)?;
            update::verify(
                TRUSTED_KEYS,
                &flag(&args, "--platform").context("--platform")?,
                &flag(&args, "--version").context("--version")?,
                &data,
                &flag(&args, "--sha256").context("--sha256")?,
                &flag(&args, "--signature").context("--signature")?,
            )
            .map_err(|e| anyhow!(e))?;
            println!("ok");
            Ok(())
        }
        _ => {
            eprintln!("usage: dbclaw-release keygen | manifest … | verify …");
            std::process::exit(2);
        }
    }
}
