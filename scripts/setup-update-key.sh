#!/usr/bin/env bash
# One-time setup of the update signing key.
#
#  1. Generates a new Ed25519 key with `dbclaw-release keygen`.
#  2. Stores the SECRET as the GitHub Actions secret UPDATE_SIGNING_KEY
#     (piped via stdin, never printed) and as a backup in
#     ~/.config/dbclaw/update-signing.key (mode 600). Move that backup into a
#     password manager: losing it means installed apps can't be updated
#     until they're reinstalled with a new key.
#  3. Writes the PUBLIC key into crates/dbclaw-core/src/update.rs
#     (TRUSTED_KEYS), so the app accepts updates signed with it.
#
# Usage: scripts/setup-update-key.sh [owner/repo]   (default: V1laZ/db-claw)
set -euo pipefail
repo=${1:-V1laZ/db-claw}
cd "$(dirname "$0")/.."

key_file="$HOME/.config/dbclaw/update-signing.key"
if [[ -e "$key_file" ]]; then
  echo "error: $key_file already exists; not overwriting an existing key." >&2
  exit 1
fi

cargo build -q -p dbclaw-release
out=$(./target/debug/dbclaw-release keygen)
secret=$(printf '%s\n' "$out" | sed -n 's/^secret: //p')
public=$(printf '%s\n' "$out" | sed -n 's/^public: //p')
unset out

umask 077
mkdir -p "$(dirname "$key_file")"
printf '%s\n' "$secret" > "$key_file"
printf '%s' "$secret" | gh secret set UPDATE_SIGNING_KEY --repo "$repo"
unset secret

sed -i.bak "s#pub const TRUSTED_KEYS: &\[&str\] = &\[\"[^\"]*\"\];#pub const TRUSTED_KEYS: \&[\&str] = \&[\"$public\"];#" \
  crates/dbclaw-core/src/update.rs
rm -f crates/dbclaw-core/src/update.rs.bak

echo "Public key:   $public"
echo "Secret saved: GitHub secret UPDATE_SIGNING_KEY on $repo, backup in $key_file"
echo "Next: move the backup into a password manager."
