#!/bin/sh
# Installs the Proofkin binary (`proofkin`) from GitHub releases (no source code or Rust needed).
#   curl -fsSL https://github.com/mcbelton/p2pagents/releases/latest/download/install.sh | sh
# Options (env vars): PROOFKIN_VERSION (default latest), PROOFKIN_REPO, PROOFKIN_INSTALL_DIR,
#   PROOFKIN_GH_USER (GitHub CLI account to download with; default: gh's active account).
#   The P2PAGENTS_* names from before the rename work too.
set -eu

repo="${PROOFKIN_REPO:-${P2PAGENTS_REPO:-mcbelton/p2pagents}}"
version="${PROOFKIN_VERSION:-${P2PAGENTS_VERSION:-latest}}"
dir="${PROOFKIN_INSTALL_DIR:-${P2PAGENTS_INSTALL_DIR:-$HOME/.local/bin}}"
gh_user="${PROOFKIN_GH_USER:-${P2PAGENTS_GH_USER:-}}"

case "$(uname -s)-$(uname -m)" in
  Linux-x86_64) target=x86_64-unknown-linux-gnu ;;
  Linux-aarch64|Linux-arm64) target=aarch64-unknown-linux-gnu ;;
  Darwin-arm64) target=aarch64-apple-darwin ;;
  *) echo "no prebuilt binary for $(uname -s)-$(uname -m); build from source (see README)" >&2; exit 1 ;;
esac
asset="proofkin-$target.tar.gz"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

if [ "$version" = latest ]; then
  url="https://github.com/$repo/releases/latest/download/$asset"
else
  url="https://github.com/$repo/releases/download/$version/$asset"
fi

# Public repos: plain HTTPS. Private repos (or PROOFKIN_GH_USER): the GitHub CLI.
if [ -z "$gh_user" ] && curl -fsSL "$url" -o "$tmp/$asset" 2>/dev/null; then
  :
elif command -v gh >/dev/null 2>&1; then
  if [ -n "$gh_user" ]; then
    GH_TOKEN="$(gh auth token --hostname github.com --user "$gh_user")"
    export GH_TOKEN
  fi
  tag=""
  [ "$version" = latest ] || tag="$version"
  # shellcheck disable=SC2086
  if ! gh release download $tag -R "$repo" -p "$asset" -D "$tmp"; then
    me="$(gh api user --jq .login 2>/dev/null || echo unknown)"
    echo "Could not download from $repo as GitHub user '$me'. A private repo looks 'not found' to" >&2
    echo "accounts without access. Use one that can read it: PROOFKIN_GH_USER=<login> sh install.sh" >&2
    echo "(see gh auth status), or ask the owner to add '$me' as a collaborator." >&2
    exit 1
  fi
else
  echo "Could not download $url; for a private repo install the GitHub CLI and sign in." >&2
  exit 1
fi

mkdir -p "$dir"
tar -xzf "$tmp/$asset" -C "$tmp"
install -m 755 "$tmp/proofkin" "$dir/proofkin"
echo "Installed: $("$dir/proofkin" --version) -> $dir/proofkin"
case ":$PATH:" in *":$dir:"*) ;; *) echo "Add $dir to your PATH." ;; esac
if [ -e "$dir/p2pagents" ]; then
  echo "p2pagents is now Proofkin: use proofkin (same profiles and data). You can delete $dir/p2pagents."
fi
cat <<'EOF'
Next steps:
  install the Copilot CLI: npm install -g @github/copilot   (each agent runs its own instance)
  optional: GitHub CLI (gh auth login) - easiest way to sign agents in to GitHub accounts
  proofkin setup        # optional: Claude/Codex support (needs Node.js 18+)
  proofkin doctor       # check everything
  proofkin auth login copilot --profile <me> --gh-user <github-login>
  proofkin peers id --profile <me>    # share your ticket with peers
EOF
