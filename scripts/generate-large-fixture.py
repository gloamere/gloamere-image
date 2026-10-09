"""Synthetic Windows QA input, never an original user image. Requires Pillow."""
from pathlib import Path
from PIL import Image
import hashlib,json

project=Path(__file__).resolve().parent.parent
output=project/'test-output'/'large-fixture'
output.mkdir(parents=True,exist_ok=True)
destination=output/'4000-square-icc-alpha.png'
if destination.exists():
    raise SystemExit('Use a new fixture identity; existing fixture preserved')
profile_source=project/'test-output'/'fixtures'/'rotated-export.png'
with Image.open(profile_source) as reference:
    profile=reference.info['icc_profile']
tile=Image.new('RGBA',(256,256))
tile.putdata([(x,y,(x*y)%256,(x+y)%256) for y in range(256) for x in range(256)])
large=Image.new('RGBA',(4000,4000))
for y in range(0,4000,256):
    for x in range(0,4000,256):
        large.paste(tile,(x,y))
large.save(destination,icc_profile=profile)
report={'source':'generated synthetic fixture','dimensions':[4000,4000],
        'icc':'sRGB from verified exported fixture','bytes':destination.stat().st_size,
        'sha256':hashlib.sha256(destination.read_bytes()).hexdigest()}
(output/'fixture.json').write_text(json.dumps(report,indent=2),encoding='utf-8')
print(json.dumps(report))
