Fix: connecting to MongoDB servers that use a company certificate.

## Fixes

- **MongoDB over TLS**: DB Claw now trusts the certificate authorities installed on your computer, not just the public ones. Servers with a company or self-hosted CA (`ssl=true` / `tls=true`, including `mongodb+srv://`) failed with *invalid peer certificate: UnknownIssuer*; they connect now. A `tlsCAFile` in the connection URL still takes precedence.

## Downloads

| Platform | File |
|---|---|
| macOS (Apple Silicon) | `db-claw-v…-macos-arm64.dmg`: open it and drag **DB Claw** to Applications |
| Windows x86_64 | `db-claw-v…-windows-x86_64-setup.exe` |
| Linux x86_64 | `db-claw-v…-linux-x86_64.AppImage`: `chmod +x` and run |

Already on 0.2.x? DB Claw updates itself: you'll get a **Restart to update** notification within a day, or use **DB Claw → Check for Updates…** right away.

## Notes

- **macOS**: the app isn't notarized, so the first launch of a downloaded copy shows *"DB Claw" Not Opened*. Click **Done**, open **System Settings → Privacy & Security**, click **Open Anyway** next to the DB Claw message and confirm. Or run `xattr -dr com.apple.quarantine "/Applications/DB Claw.app"`. Updates installed from inside the app don't ask again.
- **Windows**: SmartScreen may warn about an unknown publisher → *More info* → *Run anyway*.
- **Linux**: needs a Vulkan-capable GPU driver, plus `libxkbcommon`, `fontconfig` and `wayland`/`X11` libraries (present on typical desktops).
