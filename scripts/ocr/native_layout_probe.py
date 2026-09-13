"""Isolated diagnostic of pinned Tesseract C-API region types; no projection."""
import ctypes as c
import hashlib
import json
import os
from pathlib import Path
import sys


def inspect(raw, config, identity, namespace):
    os.environ["OMP_THREAD_LIMIT"] = "1"
    import pymupdf

    actual = namespace["fingerprint_dependencies"](config)
    if actual != identity["dependencies"] or config != identity["config"]:
        raise ValueError("identity mismatch before native layout diagnostic")
    libraries = [d for d in actual if d["name"].startswith("library:")
                 and Path(d["name"][8:]).name.startswith("libtesseract.so")]
    if len(libraries) != 1:
        raise ValueError("expected exactly one fingerprinted libtesseract")
    library = libraries[0]
    lib = c.CDLL(library["name"][8:])

    def bind(name, result, *args):
        fn = getattr(lib, name)
        fn.restype, fn.argtypes = result, list(args)
        return fn

    pointer, integer = c.c_void_p, c.c_int
    create = bind("TessBaseAPICreate", pointer)
    delete = bind("TessBaseAPIDelete", None, pointer)
    init = bind("TessBaseAPIInit2", integer, pointer, c.c_char_p, c.c_char_p, integer)
    set_psm = bind("TessBaseAPISetPageSegMode", None, pointer, integer)
    set_image = bind("TessBaseAPISetImage", None, pointer, pointer,
                     integer, integer, integer, integer)
    set_dpi = bind("TessBaseAPISetSourceResolution", None, pointer, integer)
    analyse = bind("TessBaseAPIAnalyseLayout", pointer, pointer)
    iterator_delete = bind("TessPageIteratorDelete", None, pointer)
    next_block = bind("TessPageIteratorNext", integer, pointer, integer)
    block_type = bind("TessPageIteratorBlockType", integer, pointer)
    bounding_box = bind("TessPageIteratorBoundingBox", integer, pointer, integer,
                        *([c.POINTER(integer)] * 4))
    version = bind("TessVersion", c.c_char_p)
    # Enum order from the pinned 5.3.4 public capi.h, not inferred from text.
    names = ["unknown", "flowing_text", "heading_text", "pullout_text", "equation",
             "inline_equation", "table", "vertical_text", "caption_text", "flowing_image",
             "heading_image", "pullout_image", "horizontal_line", "vertical_line", "noise"]
    pages = []
    with pymupdf.open(stream=raw, filetype="pdf") as document:
        for page in document:
            namespace["raster_pixel_count"](page, config["dpi"])
            pix = page.get_pixmap(dpi=config["dpi"], colorspace=pymupdf.csRGB, alpha=False)
            samples = c.create_string_buffer(pix.samples)
            handle = create()
            if not handle:
                raise RuntimeError("TessBaseAPICreate failed")
            iterator = None
            try:
                if init(handle, config["tessdata_path"].encode(),
                        "+".join(config["languages"]).encode(), config["oem"]) != 0:
                    raise RuntimeError("TessBaseAPIInit2 failed")
                set_psm(handle, config["psm"])
                set_image(handle, samples, pix.width, pix.height, pix.n, pix.stride)
                set_dpi(handle, config["dpi"])
                iterator = analyse(handle)
                blocks = []
                if iterator:
                    while True:
                        bounds = [integer() for _ in range(4)]
                        if not bounding_box(iterator, 0, *(c.byref(v) for v in bounds)):
                            raise RuntimeError("native block lacks bounding box")
                        kind = block_type(iterator)
                        if not 0 <= kind < len(names):
                            raise RuntimeError("unknown native block type enum")
                        blocks.append({"index": len(blocks), "type_id": kind,
                                       "type": names[kind], "bbox_pixels": [v.value for v in bounds]})
                        if len(blocks) > 10000:
                            raise RuntimeError("native block diagnostic limit exceeded")
                        if not next_block(iterator, 0):
                            break
                pages.append({"page": page.number + 1, "rotation": page.rotation,
                              "rect": list(page.rect), "raster_width": pix.width,
                              "raster_height": pix.height,
                              "raster_sha256": hashlib.sha256(pix.samples).hexdigest(),
                              "blocks": blocks})
            finally:
                if iterator:
                    iterator_delete(iterator)
                delete(handle)
    return {"schema": "tesseract-native-layout-diagnostic/v1",
            "library": library, "library_version": version().decode(), "pages": pages}


if __name__ == "__main__":
    root = Path(__file__).resolve().parents[2]
    namespace = {}
    exec((root / "src/parsing/pdf_layout_worker.py").read_text(), namespace)
    result = inspect(sys.stdin.buffer.read(), json.loads(sys.argv[1]),
                     json.loads(sys.argv[2]), namespace)
    print(json.dumps(result, ensure_ascii=False))
