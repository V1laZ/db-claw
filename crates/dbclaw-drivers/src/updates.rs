//! Talking to the update server and downloading updates. Nothing is ever
//! applied from here: a download is only handed out after its SHA-256 and
//! Ed25519 signature verified against `dbclaw_core::update::TRUSTED_KEYS`.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context as _, Result, anyhow, bail};
use dbclaw_core::update::{self, Offer};

fn client(timeout: Duration) -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .user_agent(format!("DB Claw/{} ({})", env!("CARGO_PKG_VERSION"), update::current_platform()))
        .timeout(timeout)
        .build()?)
}

/// Ask the update server whether a newer version exists for this platform.
/// `Ok(None)` = up to date.
pub async fn check(base_url: &str, current_version: &str, channel: &str) -> Result<Option<Offer>> {
    let url = format!("{}/v1/check", base_url.trim_end_matches('/'));
    let resp = client(Duration::from_secs(15))?
        .get(&url)
        .query(&[
            ("platform", update::current_platform()),
            ("version", current_version),
            ("channel", channel),
        ])
        .send()
        .await
        .with_context(|| format!("contacting {url}"))?;
    match resp.status().as_u16() {
        204 => Ok(None),
        200 => {
            let offer: Offer = resp.json().await.context("invalid update response")?;
            let newer = offer.version.parse::<update::Version>().map_err(|e| anyhow!(e))?
                > current_version.parse::<update::Version>().map_err(|e| anyhow!(e))?;
            if offer.platform != update::current_platform() || !newer {
                return Ok(None);
            }
            Ok(Some(offer))
        }
        s => bail!("update server answered HTTP {s}"),
    }
}

/// Upper bound for an update download.
const MAX_DOWNLOAD: usize = 512 * 1024 * 1024;

/// Download and verify an update; returns the path of the verified file in a
/// fresh temp folder.
pub async fn download(offer: &Offer) -> Result<PathBuf> {
    // HTTPS only (plain HTTP to this machine is allowed for testing; the
    // signature check below applies either way).
    if !offer.url.starts_with("https://") && !offer.url.starts_with("http://127.0.0.1:") {
        bail!("refusing a non-HTTPS download URL");
    }
    let mut resp = client(Duration::from_secs(30 * 60))?
        .get(&offer.url)
        .send()
        .await
        .context("downloading the update")?
        .error_for_status()?;
    let mut data = Vec::with_capacity(resp.content_length().unwrap_or(0).min(MAX_DOWNLOAD as u64) as usize);
    while let Some(chunk) = resp.chunk().await? {
        data.extend_from_slice(&chunk);
        if data.len() > MAX_DOWNLOAD {
            bail!("update download is unexpectedly large");
        }
    }
    update::verify(update::TRUSTED_KEYS, &offer.platform, &offer.version, &data, &offer.sha256, &offer.signature)
        .map_err(|e| anyhow!("the update failed verification and was discarded: {e}"))?;
    let name = offer.url.rsplit('/').next().filter(|n| !n.is_empty()).unwrap_or("update.bin");
    let name: String = name.chars().filter(|c| c.is_alphanumeric() || "._-".contains(*c)).collect();
    let dir = std::env::temp_dir().join(format!("dbclaw-update-{}-{}", offer.version, std::process::id()));
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(name);
    tokio::fs::write(&path, &data).await?;
    Ok(path)
}
