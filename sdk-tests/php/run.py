"""Run the real PHP SDK without changing the user's PHP configuration."""
import sys
sys.dont_write_bytecode = True
from pathlib import Path
import subprocess
from prepare import php_command

if len(sys.argv) != 2:
    raise SystemExit("Metric DSN argument is required")
result = subprocess.run(php_command() + [str(Path(__file__).with_name("send_error.php")), sys.argv[1]], timeout=20)
raise SystemExit(result.returncode)
