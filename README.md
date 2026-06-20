# Tetra

Tetra is a recipe/user-configuration layer for generating Podman Quadlets.

Instead of implementing Quadlet serialization itself, Tetra converts the merged recipe into a
`podlet podman run ...` invocation and delegates Quadlet generation to
[containers/podlet](https://github.com/containers/podlet).

## Usage

Install `podlet` first, then run:

```sh
cargo run -- recipe.yaml userconf.yaml --install --output-dir ./quadlets
```

To see the exact Podlet command without executing it:

```sh
cargo run -- recipe.yaml userconf.yaml --dry-run
```

The user config YAML is recursively merged over the recipe YAML before generating the Podlet command.
