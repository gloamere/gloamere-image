# Image-list readability repair

Actual Windows D3D12 local readback at 800 x 550 logical units exposed two
problems: a multiline truncated Label showed the ordinal while hiding the
filename, and the SimHei fallback list body was absent. A person comparing
assets needs to identify each filename and the current directory position.
The toolbar and crop confirmation remained visible; fix the list independently.

Use explicit top-aligned wrapped header layout, matching the toolbar's
already tested mixed-font policy. Render each virtual row's filename and
ordinal/type as separate clipped text lines, never a multiline single-line
truncate label. The thumbnail stays in its existing bounded 50-unit square.
Locate with one preceding context row and use egui end-stick behavior when selecting the final item, so changing header/font metrics does not leave the last card half clipped. Preserve full filename in hover text and the row's accessibility name. Keep
row click, Enter selection, highlight, virtualized scrolling and cache limits.
Use egui's existing painter/text path; no custom GPU renderer is introduced.

Acceptance: MSYH and forced SimHei actual Windows minimum-window readbacks
show the count, current highlighted thumbnail, recognizable filename and
separate ordinal/type. Long names may clip with full hover text. Both image
list and inspector can coexist with an unobscured crop confirmation. Native
mouse/key/accessibility input remains subject to supported Computer Use.


## Final visual-audit investigation

The local image display initially appeared to omit some navigation/zoom controls
and the Latin part of rotation captions. The original PNG pixel data and enlarged
local crops show all these controls intact. The navigation region is byte-identical
to the complete diagnostic sample. Four captures with default vs LOW_LATENCY
surface settings produced identical PNG hashes; changing memory hints also gave
no evidence of a renderer defect. No surface/renderer change is warranted.
Temporary paint tracing and diagnostic configuration were removed. Final
acceptance includes direct pixel/crop inspection of controls and thumbnails;
no screenshots are included in deliverables. This finding corrects the earlier
visual-display interpretation, rather than claiming a product paint fix.
