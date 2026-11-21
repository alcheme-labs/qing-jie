#!/bin/sh
set -eu
PROJECT_ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$PROJECT_ROOT"

# Keep local checkout and dependency-cache paths out of release binaries.
CARGO_CACHE=${CARGO_HOME:-"$HOME/.cargo"}
if [ -d "$PROJECT_ROOT/.build-tools/cargo" ]; then
    CARGO_CACHE="$PROJECT_ROOT/.build-tools/cargo"
fi
RELEASE_RUSTFLAGS=$(python3 - "$PROJECT_ROOT" "$CARGO_CACHE" <<'PY'
import os
import sys

separator = "\x1f"
encoded = os.environ.get("CARGO_ENCODED_RUSTFLAGS")
if encoded is None:
    flags = os.environ.get("RUSTFLAGS", "").split()
else:
    flags = encoded.split(separator) if encoded else []

checkout, cargo_home = sys.argv[1:]
flags += [
    f"--remap-path-prefix={checkout}=.",
    f"--remap-path-prefix={os.path.abspath(cargo_home)}=/cargo",
]
print(separator.join(flags))
PY
)
CARGO_ENCODED_RUSTFLAGS="$RELEASE_RUSTFLAGS" sh scripts/cargo.sh build --release --locked "$@"
APP="$PROJECT_ROOT/dist/轻截.app"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp target/release/qingjie "$APP/Contents/MacOS/qingjie"
/usr/bin/strip -S -x "$APP/Contents/MacOS/qingjie"
cp packaging/Info.plist "$APP/Contents/Info.plist"
cp packaging/AppIcon.icns "$APP/Contents/Resources/AppIcon.icns"
for localization in packaging/*.lproj; do
    /usr/bin/plutil -lint "$localization/Localizable.strings" "$localization/InfoPlist.strings"
    /usr/bin/ditto "$localization" "$APP/Contents/Resources/$(basename "$localization")"
done
cp LICENSE "$APP/Contents/Resources/LICENSE.txt"
sh scripts/cargo.sh metadata --offline --locked --format-version=1 --filter-platform "$(sh scripts/cargo.sh -vV | sed -n 's/^host: //p')" > target/metadata.json
python3 scripts/collect-licenses.py target/metadata.json "$APP/Contents/Resources/ThirdPartyNotices.txt"
/usr/bin/plutil -lint "$APP/Contents/Info.plist"
# TCC remembers the designated requirement. An ad-hoc signature is bound to
# the binary's cdhash, so an updated binary cannot reuse an older build's grant.
# Use the same installed Apple Development / Developer ID identity across builds.
QINGJIE_SIGNING_IDENTITY=${QINGJIE_SIGNING_IDENTITY:--}
if [ "$QINGJIE_SIGNING_IDENTITY" = "-" ]; then
    echo "提示：当前使用临时签名；应用更新后可能需要重新授予轻截的屏幕录制权限。" >&2
    echo "固定签名：设置 QINGJIE_SIGNING_IDENTITY 为已安装的代码签名证书名称或 SHA-1。" >&2
fi
/usr/bin/codesign --force --sign "$QINGJIE_SIGNING_IDENTITY" --identifier com.taiyi.qingjie --timestamp=none "$APP"
/usr/bin/codesign --verify --strict --verbose=2 "$APP"
/usr/bin/ditto -c -k --norsrc --keepParent "$APP" "$PROJECT_ROOT/dist/轻截-mac-$(uname -m).zip"
du -sh "$APP" "$PROJECT_ROOT/dist/轻截-mac-$(uname -m).zip"
