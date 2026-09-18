# ADR-0002: Auto-updates via GitHub Releases

- **Status:** Accepted
- **Date:** 2026-09-18

## Context

Ducky ships through GitHub Actions (`tauri-apps/tauri-action` on every `v*`
tag), but installed copies had no way to learn about or install new releases —
users had to notice a release and manually download the bundle. We want update
detection and installation driven by the same GitHub releases, without running
an update server.

## Decision

Use `tauri-plugin-updater` + `tauri-plugin-process` with the static-JSON flow:
`tauri-action` generates `latest.json` plus `.sig` files and attaches them to
each release; installed apps fetch
`https://github.com/ac5tin/ducky/releases/latest/download/latest.json`,
verify the artifact against the minisign public key baked into
`tauri.conf.json` (`plugins.updater.pubkey`), then install and relaunch.

- The signing keypair was generated with `tauri signer generate`
  (passwordless) at `~/.tauri/ducky.key`. The private key is stored as the
  `TAURI_SIGNING_PRIVATE_KEY` secret on `ac5tin/ducky` (with an empty
  `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`); the public key is committed.
  **Back up the private key.** If it is lost, no future release can be signed
  against the current pubkey; recovery requires shipping one release that
  users must install manually with a rotated key.
- The flow is frontend-driven via the plugins' JS bindings (precedent:
  `plugin-dialog` is also used directly from the UI). Default behavior asks
  before downloading; Settings exposes an auto-download mode, the check
  interval (startup only / 1 / 6 / 24 h, default 6 h), and a manual check
  button (`update_mode`, `update_check_interval_hours` in `AppSettings`).
- Checks are skipped in dev builds (`import.meta.env.DEV`): a dev build has
  no installed app to replace and would race the real distribution channel.

## Platform notes

- **macOS**: builds are ad-hoc signed (`signingIdentity: "-"`). The updater
  verifies its own minisign signature, not Apple code signing, so updates
  work — but Gatekeeper may still ask users to bypass "damaged"/unverified
  app warnings after an update until a Developer ID + notarization is added.
  Distribution is aarch64-only, so Intel Macs see no update at all.
- **Linux**: the updater supports AppImage only. The extra Flatpak artifact
  attached to releases is not updater-aware.
- **Windows**: NSIS installer with `passive` install mode; the installer
  itself restarts the app, so the "restart now" prompt is mostly a
  macOS/Linux concern.

## Consequences and risk

Releases now require the signing secrets to be present, otherwise
`tauri-action` fails before publishing. The pubkey pins the trust chain: any
release signed with a different key is rejected by installed apps. macOS
Intel users and Flatpak installs are outside the auto-update path (manual
download still works). End-to-end updates can only be validated after the
first tagged release that includes `latest.json`.

## Revisit when

- An Apple Developer Program certificate is available (add Developer ID
  signing + notarization, and consider an Intel macOS target).
- Release notes outgrow the current placeholder `releaseBody` (feed the
  changelog into `latest.json` notes).
- Distribution moves off GitHub Releases (the endpoint list in
  `tauri.conf.json` is the only thing to change).

## Out of scope

Flatpak auto-updates, delta updates, update channels (beta/alpha), and a
custom update server.
