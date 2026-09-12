"""Public frozen F07 inside a candidate RootDirectory, without host Python/libs."""
import hashlib
import importlib.util
import json
from pathlib import Path
import subprocess
import sys
import time


def main():
    source = Path('/opt/ocr-smoke/pdf_layout_worker.py')
    spec = importlib.util.spec_from_file_location('offline_worker', source)
    worker = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(worker)
    config = {'enabled': True, 'engine_path': '/usr/bin/tesseract',
        'tessdata_path': '/usr/share/tesseract-ocr/5/tessdata', 'languages': ['chi_sim'],
        'operator_revision': '1', 'dpi': 300, 'oem': 1, 'psm': 3,
        'detector_version': 'pdf-layout/v1', 'protocol_version': 'pdf-layout/v1'}
    dependencies = worker.fingerprint_dependencies(config)
    identity = worker.runtime_identity(config, dependencies)
    raw = Path('/opt/ocr-smoke/F07.pdf').read_bytes()
    deadline = time.monotonic() + 50
    probes = []
    for module, code in [('pymupdf', 'import pymupdf'),
                         ('onnxruntime', 'import onnxruntime'),
                         ('layout', 'import pymupdf4llm; pymupdf4llm.use_layout(True)')]:
        try:
            probe = subprocess.run([sys.executable, '-I', '-X', 'faulthandler', '-c', code],
                capture_output=True, timeout=max(.1, min(15, deadline - time.monotonic())))
            record = {'module': module, 'returncode': probe.returncode,
                      'stderr': probe.stderr.decode(errors='replace')[-8192:]}
        except subprocess.TimeoutExpired as error:
            record = {'module': module, 'error': 'timeout',
                      'stderr': (error.stderr or b'').decode(errors='replace')[-8192:]}
        probes.append(record)
        print(json.dumps({'private_runtime_import_probe': record}), file=sys.stderr, flush=True)
    try:
        result = subprocess.run([sys.executable, '-I', '-X', 'faulthandler', str(source), '1000', str(64 * 1024 * 1024),
        str(4 * 1024 * 1024), json.dumps(config), json.dumps(identity)], input=raw,
            capture_output=True, timeout=max(.1, deadline - time.monotonic()))
    except subprocess.TimeoutExpired as error:
        print(json.dumps({'schema': 'ocr-private-runtime-failure/v1', 'error': 'timeout',
            'imports': probes, 'stderr': (error.stderr or b'').decode(errors='replace')[-8192:]}), flush=True)
        raise SystemExit(1)
    if result.returncode or any(probe.get('returncode') != 0 for probe in probes):
        print(json.dumps({'schema': 'ocr-private-runtime-failure/v1',
            'worker_returncode': result.returncode, 'imports': probes,
            'stderr': result.stderr.decode(errors='replace')[-8192:],
            'filesystem_presence': {path: Path(path).exists() for path in
                ('/proc/self/maps', '/sys/devices/system/cpu', '/etc/passwd', '/etc/group', '/etc/ld.so.cache')}}), flush=True)
        raise SystemExit(1)
    payload = json.loads(result.stdout)
    paragraphs = [block['text'] for section in payload['sections'] for block in section['blocks']
                  if block['kind'] == 'paragraph']
    assert len(paragraphs) == 4 and all(paragraphs)
    assert payload['ocr_derivation']['original_sha256'] == hashlib.sha256(raw).hexdigest()
    assert payload['ocr_derivation']['runtime_identity_sha256'] == identity['sha256']
    assert len(payload['ocr_attempts']) == 1 and len(payload['ocr_attempts'][0]['attempts']) == 2
    assert payload['ocr_attempts'][0]['complete']
    print(json.dumps({'schema': 'ocr-private-runtime-smoke/v1',
        'scope': 'candidate rooted Python and OCR chain only; not full Rust publication or release acceptance',
        'imports': probes,
        'python_version': sys.version, 'python_cache_tag': sys.implementation.cache_tag,
        'python_executable_sha256': hashlib.sha256(Path(sys.executable).resolve().read_bytes()).hexdigest(),
        'worker_sha256': hashlib.sha256(source.read_bytes()).hexdigest(),
        'identity': identity, 'canonical_paragraphs': paragraphs,
        'raw_sha256': hashlib.sha256(raw).hexdigest()}, ensure_ascii=False), flush=True)


if __name__ == '__main__':
    main()
