#!/usr/bin/env bash
# Canonical C-header staging and XCFramework assembly/verification.
# Sourced by build-ffi-xcframework.sh; its resolved build variables and
# die/step/slice_archive helpers remain owned by that entry point.

# Copy the entire umbrella tree, preserving relative detail includes. A caller
# supplies an empty disposable/staging directory, never an installed location.
stage_ffi_headers() {
    local destination="$1"
    mkdir -p "$destination"
    cp -R "$ROOT/crates/$CRATE/include/phux" "$destination/phux"
    printf 'module PhuxFFI {\n    header "phux/client.h"\n    export *\n}\n' > "$destination/module.modulemap"
}

# Check every source header, not just the umbrella, in each packaged slice.
verify_ffi_headers() {
    local destination="$1" header relative
    [[ -f "$destination/phux/client.h" ]] || die "missing phux/client.h in $destination"
    while IFS= read -r header; do
        relative="${header#"$ROOT/crates/$CRATE/include/"}"
        cmp -s "$header" "$destination/$relative" || die "missing or changed header $relative in $destination"
    done < <(find "$ROOT/crates/$CRATE/include/phux" -type f -name '*.h' | LC_ALL=C sort)
    [[ -f "$destination/module.modulemap" ]] || die "missing module.modulemap in $destination"
}

assemble_xcframework() {
    HEADERS_DIR="$(mktemp -d "${TMPDIR:-/tmp}/phux-ffi-headers.XXXXXX")"
    stage_ffi_headers "$HEADERS_DIR"

    step "assembling PhuxFFI.xcframework"
    rm -rf "$OUT/PhuxFFI.xcframework"
    mkdir -p "$OUT"
    local args=() target
    for target in "${TARGETS[@]}"; do
        args+=(-library "$(slice_archive "$target")" -headers "$HEADERS_DIR")
    done
    xcodebuild -create-xcframework "${args[@]}" -output "$OUT/PhuxFFI.xcframework" >/dev/null
}

slice_identifier() {
    case "$1" in
    "$DEVICE_TARGET") echo ios-arm64 ;;
    "$SIM_TARGET") echo ios-arm64-simulator ;;
    "$MAC_TARGET") echo macos-arm64 ;;
    *) die "no xcframework slice identifier for $1" ;;
    esac
}

# LC_BUILD_VERSION platform numbers from <mach-o/loader.h>.
slice_macho_platform() {
    case "$1" in
    ios-arm64) echo 2 ;;
    ios-arm64-simulator) echo 7 ;;
    macos-arm64) echo 1 ;;
    esac
}

verify_slice() {
    local identifier="$1" framework="$OUT/PhuxFFI.xcframework"
    local archive="$framework/$identifier/$LIB"
    [[ -s "$archive" ]] || die "missing slice archive $archive"
    verify_ffi_headers "$framework/$identifier/Headers"
    [[ "$(lipo -archs "$archive")" == "arm64" ]] || die "$identifier is not arm64-only"
    local load_commands platforms expected
    load_commands="$(otool -l "$archive")"
    platforms="$(awk '$1 == "cmd" { build = ($2 == "LC_BUILD_VERSION") }
        build && $1 == "platform" { print $2 }' <<<"$load_commands" | sort -u)"
    expected="$(slice_macho_platform "$identifier")"
    [[ "$platforms" == "$expected" ]] ||
        die "$identifier Mach-O platform is '${platforms//$'\n'/,}', expected $expected"
    # Rust's precompiled standard-library members can carry legacy
    # LC_VERSION_MIN_* markers instead of LC_BUILD_VERSION. Reject markers
    # that cannot belong to this slice; simulator objects always identify as
    # LC_BUILD_VERSION platform 7, so either legacy marker is foreign there.
    local foreign_min_pattern floor
    case "$identifier" in
    ios-arm64) foreign_min_pattern=LC_VERSION_MIN_MACOSX floor="$IOS_FLOOR" ;;
    ios-arm64-simulator) foreign_min_pattern='LC_VERSION_MIN_(MACOSX|IPHONEOS)' floor="$IOS_FLOOR" ;;
    macos-arm64) foreign_min_pattern=LC_VERSION_MIN_IPHONEOS floor="$MACOS_FLOOR" ;;
    esac
    ! grep -Eq "cmd $foreign_min_pattern\$" <<<"$load_commands" ||
        die "$identifier contains a foreign LC_VERSION_MIN_* member"
    # No member may sit above the slice's deployment floor, and the crate's
    # own objects must sit exactly on it. Precompiled Rust std objects carry
    # older floors, which a consumer's link tolerates.
    awk -v floor="$floor" '
        function num(v,  p) { split(v, p, "."); return (p[1] + 0) * 10000 + (p[2] + 0) * 100 + (p[3] + 0) }
        $1 == "cmd" { build = ($2 == "LC_BUILD_VERSION"); legacy = ($2 ~ /^LC_VERSION_MIN_/) }
        (build && $1 == "minos") || (legacy && $1 == "version") {
            count += 1
            if (num($2) > num(floor)) bad = $2
            if (num($2) == num(floor)) has_floor = 1
        }
        END { if (bad != "" || count == 0 || !has_floor) exit 1 }' <<<"$load_commands" ||
        die "$identifier does not preserve the $floor deployment floor"
}

verify_xcframework() {
    step "verifying slices"
    local framework="$OUT/PhuxFFI.xcframework" expected actual target
    expected="$(for target in "${TARGETS[@]}"; do slice_identifier "$target"; done | LC_ALL=C sort)"
    actual="$(plutil -extract AvailableLibraries json -o - "$framework/Info.plist" |
        jq -r '.[].LibraryIdentifier' | LC_ALL=C sort)"
    [[ "$actual" == "$expected" ]] ||
        die "unexpected slice set; expected [${expected//$'\n'/ }], got [${actual//$'\n'/ }]"
    local identifier
    while IFS= read -r identifier; do
        verify_slice "$identifier"
    done <<<"$expected"
}

sha256_of() {
    shasum -a 256 "$1" | awk '{print $1}'
}
