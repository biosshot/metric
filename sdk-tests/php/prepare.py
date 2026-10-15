"""Install locked fixture dependencies with a verified, isolated Composer PHAR."""
import hashlib
import os
from pathlib import Path
import shutil
import subprocess
import urllib.request

ROOT = Path(__file__).resolve().parent
COMPOSER_VERSION = "2.10.3"


def php_command():
    binary = shutil.which("php")
    if not binary:
        raise RuntimeError("PHP 8.3 CLI is required")
    command = [binary]
    if os.name == "nt":
        # A fixture-only configuration: never change the operator's php.ini.
        directory = Path(binary).resolve().parent / "ext"
        command += ["-n", "-d", f"extension_dir={directory}"]
        for extension in ("openssl", "curl", "mbstring", "zip"):
            command += ["-d", f"extension={extension}"]
    return command


def main():
    directory = ROOT / ".deps"
    directory.mkdir(exist_ok=True)
    url = f"https://getcomposer.org/download/{COMPOSER_VERSION}/composer.phar"
    with urllib.request.urlopen(url + ".sha256sum", timeout=60) as response:
        digest = response.read(4096).decode("ascii").split()[0]
    if len(digest) != 64:
        raise RuntimeError("Invalid official Composer SHA256")
    with urllib.request.urlopen(url, timeout=60) as response:
        data = response.read(16 * 1024 * 1024 + 1)
    if len(data) > 16 * 1024 * 1024 or hashlib.sha256(data).hexdigest() != digest:
        raise RuntimeError("Official Composer download checksum mismatch")
    path = directory / "composer.phar"
    path.write_bytes(data)
    subprocess.run(php_command() + [str(path), "install", "--no-interaction",
        "--prefer-dist", "--no-progress", "--no-plugins", "--no-scripts"], cwd=ROOT, check=True, timeout=600)


if __name__ == "__main__":
    main()
