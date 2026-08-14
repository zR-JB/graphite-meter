# Legal pipeline

## Normal development

No legal action is required for ordinary source changes. The offline CI gate
runs:

```bash
just legal-check
```

## Routine dependency update

```bash
update dependency
just legal-generate
just ci
```

If the component's complete legal-file fingerprint is unchanged, the existing
review is reusable and only the generated version changes.

## New dependency or changed legal files

```bash
just legal-review-template
```

Inspect the exact upstream revision and its legal files, then add a reviewed
record to `reviewed-components.json`, regenerate, and run CI. New components
are never approved by matching a familiar license template.

## Custom, copied, or modified material

Use `provenance.json` for local files, forks, replacements, assets, fonts,
datasets, and other material that package managers cannot describe. Never
pretend custom material is an ordinary MIT dependency to silence the gate.

## Generated files

Do not edit these by hand:

- `COPYRIGHT`
- `legal/generated/**`
- `client/public/legal/**`

The generator also creates release `SOURCE.txt` material from the same project
metadata and reviewed component set.

## Copyright year

`legal/project.json` is the only place to update Graphite Meter's copyright
year or year range. The generator never derives it from the wall clock.
