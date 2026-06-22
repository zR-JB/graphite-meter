# Podman Quadlet units

Run Graphite Meter as a systemd-managed Podman service. Quadlet turns these
`.build` / `.container` files into generated systemd services.

- `graphite-meter.build` — builds `localhost/graphite-meter:latest` from
  `../Dockerfile` (context = repo root, `Arch=arm64`).
- `graphite-meter.container` — runs the server on `:8080`; references the build
  unit via `Image=graphite-meter.build`, so it builds first, then runs.

Requires **Podman 5.0+** (`.build` unit support).

## 1. Edit the build unit

Quadlet units must live in a systemd search directory, **not** in the repo, so
relative paths to the checkout don't work. Set the context to your checkout's
absolute path in `graphite-meter.build`:

```ini
SetWorkingDirectory=/home/youruser/source/graphite-meter
```

(Keep `File=docker/Dockerfile` — it's relative to that context.)

## 2. Install the units

**Rootless (recommended):**

```sh
mkdir -p ~/.config/containers/systemd
cp graphite-meter.build graphite-meter.container ~/.config/containers/systemd/
# allow the service to run without an active login session (start at boot):
loginctl enable-linger "$USER"
systemctl --user daemon-reload
systemctl --user start graphite-meter.service
```

**Rootful:** copy to `/etc/containers/systemd/`, then
`sudo systemctl daemon-reload && sudo systemctl start graphite-meter.service`.

> The unit is named after the file: `graphite-meter.container` →
> `graphite-meter.service`. The build runs as `graphite-meter-build.service`.

## 3. Operate

```sh
systemctl --user status graphite-meter.service     # state
journalctl --user -u graphite-meter.service -f     # logs
systemctl --user restart graphite-meter.service    # rebuilds only if the .build changed
```

Then open <http://localhost:8080>.

## Notes

- **First build is slow** on a Pi (bun install + the Go build); later starts
  reuse the built image.
- To rebuild from scratch: `podman rmi localhost/graphite-meter:latest` then
  restart the service.
- Override client build knobs (dummy runner / dev tools / label) by uncommenting
  the `BuildArg=` lines in `graphite-meter.build`.
