[简体中文](README.md) · [繁體中文](README.zh-TW.md) · **English** · [日本語](README.ja.md)

# wivrn-pose-holder

A Linux / WiVRn version of [z3y/driver-pose-holder](https://github.com/z3y/driver-pose-holder): when a controller loses tracking, it is frozen at its last valid pose, so the game does not move your hands away.

The original is a SteamVR driver that hooks `TrackedDevicePoseUpdated`. A WiVRn + xrizer setup has no SteamVR, so this is an **OpenXR API layer** that sits between the application (xrizer) and the runtime (WiVRn). Neither WiVRn nor xrizer is patched, so nothing has to be rebuilt when they update.

```
VRChat → xrizer → [pose_holder] → WiVRn → Quest
```

## What it does

| Hooked function | Behavior |
|---|---|
| `xrLocateSpace` | Controller action spaces only. When position or orientation is invalid, the last valid pose is returned with zero velocity. The first 5 frames after tracking returns still use the old pose, because the Quest reports a wrong pose right after recovery and the hand would snap |
| `xrGetCurrentInteractionProfile` | When a controller goes idle the Quest reports no interaction profile, and xrizer then marks the controller as disconnected. The layer keeps reporting the previous profile, so the controller stays connected |
| `xrGetActionStatePose` | Same idea: the pose action stays active |
| `xrPollEvent` | WiVRn sends an interaction-profile-changed event whenever a controller goes idle or wakes up, and xrizer answers each one by rebuilding every controller and tracker (Rebocap's included). When the profile the application sees has not changed, the event is dropped; real changes, such as switching to hand tracking, still get through |

The headset, Rebocap trackers (xdev spaces) and button input are passed through untouched. Switching from controllers to hand tracking changes the profile to another valid one, which is passed through as well.

## Install

Download `wivrn-pose-holder-v*-x86_64.tar.gz` from Releases (prebuilt, no Rust needed), extract it and run:

```sh
./install.sh
```

To install from source (needs Rust), clone this repo and run the same `./install.sh` in it; the script builds the library first.

The script installs two files:

```
~/.local/share/wivrn-pose-holder/libxr_pose_holder.so
~/.local/share/openxr/1/api_layers/implicit.d/XrApiLayer_pose_holder.json
```

This is an implicit layer: every OpenXR program this user starts loads it, including xrizer inside Proton (the home directory is visible in the container). Restart VRChat after installing.

Uninstall: `./install.sh --uninstall`

## Settings

All settings are environment variables. To set one for VRChat, put it at the start of the Steam launch options.

| Variable | Default | Meaning |
|---|---|---|
| `POSE_HOLDER_MIN_VALID_FRAMES` | `5` | After tracking returns, the new pose is used only after this many consecutive valid frames. Raise it if the hand still snaps on recovery |
| `POSE_HOLDER_MAX_HOLD_SECS` | `0` | Longest time to hold, in seconds; after that the controller is released. `0` holds forever, like the original |
| `POSE_HOLDER_HOLD_CONNECTION` | `1` | Set to `0` to hold the pose only and leave the controller's connection state alone |
| `DISABLE_XR_APILAYER_POSE_HOLDER` | | Set to any value to keep the layer from loading |

## Checking that it is loaded

The layer writes to stderr:

```
[pose_holder] active (min_valid_frames=5, max_hold=forever, hold_connection=true)
[pose_holder] tracking lost on action 0x… path 0x…, holding last pose
[pose_holder] tracking recovered on action 0x… path 0x…
```

To capture VRChat's stderr, use the launch option `PROTON_LOG=1 %command%`; it goes to `~/steam-438100.log`. You can also run any OpenXR program with `XR_LOADER_DEBUG=info` and look for `succeeded loading layer XR_APILAYER_TW527E_pose_holder`.

## Tests

```sh
cargo test
```

`tests/mock_runtime.rs` drives the layer the way the OpenXR loader does, against a fake runtime (negotiate → create instance → call the hooked functions). No headset is needed.

## Releasing

Change `version` in `Cargo.toml` and push to `master` or `main`. GitHub Actions runs the tests, builds, creates the `v<version>` tag and a Release with the archive attached; the release notes list the commits since the previous Release. Nothing is published when the version is unchanged or the tag already exists.

## Limitations

- Only `xrLocateSpace` is hooked. xrizer 0.6 uses nothing else; a program that uses the OpenXR 1.1 batch call `xrLocateSpaces` is not held.
- The layer relies on the valid flags the runtime reports. If the Quest reports a pose as valid when it is only an estimate, the layer cannot tell.
- When a controller is really switched off, the hand stays where it was until `POSE_HOLDER_MAX_HOLD_SECS` runs out (never, by default).
- With `POSE_HOLDER_MAX_HOLD_SECS` set, the layer sends its own profile-changed event when the hold runs out, so the application learns the controller is gone (up to 0.25 s late).

## License

MIT. The approach comes from z3y's driver-pose-holder (MIT); the code is written from scratch.
