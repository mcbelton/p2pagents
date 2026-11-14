#!/bin/sh
# Installs the p2pagents binary from GitHub releases (no source code or Rust needed).
#   private repo (needs read access, uses your GitHub CLI sign-in):
#     gh release download -R mcbelton/p2pagents -p install.sh -O - | sh
#   public repo:
#     curl -fsSL https://github.com/mcbelton/p2pagents/releases/latest/download/install.sh | sh
# Options (env vars): P2PAGENTS_VERSION (default latest), P2PAGENTS_REPO, P2PAGENTS_INSTALL_DIR,
#   P2PAGENTS_GH_USER (GitHub CLI account to download with; default: gh's active account).
set -eu

repo="${P2PAGENTS_REPO:-mcbelton/p2pagents}"
version="${P2PAGENTS_VERSION:-latest}"
dir="${P2PAGENTS_INSTALL_DIR:-$HOME/.local/bin}"
gh_user="${P2PAGENTS_GH_USER:-}"

case "$(uname -s)-$(uname -m)" in
  Linux-x86_64) target=x86_64-unknown-linux-gnu ;;
  Darwin-arm64) target=aarch64-apple-darwin ;;
  *) echo "no prebuilt binary for $(uname -s)-$(uname -m); build from source (see README)" >&2; exit 1 ;;
esac
asset="p2pagents-$target.tar.gz"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

if command -v gh >/dev/null 2>&1; then
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
    echo "accounts without access. Use one that can read it: P2PAGENTS_GH_USER=<login> sh install.sh" >&2
    echo "(see gh auth status), or ask the owner to add '$me' as a collaborator." >&2
    exit 1
  fi
else
  if [ "$version" = latest ]; then
    url="https://github.com/$repo/releases/latest/download/$asset"
  else
    url="https://github.com/$repo/releases/download/$version/$asset"
  fi
  curl -fsSL "$url" -o "$tmp/$asset"
fi

mkdir -p "$dir"
tar -xzf "$tmp/$asset" -C "$tmp"
install -m 755 "$tmp/p2pagents" "$dir/p2pagents"
echo "Installed: $("$dir/p2pagents" --version) -> $dir/p2pagents"
case ":$PATH:" in *":$dir:"*) ;; *) echo "Add $dir to your PATH." ;; esac
cat <<'EOF'
Next steps:
  install the Copilot CLI: npm install -g @github/copilot   (each agent runs its own instance)
  optional: GitHub CLI (gh auth login) - easiest way to sign agents in to GitHub accounts
  p2pagents setup        # optional: Claude/Codex support (needs Node.js 18+)
  p2pagents doctor       # check everything
  p2pagents auth login copilot --profile <me> --gh-user <github-login>
  p2pagents peers id --profile <me>    # share your ticket with peers
EOF
