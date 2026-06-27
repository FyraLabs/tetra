# Tetra

Tetra is becoming a host agent for a web control plane. The agent is organized
around one dispatcher with independent modules for settings, services, files,
and recipes. The intended production transport is an outbound WSS connection
with mTLS and signed command envelopes.

Recipes stay in YAML, but Quadlet generation now uses Tera templates. A recipe
declares metadata, UI parameters, requirements, and a list of resources to
render.

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

The local dispatcher accepts the same command envelope shape that the transport
will receive from the web UI:

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
cargo run -- agent-dispatch command.json
```

## Legacy Podlet Mode

The original single-container Podlet adapter is still available:

```sh
cargo run -- podlet recipe.yaml userconf.yaml --install --output-dir ./quadlets
```
