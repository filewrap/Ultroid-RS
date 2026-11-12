#!/bin/sh
# Download the prebuilt ntgcalls static libraries for this platform and
# extract them under vendor/ntgcalls-lib (lib/ + include/).
# The `ntgcalls-sys` build script downloads the same archive with ureq;
# that fails behind proxies that MITM TLS, so fetch it here with curl
# (system CA bundle) and point the build at it:
#   export NTGCALLS_LIB_DIR="$PWD/vendor/ntgcalls-lib/lib"
set -eu
cd "$(dirname "$0")/.."
VER="${NTGCALLS_VERSION:-3.0.7}"
case "$(uname -s)-$(uname -m)" in
  Linux-x86_64) NAME="linux-x86_64" ;;
  Linux-aarch64) NAME="linux-arm64" ;;
  Darwin-arm64) NAME="macos-arm64" ;;
  *) echo "unsupported platform: $(uname -s)-$(uname -m)" >&2; exit 1 ;;
esac
mkdir -p vendor
ZIP="vendor/ntgcalls.${NAME}-static_libs.zip"
if [ ! -f "$ZIP" ]; then
  curl -sL -o "$ZIP" "https://github.com/pytgcalls/ntgcalls/releases/download/v${VER}/ntgcalls.${NAME}-static_libs.zip"
fi
rm -rf vendor/ntgcalls-lib
mkdir -p vendor/ntgcalls-lib
unzip -o -q "$ZIP" -d vendor/ntgcalls-lib
echo "ntgcalls $VER extracted to vendor/ntgcalls-lib"
echo "build with: NTGCALLS_LIB_DIR=\"\$PWD/vendor/ntgcalls-lib/lib\" cargo build"
