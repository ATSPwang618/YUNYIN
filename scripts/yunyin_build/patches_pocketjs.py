"""把仓库内的 PocketJS 改动应用到干净的框架检出。"""

import subprocess

from .config import PKJ, PROJECT_ROOT


PATCH = PROJECT_ROOT / "scripts" / "patches" / "pocketjs-yunyin.patch"

PATCH_MARKERS = (
    ("contracts/spec/platforms.ts", "text.glyphs.streamed"),
    ("engine/core/src/font_stream.rs", "font_stream_requests"),
    ("hosts/vita/src/ffi.rs", "YUNYIN_NATIVE_TEXT_INSTALL"),
    ("hosts/vita/src/main.rs", "YUNYIN_HOST_FRAME_DIAG"),
)


def _looks_applied():
    return all(
        (PKJ / relative).is_file()
        and marker in (PKJ / relative).read_text()
        for relative, marker in PATCH_MARKERS
    )


def apply_pocketjs_patch():
    """应用固定的 PocketJS 补丁，避免构建依赖 /private/tmp 的脏状态。"""
    if not PATCH.exists():
        raise SystemExit(f"[build-vpk] PocketJS patch missing: {PATCH}")
    if not (PKJ / ".git").exists():
        raise SystemExit(f"[build-vpk] PocketJS checkout is not a git repository: {PKJ}")

    check = subprocess.run(
        ["git", "apply", "--check", str(PATCH)],
        cwd=str(PKJ),
        text=True,
        capture_output=True,
    )
    if check.returncode == 0:
        subprocess.run(["git", "apply", "--whitespace=nowarn", str(PATCH)],
                       cwd=str(PKJ), check=True)
        print(f"[build-vpk] applied PocketJS patch: {PATCH.name}")
        return

    # A repeated build sees the same patch already applied.  Accept that
    # state, but reject a checkout with unrelated or incomplete local edits.
    reverse = subprocess.run(
        ["git", "apply", "--reverse", "--check", str(PATCH)],
        cwd=str(PKJ),
        text=True,
        capture_output=True,
    )
    if reverse.returncode == 0:
        print(f"[build-vpk] PocketJS patch already applied: {PATCH.name}")
        return

    # The host compatibility layer may normalize a few generated build.rs
    # blank lines after the static patch is applied.  In that case git's
    # reverse check is intentionally stricter than the source state we need;
    # use several independent feature markers to recognize the fully patched
    # checkout instead of applying the patch a second time.
    if _looks_applied():
        print(f"[build-vpk] PocketJS patch markers already present: {PATCH.name}")
        return

    details = (check.stderr or check.stdout or "patch context does not match").strip()
    raise SystemExit(
        f"[build-vpk] PocketJS checkout is neither clean-base nor fully patched; "
        f"refusing to guess: {details}"
    )
