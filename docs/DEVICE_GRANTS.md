# Device grants (K3)

What a paired device may do on the host is decided by a set of **grants**
stored in the device's record in `<data_dir>/devices.json` — not by the
permissions inside its JWT. Grants are re-read on every HTTP request and every
`KernelCommand`, so a change (or `vyn device revoke`) applies to an already
open connection on its next request. No re-pair is needed.

## Grants

| Grant | Allows |
|---|---|
| `plugins.view` | list plugins, plugin details, manifests |
| `plugins.logs` | read plugin logs |
| `plugins.control` | start / stop / restart plugins |
| `plugins.install` | reserved — the install API is a separate change, no route uses it yet |
| `devices.manage` | list connected devices, mint pairing tickets |

## Presets

| Preset | Grants |
|---|---|
| `guest` | none — the device can only talk to the agent |
| `user` | `plugins.view`, `plugins.logs`, `plugins.control` |
| `owner` | all five |

Devices paired before K3 have no `grants` field and are treated as `user`.
A grant string in `devices.json` that is not one of the five is ignored with a
warning; the rest of the record keeps working.

## Route / command to grant

| HTTP route | Grant for a device caller | Non-device caller (plugin, local CLI) |
|---|---|---|
| `GET /plugins`, `GET /plugins/{id}` | `plugins.view` | any valid JWT (unchanged) |
| `GET /plugins/{id}/logs` | `plugins.logs` | any valid JWT (unchanged) |
| `POST /plugins/{id}/start\|stop\|restart` | `plugins.control` | `PERMISSION_KERNEL_ADMIN` (unchanged) |
| `POST /devices/pair` | `devices.manage` | `PERMISSION_KERNEL_ADMIN` (unchanged) |
| `GET /devices` | `devices.manage` | any valid JWT (unchanged) |
| `/health`, `/metrics`, `POST /devices/consume` | unchanged | unchanged |

| KernelCommand (IPC / WS) | Grant for a device connection |
|---|---|
| `health_check` | none |
| `list_plugins`, `get_manifest` | `plugins.view` |
| `list_devices` | `devices.manage` |
| `reload_config` and any other command | all grants (owner) |

A "device caller" is a JWT whose `sub` (HTTP) or registered `device_id` (IPC/WS)
has a row in the device store. Anything else keeps the pre-K3 rules.

## Decisions

1. A device is judged **only** by its record's grants. `PERMISSION_KERNEL_ADMIN`
   in a device's JWT confers nothing on the host, otherwise revoking a grant
   could be bypassed with an old token. A revoked or expired device has no
   grants at all.
2. A new device defaults to the `user` preset (same as the migration rule);
   `guest` is opt-in.
3. `GET /devices` needs `devices.manage` because it reveals other devices.
4. Grants are managed only by the local CLI on the host (`vyn device grants`);
   there is no HTTP route for it.
5. A ticket minted by a device cannot carry a preset above the caller's own
   grants (`403 preset exceeds caller grants`). A device caller that gives no
   preset is checked as `user`.

## CLI

```
vyn device connect --preset guest|user|owner      # default user; re-pair keeps grants
vyn device pair    --preset guest|user|owner      # preset of the device minted from the ticket
vyn device grants <id>                            # show
vyn device grants <id> --preset owner             # replace with a preset
vyn device grants <id> --add plugins.install,devices.manage --remove plugins.control
vyn device list                                   # GRANTS column
```
