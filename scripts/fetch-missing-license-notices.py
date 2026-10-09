"""Fetch omitted upstream notices at exact published VCS commits, without credentials."""
import argparse
import hashlib
import json
import re
import urllib.parse
import urllib.request
from pathlib import Path

parser = argparse.ArgumentParser()
parser.add_argument("--registry", type=Path, required=True)
parser.add_argument("--output", type=Path, required=True)
args = parser.parse_args()
inventory_path = args.output / "inventory.json"
inventory = json.loads(inventory_path.read_text("utf-8"))
cache = {}
errors = []

def get(url):
    request = urllib.request.Request(url, headers={"User-Agent": "LightImageTool-license-packaging"})
    with urllib.request.urlopen(request, timeout=15) as response:
        data = response.read(1024 * 1024 + 1)
    if len(data) > 1024 * 1024:
        raise ValueError("notice response exceeds one MiB")
    return data

for record in inventory["packages"]:
    if record["notices"]:
        continue
    package_id = f'{record["name"]}-{record["version"]}'
    sources = list(args.registry.glob(f"*/{package_id}/.cargo_vcs_info.json"))
    repository = record.get("repository") or ""
    parts = urllib.parse.urlparse(repository)
    components = parts.path.strip("/").split("/")
    if parts.hostname != "github.com" or len(components) < 2 or len(sources) != 1:
        errors.append(f"{package_id}: no exact GitHub publication reference")
        continue
    vcs = json.loads(sources[0].read_text("utf-8"))
    sha = vcs.get("git", {}).get("sha1", "")
    if not re.fullmatch("[0-9a-f]{40}", sha):
        errors.append(f"{package_id}: no exact VCS commit")
        continue
    repository_name = "/".join(components[:2]).removesuffix(".git")
    key = (repository_name, sha)
    try:
        if key not in cache:
            listing_url = f"https://api.github.com/repos/{repository_name}/contents/?ref={sha}"
            listing = json.loads(get(listing_url))
            cache[key] = [(entry["name"], entry["download_url"], get(entry["download_url"]))
                          for entry in listing if entry["type"] == "file" and
                          re.search(r"(license|licence|copying|notice)", entry["name"], re.I)]
        fetched = []
        for name, url, data in cache[key]:
            relative = Path(package_id) / "upstream-root" / name
            destination = args.output / relative
            destination.parent.mkdir(parents=True, exist_ok=True)
            destination.write_bytes(data)
            record["notices"].append(str(relative).replace("\\", "/"))
            fetched.append({"url": url, "publication_commit": sha,
                            "sha256": hashlib.sha256(data).hexdigest()})
        record["fetched_notice_sources"] = fetched
        if not fetched:
            errors.append(f"{package_id}: upstream root contains no notice")
    except Exception as error:
        errors.append(f"{package_id}: {type(error).__name__}: {error}")
inventory["missing_notice_files"] = [f'{r["name"]}-{r["version"]}' for r in inventory["packages"] if not r["notices"]]
inventory["upstream_fetch_errors"] = errors
inventory_path.write_text(json.dumps(inventory, ensure_ascii=False, indent=2), "utf-8")
print(json.dumps({"missing": inventory["missing_notice_files"], "errors": errors}, indent=2))
