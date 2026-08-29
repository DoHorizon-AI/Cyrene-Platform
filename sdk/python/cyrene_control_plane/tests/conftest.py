from pathlib import Path
import sys

PACKAGE_ROOT = Path(__file__).resolve().parents[1]
ARTIFACT_SOURCE = PACKAGE_ROOT.parent / "cyrene_artifacts" / "src"

for source in (PACKAGE_ROOT / "src", ARTIFACT_SOURCE):
    source_text = str(source)
    if source_text not in sys.path:
        sys.path.insert(0, source_text)
