## What and why

<!-- What does this change, and why is it needed? Link the issue: "Fixes #123". -->

## How it was tested

<!-- Automated checks plus hardware testing. Delete lines that don't apply. -->

- [ ] `cargo fmt --all -- --check`
- [ ] `cargo clippy --locked --all-targets -- -D warnings`
- [ ] `cargo test --locked`
- [ ] `cargo build --locked --release`
- [ ] Tested on hardware — Mac and macOS version: … Phone and Android version: …
      (not possible without the phone: say so here and describe what was verified instead)
- [ ] Nothing is left behind after stopping: `ifconfig -l | grep feth` is empty and `networksetup -listnetworkserviceorder | grep -c "USB Android Tethering"` matches the expected state
- [ ] Performance-sensitive change: `scripts/bench.sh` results before and after are included below

## Checklist

- [ ] [ARCHITECTURE.md](https://github.com/frosty-universe/usb-android-tethering/blob/main/docs/ARCHITECTURE.md) invariants still hold (teardown order, transfer ownership, service lifecycle)
- [ ] User-visible change recorded in `CHANGELOG.md` under **Unreleased**
- [ ] Docs updated (README / docs/) if behaviour or commands changed
- [ ] No new dependencies, or the reason is explained above
