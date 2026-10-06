"""Build a deterministic source-only archive; never package runtime research data."""
import argparse
import gzip
import tarfile
from pathlib import Path

PROJECT_ROOT = Path(__file__).resolve().parents[1]
ROOT_FILES = (
    'AGENTS.md', 'README.md', '.gitignore', 'Cargo.toml', 'Cargo.lock',
    'pyproject.toml', 'package.json', 'package-lock.json', 'tsconfig.json',
)
SOURCE_PATTERNS = (
    'orphanwiki/*.py', 'src/*.rs', 'config/*.json',
    'web/*.html', 'web/src/*.ts', 'web/src/*.css',
    'scripts/*.py', 'scripts/*.mjs',
    'tests/python/*.py', 'tests/web/*.cjs', 'benchmarks/*.py',
)


def package_sources(output):
    output = Path(output).resolve()
    if not output.name.endswith('.tar.gz'):
        raise ValueError('Il pacchetto deve avere estensione .tar.gz')
    sources = {PROJECT_ROOT / name for name in ROOT_FILES}
    for pattern in SOURCE_PATTERNS:
        sources.update(PROJECT_ROOT.glob(pattern))
    for source in sources:
        if source.is_symlink() or not source.is_file():
            raise ValueError(f'Sorgente mancante o collegamento simbolico: {source}')
    output.parent.mkdir(parents=True, exist_ok=True)
    prepared = output.with_suffix('.gz.tmp')
    try:
        with prepared.open('wb') as stream:
            with gzip.GzipFile(filename='', fileobj=stream, mode='wb', mtime=0) as compressed:
                with tarfile.open(fileobj=compressed, mode='w', format=tarfile.PAX_FORMAT) as archive:
                    for source in sorted(sources):
                        relative = source.relative_to(PROJECT_ROOT).as_posix()
                        info = archive.gettarinfo(str(source), arcname=relative)
                        info.uid = info.gid = info.mtime = 0
                        info.uname = info.gname = ''
                        info.mode = 0o644
                        info.pax_headers = {}
                        with source.open('rb') as content:
                            archive.addfile(info, content)
        prepared.replace(output)
    finally:
        prepared.unlink(missing_ok=True)
    return len(sources)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', default='data/orphanwiki-paws.tar.gz')
    args = parser.parse_args()
    count = package_sources(args.output)
    print(f'Pacchetto PAWS: {args.output}; {count} file sorgente')
