# Sourced by gen-third-party.sh, bundle-dylibs.sh and test-third-party.sh.
# BREW can be set to another brew executable (the self-test uses a fake one).
BREW="${BREW:-brew}"
export HOMEBREW_NO_AUTO_UPDATE=1
tp_die() { echo "third-party: $*" >&2; exit 1; }

# ver_matches STABLE REVISION INSTALLED: installed dir is "X" (revision 0) or "X_N".
ver_matches() {
  local stable="$1" rev="${2:-0}" ver="$3"
  [ "$ver" = "$stable" ] && [ "$rev" = 0 ] && return 0
  [ "$rev" != 0 ] && [ "$ver" = "${stable}_$rev" ] && return 0
  return 1
}

# Path of the formula copy Homebrew kept in the installed keg (may not exist).
installed_rb() { echo "$("$BREW" --cellar "$1")/$2/.brew/$1.rb"; }

# rb_field FILE KEY: first `KEY "value"` line of a formula file.
rb_field() { sed -nE "s/^  $2 \"([^\"]+)\".*/\1/p" "$1" | head -1; }

# formula_meta PKG VER JSON: prints "license<TAB>homepage", preferring the installed formula.
formula_meta() {
  local pkg="$1" ver="$2" j="$3" rb lic home
  rb="$(installed_rb "$pkg" "$ver")"
  if [ -f "$rb" ]; then lic="$(rb_field "$rb" license)"; home="$(rb_field "$rb" homepage)"; fi
  [ -n "${lic:-}" ] || lic="$(echo "$j" | jq -r '.formulae[0].license // empty')"
  [ -n "${home:-}" ] || home="$(echo "$j" | jq -r '.formulae[0].homepage // empty')"
  [ -n "$lic" ] || tp_die "no license id for $pkg"
  printf '%s\t%s\n' "$lic" "$home"
}

# One table row for a Homebrew formula: library | version | license | homepage | source | sha256.
brew_row() {
  local pkg="$1" ver="$2" j rb url sha tag rev lic home stable srev
  j="$("$BREW" info --json=v2 "$pkg")" || tp_die "brew info $pkg failed"
  IFS=$'\t' read -r lic home < <(formula_meta "$pkg" "$ver" "$j")
  stable="$(echo "$j" | jq -r '.formulae[0].versions.stable')"
  srev="$(echo "$j" | jq -r '.formulae[0].revision // 0')"
  if ver_matches "$stable" "$srev" "$ver"; then
    echo "$j" | jq -r --arg v "$ver" --arg l "$lic" --arg h "$home" '.formulae[0] |
      (.urls.stable) as $u |
      "| \(.name) | \($v) | \($l) | \($h) | \(if $u.revision then "\($u.url) @ \($u.revision)" else $u.url end) | \($u.checksum // "git revision") |"'
    return
  fi
  # Installed version is not the formula's current one: use the formula copy kept with it.
  rb="$(installed_rb "$pkg" "$ver")"
  [ -f "$rb" ] || tp_die "$pkg $ver: no installed formula at $rb to find its source"
  url="$(rb_field "$rb" url)"
  sha="$(sed -nE 's/^  sha256 "([0-9a-f]{64})".*/\1/p' "$rb" | head -1)"
  rev="$(sed -nE 's/.*revision: *"([^"]+)".*/\1/p' "$rb" | head -1)"
  tag="$(sed -nE 's/.*tag: *"([^"]+)".*/\1/p' "$rb" | head -1)"
  [ -n "$url" ] || tp_die "$pkg $ver: no url in $rb"
  if [ -n "$sha" ]; then
    echo "| $pkg | $ver | $lic | $home | $url | $sha |"
  elif [ -n "$rev" ] || [ -n "$tag" ]; then
    echo "| $pkg | $ver | $lic | $home | $url @ ${rev:-$tag} | git revision |"
  else
    tp_die "$pkg $ver: no sha256 or git revision/tag in $rb"
  fi
}
