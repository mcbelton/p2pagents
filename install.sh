#!/bin/sh
# Installs the p2pagents binary from GitHub releases (no source code or Rust needed).
#   private repo (needs read access, uses your GitHub CLI sign-in):
#     gh release download -R mcbelton/p2pagents -p install.sh -O - | sh
#   public repo:
#     curl -fsSL https://github.com/mcbelton/p2pagents/releases/latest/download/install.sh | sh
# Options (env vars): P2PAGENTS_VERSION (default latest), P2PAGENTS_REPO, P2PAGENTS_INSTALL_DIR.
set -eu

repo="${P2PAGENTS_REPO:-mcbelton/p2pagents}"
version="${P2PAGENTS_VERSION:-latest}"
dir="${P2PAGENTS_INSTALL_DIR:-$HOME/.local/bin}"

case "$(uname -s)-$(uname -m)" in
  Linux-x86_64) target=x86_64-unknown-linux-gnu ;;
  Darwin-arm64) target=aarch64-apple-darwin ;;
  *) echo "no prebuilt binary for $(uname -s)-$(uname -m); build from source (see README)" >&2; exit 1 ;;
esac
asset="p2pagents-$target.tar.gz"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

if command -v gh >/dev/null 2>&1; then
  if [ "$version" = latest ]; then
    gh release download -R "$repo" -p "$asset" -D "$tmp"
  else
    gh release download "$version" -R "$repo" -p "$asset" -D "$tmp"
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
