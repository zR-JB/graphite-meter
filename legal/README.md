# Legal pipeline

## Normal development

No legal action is required beyond the normal fast gate:

```bash
just check
```

## Routine dependency update

```bash
just legal-generate
just ci
```

If the component's complete legal-file fingerprint is unchanged, the existing
review is reusable; generation refreshes its reviewed version and all shipped
legal metadata. A changed license expression, notice, or legal-file byte still
requires maintainer review.

## New dependency or changed legal facts

For a new dependency or changed legal file, generate the maintainer review
template first:

```bash
just legal-review template
```

For an independent deterministic audit manifest of discovered components:

```bash
just legal-review audit
```

The private generator modes and packaging helpers are not part of the normal
developer interface. Use `just --list` for the current public command list.

Inspect the exact upstream revision and its legal files, then add a reviewed
record to `reviewed-components.json`, regenerate, and run CI. New components
are never approved by matching a familiar license template. `legal-check`,
`legal-generate`, and `legal-review` are the public legal interface; packaging
helpers and generator modes are private implementation details.

## Custom, copied, or modified material

Use `provenance.json` for local files, forks, replacements, assets, fonts,
datasets, and other material that package managers cannot describe. Never
pretend custom material is an ordinary MIT dependency to silence the gate.

## Generated files

Do not edit these by hand:

- `COPYRIGHT`
- `legal/generated/**`
- `client/public/legal/**`
- `go/internal/legal/assets/**`

The generator also creates release `SOURCE.txt` material from the same project
metadata and reviewed component set.

## Copyright year

`legal/project.json` is the only place to update Graphite Meter's copyright
year or year range. The generator never derives it from the wall clock.
