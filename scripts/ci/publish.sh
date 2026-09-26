#!/usr/bin/env bash
# Registry writes of the approved ghcr-release job; release.py publish owns the GitHub Release.
set -euo pipefail

STABLE='(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)'

fail() { echo "$*" >&2; exit 1; }

# Run a Skopeo shell script logged in to GHCR; the token never enters argv.
registry() {
  local script=$1
  shift
  docker run --rm --entrypoint /bin/sh -e REGISTRY_TOKEN -e REGISTRY_ACTOR \
    -e IMAGE="ghcr.io/${REPOSITORY,,}" -e DIGEST "$@" "$SKOPEO_IMAGE" -ec '
      auth_dir=$(mktemp -d)
      trap "skopeo logout ghcr.io >/dev/null 2>&1 || true; rm -rf $auth_dir" EXIT
      export REGISTRY_AUTH_FILE="$auth_dir/auth.json"
      printf "%s" "$REGISTRY_TOKEN" | skopeo login ghcr.io --username "$REGISTRY_ACTOR" --password-stdin
      unset REGISTRY_TOKEN
    '"$script"
}

publish_image() {
  [[ "$IMAGE_TAG" =~ ^$STABLE(-(alpha|beta|rc)\.(0|[1-9][0-9]*))?$ ]] || fail "invalid image tag: $IMAGE_TAG"
  [[ "$DIGEST" =~ ^sha256:[0-9a-f]{64}$ ]] || fail "invalid verified digest"
  registry '
    archive=oci-archive:/work/graphite-meter.oci.tar
    [ "$(skopeo inspect --format "{{.Digest}}" "$archive")" = "$DIGEST" ] || {
      echo "archive is not the verified $DIGEST" >&2
      exit 1
    }
    if current=$(skopeo inspect --format "{{.Digest}}" "docker://$IMAGE:$IMAGE_TAG" 2>"$auth_dir/lookup"); then
      [ "$current" = "$DIGEST" ] || { echo "$IMAGE:$IMAGE_TAG already exists at $current" >&2; exit 1; }
      echo "$IMAGE:$IMAGE_TAG already points at $DIGEST"
      exit 0
    fi
    grep -Eiq "manifest unknown|name unknown|not found|404" "$auth_dir/lookup" || {
      cat "$auth_dir/lookup" >&2
      echo "registry lookup failed; refusing to publish" >&2
      exit 1
    }
    skopeo copy --all --preserve-digests "$archive" "docker://$IMAGE:$IMAGE_TAG"
    [ "$(skopeo inspect --format "{{.Digest}}" "docker://$IMAGE:$IMAGE_TAG")" = "$DIGEST" ] || {
      echo "published $IMAGE:$IMAGE_TAG does not match $DIGEST" >&2
      exit 1
    }
    echo "published $IMAGE:$IMAGE_TAG @ $DIGEST"
  ' -e IMAGE_TAG -v "$ARCHIVE_DIR:/work:ro"
}

promote_aliases() {
  [[ "$DIGEST" =~ ^sha256:[0-9a-f]{64}$ ]] || fail "invalid verified digest"
  [[ "$VERSION" =~ ^$STABLE$ ]] || fail "invalid stable version: $VERSION"
  local series=${VERSION%.*} tags
  tags=$(gh api --paginate "repos/$REPOSITORY/releases?per_page=100" \
    --jq '.[] | select(.draft == false and .prerelease == false) | .tag_name' \
    | { grep -E "^v$STABLE\$" || true; } | sort -V)
  grep -qx "v$VERSION" <<<"$tags" || fail "v$VERSION is not a published stable release yet"
  # Aliases follow the highest published releases, so any later run repairs a cancelled one.
  registry '
    [ "$(skopeo inspect --format "{{.Digest}}" "docker://$IMAGE:$VERSION")" = "$DIGEST" ] || {
      echo "$VERSION is not the verified $DIGEST" >&2
      exit 1
    }
    promote() {
      digest=$(skopeo inspect --format "{{.Digest}}" "docker://$IMAGE:$2")
      case $digest in sha256:*) ;; *) echo "$2 has no digest" >&2; exit 1 ;; esac
      skopeo copy --all --preserve-digests "docker://$IMAGE@$digest" "docker://$IMAGE:$1"
      [ "$(skopeo inspect --format "{{.Digest}}" "docker://$IMAGE:$1")" = "$digest" ] || {
        echo "$1 does not match $2 at $digest" >&2
        exit 1
      }
      echo "promoted $IMAGE:$1 -> $2 @ $digest"
    }
    promote "$SERIES" "$SERIES_TARGET"
    promote latest "$LATEST_TARGET"
  ' -e VERSION -e SERIES="$series" \
    -e SERIES_TARGET="$(grep -E "^v${series//./\\.}\\.[0-9]+\$" <<<"$tags" | tail -n1 | cut -c2-)" \
    -e LATEST_TARGET="$(tail -n1 <<<"$tags" | cut -c2-)"
}

case ${1-} in
  image) publish_image ;;
  aliases) promote_aliases ;;
  *) fail "usage: publish.sh image|aliases" ;;
esac
