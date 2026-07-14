# Tetra

Tetra is a modular host agent and recipe renderer for a web control plane. The
agent is organized around one dispatcher with independent feature-gated modules
for settings, services, files, recipes, SELinux, storage, Samba, NFS,
networking, Podman, Quadlets, users, and virtual machines.

The intended production transport is an outbound WSS connection with mTLS, or a
virtio-vsock stream when the agent is running inside a VM. Both transports carry
the same signed command envelopes. The local `agent-dispatch` command accepts
the same command envelope shape that the transport will receive from the web UI.
See [docs/agent-protocol.md](docs/agent-protocol.md) for the controller-facing
API, connection negotiation, and agent protocol reference.

Recipes stay in YAML, and Quadlet generation uses Tera templates. A recipe
declares metadata, UI parameters, requirements, and a list of resources to
render.

## Feature Flags

Default build features include `files`, `recipes`, `selinux`, `services`, and
`quadlets`. Other host-management surfaces are optional so users can install
only what they want enabled:

- `storage`
- `samba`
- `nfs`
- `network`
- `podman`
- `users`
- `virtual-machines`

Example with every module compiled in:

```sh
cargo build --all-features
```

Example with only recipe rendering and Quadlet management:

```sh
cargo build --no-default-features --features recipes,quadlets
```

## Render Quadlets

```sh
cargo run -- render schema.yaml --templates-dir ./templates --output-dir ./quadlets
```

To preview rendered files without writing them:

```sh
cargo run -- render schema.yaml --templates-dir ./templates --dry-run
```

Parameter values can be supplied as a simple YAML map:

```yaml
domain: cloud.example.com
enable_redis: true
```

Then pass it with:

```sh
cargo run -- render schema.yaml --values values.yaml --templates-dir ./templates --output-dir ./quadlets
```

## Agent Commands

The local dispatcher accepts commands like:

```json
{
  "id": "cmd-1",
  "module": "settings",
  "action": "get_system",
  "payload": {}
}
```

Run it locally with:

```sh
cargo run -- agent-dispatch examples/settings.command.json
```

For browser-based hardware testing on a private network, run the development
HTTP agent API on the host:

```sh
cargo run --all-features -- agent-serve \
  --listen 100.x.y.z:7777 \
  --bearer-token "$TETRA_AGENT_TOKEN"
```

Use the server's Tailscale IP or MagicDNS name for `100.x.y.z`. Then open
`ui/agent-test.html` on your workstation and set the agent URL to
`http://100.x.y.z:7777`. The test UI can check health, load capabilities, and
submit command envelopes to `/dispatch`.

The test UI includes a Demo Quadlet panel that writes `tetra-demo.container`,
runs `systemctl daemon-reload`, starts `tetra-demo.service`, and shows
`journalctl` output for the unit. Use `System` scope with a root-run agent for
`/etc/containers/systemd`, or `User` scope with an agent running as your login
user for `~/.config/containers/systemd`.

The HTTP API is a development harness around the same Kameo-backed agent
backend. Keep it on Tailscale or localhost, use a bearer token, and prefer
`"dry_run": true` before testing mutating host actions.

To let the web UI discover enabled modules and actions, call dispatcher-level
capabilities:

```json
{
  "id": "cmd-capabilities",
  "module": "agent",
  "action": "capabilities",
  "payload": {}
}
```

Each module also supports its own `capabilities` action.

Mutating host actions execute by default and accept `"dry_run": true` in their
payload to return the command or write plan without changing the host. Fedora
and SELinux-oriented installs can use the `selinux` module to inspect SELinux
status, list and set booleans, manage file-context rules with `semanage
fcontext`, and run `restorecon` after managed file changes.

Modules that create or manage paths also accept a shared `selinux` payload to
apply file-context rules and relabel paths as part of the same action. For
example, a Samba share path can be labeled while updating generated config:

```json
{
  "id": "cmd-samba-config",
  "module": "samba",
  "action": "set_config",
  "payload": {
    "contents": "[media]\npath = /srv/media\n",
    "dry_run": true,
    "selinux": {
      "path": "/srv/media",
      "context_type": "samba_share_t",
      "recursive": true
    }
  }
}
```
