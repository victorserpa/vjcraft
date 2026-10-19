#!/usr/bin/env bash
# Self-test for brew_row in scripts/third-party-lib.sh with a fake brew and fixtures.
# Negative control: the old comparison (installed == stable) must fail the revisioned case.
set -euo pipefail
cd "$(dirname "$0")/.."
T="$(mktemp -d)"; trap 'rm -rf "$T"' EXIT
mkdir -p "$T/cellar/tarball/1.2_1/.brew" "$T/cellar/gitpkg/0.9/.brew" "$T/cellar/oldpkg/2.0/.brew"
cat > "$T/tarball.json" <<'J'
{"formulae":[{"name":"tarball","license":"MIT","homepage":"https://t.example","revision":1,
 "versions":{"stable":"1.2"},"urls":{"stable":{"url":"https://t.example/t-1.2.tar.gz","checksum":"aaaa"}}}]}
J
cat > "$T/gitpkg.json" <<'J'
{"formulae":[{"name":"gitpkg","license":"GPL-2.0-only","homepage":"https://g.example","revision":0,
 "versions":{"stable":"1.0"},"urls":{"stable":{"url":"https://g.example/g.git","revision":"new"}}}]}
J
cat > "$T/oldpkg.json" <<'J'
{"formulae":[{"name":"oldpkg","license":"MIT","homepage":"https://o.example","revision":0,
 "versions":{"stable":"3.0"},"urls":{"stable":{"url":"https://o.example/o-3.0.tgz","checksum":"bbbb"}}}]}
J
cat > "$T/cellar/gitpkg/0.9/.brew/gitpkg.rb" <<'R'
class Gitpkg < Formula
  homepage "https://g.example/old"
  url "https://g.example/g.git",
      tag:      "v0.9",
      revision: "deadbeef"
  license "GPL-2.0-only"
end
R
cat > "$T/cellar/oldpkg/2.0/.brew/oldpkg.rb" <<R
class Oldpkg < Formula
  url "https://o.example/o-2.0.tgz"
  sha256 "$(printf 'c%.0s' $(seq 64))"
end
R
cat > "$T/brew" <<EOS
#!/usr/bin/env bash
case "\$1" in
  info) cat "$T/\${3}.json" ;;
  --cellar) echo "$T/cellar/\$2" ;;
esac
EOS
chmod +x "$T/brew"
export BREW="$T/brew"
. scripts/third-party-lib.sh
fail=0
expect() { # name, pkg, ver, grep pattern
  local row; row="$(brew_row "$2" "$3" 2>&1)" || row="ERR $row"
  if echo "$row" | grep -qF -- "$4"; then echo "ok   $1"; else echo "FAIL $1: $row"; fail=1; fi
}
expect "revisioned tarball (1.2_1 == 1.2 rev 1)" tarball 1.2_1 "| tarball | 1.2_1 | MIT | https://t.example | https://t.example/t-1.2.tar.gz | aaaa |"
expect "git source via .brew fallback" gitpkg 0.9 "| gitpkg | 0.9 | GPL-2.0-only | https://g.example/old | https://g.example/g.git @ deadbeef | git revision |"
expect "tarball via .brew fallback" oldpkg 2.0 "https://o.example/o-2.0.tgz | cccc"
# Negative control: old comparison.
ver_matches() { [ "$3" = "$1" ]; }
if ( brew_row tarball 1.2_1 ) >/dev/null 2>&1; then echo "FAIL negative control: old comparison still produced a row"; fail=1
else echo "ok   negative control: old comparison fails the revisioned case"; fi
exit $fail
