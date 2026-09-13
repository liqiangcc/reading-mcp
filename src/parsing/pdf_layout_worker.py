"""Isolated optional PDF layout worker; stdout is a versioned JSON protocol.

Native source spans or explicitly enabled local OCR words are projected. No
generated descriptions or remote model calls. Ambiguous hyphens are preserved.
"""
import contextlib
import importlib.metadata
import json
import re
import csv
import os
import subprocess
import tempfile
import sys
import unicodedata
import hashlib
import statistics
import stat
import time
import selectors
import signal

OCR_PROCESS_ENV = {"PATH": "/usr/bin:/bin", "LANG": "C.UTF-8", "OMP_THREAD_LIMIT": "1"}
import math

VERSION = "pdf-layout/v1"
ENGINE = "pymupdf4llm-layout/1.28.2"
OCR_CONFIG = {"enabled": False}
EXPECTED_IDENTITY = None
PAGE_DEADLINE = None
RASTER_BUDGET = None
PAGE_RASTER = None
INPUT_SHA256 = None
WORKER_SOURCE = None
INSPECTION_POLICY = "ocr-original-region-inspection/v5"

# Approved immutable local classifier. A model path is operator/package-owned;
# no network lookup, arbitrary replacement model or caller threshold is allowed.
VISUAL_MODEL_FILES = {
    'README.md': (1669, '91328b1af981a8992c803b6d4be88185b4407e09450650ec5f5a4941a8b99d99'),
    'inference.yml': (1838, 'd60f782a16f96afb27e8280399899a94c3e9ffc694ffb2f913ea00af1c522f1e'),
    'inference.onnx': (129736329, '77afb2caa74dd13240d087d2eced91d7fcd2caebd16006a0a66162fc8707ff0e'),
}


def fingerprint_visual_model(directory):
    """Stream verified package inputs before loading any inference library."""
    if not os.path.isabs(directory) or '\0' in directory:
        raise ValueError('visual model directory must be absolute')
    dependencies = []
    for name, (size, expected) in sorted(VISUAL_MODEL_FILES.items()):
        descriptor = os.open(os.path.join(directory, name), os.O_RDONLY | os.O_NONBLOCK | os.O_NOFOLLOW)
        with os.fdopen(descriptor, 'rb') as stream:
            metadata = os.fstat(stream.fileno())
            if not stat.S_ISREG(metadata.st_mode) or metadata.st_size != size:
                raise ValueError('visual model file type/size mismatch')
            digest = hashlib.sha256()
            for chunk in iter(lambda: stream.read(65536), b''):
                digest.update(chunk)
            actual = digest.hexdigest()
            if actual != expected:
                raise ValueError('visual model digest mismatch')
        dependencies.append({'name':'layout-model:' + name, 'sha256':actual})
    return dependencies


def visual_model_predictor(directory):
    """Fixed RGB CPU inference, shared with hosted acceptance, no scripts import.

    The owning stage must enforce the shared deadline/cgroup. This adapter does
    not enable classification in normal ingestion on its own.
    """
    dependencies = fingerprint_visual_model(directory)
    import cv2
    import numpy as np
    import onnxruntime as ort
    import yaml
    with open(os.path.join(directory, 'inference.yml')) as stream:
        config = yaml.safe_load(stream)
    expected = [
        {'interp':2, 'keep_ratio':False, 'target_size':[800,800], 'type':'Resize'},
        {'mean':[0.,0.,0.], 'norm_type':'none', 'std':[1.,1.,1.], 'type':'NormalizeImage'},
        {'type':'Permute'}]
    if config['Preprocess'] != expected or config['draw_threshold'] != .5:
        raise ValueError('unsupported visual preprocessing')
    options = ort.SessionOptions()
    options.intra_op_num_threads = options.inter_op_num_threads = 1
    options.enable_cpu_mem_arena = False
    session = ort.InferenceSession(os.path.join(directory, 'inference.onnx'), sess_options=options,
                                   providers=['CPUExecutionProvider'])
    names = {item.name for item in session.get_inputs()}
    if not names <= {'image', 'im_shape', 'scale_factor'} or 'image' not in names:
        raise ValueError('unexpected visual model inputs')

    def predict(rgb_bytes, width, height):
        if (type(width) is not int or type(height) is not int or width <= 0 or height <= 0
                or width * height > 16_000_000 or len(rgb_bytes) != width * height * 3):
            raise ValueError('invalid or oversized visual RGB raster')
        image = np.frombuffer(rgb_bytes, dtype=np.uint8).reshape(height, width, 3)
        tensor = cv2.resize(image, (800,800), interpolation=cv2.INTER_CUBIC).astype(np.float32) / 255.0
        values = {'image':tensor.transpose(2,0,1)[None],
                  'im_shape':np.array([[800,800]], dtype=np.float32),
                  'scale_factor':np.array([[800/height,800/width]], dtype=np.float32)}
        outputs = session.run(None, {name:values[name] for name in names})
        if (not outputs or outputs[0].ndim != 2 or outputs[0].shape[1] != 6 or len(outputs[0]) > 10000
                or sum(value.size for value in outputs) > 100_000
                or not all(np.isfinite(value).all() for value in outputs)):
            raise ValueError('invalid or oversized visual model output')
        boxes = []
        for label, score, *bounds in outputs[0].tolist():
            if score >= .5:
                if label != int(label) or not 0 <= int(label) < len(config['label_list']) or score > 1:
                    raise ValueError('invalid selected visual label/score')
                boxes.append({'cls_id':int(label), 'label':config['label_list'][int(label)],
                              'score':score, 'coordinate':bounds})
        return [{'res':{'boxes':boxes}, 'raw_outputs':[value.tolist() for value in outputs],
                 'postprocess':'pinned draw_threshold only; no layout NMS or gold selection'}]
    return predict, dependencies


def visual_model_child(model_directory, width, height):
    """Protocol for the short-lived classifier child owned by this worker."""
    raw = sys.stdin.buffer.read()
    predictor, dependencies = visual_model_predictor(model_directory)
    result = predictor(raw, width, height)
    json.dump({'schema':'ocr-visual-model-attempt/v1', 'raster_size':[width, height],
               'raster_sha256':hashlib.sha256(raw).hexdigest(),
               'dependencies':dependencies, 'attempts':result}, sys.stdout,
              ensure_ascii=False, separators=(',', ':'))


class OcrRequired(RuntimeError):
    """Image-bearing pages without native body text need enabled inspection."""


def has_native_body(page_layout):
    return any(any(span.get('text', '').strip() for line in box.get('textlines', [])
                   for span in line.get('spans', []))
               and box.get('boxclass') not in ('page-footer', 'page-header')
               for box in page_layout['boxes'])


class OcrStageFailure(RuntimeError):
    """Only explicit engine/budget producers assign these public categories."""
    def __init__(self, code, reason):
        super().__init__(reason)
        if code not in ('OCR_UNAVAILABLE', 'OCR_TIMEOUT', 'OCR_RESOURCE_LIMIT'):
            raise ValueError('unsupported OCR failure category')
        self.code = code

class OcrRasterBudget:
    """Allocation accounting is separate from the count of required OCR pages."""
    def __init__(self):
        self.pages = set()
        self.pixels = 0

    def reserve_raster(self, pixels):
        if self.pixels + pixels > 64_000_000:
            raise OcrStageFailure('OCR_RESOURCE_LIMIT', "OCR exceeds 64 million total raster pixel limit")
        self.pixels += pixels

    def require_page(self, page_number):
        if page_number not in self.pages and len(self.pages) >= 8:
            raise OcrStageFailure('OCR_RESOURCE_LIMIT', "OCR exceeds 8 required page limit")
        self.pages.add(page_number)

def page_time_remaining():
    remaining = 15.0 if PAGE_DEADLINE is None else PAGE_DEADLINE - time.monotonic()
    if remaining <= 0:
        raise OcrStageFailure('OCR_TIMEOUT', "local OCR page exceeded shared 15 second budget")
    return remaining

def raster_pixel_count(page, dpi):
    # Round the transformed rectangle outward, as the renderer does. Check
    # before allocating the pixmap; never downscale an oversized page.
    rect = page.rect
    scale = dpi / 72
    coordinates = [rect.x0, rect.y0, rect.x1, rect.y1]
    if not all(math.isfinite(value) for value in coordinates):
        raise RuntimeError("invalid OCR page rectangle")
    width = math.ceil(rect.x1 * scale) - math.floor(rect.x0 * scale)
    height = math.ceil(rect.y1 * scale) - math.floor(rect.y0 * scale)
    if width <= 0 or height <= 0 or width * height > 16_000_000:
        raise OcrStageFailure('OCR_RESOURCE_LIMIT', "OCR raster exceeds 16 million pixel page limit")
    return width * height
RETRY_POLICY = {"version": "ocr-regional-retry/v1", "primary_psm": 3,
                "retry_psm": 6, "max_retries_per_page": 1, "overlap_percent": 90,
                "vertical_overlap_percent": 50, "roi_padding_pixels": 2}

def runtime_identity(config, dependencies, runtime_package=None):
    # Match Rust struct field order and serde_json's compact UTF-8 encoding.
    fields = ("enabled", "engine_path", "tessdata_path", "languages", "operator_revision",
              "dpi", "oem", "psm", "detector_version", "protocol_version")
    ordered_config = {key: config[key] for key in fields}
    dependencies = sorted(dependencies, key=lambda d: d["name"])
    components = [ordered_config, RETRY_POLICY, INSPECTION_POLICY, dependencies]
    if runtime_package is not None:
        runtime_package = {key: runtime_package[key] for key in
                           ('schema', 'manifest_sha256', 'source_sha', 'python_path')}
        components.append(runtime_package)
    encoded = json.dumps(components,
                         ensure_ascii=False, separators=(",", ":")).encode()
    result = {"config": ordered_config, "retry_policy": RETRY_POLICY,
            "inspection_policy": INSPECTION_POLICY,
            "dependencies": dependencies, "sha256": "sha256:" + hashlib.sha256(encoded).hexdigest()}
    if runtime_package is not None:
        result['runtime_package'] = runtime_package
    return result


def runtime_manifest(path):
    def unique(pairs):
        result = {}
        for key, value in pairs:
            if key in result:
                raise ValueError('duplicate runtime manifest field')
            result[key] = value
        return result
    descriptor = os.open(path, os.O_RDONLY | os.O_NONBLOCK | os.O_CLOEXEC | os.O_NOFOLLOW)
    with os.fdopen(descriptor, 'rb') as stream:
        info = os.fstat(stream.fileno())
        if not stat.S_ISREG(info.st_mode) or info.st_size > 32 * 1024 * 1024:
            raise ValueError('runtime manifest must be a bounded regular file')
        encoded = stream.read(32 * 1024 * 1024 + 1)
        if len(encoded) != info.st_size:
            raise ValueError('runtime manifest changed or exceeded limit')
    manifest = json.loads(encoded, object_pairs_hook=unique)
    if (manifest.get('schema') != 'ocr-private-runtime-archive/v1'
            or not re.fullmatch('[0-9a-f]{40}', manifest.get('source_sha', ''))):
        raise ValueError('invalid runtime manifest identity')
    return manifest, {'schema': manifest['schema'],
        'manifest_sha256': hashlib.sha256(encoded).hexdigest(),
        'source_sha': manifest['source_sha'], 'python_path': '/opt/ocr-python/bin/python'}


def verify_runtime_package(root, manifest_path):
    """Startup only, before trusting the installed interpreter or a parsed-cache hit.

    Read each inventory entry relative to an owned directory fd. Archive symlinks
    are compared, never followed into the host. The caller supplies a five-second
    process budget; no document bytes are read by this mode.
    """
    from pathlib import PurePosixPath
    manifest, identity = runtime_manifest(manifest_path)
    entries = manifest.get('entries')
    if not isinstance(entries, list) or not 0 < len(entries) <= 50000:
        raise ValueError('invalid runtime inventory count')
    indexed = {}
    total = 0
    for item in entries:
        name = item.get('path')
        if (not isinstance(name, str) or not name or len(name.encode()) > 4096
                or '\0' in name or PurePosixPath(name).is_absolute()
                or '..' in PurePosixPath(name).parts or str(PurePosixPath(name)) != name
                or name == '.' or name in indexed):
            raise ValueError('invalid runtime inventory path')
        indexed[name] = item
        kind = item.get('kind')
        fields = {'path', 'kind'}
        if kind in ('file', 'directory'):
            fields.add('mode')
            if type(item.get('mode')) is not int or not 0 <= item['mode'] <= 0o7777:
                raise ValueError('invalid runtime inventory mode')
        if kind == 'file':
            fields |= {'bytes', 'sha256'}
            if (type(item.get('bytes')) is not int or not 0 <= item['bytes'] <= 256 * 1024 * 1024
                    or not re.fullmatch('[0-9a-f]{64}', item.get('sha256', ''))):
                raise ValueError('invalid runtime inventory file')
            total += item['bytes']
        elif kind == 'symlink':
            fields.add('target')
            target = item.get('target')
            if not isinstance(target, str) or not target or '\0' in target or len(target.encode()) > 4096:
                raise ValueError('invalid runtime inventory symlink')
            components = [] if target.startswith('/') else list(PurePosixPath(name).parent.parts)
            for part in PurePosixPath(target).parts:
                if part in ('/', '.'):
                    continue
                if part == '..':
                    if not components:
                        raise ValueError('runtime symlink escapes root')
                    components.pop()
                else:
                    components.append(part)
        elif kind != 'directory':
            raise ValueError('invalid runtime inventory kind')
        if set(item) != fields:
            raise ValueError('unexpected runtime inventory fields')
    if (list(indexed) != sorted(indexed) or total > 1024 * 1024 * 1024
            or type(manifest.get('regular_file_bytes')) is not int
            or manifest['regular_file_bytes'] != total):
        raise ValueError('runtime inventory size/order mismatch')
    for name in indexed:
        for parent in PurePosixPath(name).parents:
            if str(parent) != '.' and indexed.get(str(parent), {}).get('kind') != 'directory':
                raise ValueError('runtime inventory non-directory ancestor')
    provenance = {}
    for name in ('engine-manifest.json', 'python-manifest.json', 'requirements.lock', 'apt-source-uris.txt'):
        record = indexed.get('usr/share/doc/reading-mcp-ocr/' + name, {})
        if record.get('kind') != 'file':
            raise ValueError('runtime provenance missing')
        provenance[name] = record['sha256']
    if manifest.get('provenance') != provenance:
        raise ValueError('runtime provenance mismatch')
    root_fd = os.open(root, os.O_RDONLY | os.O_DIRECTORY | os.O_CLOEXEC | os.O_NOFOLLOW)
    try:
        for name, item in indexed.items():
            parent = os.dup(root_fd)
            try:
                parts = name.split('/')
                for part in parts[:-1]:
                    child = os.open(part, os.O_RDONLY | os.O_DIRECTORY | os.O_CLOEXEC | os.O_NOFOLLOW, dir_fd=parent)
                    os.close(parent)
                    parent = child
                info = os.stat(parts[-1], dir_fd=parent, follow_symlinks=False)
                if item['kind'] == 'symlink':
                    if not stat.S_ISLNK(info.st_mode) or os.readlink(parts[-1], dir_fd=parent) != item['target']:
                        raise ValueError('runtime symlink mismatch')
                    continue
                if stat.S_IMODE(info.st_mode) != item['mode']:
                    raise ValueError('runtime mode mismatch')
                if item['kind'] == 'directory':
                    if not stat.S_ISDIR(info.st_mode):
                        raise ValueError('runtime directory mismatch')
                    continue
                descriptor = os.open(parts[-1], os.O_RDONLY | os.O_NONBLOCK | os.O_CLOEXEC | os.O_NOFOLLOW, dir_fd=parent)
                with os.fdopen(descriptor, 'rb') as stream:
                    current = os.fstat(stream.fileno())
                    if (not stat.S_ISREG(current.st_mode) or current.st_size != item['bytes']
                            or stat.S_IMODE(current.st_mode) != item['mode']):
                        raise ValueError('runtime file type/size/mode mismatch')
                    digest, size = hashlib.sha256(), 0
                    for chunk in iter(lambda: stream.read(65536), b''):
                        size += len(chunk)
                        if size > item['bytes']:
                            raise ValueError('runtime file grew during verification')
                        digest.update(chunk)
                    if size != item['bytes'] or digest.hexdigest() != item['sha256']:
                        raise ValueError('runtime file digest mismatch')
            finally:
                os.close(parent)
        # Extra modules/libraries must not silently enter the installed runtime.
        observed = set()
        def walk_error(error):
            raise error
        for directory, dirs, files, _ in os.fwalk('.', dir_fd=root_fd, follow_symlinks=False, onerror=walk_error):
            prefix = '' if directory == '.' else directory[2:] + '/'
            for leaf in dirs + files:
                name = prefix + leaf
                if name not in indexed:
                    raise ValueError('unlisted runtime entry')
                observed.add(name)
        if observed != set(indexed):
            raise ValueError('missing runtime entry')
    finally:
        os.close(root_fd)
    return identity

def dependency_sha256(path):
    descriptor = os.open(path, os.O_RDONLY | os.O_NONBLOCK | os.O_CLOEXEC)
    with os.fdopen(descriptor, "rb") as stream:
        if not stat.S_ISREG(os.fstat(stream.fileno()).st_mode):
            raise ValueError("OCR dependency must be a regular file")
        digest = hashlib.sha256()
        for chunk in iter(lambda: stream.read(64 * 1024), b""):
            digest.update(chunk)
        return digest.hexdigest()

def dependency_command_output(command, timeout=5.0):
    """Bound dependency discovery before PDF processing, without buffering arbitrary output."""
    deadline = time.monotonic() + timeout
    process = subprocess.Popen(command, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
                               stderr=subprocess.PIPE, env=OCR_PROCESS_ENV, start_new_session=True)
    streams = [bytearray(), bytearray()]
    try:
        with selectors.DefaultSelector() as selector:
            for index, stream in enumerate((process.stdout, process.stderr)):
                os.set_blocking(stream.fileno(), False)
                selector.register(stream, selectors.EVENT_READ, index)
            while True:
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    raise OcrStageFailure('OCR_TIMEOUT', "OCR dependency discovery timed out")
                for key, _ in selector.select(min(remaining, .01)):
                    chunk = os.read(key.fd, 8192)
                    if not chunk:
                        selector.unregister(key.fileobj)
                    elif len(streams[key.data]) + len(chunk) > 64 * 1024:
                        raise OcrStageFailure('OCR_RESOURCE_LIMIT', "OCR dependency output limit exceeded")
                    else:
                        streams[key.data].extend(chunk)
                # Reserve PID/group until cleanup; poll()/wait() here would reap it.
                exited = os.waitid(os.P_PID, process.pid, os.WEXITED | os.WNOHANG | os.WNOWAIT)
                if exited is not None and not selector.get_map():
                    break
    finally:
        try:
            os.killpg(process.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        process.wait()
        process.stdout.close()
        process.stderr.close()
    return subprocess.CompletedProcess(command, process.returncode,
                                       bytes(streams[0]).decode("utf-8", "strict"),
                                       bytes(streams[1]).decode("utf-8", "strict"))

def fingerprint_dependencies(config):
    sha = dependency_sha256
    paths = [("engine", config["engine_path"])] + [(f"model:{lang}", os.path.join(config["tessdata_path"], lang + ".traineddata")) for lang in config["languages"]]
    output = dependency_command_output(["/usr/bin/ldd", config["engine_path"]])
    if output.returncode != 0 or "not found" in output.stdout or "not found" in output.stderr: raise RuntimeError("OCR dependency ldd failure")
    libraries = sorted({token for token in output.stdout.split() if token.startswith("/")})
    paths += [("library:" + path, path) for path in libraries]
    return [{"name": name, "sha256": sha(path)} for name, path in sorted(paths)]

def cjk(value):
    return value and ("\u3400" <= value <= "\u9fff" or "\uf900" <= value <= "\ufaff")


def prepare_page_raster(page, dpi=None):
    import pymupdf
    dpi = OCR_CONFIG['dpi'] if dpi is None else dpi
    pixels = raster_pixel_count(page, dpi)
    if RASTER_BUDGET is not None:
        RASTER_BUDGET.reserve_raster(pixels)
    return page.get_pixmap(dpi=dpi, colorspace=pymupdf.csRGB, alpha=False)

def blank_raster_evidence(page, pixmap):
    samples = pixmap.samples
    if (pixmap.n != 3 or len(samples) != pixmap.width * pixmap.height * 3
            or samples.count(b"\xff") != len(samples) or page.get_texttrace()):
        return None
    return {"width": pixmap.width, "height": pixmap.height, "channels": 3,
            "white_samples": len(samples), "glyph_spans": 0,
            "samples_sha256": hashlib.sha256(samples).hexdigest()}

def native_raster_coverage(page, pixmap, regions, dpi=None):
    # Exact white-background coverage, never an English length/confidence rule.
    # Unsupported transforms do not prove coverage and therefore require OCR.
    if not regions or page.rotation != 0 or pixmap.n != 3:
        return None
    samples = pixmap.samples
    if len(samples) != pixmap.width * pixmap.height * 3:
        raise ValueError('invalid coverage raster')
    masked = bytearray(samples)
    masks = []
    scale = (OCR_CONFIG['dpi'] if dpi is None else dpi) / 72
    compact = lambda text: ''.join(unicodedata.normalize('NFKC', text).split())
    for span in page.get_texttrace():
        page_time_remaining()
        try:
            text = ''.join(chr(character[0]) for character in span['chars'])
            bbox = list(span['bbox'])
        except (KeyError, ValueError, TypeError):
            continue
        if (not compact(text) or '\ufffd' in text or '\0' in text or len(bbox) != 4
                or not all(math.isfinite(value) for value in bbox)
                or not (0 <= bbox[0] < bbox[2] <= page.rect.width
                        and 0 <= bbox[1] < bbox[3] <= page.rect.height)):
            continue
        x, y = (bbox[0] + bbox[2]) / 2, (bbox[1] + bbox[3]) / 2
        candidates = [region for region in regions
            if region['bbox'][0] <= x <= region['bbox'][2]
            and region['bbox'][1] <= y <= region['bbox'][3]
            and compact(text) in compact(region['text'])]
        if len(candidates) != 1:
            continue
        pixel_bbox = [max(0, math.floor(bbox[0] * scale) - 2),
                      max(0, math.floor(bbox[1] * scale) - 2),
                      min(pixmap.width, math.ceil(bbox[2] * scale) + 2),
                      min(pixmap.height, math.ceil(bbox[3] * scale) + 2)]
        x0, y0, x1, y1 = pixel_bbox
        for row in range(y0, y1):
            start = (row * pixmap.width + x0) * 3
            masked[start:start + (x1 - x0) * 3] = b'\xff' * ((x1 - x0) * 3)
        masks.append({'source_box': candidates[0]['source_box'], 'text': text,
                      'bbox': bbox, 'pixel_bbox': pixel_bbox})
    return {'schema': 'ocr-native-raster-coverage/v1', 'width': pixmap.width,
            'height': pixmap.height, 'padding_pixels': 2,
            'source_samples_sha256': hashlib.sha256(samples).hexdigest(),
            'masked_samples_sha256': hashlib.sha256(masked).hexdigest(),
            'uncovered_samples': len(masked) - masked.count(255), 'masks': masks}

def require_disabled_page_coverage(page, page_layout):
    """Never publish a native subset while an unrepresented raster remains.

    Existing layout visual regions retain their original-page evidence. This is
    coverage inspection only: it does not classify an image as recognized text.
    No OCR configuration, models, engine or subprocess is needed.
    """
    global PAGE_DEADLINE
    if not page.get_image_info():
        return
    if not has_native_body(page_layout):
        raise OcrRequired('image-only page requires local OCR inspection')
    previous = PAGE_DEADLINE
    deadline = time.monotonic() + 15
    PAGE_DEADLINE = deadline if previous is None else min(previous, deadline)
    try:
        pixmap = prepare_page_raster(page, dpi=300)
        coverage = native_raster_coverage(page, pixmap, native_text_regions(page_layout), dpi=300)
        page_time_remaining()
        if coverage is None or not coverage['masks']:
            raise OcrRequired('native image coverage cannot be proven')
        if coverage['uncovered_samples'] == 0:
            return
        visual_boxes = [box for box in page_layout['boxes']
                        if box.get('boxclass') in ('image', 'picture', 'figure', 'table')]
        if not visual_boxes:
            # Coverage already counted unknown samples. No visual source can
            # explain them, so avoid allocating and rescanning a second copy.
            raise OcrRequired('mixed page contains raster content without source coverage')
        masked = bytearray(pixmap.samples)
        rectangles = [mask['pixel_bbox'] for mask in coverage['masks']]
        for box in visual_boxes:
            bbox = [box[key] for key in ('x0', 'y0', 'x1', 'y1')]
            if (not all(math.isfinite(value) for value in bbox)
                    or not (0 <= bbox[0] < bbox[2] <= page.rect.width
                            and 0 <= bbox[1] < bbox[3] <= page.rect.height)):
                raise OcrRequired('invalid original visual coverage')
            # Use only the actual retained layout rectangle, without padding.
            # Pixels crossing the boundary remain unknown instead of being erased.
            scale = 300 / 72
            rectangles.append([math.ceil(bbox[0] * scale), math.ceil(bbox[1] * scale),
                               math.floor(bbox[2] * scale), math.floor(bbox[3] * scale)])
        for x0, y0, x1, y1 in rectangles:
            page_time_remaining()
            for row in range(y0, y1):
                start = (row * pixmap.width + x0) * 3
                masked[start:start + max(0, x1 - x0) * 3] = b'\xff' * (max(0, x1 - x0) * 3)
        page_time_remaining()
        if masked.count(255) != len(masked):
            raise OcrRequired('mixed page contains raster content without source coverage')
    finally:
        PAGE_DEADLINE = previous

def ocr_page(page, language=None, excluded_regions=()):
    """Run the deployer-selected local Tesseract and retain engine grouping."""
    config = OCR_CONFIG
    if excluded_regions:
        raise ValueError("native exclusions require the observation-preserving wrapper")
    if not config.get("enabled", False):
        return None
    language = "+".join(config["languages"])
    page_time_remaining()
    pixmap = PAGE_RASTER if PAGE_RASTER is not None else prepare_page_raster(page)
    if RASTER_BUDGET is not None:
        RASTER_BUDGET.require_page(page.number)
    with tempfile.TemporaryDirectory(prefix="reading-mcp-ocr-") as directory:
        image = os.path.join(directory, "page.png")
        output = os.path.join(directory, "words")
        scale_x = pixmap.width / page.rect.width
        scale_y = pixmap.height / page.rect.height
        pixmap.save(image)
        command = [config["engine_path"], image, output,
                   "--tessdata-dir", config["tessdata_path"],
                   "-l", language, "--oem", str(config["oem"]), "--psm", str(config["psm"]), "--dpi", str(config["dpi"]), "tsv"]
        try:
            subprocess.run(command, check=True, stdout=subprocess.DEVNULL,
                           stderr=subprocess.PIPE, timeout=page_time_remaining(), env=OCR_PROCESS_ENV)
        except FileNotFoundError as error:
            raise OcrStageFailure('OCR_UNAVAILABLE', "local OCR engine is not installed") from error
        except subprocess.TimeoutExpired as error:
            raise OcrStageFailure('OCR_TIMEOUT', "local OCR page exceeded 15 second budget") from error
        rows = []
        with open(output + ".tsv", encoding="utf-8", newline="") as stream:
            for row in csv.DictReader(stream, delimiter="\t"):
                if row.get("level") == "5" and row.get("text", "").strip():
                    rows.append(row)
        lines = {}
        for row in rows:
            key = (int(row["block_num"]), int(row["par_num"]), int(row["line_num"]))
            lines.setdefault(key, []).append(row)
        textlines = []
        for key, words in lines.items():
            words.sort(key=lambda item: int(item["left"]))
            text = ""
            for item in words:
                token = item["text"]
                if text and not (cjk(text[-1]) and cjk(token[0])):
                    text += " "
                text += token
            left = min(int(item["left"]) for item in words) / scale_x
            top = min(int(item["top"]) for item in words) / scale_y
            right = max(int(item["left"]) + int(item["width"]) for item in words) / scale_x
            bottom = max(int(item["top"]) + int(item["height"]) for item in words) / scale_y
            textlines.append({"text": text, "bbox": [left, top, right, bottom],
                              "spans": [{"text": item["text"], "bbox": [int(item["left"]) / scale_x,
                              int(item["top"]) / scale_y,
                              (int(item["left"]) + int(item["width"])) / scale_x,
                              (int(item["top"]) + int(item["height"])) / scale_y],
                              "flags": 0, "ocr_block": key[0], "ocr_paragraph": key[1],
                              "ocr_line": key[2], "confidence": float(item["conf"])} for item in words]})
        if not textlines:
            return None
        boxes = []
        by_block = {}
        for line in textlines:
            first = line["spans"][0]
            by_block.setdefault((first["ocr_block"], first["ocr_paragraph"]), []).append(line)
        for (block, paragraph), block_lines in by_block.items():
            x0, y0 = min(l["bbox"][0] for l in block_lines), min(l["bbox"][1] for l in block_lines)
            x1, y1 = max(l["bbox"][2] for l in block_lines), max(l["bbox"][3] for l in block_lines)
            boxes.append({"boxclass": "text", "bbox": [x0, y0, x1, y1], "x0": x0, "y0": y0, "x1": x1, "y1": y1,
                         "textlines": block_lines,
                         "ocr_block": block, "ocr_paragraph": paragraph})
        return boxes

def _bbox_area(box):
    b=box["bbox"]; return max(0,b[2]-b[0])*max(0,b[3]-b[1])
def _bbox_overlap(a,b):
    return max(0,min(a[2],b[2])-max(a[0],b[0]))*max(0,min(a[3],b[3])-max(a[1],b[1]))
def _merge_components(pairs):
    parent={i:i for pair in pairs for i in pair}
    def find(x):
        while parent[x]!=x: parent[x]=parent[parent[x]]; x=parent[x]
        return x
    for a,b in pairs:
        a,b=find(a),find(b)
        if a!=b: parent[b]=a
    groups={}
    for i in parent: groups.setdefault(find(i),set()).add(i)
    return list(groups.values())
def _adjacent(a,b,gap):
    for x in [l["bbox"] for l in a.get("textlines",[])]:
        for y in [l["bbox"] for l in b.get("textlines",[])]:
            vertical=max(0,min(x[3],y[3])-max(x[1],y[1])); horizontal=max(0,max(x[0],y[0])-min(x[2],y[2]))
            if vertical >= min(x[3]-x[1],y[3]-y[1])*.5 and horizontal <= gap: return True
    return False

def _attempt_reference(page, attempt, box, line=None, word=None):
    reference = {"page": page, "attempt": attempt, "box": box}
    if line is not None:
        reference["line"] = line
    if word is not None:
        reference["word"] = word
    return reference

def _regional_evidence(page, bounds, primary, retry, selected, components):
    """References address immutable observations, not projected text or gold."""
    attempts = [{"id": "primary", "psm": 3, "boxes": primary}]
    if retry is not None:
        attempts.append({"id": "retry", "psm": 6, "boxes": retry})
    selection = []
    for selected_index, box in enumerate(selected):
        matches = [(attempt, index) for attempt in attempts
                   for index, observed in enumerate(attempt["boxes"]) if observed is box]
        if len(matches) != 1:
            raise ValueError("OCR selected box has ambiguous attempt reference")
        attempt, index = matches[0]
        selection.append({
            "selected_box": selected_index,
            "source": _attempt_reference(page, attempt["id"], index),
            "words": [_attempt_reference(page, attempt["id"], index, line_index, word_index)
                      for line_index, line in enumerate(box.get("textlines", []))
                      for word_index, _ in enumerate(line.get("spans", []))],
        })
    return {"schema": "ocr-regional-observations/v3", "page": page, "page_bounds": bounds,
            "complete": all(component["resolved"] for component in components),
            "attempts": attempts, "selection": selection, "components": components}

def native_text_regions(page_layout):
    regions = []
    for index, box in enumerate(page_layout["boxes"]):
        text = "\n".join(line_text(line) for line in (box.get("textlines") or []))
        if text.strip():
            regions.append({"source_box": index, "source_class": box["boxclass"],
                            "bbox": [box[k] for k in ("x0", "y0", "x1", "y1")], "text": text})
    return regions

def exclude_native_boxes(selected, evidence, regions):
    evidence["native_regions"] = regions
    evidence["excluded_sources"] = []
    retained, selection = [], []
    for box, chosen in zip(selected, evidence["selection"]):
        centers = [((w["bbox"][0] + w["bbox"][2]) / 2, (w["bbox"][1] + w["bbox"][3]) / 2)
                   for line in box["textlines"] for w in line["spans"]]
        covered = [any(r["bbox"][0] <= x <= r["bbox"][2] and r["bbox"][1] <= y <= r["bbox"][3]
                       for r in regions) for x, y in centers]
        if covered and all(covered):
            evidence["excluded_sources"].append(chosen["source"])
        else:
            if any(covered):
                evidence["complete"] = False
                evidence["projection_failure"] = "partial_native_overlap"
            retained.append(box)
            selection.append(dict(chosen, selected_box=len(selection)))
    evidence["selection"] = selection
    return retained, evidence

def merge_native_order(original, selected, evidence):
    """Merge unique native anchors into the retained engine sequence.

    No text matching/reordering by gold or coordinate sort. Ambiguous anchors
    cannot authorize a complete projection. All raw observations stay intact.
    """
    if not evidence.get('native_regions') or not selected or not evidence['complete']:
        return original + selected
    count = len(original)
    attempts = {attempt['id']: attempt for attempt in evidence['attempts']}
    replaced = {ref['box'] for component in evidence['components'] for ref in component['primary_refs']}
    replacements = {component['indices'][0]: component['candidate_refs'] for component in evidence['components']}
    sources = []
    for index in range(len(attempts['primary']['boxes'])):
        sources.extend(replacements.get(index, []))
        if index not in replaced:
            sources.append(_attempt_reference(evidence['page'], 'primary', index))
    auxiliary = {'page-header', 'page-footer', 'footnote', 'caption'}
    entries, ordered, native_ids, body_ids = [], [], set(), []
    try:
        for source in sources:
            page_time_remaining()
            if source in evidence['excluded_sources']:
                box = attempts[source['attempt']]['boxes'][source['box']]
                centers = [((word['bbox'][0] + word['bbox'][2]) / 2,
                            (word['bbox'][1] + word['bbox'][3]) / 2)
                           for line in box['textlines'] for word in line['spans']]
                candidates = [region for region in evidence['native_regions'] if centers and
                    all(region['bbox'][0] <= x <= region['bbox'][2] and
                        region['bbox'][1] <= y <= region['bbox'][3] for x, y in centers)]
                if len(candidates) != 1 or candidates[0]['source_box'] in native_ids:
                    raise ValueError('nonunique native anchor')
                region = candidates[0]
                index = region['source_box']
                native_ids.add(index)
                if region['source_class'] not in auxiliary:
                    body_ids.append(index)
                entries.append({'origin':'native', 'source_box':index, 'projected_box':index})
                ordered.append(dict(original[index], _original_box=index))
            else:
                indices = [index for index, entry in enumerate(evidence['selection']) if entry['source'] == source]
                if len(indices) != 1:
                    raise ValueError('missing selected source')
                index = indices[0]
                entries.append({'origin':'local_ocr', 'source':source, 'projected_box':count + index})
                ordered.append(dict(selected[index], _original_box=count + index))
        required = sorted(region['source_box'] for region in evidence['native_regions']
                          if region['source_class'] not in auxiliary)
        if body_ids != required:
            raise ValueError('native order missing or inconsistent with engine order')
    except (ValueError, IndexError, KeyError):
        evidence['complete'] = False
        evidence['projection_failure'] = 'unproven_mixed_order'
        return original + selected
    # Nontext regions and unanchored notes are retained, not promoted into body.
    prefix = [dict(box, _original_box=index) for index, box in enumerate(original) if index not in native_ids]
    evidence['mixed_order'] = {'schema':'ocr-native-anchor-order/v1',
                              'original_box_count':count, 'entries':entries}
    return prefix + ordered

def _regional_ocr(page, excluded_regions=(), inspect_native=False, original_boxes=None):
    global PAGE_DEADLINE, PAGE_RASTER
    previous = PAGE_DEADLINE
    previous_raster = PAGE_RASTER
    deadline = time.monotonic() + 15
    PAGE_DEADLINE = deadline if previous is None else min(previous, deadline)
    try:
        PAGE_RASTER = prepare_page_raster(page)
        blank = blank_raster_evidence(page, PAGE_RASTER)
        if blank is not None:
            page_time_remaining()
            return [], {"schema": "ocr-regional-observations/v3", "page": page.number + 1,
                        "page_bounds": [0, 0, page.rect.width, page.rect.height],
                        "complete": True, "attempts": [], "selection": [], "components": [],
                        "blank_raster": blank}
        coverage = native_raster_coverage(page, PAGE_RASTER, excluded_regions) if inspect_native else None
        if coverage is not None and coverage['uncovered_samples'] == 0 and coverage['masks']:
            page_time_remaining()
            return [], {'schema': 'ocr-regional-observations/v3', 'page': page.number + 1,
                        'page_bounds': [0, 0, page.rect.width, page.rect.height],
                        'complete': True, 'attempts': [], 'selection': [], 'components': [],
                        'native_regions': list(excluded_regions), 'native_coverage': coverage}
        selected, evidence = _regional_ocr_attempts(page)
        if coverage is not None:
            evidence['native_coverage'] = coverage
        result = exclude_native_boxes(selected, evidence, list(excluded_regions))
        if original_boxes is not None:
            result = (merge_native_order(original_boxes, result[0], result[1]), result[1])
        page_time_remaining()
        return result
    finally:
        PAGE_DEADLINE = previous
        PAGE_RASTER = previous_raster

def _regional_ocr_attempts(page, excluded_regions=()):
    global OCR_CONFIG
    primary=ocr_page(page, excluded_regions=excluded_regions); primary=primary or []
    conflicts=[(i,j) for i,a in enumerate(primary) for j,b in enumerate(primary[i+1:],i+1)
               if min(_bbox_area(a),_bbox_area(b)) and _bbox_overlap(a["bbox"],b["bbox"])/min(_bbox_area(a),_bbox_area(b))>=.9]
    components=_merge_components(conflicts)
    heights=[l["bbox"][3]-l["bbox"][1] for b in primary for l in b.get("textlines",[]) if l["bbox"][3]>l["bbox"][1]]
    gap=statistics.median(heights) if heights else 0
    changed=True
    while changed:
        changed=False
        for component in components:
            for i,box in enumerate(primary):
                if i not in component and any(_adjacent(primary[j],box,gap) for j in component): component.add(i); changed=True
    changed=True
    while changed:
        changed=False
        for i in range(len(components)):
            for j in range(i+1,len(components)):
                if components[i] & components[j]:
                    components[i].update(components.pop(j)); changed=True; break
            if changed: break
    page_number = page.number + 1
    if not components:
        return primary, _regional_evidence(page_number, [0,0,page.rect.width,page.rect.height], primary, None, primary, [])
    original=list(primary); retry_config=dict(OCR_CONFIG); retry_config["psm"]=6
    page_time_remaining()
    saved=OCR_CONFIG; OCR_CONFIG=retry_config
    try: retry=ocr_page(page, excluded_regions=excluded_regions) or []
    finally: OCR_CONFIG=saved
    tolerance=2*72/saved["dpi"]; rect=[0,0,page.rect.width,page.rect.height]; replacements=[]; diagnostics=[]
    for component in components:
        raw=[min(original[i]["bbox"][k] for i in component) for k in (0,1)]+[max(original[i]["bbox"][k] for i in component) for k in (2,3)]
        roi=[max(rect[0],raw[0]-tolerance),max(rect[1],raw[1]-tolerance),min(rect[2],raw[2]+tolerance),min(rect[3],raw[3]+tolerance)]
        candidate_indices=[i for i,b in enumerate(retry) if all(b["bbox"][k]>=roi[k] for k in (0,1)) and all(b["bbox"][k]<=roi[k] for k in (2,3))]
        candidates=[retry[i] for i in candidate_indices]
        centers=[((s["bbox"][0]+s["bbox"][2])/2,(s["bbox"][1]+s["bbox"][3])/2) for i in component for l in original[i].get("textlines",[]) for s in l.get("spans",[])]
        covered=all(any(l["bbox"][0]<=x<=l["bbox"][2] and l["bbox"][1]<=y<=l["bbox"][3] for b in candidates for l in b.get("textlines",[])) for x,y in centers)
        resolved=bool(candidates) and covered
        diagnostics.append({"indices":sorted(component),"roi":raw,"effective_roi":roi,
                            "candidate_count":len(candidates),"resolved":resolved,
                            "failure":None if resolved else "candidate_does_not_cover_component",
                            "primary_refs":[_attempt_reference(page_number,"primary",i) for i in sorted(component)],
                            "candidate_refs":[_attempt_reference(page_number,"retry",i) for i in candidate_indices],
                            "replaced_refs":[_attempt_reference(page_number,"primary",i) for i in sorted(component)] if resolved else []})
        if resolved: replacements.append((min(component),set(component),candidates))
    members=set().union(*(m for _,m,_ in replacements)) if replacements else set(); selected=[]
    by_first={i:c for i,_,c in replacements}
    for i,box in enumerate(original):
        if i in by_first: selected.extend(by_first[i])
        if i not in members: selected.append(box)
    return selected, _regional_evidence(page_number, rect, original, retry, selected, diagnostics)


def line_text(line):
    is_ocr_line = any(span.get("ocr_block") is not None for span in line["spans"])
    result = ""
    previous = None
    for span in line["spans"]:
        # Preserve the engine's actual characters.  Compatibility folding (NFKC)
        # would silently turn full-width/CJK punctuation into different output.
        text = unicodedata.normalize("NFC" if is_ocr_line else "NFKC", span["text"])
        if previous and result and text and not result[-1].isspace() and not text[0].isspace():
            gap = span["bbox"][0] - previous["bbox"][2]
            # Style changes and superscripts do not introduce word boundaries.
            if span.get("size") is not None and previous.get("size") is not None:
                separates = gap > min(span["size"], previous["size"]) * 0.12
            else:
                separates = gap > 2.0
            left, right = result[-1], text[0]
            left_punct = unicodedata.category(left).startswith("P")
            right_punct = unicodedata.category(right).startswith("P")
            no_boundary = cjk(left) and cjk(right)
            if is_ocr_line:
                no_boundary = (no_boundary
                               or (right_punct and (cjk(left) or left_punct))
                               or (left_punct and (cjk(right) or right_punct)))
            if separates and not no_boundary:
                result += " "
        result += text
        previous = span
    return result.strip() if is_ocr_line else " ".join(result.split())


def join_lines(lines, vocabulary):
    text = ""
    uncertain = 0
    for line in lines:
        if not line:
            continue
        if text.endswith("\u00ad"):
            text = text[:-1] + line
        elif text.endswith("-"):
            left = re.search(r"([\w-]+)-$", text)
            right = re.match(r"([\w-]+)", line)
            if left and right:
                a, b = left[1], right[1]
                combined = a + b
                # Only remove a visible hyphen with independent, unbroken evidence
                # in this PDF. Preserve compounds, capitalization and ambiguity.
                if a.islower() and b.islower() and "-" not in combined and combined in vocabulary and a + "-" + b not in vocabulary:
                    text = text[:-1] + line
                else:
                    text += line
                    uncertain += 1
            else:
                text += " " + line
        else:
            text += (" " if text else "") + line
    return text, uncertain


def project_visual_observations(page_number, page_rect, raster_size, boxes, predictions):
    """Candidate pure adapter; raw observations and engine order are immutable.

    This helper does not load a model or choose its configuration. The caller
    must preserve and bind the complete model attempt before publication. No
    runtime path enables it until that typed identity/evidence wiring exists.
    """
    import copy
    if (len(page_rect) != 4 or page_rect[:2] != [0, 0]
            or not all(math.isfinite(v) for v in page_rect)
            or page_rect[2] <= 0 or page_rect[3] <= 0
            or len(raster_size) != 2 or any(type(v) is not int or v <= 0 for v in raster_size)):
        raise ValueError('unsupported visual page transform')
    regions, text_regions = [], []
    for index, prediction in enumerate(predictions):
        if prediction['label'] not in ('image', 'formula', 'table', 'text'):
            continue
        bounds = prediction['coordinate']
        score = prediction['score']
        if (len(bounds) != 4 or not all(math.isfinite(v) for v in bounds)
                or not math.isfinite(score) or not .5 <= score <= 1
                or not (0 <= bounds[0] < bounds[2] <= raster_size[0]
                        and 0 <= bounds[1] < bounds[3] <= raster_size[1])):
            raise ValueError('invalid selected visual observation')
        bbox = [bounds[0] * page_rect[2] / raster_size[0],
                bounds[1] * page_rect[3] / raster_size[1],
                bounds[2] * page_rect[2] / raster_size[0],
                bounds[3] * page_rect[3] / raster_size[1]]
        destination = text_regions if prediction['label'] == 'text' else regions
        destination.append({'prediction_index':index, 'label':prediction['label'],
                        'model_score':score, 'pixel_bbox':list(bounds), 'bbox':bbox,
                        'box_sources':[]})
    projected = copy.deepcopy(boxes)
    bindings, failures = [], []
    for index, box in enumerate(boxes):
        if box.get('ocr_block') is None:
            continue
        words = [span for line in box.get('textlines', []) for span in line['spans']]
        if not words:
            continue
        centers = [((word['bbox'][0] + word['bbox'][2]) / 2,
                    (word['bbox'][1] + word['bbox'][3]) / 2) for word in words]
        matches = []
        partial = False
        for region in regions:
            b = region['bbox']
            included = [b[0] <= x <= b[2] and b[1] <= y <= b[3] for x, y in centers]
            if all(included):
                matches.append(region)
            elif any(included):
                partial = True
        if partial or len(matches) > 1:
            failures.append({'source_box':index, 'reason':'ambiguous_visual_word_coverage'})
            continue
        if not matches:
            continue
        region = matches[0]
        region['box_sources'].append(index)
        source_bbox = [box[key] for key in ('x0', 'y0', 'x1', 'y1')]
        # Model bounds may exclude a small part of an observed word. Preserve
        # BOTH geometries, never crop away source glyphs to fit a prediction.
        b = region['bbox']
        bbox = [min(b[0], source_bbox[0]), min(b[1], source_bbox[1]),
                max(b[2], source_bbox[2]), max(b[3], source_bbox[3])]
        projected[index]['boxclass'] = 'table' if region['label'] == 'table' else region['label']
        for key, value in zip(('x0', 'y0', 'x1', 'y1'), bbox):
            projected[index][key] = value
        projected[index]['bbox'] = bbox
        bindings.append({'source_box':index, 'prediction_index':region['prediction_index'],
                         'source_bbox':source_bbox, 'projected_bbox':bbox})
    unanchored = [region['prediction_index'] for region in regions if not region['box_sources']]
    def text_region(box):
        centers = [((word['bbox'][0] + word['bbox'][2]) / 2,
                    (word['bbox'][1] + word['bbox'][3]) / 2)
                   for line in box.get('textlines', []) for word in line['spans']]
        matches = [region['prediction_index'] for region in text_regions if centers and
                   all(region['bbox'][0] <= x <= region['bbox'][2] and
                       region['bbox'][1] <= y <= region['bbox'][3] for x,y in centers)]
        return matches[0] if len(matches) == 1 else None
    ordered, source_groups, merges = [], [], []
    for index, box in enumerate(projected):
        model_ref = text_region(box)
        if (ordered and box.get('ocr_block') is not None and ordered[-1].get('ocr_block') is not None
                and box['boxclass'] == ordered[-1]['boxclass'] == 'text'
                and model_ref is not None and model_ref == text_region(ordered[-1])):
            before = ' '.join(line_text(line) for line in ordered[-1]['textlines']).strip()
            after = ' '.join(line_text(line) for line in box['textlines']).strip()
            heights = [line['bbox'][3] - line['bbox'][1]
                       for item in (ordered[-1], box) for line in item['textlines']]
            height = statistics.median(heights)
            gap = box['y0'] - ordered[-1]['y1']
            continuation = (before and after and before[-1] not in '.!?。！？:;；：“”"'
                            and (after[0].islower() or (cjk(before[-1]) and cjk(after[0]))))
            if continuation and 0 <= gap <= height and abs(box['x0'] - ordered[-1]['x0']) <= height:
                previous = ordered[-1]
                previous['textlines'].extend(box['textlines'])
                for key in ('x0', 'y0'):
                    previous[key] = min(previous[key], box[key])
                for key in ('x1', 'y1'):
                    previous[key] = max(previous[key], box[key])
                previous['bbox'] = [previous[key] for key in ('x0','y0','x1','y1')]
                source_groups[-1].append(index)
                continue
        ordered.append(box)
        source_groups.append([index])
    for box, sources in zip(ordered, source_groups):
        if len(sources) > 1:
            merges.append({'source_boxes':sources, 'prediction_index':text_region(box),
                           'bbox':box['bbox'], 'reason':'unfinished_same_region_adjacent_lines'})
    # Tesseract may emit a lower-page figure while finishing the left column,
    # before returning to upper right-column prose. Only move model-bound
    # objects proven wholly below EVERY prose box. Preserve the entire prose
    # permutation and the engine's order between coarse objects; never y/x sort.
    bound_sources = {binding['source_box'] for binding in bindings}
    prose_indices = [index for index, box in enumerate(ordered) if box['boxclass'] == 'text']
    terminal = []
    if prose_indices:
        prose_bottom = max(ordered[index]['y1'] for index in prose_indices)
        terminal = [index for index, (box, sources) in enumerate(zip(ordered, source_groups))
                    if box['boxclass'] in ('image', 'formula', 'table')
                    and all(source in bound_sources for source in sources)
                    and box['y0'] >= prose_bottom]
    terminal_set = set(terminal)
    # Do not move a proven trailing object across some OTHER unproven coarse
    # object. An overlapping/reversed terminal sequence is also unresolved.
    unrelated = [index for index, box in enumerate(ordered)
                 if box['boxclass'] != 'text' and index not in terminal_set]
    ordering = {'schema':'ocr-terminal-visual-order/v1', 'moves':[],
                'input_source_groups':copy.deepcopy(source_groups),
                'output_group_indices':list(range(len(ordered)))}
    if terminal and (any(index > terminal[0] for index in unrelated)
                     or any(ordered[left]['y1'] > ordered[right]['y0']
                            for left, right in zip(terminal, terminal[1:]))):
        failures.append({'reason':'unproven_terminal_visual_order',
                         'source_boxes':[source for index in terminal for source in source_groups[index]]})
    elif terminal:
        permutation = [index for index in range(len(ordered)) if index not in terminal_set] + terminal
        for destination, index in enumerate(permutation):
            if index in terminal_set and destination != index:
                ordering['moves'].append({'source_boxes':list(source_groups[index]),
                    'from_group':index, 'to_group':destination,
                    'prose_source_boxes':[source for p in prose_indices for source in source_groups[p]],
                    'prose_bottom':prose_bottom, 'visual_top':ordered[index]['y0'],
                    'reason':'model_bound_wholly_below_all_prose'})
        ordering['output_group_indices'] = permutation
        ordered = [ordered[index] for index in permutation]
        source_groups = [source_groups[index] for index in permutation]
    return ordered, {'schema':'ocr-visual-projection/v1', 'page':page_number,
        'page_bounds':list(page_rect), 'raster_size':list(raster_size),
        'regions':regions, 'bindings':bindings, 'failures':failures,
        'text_regions':text_regions, 'paragraph_merges':merges,
        'terminal_visual_order':ordering,
        'projected_source_groups':source_groups,
        'unanchored_visual_regions':unanchored, 'complete':not failures and not unanchored}

def project(layout):
    vocabulary = set()
    for page in layout["pages"]:
        for box in page["boxes"]:
            for line in (box.get("textlines") or []):
                vocabulary.update(re.findall(r"\b[\w]+(?:-[\w]+)*\b", line_text(line)))
    sections = [{"title": "Front matter", "blocks": []}]
    auxiliary = []
    regions = []
    ocr_evidence = []
    uncertain = 0
    abstract_headings = {"abstract", "摘要", "概要"}
    has_abstract = any(" ".join(line_text(l) for l in (b.get("textlines") or [])).strip().casefold() in abstract_headings
                       for p in layout["pages"] for b in p["boxes"] if b["boxclass"] == "section-header")
    body_started = False
    has_heading = any(b["boxclass"] == "section-header" for p in layout["pages"] for b in p["boxes"])
    if not has_heading:
        sections[0]["title"] = "Document"
    for page in layout["pages"]:
        page_no = page["page_number"]
        for index, box in enumerate(page["boxes"]):
            cls = box["boxclass"]
            lines = [line_text(line) for line in (box.get("textlines") or [])]
            text, count = join_lines(lines, vocabulary)
            uncertain += count
            bbox = [box[k] for k in ("x0", "y0", "x1", "y1")]
            region = {"page": page_no, "box": box.get('_original_box', index), "bbox": bbox, "class": cls}
            regions.append(region)
            if box.get("ocr_block") is not None:
                for line in box.get("textlines") or []:
                    for span in line.get("spans") or []:
                        ocr_evidence.append({"page": page_no, "block": span.get("ocr_block"),
                            "paragraph": span.get("ocr_paragraph"), "line": span.get("ocr_line"),
                            "text": span["text"], "bbox": span["bbox"],
                            "confidence": span.get("confidence")})
            if cls == "section-header" and text and (body_started or not has_abstract or text.casefold() in abstract_headings):
                body_started = True
                sections.append({"title": text, "blocks": []})
                continue
            if not text:
                continue  # non-text regions remain in the original PDF and region evidence
            spans = [s for line in (box.get("textlines") or []) for s in line["spans"] if s["text"].strip()]
            mono = bool(spans) and all(s["flags"] & 8 for s in spans)
            is_note = cls in ("footnote", "page-header", "page-footer", "caption")
            if spans and spans[0]["flags"] & 1:
                is_note = True
            if len(spans) > 1 and spans[0].get("origin", [0, 0])[1] < spans[1].get("origin", [0, 0])[1] and spans[0]["size"] <= spans[1]["size"] * 0.8:
                is_note = True
            if bbox[1] > page.get("height", float("inf")) * 0.9 and re.search(r"\b(conference|proceedings|copyright|arxiv)\b|©", text, re.IGNORECASE):
                is_note = True
            is_front = has_heading and not body_started
            kind = "paragraph" if cls == "text" and not mono and not is_note and not is_front else "preformatted"
            if cls == "table":
                kind = "table"
            if kind != "paragraph":
                text = "\n".join(lines).strip()
            block = {"text": text, "kind": kind, "parts": [{"start": 0, "end": len(text), "page": page_no}], "region": region}
            if is_note:
                auxiliary.append(block)
                continue
            blocks = sections[-1]["blocks"]
            # Rejoin layout fragments only for an unfinished prose paragraph across
            # a page/column break. Notes never interrupt this main-text stream.
            if blocks and kind == "paragraph" and blocks[-1]["kind"] == "paragraph":
                prev = blocks[-1]
                before = prev["text"]
                prior_region = prev["region"]
                crosses = page_no != prior_region["page"] or bbox[0] > prior_region["bbox"][2]
                if crosses and before and before[-1] not in '.!?。！？:;”"' and text[0].islower():
                    joined, extra = join_lines([before, text], vocabulary)
                    uncertain += extra
                    prefix = len(joined) - len(text)
                    # A removed wrap hyphen also shortens its original-page range.
                    prev["parts"][-1]["end"] = min(prev["parts"][-1]["end"], prefix)
                    prev["parts"].append({"start": prefix, "end": len(joined), "page": page_no})
                    prev["text"] = joined
                    prev["region"] = region
                    continue
            blocks.append(block)
    if auxiliary:
        sections.append({"title": "Page notes and captions", "blocks": auxiliary})
    return {"schema_version": VERSION, "engine": ENGINE, "page_count": layout["page_count"], "pages_without_text": sum(not any((b.get("textlines") or []) for b in p["boxes"]) for p in layout["pages"]), "sections": sections, "regions": regions, "ocr_evidence": ocr_evidence, "preserved_ambiguous_hyphens": uncertain}


def main():
    global OCR_CONFIG, EXPECTED_IDENTITY, RASTER_BUDGET, INPUT_SHA256, WORKER_SOURCE
    protocol_stdout = sys.stdout
    if len(sys.argv) == 5 and sys.argv[1] == '--visual-model':
        visual_model_child(sys.argv[2], int(sys.argv[3]), int(sys.argv[4]))
        return
    if len(sys.argv) == 4 and sys.argv[1] == '--verify-runtime':
        print(json.dumps(verify_runtime_package(sys.argv[2], sys.argv[3]), separators=(',', ':')))
        return
    if len(sys.argv) == 4 and sys.argv[1] == '--runtime-identity':
        config, expected_package = json.loads(sys.argv[2]), json.loads(sys.argv[3])
        actual_package = runtime_manifest('/run/reading-mcp-ocr-package.json')[1]
        if actual_package != expected_package or sys.executable != actual_package['python_path']:
            raise ValueError('OCR runtime package identity mismatch')
        print(json.dumps(runtime_identity(config, fingerprint_dependencies(config), actual_package), ensure_ascii=False, separators=(',', ':')))
        return
    max_pages, max_bytes, max_chars = map(int, sys.argv[1:4])
    if len(sys.argv) > 4 and sys.argv[4]:
        OCR_CONFIG = json.loads(sys.argv[4])
    if len(sys.argv) > 5 and sys.argv[5]:
        EXPECTED_IDENTITY = json.loads(sys.argv[5])
        if EXPECTED_IDENTITY.get("config") != OCR_CONFIG:
            raise ValueError("OCR config does not match expected identity")
    if len(sys.argv) > 6 and sys.argv[6]:
        WORKER_SOURCE = sys.argv[6]
    actual_dependencies = None
    if OCR_CONFIG.get("enabled"):
        RASTER_BUDGET = OcrRasterBudget()
        if EXPECTED_IDENTITY is None: raise ValueError("OCR expected identity is required")
        try:
            actual_dependencies = fingerprint_dependencies(OCR_CONFIG)
            if EXPECTED_IDENTITY.get("dependencies") != actual_dependencies:
                raise ValueError("OCR dependency identity mismatch")
            package = EXPECTED_IDENTITY.get('runtime_package')
            if package is not None:
                actual_package = runtime_manifest('/run/reading-mcp-ocr-package.json')[1]
                if package != actual_package or sys.executable != package['python_path']:
                    raise ValueError('OCR runtime package identity mismatch')
            actual_identity = runtime_identity(OCR_CONFIG, actual_dependencies, package)
            if EXPECTED_IDENTITY != actual_identity:
                raise ValueError("OCR policy/runtime identity mismatch")
        except OcrStageFailure:
            raise
        except (OSError, ValueError, RuntimeError, KeyError) as error:
            raise OcrStageFailure('OCR_UNAVAILABLE', 'OCR dependencies unavailable or identity mismatch') from error
    visual_results = {}
    visual_projections = {}
    visual_model_enabled = bool(OCR_CONFIG.get('enabled') and EXPECTED_IDENTITY
                                and EXPECTED_IDENTITY.get('runtime_package') is not None)
    if visual_model_enabled:
        if not WORKER_SOURCE:
            raise OcrStageFailure('OCR_UNAVAILABLE', 'visual model worker source is missing')
        # Render with the small PyMuPDF binding first, classify in a child, and
        # only then import pymupdf4llm. The child exits before layout/OCR work.
        import pymupdf
        raw_for_visual = sys.stdin.buffer.read(max_bytes + 1)
        if len(raw_for_visual) > max_bytes:
            raise OcrStageFailure('OCR_RESOURCE_LIMIT', 'PDF exceeds byte limit')
        INPUT_SHA256 = hashlib.sha256(raw_for_visual).hexdigest()
        with pymupdf.open(stream=raw_for_visual, filetype='pdf') as visual_doc:
            if visual_doc.needs_pass or not 0 < len(visual_doc) <= max_pages:
                raise OcrStageFailure('OCR_RESOURCE_LIMIT', 'invalid visual PDF page count')
            for visual_page in visual_doc:
                if RASTER_BUDGET is not None:
                    RASTER_BUDGET.reserve_raster(raster_pixel_count(visual_page, OCR_CONFIG['dpi']))
                pixmap = visual_page.get_pixmap(dpi=OCR_CONFIG['dpi'], colorspace=pymupdf.csRGB, alpha=False)
                child = subprocess.run([sys.executable, '-I', '-X', 'faulthandler', '-c',
                    WORKER_SOURCE, '--visual-model', '/opt/ocr-layout-model',
                    str(pixmap.width), str(pixmap.height)], input=pixmap.samples,
                    capture_output=True, timeout=min(15, page_time_remaining()), check=False)
                if child.returncode != 0 or len(child.stdout) > 4 * 1024 * 1024:
                    raise OcrStageFailure('OCR_UNAVAILABLE', 'visual model child failed')
                observation = json.loads(child.stdout)
                if (observation.get('schema') != 'ocr-visual-model-attempt/v1'
                        or observation.get('raster_size') != [pixmap.width, pixmap.height]
                        or observation.get('raster_sha256') != hashlib.sha256(pixmap.samples).hexdigest()):
                    raise OcrStageFailure('OCR_UNAVAILABLE', 'visual model raster identity mismatch')
                observation['page'] = visual_page.number + 1
                visual_results[visual_page.number] = observation
        # The bytes are retained as the canonical input for the normal parse;
        # the stream is not readable a second time after the pre-classification.
        visual_input = raw_for_visual
    else:
        visual_input = None
    for package in ("pymupdf", "pymupdf4llm", "pymupdf-layout"):
        try:
            version = importlib.metadata.version(package)
        except importlib.metadata.PackageNotFoundError as error:
            if OCR_CONFIG.get('enabled'):
                raise OcrStageFailure('OCR_UNAVAILABLE', 'pinned PDF dependencies unavailable') from error
            raise
        if version != "1.28.2":
            if OCR_CONFIG.get('enabled'):
                raise OcrStageFailure('OCR_UNAVAILABLE', 'pinned PDF dependency version mismatch')
            raise ValueError(f"{package} must be version 1.28.2; run setup-pdf-layout.sh")
    # Libraries may print status messages; reserve stdout for protocol output.
    with contextlib.redirect_stdout(sys.stderr):
        try:
            import pymupdf
            import pymupdf4llm
        except (ImportError, OSError) as error:
            if OCR_CONFIG.get('enabled'):
                raise OcrStageFailure('OCR_UNAVAILABLE', 'PDF native dependency import failed') from error
            raise
        pymupdf4llm.use_layout(True)
        raw = visual_input if visual_input is not None else sys.stdin.buffer.read(max_bytes + 1)
        if len(raw) > max_bytes:
            raise ValueError("PDF exceeds byte limit")
        INPUT_SHA256 = hashlib.sha256(raw).hexdigest()
        with pymupdf.open(stream=raw, filetype="pdf") as doc:
            if doc.needs_pass:
                raise ValueError("encrypted PDF requires a password")
            if len(doc) > max_pages and OCR_CONFIG.get('enabled'):
                raise OcrStageFailure('OCR_RESOURCE_LIMIT', 'PDF exceeds page limit')
            if not 0 < len(doc) <= max_pages:
                raise ValueError("PDF exceeds page limit or has no pages")
            layout = json.loads(pymupdf4llm.to_json(doc, use_ocr=False))
            if not OCR_CONFIG.get('enabled', False):
                RASTER_BUDGET = OcrRasterBudget()
                for page, page_layout in zip(doc, layout['pages']):
                    require_disabled_page_coverage(page, page_layout)
            if OCR_CONFIG.get("enabled", False):
                language = "+".join(OCR_CONFIG["languages"])
                for page, page_layout in zip(doc, layout["pages"]):
                    has_body_text = any((box.get("textlines") or []) and box.get("boxclass") not in ("page-footer", "page-header")
                                        for box in page_layout["boxes"])
                    has_image_region = bool(page.get_image_info()) or any(
                        box.get("boxclass") in ("image", "picture", "figure", "table")
                        for box in page_layout["boxes"])
                    if not has_body_text or has_image_region:
                        excluded = native_text_regions(page_layout)
                        projected_boxes, retry_diagnostic = _regional_ocr(page, excluded,
                            inspect_native=has_body_text, original_boxes=page_layout['boxes'])
                        if projected_boxes:
                            page_layout["boxes"] = projected_boxes
                        page_layout["ocr_retry_diagnostic"] = retry_diagnostic
                    if visual_model_enabled:
                        observation = visual_results.get(page.number)
                        if observation is None:
                            raise OcrStageFailure('OCR_UNAVAILABLE', 'missing visual page observation')
                        selected = page_layout['boxes']
                        predictions = [attempt['res']['boxes'] for attempt in observation['attempts']]
                        if len(predictions) != 1:
                            raise OcrStageFailure('OCR_UNAVAILABLE', 'visual model attempt count mismatch')
                        selected, projection = project_visual_observations(
                            page.number + 1, [0, 0, page.rect.width, page.rect.height],
                            observation['raster_size'], selected, predictions[0])
                        if not projection['complete']:
                            raise ValueError('visual projection unresolved')
                        page_layout['boxes'] = selected
                        page_layout['ocr_visual_projection'] = projection
                        visual_projections[page.number] = projection
        observations = [p["ocr_retry_diagnostic"] for p in layout["pages"]
                        if "ocr_retry_diagnostic" in p]
        if any(not observation["complete"] for observation in observations):
            json.dump({"schema": "ocr-worker-failure/v1",
                       "original_sha256": hashlib.sha256(raw).hexdigest(),
                       "runtime_identity_sha256": actual_identity["sha256"],
                       "ocr_attempts": observations,
                       "error": "OCR_NO_SUPPORTED_PROJECTION"}, protocol_stdout, ensure_ascii=False)
            raise ValueError("OCR geometric conflict remains unresolved")
        result = project(layout)
        result["ocr_attempts"] = observations
        if visual_model_enabled:
            result["ocr_visual_attempts"] = [
                dict(visual_results[index], projection=visual_projections[index])
                for index in sorted(visual_results)]
        if OCR_CONFIG.get("enabled", False):
            result["ocr_derivation"] = {"schema": "ocr-derivation/v3", "original_sha256": hashlib.sha256(raw).hexdigest(),
                "inspection_policy": INSPECTION_POLICY,
                "retry_policy": RETRY_POLICY, "runtime_identity_sha256": actual_identity["sha256"],
                "engine_sha256": next(d["sha256"] for d in actual_dependencies if d["name"] == "engine"), "model_sha256": [d["sha256"] for d in actual_dependencies if d["name"].startswith("model:")],
                "library_sha256": [d["sha256"] for d in actual_dependencies if d["name"].startswith("library:")], "languages": OCR_CONFIG["languages"], "dpi": OCR_CONFIG["dpi"], "oem": OCR_CONFIG["oem"], "psm": OCR_CONFIG["psm"],
                "detector_version": OCR_CONFIG["detector_version"], "protocol_version": OCR_CONFIG["protocol_version"],
                "operator_revision": OCR_CONFIG["operator_revision"], "pages": []}
        if not any(b["kind"] == "paragraph" for s in result["sections"] for b in s["blocks"]):
            if OCR_CONFIG.get("enabled", False):
                json.dump({"schema": "ocr-worker-failure/v1",
                           "original_sha256": hashlib.sha256(raw).hexdigest(),
                           "runtime_identity_sha256": actual_identity["sha256"],
                           "error": "OCR_NO_SUPPORTED_PROJECTION",
                           "ocr_attempts": observations}, protocol_stdout, ensure_ascii=False)
                raise ValueError("no supported prose text in inspected original pages")
            raise ValueError("no supported prose text; scanned/image-only PDFs need OCR (not enabled)")
        if sum(len(b["text"]) for s in result["sections"] for b in s["blocks"]) > max_chars:
            if OCR_CONFIG.get('enabled'):
                raise OcrStageFailure('OCR_RESOURCE_LIMIT', 'PDF exceeds normalized text limit')
            raise ValueError("PDF exceeds normalized text limit")
    json.dump(result, sys.stdout, ensure_ascii=False, separators=(",", ":"))


def run():
    try:
        main()
        return 0
    except OcrRequired:
        json.dump({'schema': 'pdf-layout-ocr-required/v1',
                   'original_sha256': INPUT_SHA256, 'error': 'OCR_REQUIRED'},
                  sys.stdout, separators=(',', ':'))
        print('PDF layout requires enabled local OCR inspection', file=sys.stderr)
        return 1
    except OcrStageFailure as error:
        if OCR_CONFIG.get('enabled') and EXPECTED_IDENTITY is not None:
            # Before input is consumed there is deliberately no source-hash
            # claim. Ingestion failures bind the actual complete input bytes.
            json.dump({'schema': 'ocr-worker-failure/v2',
                'stage': 'dependency' if INPUT_SHA256 is None else 'ingestion',
                'original_sha256': INPUT_SHA256,
                'runtime_identity_sha256': EXPECTED_IDENTITY['sha256'],
                'error': error.code}, sys.stdout, separators=(',', ':'))
        print(f"PDF layout failed: {error}", file=sys.stderr)
        return 1
    except Exception as error:
        print(f"PDF layout failed: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(run())
