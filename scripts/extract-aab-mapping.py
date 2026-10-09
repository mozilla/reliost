#!/usr/bin/env python3
"""Extracts the R8/ProGuard mapping file from an Android App Bundle (.aab).

Usage: extract-aab-mapping.py <path/to/app.aab> <output-dir>

Writes <output-dir>/<proguard uuid>/mapping.txt.zst, which is the layout that
the /deobfuscate/java/v1 endpoint expects on a mapping file server. Requires the
`zstd` command line tool.

The UUID is computed from the mapping file, in the same way as the Sentry Gradle
plugin does it. Once https://bugzilla.mozilla.org/show_bug.cgi?id=2079950 lands,
the build will record the UUID in the Android package itself, and this script
could read it from there instead of duplicating the computation.
"""

import hashlib
import shutil
import subprocess
import sys
import uuid
import zipfile
from pathlib import Path

MAPPING_PATH = "BUNDLE-METADATA/com.android.tools.build.obfuscation/proguard.map"
PG_MAP_HASH_PREFIX = b"# pg_map_hash: SHA-256 "


def proguard_uuid(aab):
    """Computes the ProGuard UUID from the pg_map_hash line in the header."""
    with aab.open(MAPPING_PATH) as mapping:
        for line in mapping:
            if line.startswith(PG_MAP_HASH_PREFIX):
                pg_map_hash = line[len(PG_MAP_HASH_PREFIX) :].strip()
                return uuid.UUID(bytes=hashlib.md5(pg_map_hash).digest(), version=3)
            if not line.startswith(b"#"):
                break
    sys.exit(f"No '{PG_MAP_HASH_PREFIX.decode()}' line in the mapping file header")


def main():
    if len(sys.argv) != 3:
        sys.exit(f"Usage: {sys.argv[0]} <path/to/app.aab> <output-dir>")
    aab_path, output_dir = Path(sys.argv[1]), Path(sys.argv[2])
    if shutil.which("zstd") is None:
        sys.exit("The zstd command line tool is required")

    with zipfile.ZipFile(aab_path) as aab:
        uuid_dir = output_dir / str(proguard_uuid(aab))
        uuid_dir.mkdir(parents=True, exist_ok=True)
        output_path = uuid_dir / "mapping.txt.zst"
        with aab.open(MAPPING_PATH) as mapping:
            zstd = subprocess.Popen(
                ["zstd", "-q", "-f", "-19", "-T0", "-o", str(output_path)],
                stdin=subprocess.PIPE,
            )
            shutil.copyfileobj(mapping, zstd.stdin)
            zstd.stdin.close()
            if zstd.wait() != 0:
                sys.exit("zstd failed")

    print(output_path)


if __name__ == "__main__":
    main()
