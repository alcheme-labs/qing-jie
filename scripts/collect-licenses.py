"""Bundle dependency license texts from Cargo's exact resolved packages."""
import json
from pathlib import Path
import sys

metadata = json.loads(Path(sys.argv[1]).read_text())
resolved = {node["id"] for node in metadata["resolve"]["nodes"]}
sections = ["轻截 — third-party Rust dependencies\n"]
for package in sorted(metadata["packages"], key=lambda p: p["name"]):
    if package["id"] not in resolved or package["name"] == "qingjie":
        continue
    root = Path(package["manifest_path"]).parent
    files = sorted(p for p in root.iterdir() if p.is_file() and
                   p.name.upper().startswith(("LICENSE", "COPYING", "NOTICE")))
    if package.get("license_file"):
        files = sorted(set(files + [root / package["license_file"]]))
    if not files and package.get("repository") == "https://github.com/madsmtm/objc2":
        files = [Path(__file__).resolve().parent.parent / "packaging/licenses/objc2-MIT.txt"]
    if not files:
        raise SystemExit(f"No license text for {package['name']}")
    sections.append(f"\n{'=' * 60}\n{package['name']} {package['version']}\n"
                    f"{package.get('repository', '')}\nLicense: {package.get('license', '')}\n")
    for path in files:
        sections.append(f"\n--- {path.name} ---\n{path.read_text()}\n")
Path(sys.argv[2]).write_text("".join(sections))
